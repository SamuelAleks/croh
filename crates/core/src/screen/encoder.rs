//! Frame encoding for screen streaming.
//!
//! This module provides frame encoders that compress captured frames
//! before transmission over the network.
//!
//! ## Encoder Selection
//!
//! Encoders are selected based on the `ScreenCompression` setting:
//! - `Raw`: No compression, useful for testing and debugging
//! - `Png`: Lossless PNG compression (good for static content)
//! - `H264`: Not yet implemented (future: video codec support)
//!
//! ## Quality Settings
//!
//! The `ScreenQuality` setting affects compression:
//! - `Fast`: Prioritize encoding speed (lower compression)
//! - `Balanced`: Balance between speed and quality
//! - `Quality`: Prioritize visual quality (higher compression)
//! - `Auto`: Adapt based on network conditions

use std::time::Instant;

use crate::error::{Error, Result};
use crate::iroh::protocol::{ScreenCompression, ScreenQuality};

use super::types::CapturedFrame;

/// Encoded frame ready for transmission.
#[derive(Debug)]
pub struct EncodedFrame {
    /// Encoded frame data
    pub data: Vec<u8>,
    /// Frame width
    pub width: u32,
    /// Frame height
    pub height: u32,
    /// Compression format used
    pub compression: ScreenCompression,
    /// Whether this is a keyframe (can be decoded independently)
    pub is_keyframe: bool,
    /// Original uncompressed size
    pub original_size: usize,
    /// Encoding time in microseconds
    pub encode_time_us: u64,
}

impl EncodedFrame {
    /// Get the compression ratio (original_size / compressed_size).
    pub fn compression_ratio(&self) -> f32 {
        if self.data.is_empty() {
            return 0.0;
        }
        self.original_size as f32 / self.data.len() as f32
    }
}

/// Frame encoder trait.
///
/// Implementations provide different compression algorithms for screen frames.
pub trait FrameEncoder: Send {
    /// Get the encoder name for logging.
    fn name(&self) -> &'static str;

    /// Get the compression format this encoder produces.
    fn compression(&self) -> ScreenCompression;

    /// Encode a captured frame.
    fn encode(&mut self, frame: &CapturedFrame) -> Result<EncodedFrame>;

    /// Force the next frame to be a keyframe (for video codecs).
    fn force_keyframe(&mut self);

    /// Set the quality preset.
    fn set_quality(&mut self, quality: ScreenQuality);

    /// Set the target bitrate in kbps (for video codecs).
    fn set_bitrate(&mut self, _kbps: u32) {
        // Default no-op for image encoders
    }
}

/// Raw encoder - no compression, passes through frames as-is.
///
/// This is useful for:
/// - Testing and debugging
/// - Very high bandwidth local connections
/// - Baseline performance measurement
pub struct RawEncoder {
    quality: ScreenQuality,
}

impl RawEncoder {
    /// Create a new raw encoder.
    pub fn new() -> Self {
        Self {
            quality: ScreenQuality::default(),
        }
    }
}

impl Default for RawEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameEncoder for RawEncoder {
    fn name(&self) -> &'static str {
        "Raw"
    }

    fn compression(&self) -> ScreenCompression {
        ScreenCompression::Raw
    }

    fn encode(&mut self, frame: &CapturedFrame) -> Result<EncodedFrame> {
        let start = Instant::now();

        // Convert to RGBA if needed
        let data = frame.to_rgba();
        let original_size = data.len();

        Ok(EncodedFrame {
            data,
            width: frame.width,
            height: frame.height,
            compression: ScreenCompression::Raw,
            is_keyframe: true, // Raw frames are always independent
            original_size,
            encode_time_us: start.elapsed().as_micros() as u64,
        })
    }

    fn force_keyframe(&mut self) {
        // No-op for raw encoder - all frames are keyframes
    }

    fn set_quality(&mut self, quality: ScreenQuality) {
        self.quality = quality;
    }
}

/// PNG encoder - lossless compression using miniz.
///
/// Good for:
/// - Static content (text, documents)
/// - When lossless quality is required
/// - Screenshots and single frames
///
/// Uses the `png` crate with configurable compression level.
/// Performance optimization: reuses RGBA buffer across frames.
pub struct PngEncoder {
    quality: ScreenQuality,
    compression_level: png::Compression,
    /// Reusable RGBA conversion buffer
    rgba_buf: Vec<u8>,
}

impl PngEncoder {
    /// Create a new PNG encoder.
    pub fn new() -> Self {
        Self {
            quality: ScreenQuality::default(),
            compression_level: png::Compression::Fast,
            rgba_buf: Vec::new(),
        }
    }

    /// Update compression level based on quality setting.
    fn update_compression_level(&mut self) {
        self.compression_level = match self.quality {
            ScreenQuality::Fast => png::Compression::Fast,
            ScreenQuality::Balanced => png::Compression::Default,
            ScreenQuality::Quality => png::Compression::Best,
            ScreenQuality::Auto => png::Compression::Default,
        };
    }
}

impl Default for PngEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameEncoder for PngEncoder {
    fn name(&self) -> &'static str {
        "PNG"
    }

    fn compression(&self) -> ScreenCompression {
        // PNG is closest to "Raw" in the protocol since we don't have a PNG variant
        // In practice, the receiver will detect PNG from the data header
        ScreenCompression::Raw
    }

    fn encode(&mut self, frame: &CapturedFrame) -> Result<EncodedFrame> {
        let start = Instant::now();

        // Convert to RGBA using reusable buffer
        frame.to_rgba_into(&mut self.rgba_buf);
        let original_size = self.rgba_buf.len();

        // Encode to PNG
        let mut png_data = Vec::with_capacity(original_size / 4); // Rough estimate

        {
            let mut encoder = png::Encoder::new(&mut png_data, frame.width, frame.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(self.compression_level);

            let mut writer = encoder
                .write_header()
                .map_err(|e| Error::Screen(format!("PNG header error: {}", e)))?;

            writer
                .write_image_data(&self.rgba_buf)
                .map_err(|e| Error::Screen(format!("PNG encode error: {}", e)))?;
        }

        Ok(EncodedFrame {
            data: png_data,
            width: frame.width,
            height: frame.height,
            compression: ScreenCompression::Raw, // Protocol doesn't have PNG, use Raw
            is_keyframe: true,
            original_size,
            encode_time_us: start.elapsed().as_micros() as u64,
        })
    }

    fn force_keyframe(&mut self) {
        // No-op for PNG encoder - all frames are keyframes
    }

    fn set_quality(&mut self, quality: ScreenQuality) {
        self.quality = quality;
        self.update_compression_level();
    }
}

/// Zstd encoder - fast lossless compression.
///
/// Good for:
/// - Screen streaming with good compression ratio
/// - Fast encoding/decoding
/// - Better than PNG for repeated content
///
/// Performance optimizations:
/// - Reuses header buffer across frames to reduce allocations
/// - Reuses RGBA conversion buffer to avoid allocation per frame
/// - Pre-allocates output buffer based on expected compression ratio
pub struct ZstdEncoder {
    quality: ScreenQuality,
    compression_level: i32,
    /// Reusable header buffer (12 bytes: magic + width + height)
    header_buf: Vec<u8>,
    /// Reusable RGBA conversion buffer
    rgba_buf: Vec<u8>,
    /// Last frame size for pre-allocation hints
    last_output_size: usize,
}

impl ZstdEncoder {
    /// Create a new Zstd encoder.
    pub fn new() -> Self {
        let mut encoder = Self {
            quality: ScreenQuality::default(),
            compression_level: 1, // Default fast
            header_buf: Vec::with_capacity(12),
            rgba_buf: Vec::new(),
            last_output_size: 0,
        };
        encoder.update_compression_level();
        encoder
    }

    /// Update compression level based on quality setting.
    fn update_compression_level(&mut self) {
        self.compression_level = match self.quality {
            ScreenQuality::Fast => 1,     // Fastest
            ScreenQuality::Balanced => 3, // Good balance
            ScreenQuality::Quality => 9,  // High compression
            ScreenQuality::Auto => 3,
        };
    }
}

impl Default for ZstdEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameEncoder for ZstdEncoder {
    fn name(&self) -> &'static str {
        "Zstd"
    }

    fn compression(&self) -> ScreenCompression {
        ScreenCompression::Raw // Protocol doesn't have Zstd variant
    }

    fn encode(&mut self, frame: &CapturedFrame) -> Result<EncodedFrame> {
        let start = Instant::now();

        // Convert to RGBA using reusable buffer
        frame.to_rgba_into(&mut self.rgba_buf);
        let original_size = self.rgba_buf.len();

        // Reuse header buffer: magic + width + height
        self.header_buf.clear();
        self.header_buf.extend_from_slice(b"ZRGB"); // Magic bytes
        self.header_buf.extend_from_slice(&frame.width.to_le_bytes());
        self.header_buf.extend_from_slice(&frame.height.to_le_bytes());

        // Compress with zstd
        let compressed = zstd::encode_all(&self.rgba_buf[..], self.compression_level)
            .map_err(|e| Error::Screen(format!("Zstd encode error: {}", e)))?;

        // Pre-allocate output buffer based on previous frame size if available,
        // otherwise estimate based on header + compressed size
        let estimated_size = if self.last_output_size > 0 {
            self.last_output_size
        } else {
            12 + compressed.len()
        };

        // Combine header and compressed data with pre-allocation
        let mut data = Vec::with_capacity(estimated_size);
        data.extend_from_slice(&self.header_buf);
        data.extend(compressed);

        // Update hint for next frame
        self.last_output_size = data.len();

        Ok(EncodedFrame {
            data,
            width: frame.width,
            height: frame.height,
            compression: ScreenCompression::Raw,
            is_keyframe: true,
            original_size,
            encode_time_us: start.elapsed().as_micros() as u64,
        })
    }

    fn force_keyframe(&mut self) {
        // No-op - all frames are keyframes
    }

    fn set_quality(&mut self, quality: ScreenQuality) {
        self.quality = quality;
        self.update_compression_level();
    }
}

/// Create an encoder based on the compression setting.
///
/// When the `ffmpeg` feature is enabled and H.264 is requested, this will
/// attempt to create an FFmpeg encoder with hardware acceleration. Falls
/// back to Zstd if FFmpeg is unavailable or fails.
pub fn create_encoder(
    compression: ScreenCompression,
    quality: ScreenQuality,
) -> Box<dyn FrameEncoder> {
    create_encoder_with_size(compression, quality, 1920, 1080)
}

/// Create an encoder with specific frame dimensions.
///
/// This variant is preferred when the frame size is known, as it allows
/// FFmpeg encoders to pre-allocate buffers correctly.
#[allow(unused_variables)]
pub fn create_encoder_with_size(
    compression: ScreenCompression,
    quality: ScreenQuality,
    width: u32,
    height: u32,
) -> Box<dyn FrameEncoder> {
    let mut encoder: Box<dyn FrameEncoder> = match compression {
        ScreenCompression::Raw => Box::new(RawEncoder::new()),

        ScreenCompression::H264 => {
            #[cfg(feature = "ffmpeg")]
            {
                match super::ffmpeg::FfmpegEncoder::new(width, height, quality) {
                    Ok(enc) => {
                        tracing::info!(
                            "Created FFmpeg H.264 encoder: {} ({})",
                            enc.name(),
                            enc.encoder_name()
                        );
                        Box::new(enc)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "FFmpeg H.264 encoder unavailable: {}. Falling back to Zstd.",
                            e
                        );
                        Box::new(ZstdEncoder::new())
                    }
                }
            }
            #[cfg(not(feature = "ffmpeg"))]
            {
                tracing::debug!("FFmpeg feature not enabled, using Zstd fallback for H.264");
                Box::new(ZstdEncoder::new())
            }
        }

        // Other video codecs fall back to Zstd for now
        ScreenCompression::WebP | ScreenCompression::Vp9 | ScreenCompression::Av1 => {
            Box::new(ZstdEncoder::new())
        }
    };

    encoder.set_quality(quality);
    encoder
}

/// Select the best available encoder for the given settings.
///
/// Prefers H.264 with hardware acceleration when available, falls back to Zstd.
pub fn auto_select_encoder(quality: ScreenQuality) -> Box<dyn FrameEncoder> {
    auto_select_encoder_with_size(quality, 1920, 1080)
}

/// Select the best available encoder with specific frame dimensions.
#[allow(unused_variables)]
pub fn auto_select_encoder_with_size(
    quality: ScreenQuality,
    width: u32,
    height: u32,
) -> Box<dyn FrameEncoder> {
    #[cfg(feature = "ffmpeg")]
    {
        // Try H.264 first for best compression
        match super::ffmpeg::FfmpegEncoder::new(width, height, quality) {
            Ok(enc) => {
                tracing::info!(
                    "Auto-selected FFmpeg H.264 encoder: {} ({})",
                    enc.name(),
                    enc.encoder_name()
                );
                return Box::new(enc);
            }
            Err(e) => {
                tracing::debug!("FFmpeg encoder unavailable: {}, using Zstd", e);
            }
        }
    }

    // Fall back to Zstd
    let mut encoder = Box::new(ZstdEncoder::new());
    encoder.set_quality(quality);
    encoder
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::types::PixelFormat;

    fn create_test_frame(width: u32, height: u32) -> CapturedFrame {
        let bytes_per_pixel = 4;
        let stride = width * bytes_per_pixel;
        let size = (stride * height) as usize;

        // Create a gradient pattern
        let mut data = vec![0u8; size];
        for y in 0..height {
            for x in 0..width {
                let offset = ((y * stride) + (x * bytes_per_pixel)) as usize;
                data[offset] = (x % 256) as u8; // R
                data[offset + 1] = (y % 256) as u8; // G
                data[offset + 2] = 128; // B
                data[offset + 3] = 255; // A
            }
        }

        CapturedFrame::new(width, height, PixelFormat::Rgba8, data, stride)
    }

    #[test]
    fn test_raw_encoder() {
        let mut encoder = RawEncoder::new();
        let frame = create_test_frame(100, 100);

        let encoded = encoder.encode(&frame).unwrap();

        assert_eq!(encoded.width, 100);
        assert_eq!(encoded.height, 100);
        assert!(encoded.is_keyframe);
        assert_eq!(encoded.compression, ScreenCompression::Raw);
        assert_eq!(encoded.data.len(), 100 * 100 * 4);
        assert_eq!(encoded.compression_ratio(), 1.0);
    }

    #[test]
    fn test_png_encoder() {
        let mut encoder = PngEncoder::new();
        let frame = create_test_frame(100, 100);

        let encoded = encoder.encode(&frame).unwrap();

        assert_eq!(encoded.width, 100);
        assert_eq!(encoded.height, 100);
        assert!(encoded.is_keyframe);
        // PNG should compress the gradient pattern
        assert!(encoded.data.len() < 100 * 100 * 4);
        assert!(encoded.compression_ratio() > 1.0);

        // Verify it's valid PNG (starts with PNG signature)
        assert_eq!(&encoded.data[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }

    #[test]
    fn test_zstd_encoder() {
        let mut encoder = ZstdEncoder::new();
        let frame = create_test_frame(100, 100);

        let encoded = encoder.encode(&frame).unwrap();

        assert_eq!(encoded.width, 100);
        assert_eq!(encoded.height, 100);
        assert!(encoded.is_keyframe);
        // Zstd should compress well
        assert!(encoded.data.len() < 100 * 100 * 4);
        assert!(encoded.compression_ratio() > 1.0);

        // Verify header
        assert_eq!(&encoded.data[0..4], b"ZRGB");
    }

    #[test]
    fn test_quality_settings() {
        let mut encoder = ZstdEncoder::new();

        encoder.set_quality(ScreenQuality::Fast);
        assert_eq!(encoder.compression_level, 1);

        encoder.set_quality(ScreenQuality::Quality);
        assert_eq!(encoder.compression_level, 9);
    }

    #[test]
    fn test_create_encoder() {
        let encoder = create_encoder(ScreenCompression::Raw, ScreenQuality::Fast);
        assert_eq!(encoder.name(), "Raw");

        let encoder = create_encoder(ScreenCompression::H264, ScreenQuality::Balanced);
        assert_eq!(encoder.name(), "Zstd"); // Falls back to Zstd until H264 is implemented
    }
}
