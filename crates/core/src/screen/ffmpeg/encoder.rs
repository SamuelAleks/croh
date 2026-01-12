//! FFmpeg-based H.264 video encoder.
//!
//! This module provides a low-latency H.264 encoder using FFmpeg with automatic
//! hardware acceleration detection and fallback to software encoding.

use std::time::Instant;

use ffmpeg::codec::encoder::video::Encoder as VideoEncoder;
use ffmpeg::util::format::Pixel;
use ffmpeg::util::frame::video::Video as VideoFrame;
use ffmpeg_the_third as ffmpeg;
use libc::EAGAIN;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::iroh::protocol::{ScreenCompression, ScreenQuality};
use crate::screen::encoder::{EncodedFrame, FrameEncoder};
use crate::screen::types::CapturedFrame;

use super::hwaccel::{
    detect_hardware_encoders, get_balanced_options, get_low_latency_options, get_quality_options,
    DetectedEncoder, HardwareAccel,
};
use super::scaler::AutoScaler;

/// FFmpeg-based H.264 encoder with hardware acceleration support.
pub struct FfmpegEncoder {
    /// The FFmpeg encoder
    encoder: VideoEncoder,
    /// Pixel format scaler (BGRA -> YUV)
    scaler: AutoScaler,
    /// Hardware acceleration backend in use
    hwaccel: HardwareAccel,
    /// Encoder name for logging
    encoder_name: &'static str,
    /// Frame width
    width: u32,
    /// Frame height
    height: u32,
    /// Frame counter for PTS
    frame_count: i64,
    /// Force next frame to be a keyframe
    force_keyframe: bool,
    /// Current quality setting
    quality: ScreenQuality,
    /// Target bitrate in kbps
    target_bitrate_kbps: u32,
    /// Pixel format used by encoder
    pixel_format: Pixel,
    /// Reusable input frame
    input_frame: VideoFrame,
    /// Reusable packet for receiving encoded data
    packet: ffmpeg::Packet,
}

// Safety: The encoder is accessed through &mut self, ensuring single-threaded access.
// FFmpeg encoder contexts are safe to use from a single thread.
unsafe impl Send for FfmpegEncoder {}

impl FfmpegEncoder {
    /// Create a new FFmpeg encoder with automatic hardware detection.
    pub fn new(width: u32, height: u32, quality: ScreenQuality) -> Result<Self> {
        ffmpeg::init().map_err(|e| Error::Screen(format!("FFmpeg init failed: {}", e)))?;

        let encoders = detect_hardware_encoders();

        if encoders.is_empty() {
            return Err(Error::Screen(
                "No H.264 encoder available. Ensure FFmpeg is built with libx264 or hardware encoder support.".into()
            ));
        }

        let mut last_error = String::new();
        for detected in &encoders {
            match Self::try_create(detected, width, height, quality) {
                Ok(encoder) => {
                    info!(
                        "Created {} encoder for {}x{} @ {:?} quality",
                        detected.name, width, height, quality
                    );
                    return Ok(encoder);
                }
                Err(e) => {
                    debug!("Encoder {} failed: {}", detected.name, e);
                    last_error = e.to_string();
                    continue;
                }
            }
        }

        Err(Error::Screen(format!(
            "All encoder backends failed. Last error: {}",
            last_error
        )))
    }

    fn try_create(
        detected: &DetectedEncoder,
        width: u32,
        height: u32,
        quality: ScreenQuality,
    ) -> Result<Self> {
        let codec = ffmpeg::encoder::find_by_name(detected.name)
            .ok_or_else(|| Error::Screen(format!("Encoder {} not found", detected.name)))?;

        let pixel_format = if detected.hwaccel.is_hardware() && detected.supports_nv12 {
            Pixel::NV12
        } else {
            Pixel::YUV420P
        };

        let bitrate = Self::calculate_bitrate(width, height, quality);

        // Get encoder options
        let opts = match quality {
            ScreenQuality::Fast => get_low_latency_options(detected.hwaccel),
            ScreenQuality::Balanced => get_balanced_options(detected.hwaccel),
            ScreenQuality::Quality => get_quality_options(detected.hwaccel),
            ScreenQuality::Auto => get_balanced_options(detected.hwaccel),
        };

        // Create encoder context
        let mut encoder = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| Error::Screen(format!("Failed to create video encoder: {}", e)))?;

        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(pixel_format);
        encoder.set_time_base(ffmpeg::Rational::new(1, 30));
        encoder.set_frame_rate(Some(ffmpeg::Rational::new(30, 1)));
        encoder.set_bit_rate(bitrate as usize);
        encoder.set_gop(30); // Keyframe every second at 30fps

        let encoder = encoder.open_with(opts).map_err(|e| {
            Error::Screen(format!("Failed to open encoder {}: {}", detected.name, e))
        })?;

        let scaler = if detected.hwaccel.is_hardware() {
            AutoScaler::for_hardware()
        } else {
            AutoScaler::for_software()
        };

        let input_frame = VideoFrame::new(pixel_format, width, height);

        Ok(Self {
            encoder,
            scaler,
            hwaccel: detected.hwaccel,
            encoder_name: detected.name,
            width,
            height,
            frame_count: 0,
            force_keyframe: false,
            quality,
            target_bitrate_kbps: bitrate / 1000,
            pixel_format,
            input_frame,
            packet: ffmpeg::Packet::empty(),
        })
    }

    fn calculate_bitrate(width: u32, height: u32, quality: ScreenQuality) -> u32 {
        let pixels = width * height;
        let base = (pixels as f32 * 3.0) as u32;
        let base = base.clamp(500_000, 20_000_000);

        match quality {
            ScreenQuality::Fast => base / 2,
            ScreenQuality::Balanced => base,
            ScreenQuality::Quality => base * 3 / 2,
            ScreenQuality::Auto => base,
        }
    }

    pub fn hwaccel(&self) -> HardwareAccel {
        self.hwaccel
    }

    pub fn encoder_name(&self) -> &'static str {
        self.encoder_name
    }

    pub fn bitrate_kbps(&self) -> u32 {
        self.target_bitrate_kbps
    }

    fn encode_frame_internal(&mut self, frame: &CapturedFrame) -> Result<Vec<u8>> {
        // Scale and copy frame data inline to avoid borrow conflicts
        {
            let yuv_frame = self.scaler.scale(frame)?;

            // Copy frame data based on pixel format
            match self.pixel_format {
                Pixel::YUV420P => {
                    let src_y = yuv_frame.data(0);
                    let dst_y = self.input_frame.data_mut(0);
                    dst_y[..src_y.len()].copy_from_slice(src_y);

                    let src_u = yuv_frame.data(1);
                    let dst_u = self.input_frame.data_mut(1);
                    dst_u[..src_u.len()].copy_from_slice(src_u);

                    let src_v = yuv_frame.data(2);
                    let dst_v = self.input_frame.data_mut(2);
                    dst_v[..src_v.len()].copy_from_slice(src_v);
                }
                Pixel::NV12 => {
                    let src_y = yuv_frame.data(0);
                    let dst_y = self.input_frame.data_mut(0);
                    dst_y[..src_y.len()].copy_from_slice(src_y);

                    let src_uv = yuv_frame.data(1);
                    let dst_uv = self.input_frame.data_mut(1);
                    dst_uv[..src_uv.len()].copy_from_slice(src_uv);
                }
                _ => {
                    return Err(Error::Screen(format!(
                        "Unsupported pixel format: {:?}",
                        self.pixel_format
                    )));
                }
            }
        }

        self.input_frame.set_pts(Some(self.frame_count));

        if self.force_keyframe {
            self.input_frame.set_kind(ffmpeg::picture::Type::I);
            self.force_keyframe = false;
        }

        self.encoder
            .send_frame(&self.input_frame)
            .map_err(|e| Error::Screen(format!("Failed to send frame: {}", e)))?;

        self.frame_count += 1;

        let mut output = Vec::new();
        loop {
            match self.encoder.receive_packet(&mut self.packet) {
                Ok(()) => {
                    if let Some(data) = self.packet.data() {
                        output.extend_from_slice(data);
                    }
                }
                Err(ffmpeg::Error::Other { errno }) if errno == EAGAIN => break,
                Err(ffmpeg::Error::Eof) => break,
                Err(e) => {
                    return Err(Error::Screen(format!("Failed to receive packet: {}", e)));
                }
            }
        }

        Ok(output)
    }

    pub fn flush(&mut self) -> Result<Vec<u8>> {
        self.encoder
            .send_eof()
            .map_err(|e| Error::Screen(format!("Failed to send EOF: {}", e)))?;

        let mut output = Vec::new();
        loop {
            match self.encoder.receive_packet(&mut self.packet) {
                Ok(()) => {
                    if let Some(data) = self.packet.data() {
                        output.extend_from_slice(data);
                    }
                }
                Err(ffmpeg::Error::Eof) => break,
                Err(ffmpeg::Error::Other { errno }) if errno == EAGAIN => break,
                Err(e) => {
                    warn!("Error during flush: {}", e);
                    break;
                }
            }
        }

        Ok(output)
    }
}

impl FrameEncoder for FfmpegEncoder {
    fn name(&self) -> &'static str {
        match self.hwaccel {
            HardwareAccel::Nvenc => "H.264 (NVENC)",
            HardwareAccel::Vaapi => "H.264 (VAAPI)",
            HardwareAccel::VideoToolbox => "H.264 (VideoToolbox)",
            HardwareAccel::Qsv => "H.264 (QuickSync)",
            HardwareAccel::Amf => "H.264 (AMF)",
            HardwareAccel::None => "H.264 (x264)",
        }
    }

    fn compression(&self) -> ScreenCompression {
        ScreenCompression::H264
    }

    fn encode(&mut self, frame: &CapturedFrame) -> Result<EncodedFrame> {
        let start = Instant::now();
        let original_size = frame.data.len();

        let data = self.encode_frame_internal(frame)?;
        let is_keyframe = is_h264_keyframe(&data);

        Ok(EncodedFrame {
            data,
            width: self.width,
            height: self.height,
            compression: ScreenCompression::H264,
            is_keyframe,
            original_size,
            encode_time_us: start.elapsed().as_micros() as u64,
        })
    }

    fn force_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    fn set_quality(&mut self, quality: ScreenQuality) {
        self.quality = quality;
    }

    fn set_bitrate(&mut self, kbps: u32) {
        self.target_bitrate_kbps = kbps;
    }
}

fn is_h264_keyframe(data: &[u8]) -> bool {
    let mut i = 0;
    while i < data.len() {
        if i + 3 < data.len() && data[i] == 0x00 && data[i + 1] == 0x00 {
            let (start_code_len, nal_start) = if data[i + 2] == 0x01 {
                (3, i + 3)
            } else if i + 4 < data.len() && data[i + 2] == 0x00 && data[i + 3] == 0x01 {
                (4, i + 4)
            } else {
                i += 1;
                continue;
            };

            if nal_start < data.len() {
                let nal_type = data[nal_start] & 0x1F;
                if nal_type == 5 {
                    return true;
                }
            }

            i += start_code_len;
        } else {
            i += 1;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::types::PixelFormat;

    fn create_test_frame(width: u32, height: u32) -> CapturedFrame {
        let stride = width * 4;
        let size = (stride * height) as usize;

        let mut data = vec![0u8; size];
        for y in 0..height {
            for x in 0..width {
                let offset = ((y * stride) + (x * 4)) as usize;
                data[offset] = (x % 256) as u8;
                data[offset + 1] = (y % 256) as u8;
                data[offset + 2] = 128;
                data[offset + 3] = 255;
            }
        }

        CapturedFrame::new(width, height, PixelFormat::Bgra8, data, stride)
    }

    #[test]
    fn test_is_h264_keyframe() {
        let keyframe_data = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x00, 0x00];
        assert!(is_h264_keyframe(&keyframe_data));

        let non_keyframe_data = vec![0x00, 0x00, 0x00, 0x01, 0x41, 0x00, 0x00];
        assert!(!is_h264_keyframe(&non_keyframe_data));

        let short_start = vec![0x00, 0x00, 0x01, 0x65, 0x00];
        assert!(is_h264_keyframe(&short_start));
    }

    #[test]
    fn test_bitrate_calculation() {
        let bitrate_1080p = FfmpegEncoder::calculate_bitrate(1920, 1080, ScreenQuality::Balanced);
        assert!(bitrate_1080p > 4_000_000);
        assert!(bitrate_1080p < 10_000_000);

        let bitrate_720p = FfmpegEncoder::calculate_bitrate(1280, 720, ScreenQuality::Balanced);
        assert!(bitrate_720p < bitrate_1080p);

        let bitrate_fast = FfmpegEncoder::calculate_bitrate(1920, 1080, ScreenQuality::Fast);
        assert!(bitrate_fast < bitrate_1080p);
    }

    #[test]
    fn test_encoder_creation() {
        ffmpeg::init().ok();

        match FfmpegEncoder::new(640, 480, ScreenQuality::Fast) {
            Ok(encoder) => {
                println!(
                    "Created encoder: {} ({})",
                    encoder.name(),
                    encoder.encoder_name()
                );
                assert_eq!(encoder.width, 640);
                assert_eq!(encoder.height, 480);
            }
            Err(e) => {
                println!(
                    "Encoder creation failed (expected if no H.264 support): {}",
                    e
                );
            }
        }
    }

    #[test]
    fn test_encode_frame() {
        ffmpeg::init().ok();

        let mut encoder = match FfmpegEncoder::new(640, 480, ScreenQuality::Fast) {
            Ok(enc) => enc,
            Err(_) => {
                println!("Skipping test: no H.264 encoder available");
                return;
            }
        };

        let frame = create_test_frame(640, 480);

        let encoded = encoder.encode(&frame).unwrap();
        assert!(!encoded.data.is_empty());
        println!(
            "Encoded frame: {} bytes -> {} bytes (ratio: {:.1}x)",
            encoded.original_size,
            encoded.data.len(),
            encoded.compression_ratio()
        );
    }
}
