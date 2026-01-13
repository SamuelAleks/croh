//! Frame decoding for screen streaming.
//!
//! This module provides frame decoders that decompress received frames
//! for display on the viewer side.
//!
//! ## Decoder Selection
//!
//! Decoders are matched based on frame data magic bytes or metadata:
//! - `ZRGB` header: Zstd-compressed RGBA data
//! - PNG signature: PNG image data
//! - Raw: Uncompressed RGBA data

use std::time::Instant;

use crate::error::{Error, Result};
use crate::iroh::protocol::ScreenCompression;

/// Decoded frame ready for display.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// RGBA pixel data (4 bytes per pixel).
    pub data: Vec<u8>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Decode time in microseconds.
    pub decode_time_us: u64,
}

impl DecodedFrame {
    /// Get the expected data size for the given dimensions.
    pub fn expected_size(width: u32, height: u32) -> usize {
        (width * height * 4) as usize
    }

    /// Validate that the data size matches the dimensions.
    pub fn is_valid(&self) -> bool {
        self.data.len() == Self::expected_size(self.width, self.height)
    }
}

/// Frame decoder trait.
///
/// Implementations provide different decompression algorithms for screen frames.
pub trait FrameDecoder: Send {
    /// Get the decoder name for logging.
    fn name(&self) -> &'static str;

    /// Check if this decoder can handle the given data.
    fn can_decode(&self, data: &[u8]) -> bool;

    /// Decode a frame.
    fn decode(&mut self, data: &[u8], width: u32, height: u32) -> Result<DecodedFrame>;
}

/// Raw decoder - no decompression, passes through frames as-is.
pub struct RawDecoder;

impl RawDecoder {
    /// Create a new raw decoder.
    pub fn new() -> Self {
        Self
    }
}

impl Default for RawDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder for RawDecoder {
    fn name(&self) -> &'static str {
        "Raw"
    }

    fn can_decode(&self, data: &[u8]) -> bool {
        // Raw decoder can handle anything that's not recognized as another format
        // Check it's not PNG or ZRGB
        if data.len() >= 8 {
            // PNG signature
            if data[0..8] == [137, 80, 78, 71, 13, 10, 26, 10] {
                return false;
            }
            // ZRGB header
            if &data[0..4] == b"ZRGB" {
                return false;
            }
        }
        true
    }

    fn decode(&mut self, data: &[u8], width: u32, height: u32) -> Result<DecodedFrame> {
        let start = Instant::now();

        let expected_size = DecodedFrame::expected_size(width, height);
        if data.len() != expected_size {
            return Err(Error::Screen(format!(
                "Raw frame size mismatch: expected {} bytes for {}x{}, got {}",
                expected_size,
                width,
                height,
                data.len()
            )));
        }

        Ok(DecodedFrame {
            data: data.to_vec(),
            width,
            height,
            decode_time_us: start.elapsed().as_micros() as u64,
        })
    }
}

/// PNG decoder - lossless decompression.
pub struct PngDecoder;

impl PngDecoder {
    /// Create a new PNG decoder.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PngDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder for PngDecoder {
    fn name(&self) -> &'static str {
        "PNG"
    }

    fn can_decode(&self, data: &[u8]) -> bool {
        // Check for PNG signature
        data.len() >= 8 && data[0..8] == [137, 80, 78, 71, 13, 10, 26, 10]
    }

    fn decode(&mut self, data: &[u8], _width: u32, _height: u32) -> Result<DecodedFrame> {
        let start = Instant::now();

        let decoder = png::Decoder::new(data);
        let mut reader = decoder
            .read_info()
            .map_err(|e| Error::Screen(format!("PNG header error: {}", e)))?;

        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| Error::Screen(format!("PNG decode error: {}", e)))?;

        let width = info.width;
        let height = info.height;

        // Convert to RGBA if needed
        let rgba_data = match info.color_type {
            png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
            png::ColorType::Rgb => {
                // Convert RGB to RGBA
                let rgb = &buf[..info.buffer_size()];
                let mut rgba = Vec::with_capacity((width * height * 4) as usize);
                for chunk in rgb.chunks(3) {
                    rgba.extend_from_slice(chunk);
                    rgba.push(255); // Alpha
                }
                rgba
            }
            png::ColorType::Grayscale => {
                // Convert grayscale to RGBA
                let gray = &buf[..info.buffer_size()];
                let mut rgba = Vec::with_capacity((width * height * 4) as usize);
                for &g in gray {
                    rgba.extend_from_slice(&[g, g, g, 255]);
                }
                rgba
            }
            png::ColorType::GrayscaleAlpha => {
                // Convert grayscale+alpha to RGBA
                let ga = &buf[..info.buffer_size()];
                let mut rgba = Vec::with_capacity((width * height * 4) as usize);
                for chunk in ga.chunks(2) {
                    let g = chunk[0];
                    let a = chunk[1];
                    rgba.extend_from_slice(&[g, g, g, a]);
                }
                rgba
            }
            _ => {
                return Err(Error::Screen(format!(
                    "Unsupported PNG color type: {:?}",
                    info.color_type
                )));
            }
        };

        Ok(DecodedFrame {
            data: rgba_data,
            width,
            height,
            decode_time_us: start.elapsed().as_micros() as u64,
        })
    }
}

/// Zstd decoder - fast lossless decompression.
///
/// Performance optimizations:
/// - Reuses decompression buffer across frames
/// - Pre-allocates based on expected frame size
pub struct ZstdDecoder {
    /// Reusable decompression buffer
    decompress_buf: Vec<u8>,
}

impl ZstdDecoder {
    /// Create a new Zstd decoder.
    pub fn new() -> Self {
        Self {
            decompress_buf: Vec::new(),
        }
    }
}

impl Default for ZstdDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder for ZstdDecoder {
    fn name(&self) -> &'static str {
        "Zstd"
    }

    fn can_decode(&self, data: &[u8]) -> bool {
        // Check for ZRGB header
        data.len() >= 12 && &data[0..4] == b"ZRGB"
    }

    fn decode(&mut self, data: &[u8], _width: u32, _height: u32) -> Result<DecodedFrame> {
        let start = Instant::now();

        if data.len() < 12 {
            return Err(Error::Screen("Zstd frame too small".into()));
        }

        // Parse header: ZRGB + width (4 bytes) + height (4 bytes)
        if &data[0..4] != b"ZRGB" {
            return Err(Error::Screen("Invalid Zstd frame header".into()));
        }

        let width = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let height = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

        // Pre-allocate buffer for expected decompressed size
        let expected_size = DecodedFrame::expected_size(width, height);
        self.decompress_buf.clear();
        self.decompress_buf.reserve(expected_size);

        // Decompress using streaming decoder into our buffer
        let compressed = &data[12..];
        let mut decoder = zstd::stream::Decoder::new(compressed)
            .map_err(|e| Error::Screen(format!("Zstd decoder init error: {}", e)))?;

        std::io::copy(&mut decoder, &mut self.decompress_buf)
            .map_err(|e| Error::Screen(format!("Zstd decode error: {}", e)))?;

        if self.decompress_buf.len() != expected_size {
            return Err(Error::Screen(format!(
                "Zstd decompressed size mismatch: expected {}, got {}",
                expected_size,
                self.decompress_buf.len()
            )));
        }

        // We need to return owned data, so swap with a new buffer
        let mut result_data = Vec::with_capacity(expected_size);
        std::mem::swap(&mut result_data, &mut self.decompress_buf);

        Ok(DecodedFrame {
            data: result_data,
            width,
            height,
            decode_time_us: start.elapsed().as_micros() as u64,
        })
    }
}

/// Auto-detecting decoder that selects the appropriate decoder based on data.
///
/// When the `ffmpeg` feature is enabled, this also supports H.264 decoding.
pub struct AutoDecoder {
    raw: RawDecoder,
    png: PngDecoder,
    zstd: ZstdDecoder,
    #[cfg(feature = "ffmpeg")]
    h264: Option<super::ffmpeg::FfmpegDecoder>,
}

impl AutoDecoder {
    /// Create a new auto-detecting decoder.
    pub fn new() -> Self {
        Self {
            raw: RawDecoder::new(),
            png: PngDecoder::new(),
            zstd: ZstdDecoder::new(),
            #[cfg(feature = "ffmpeg")]
            h264: super::ffmpeg::FfmpegDecoder::new().ok(),
        }
    }

    /// Check if H.264 decoding is available.
    #[cfg(feature = "ffmpeg")]
    pub fn has_h264_support(&self) -> bool {
        self.h264.is_some()
    }

    /// Check if H.264 decoding is available.
    #[cfg(not(feature = "ffmpeg"))]
    pub fn has_h264_support(&self) -> bool {
        false
    }

    /// Detect the format and decode.
    pub fn decode(&mut self, data: &[u8], width: u32, height: u32) -> Result<DecodedFrame> {
        // Try H.264 first if available (check for NAL start codes)
        #[cfg(feature = "ffmpeg")]
        if let Some(ref mut decoder) = self.h264 {
            if decoder.can_decode(data) {
                return decoder.decode(data, width, height);
            }
        }

        // Fall back to other decoders
        if self.zstd.can_decode(data) {
            self.zstd.decode(data, width, height)
        } else if self.png.can_decode(data) {
            self.png.decode(data, width, height)
        } else {
            self.raw.decode(data, width, height)
        }
    }

    /// Detect the compression format from data.
    pub fn detect_format(&self, data: &[u8]) -> &'static str {
        #[cfg(feature = "ffmpeg")]
        if let Some(ref decoder) = self.h264 {
            if decoder.can_decode(data) {
                return "H.264";
            }
        }

        if self.zstd.can_decode(data) {
            "Zstd"
        } else if self.png.can_decode(data) {
            "PNG"
        } else {
            "Raw"
        }
    }
}

impl Default for AutoDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a decoder for the specified compression format.
///
/// When the `ffmpeg` feature is enabled and H.264 is requested, this will
/// create an FFmpeg decoder. Falls back to Zstd for other formats.
pub fn create_decoder(compression: ScreenCompression) -> Box<dyn FrameDecoder> {
    match compression {
        ScreenCompression::Raw => Box::new(RawDecoder::new()),

        ScreenCompression::H264 => {
            #[cfg(feature = "ffmpeg")]
            {
                match super::ffmpeg::FfmpegDecoder::new() {
                    Ok(dec) => {
                        tracing::debug!("Created FFmpeg H.264 decoder");
                        Box::new(dec)
                    }
                    Err(e) => {
                        tracing::warn!("FFmpeg H.264 decoder unavailable: {}", e);
                        Box::new(ZstdDecoder::new())
                    }
                }
            }
            #[cfg(not(feature = "ffmpeg"))]
            {
                tracing::debug!("FFmpeg feature not enabled, using Zstd fallback");
                Box::new(ZstdDecoder::new())
            }
        }

        // Other formats use Zstd
        _ => Box::new(ZstdDecoder::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::encoder::{FrameEncoder, PngEncoder, RawEncoder, ZstdEncoder};
    use crate::screen::types::{CapturedFrame, PixelFormat};

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
    fn test_raw_roundtrip() {
        let frame = create_test_frame(100, 100);
        let mut encoder = RawEncoder::new();
        let mut decoder = RawDecoder::new();

        let encoded = encoder.encode(&frame).unwrap();
        let decoded = decoder
            .decode(&encoded.data, encoded.width, encoded.height)
            .unwrap();

        assert_eq!(decoded.width, 100);
        assert_eq!(decoded.height, 100);
        assert!(decoded.is_valid());
        assert_eq!(decoded.data, frame.to_rgba());
    }

    #[test]
    fn test_png_roundtrip() {
        let frame = create_test_frame(100, 100);
        let mut encoder = PngEncoder::new();
        let mut decoder = PngDecoder::new();

        let encoded = encoder.encode(&frame).unwrap();
        assert!(decoder.can_decode(&encoded.data));

        let decoded = decoder
            .decode(&encoded.data, encoded.width, encoded.height)
            .unwrap();

        assert_eq!(decoded.width, 100);
        assert_eq!(decoded.height, 100);
        assert!(decoded.is_valid());
        assert_eq!(decoded.data, frame.to_rgba());
    }

    #[test]
    fn test_zstd_roundtrip() {
        let frame = create_test_frame(100, 100);
        let mut encoder = ZstdEncoder::new();
        let mut decoder = ZstdDecoder::new();

        let encoded = encoder.encode(&frame).unwrap();
        assert!(decoder.can_decode(&encoded.data));

        let decoded = decoder
            .decode(&encoded.data, encoded.width, encoded.height)
            .unwrap();

        assert_eq!(decoded.width, 100);
        assert_eq!(decoded.height, 100);
        assert!(decoded.is_valid());
        assert_eq!(decoded.data, frame.to_rgba());
    }

    #[test]
    fn test_auto_decoder() {
        let frame = create_test_frame(50, 50);
        let mut auto = AutoDecoder::new();

        // Test with Zstd
        let mut zstd_enc = ZstdEncoder::new();
        let zstd_encoded = zstd_enc.encode(&frame).unwrap();
        assert_eq!(auto.detect_format(&zstd_encoded.data), "Zstd");
        let decoded = auto.decode(&zstd_encoded.data, 50, 50).unwrap();
        assert_eq!(decoded.data, frame.to_rgba());

        // Test with PNG
        let mut png_enc = PngEncoder::new();
        let png_encoded = png_enc.encode(&frame).unwrap();
        assert_eq!(auto.detect_format(&png_encoded.data), "PNG");
        let decoded = auto.decode(&png_encoded.data, 50, 50).unwrap();
        assert_eq!(decoded.data, frame.to_rgba());

        // Test with Raw
        let mut raw_enc = RawEncoder::new();
        let raw_encoded = raw_enc.encode(&frame).unwrap();
        assert_eq!(auto.detect_format(&raw_encoded.data), "Raw");
        let decoded = auto.decode(&raw_encoded.data, 50, 50).unwrap();
        assert_eq!(decoded.data, frame.to_rgba());
    }

    #[test]
    fn test_format_detection() {
        let auto = AutoDecoder::new();

        // PNG signature
        let png_data = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0];
        assert_eq!(auto.detect_format(&png_data), "PNG");

        // ZRGB header
        let zstd_data = b"ZRGB\x64\x00\x00\x00\x64\x00\x00\x00rest";
        assert_eq!(auto.detect_format(zstd_data), "Zstd");

        // Unknown/raw
        let raw_data = vec![0, 1, 2, 3, 4, 5, 6, 7];
        assert_eq!(auto.detect_format(&raw_data), "Raw");
    }
}
