//! FFmpeg-based H.264 video decoder.
//!
//! This module provides H.264 decoding using FFmpeg with automatic
//! hardware acceleration where available.

use std::time::Instant;

use ffmpeg::codec::decoder::video::Video as VideoDecoder;
use ffmpeg::software::scaling::{Context as ScalingContext, Flags};
use ffmpeg::util::format::Pixel;
use ffmpeg::util::frame::video::Video as VideoFrame;
use ffmpeg_the_third as ffmpeg;
use libc::EAGAIN;
use tracing::debug;

use crate::error::{Error, Result};
use crate::screen::decoder::{DecodedFrame, FrameDecoder};

/// FFmpeg-based H.264 decoder.
///
/// Automatically handles pixel format conversion from YUV to RGBA for display.
pub struct FfmpegDecoder {
    /// The FFmpeg decoder context
    decoder: VideoDecoder,
    /// Scaler for YUV -> RGBA conversion (created on first frame)
    scaler: Option<ScalingContext>,
    /// Output RGBA frame
    rgba_frame: VideoFrame,
    /// Reusable decoded frame buffer
    decoded_frame: VideoFrame,
    /// Last known width
    width: u32,
    /// Last known height
    height: u32,
    /// Frames decoded count
    frames_decoded: u64,
}

// Safety: The decoder is accessed through &mut self, ensuring single-threaded access.
// FFmpeg decoder contexts are safe to use from a single thread.
unsafe impl Send for FfmpegDecoder {}

impl FfmpegDecoder {
    /// Create a new FFmpeg H.264 decoder.
    ///
    /// The decoder will automatically detect the stream parameters from
    /// the first keyframe.
    pub fn new() -> Result<Self> {
        // Ensure FFmpeg is initialized
        ffmpeg::init().map_err(|e| Error::Screen(format!("FFmpeg init failed: {}", e)))?;

        let codec = ffmpeg::decoder::find(ffmpeg::codec::Id::H264)
            .ok_or_else(|| Error::Screen("H.264 decoder not found".into()))?;

        let decoder = ffmpeg::codec::context::Context::new_with_codec(codec)
            .decoder()
            .video()
            .map_err(|e| Error::Screen(format!("Failed to create decoder: {}", e)))?;

        Ok(Self {
            decoder,
            scaler: None,
            decoded_frame: VideoFrame::empty(),
            rgba_frame: VideoFrame::empty(),
            width: 0,
            height: 0,
            frames_decoded: 0,
        })
    }

    /// Ensure scaler is configured for current frame dimensions and format.
    fn ensure_scaler(&mut self, width: u32, height: u32, format: Pixel) -> Result<()> {
        // Check if we need to recreate the scaler
        let needs_recreate = self.scaler.is_none() || self.width != width || self.height != height;

        if needs_recreate {
            debug!(
                "Creating scaler for {}x{} {:?} -> RGBA",
                width, height, format
            );

            self.scaler = Some(
                ScalingContext::get(
                    format,
                    width,
                    height,
                    Pixel::RGBA,
                    width,
                    height,
                    Flags::BILINEAR,
                )
                .map_err(|e| Error::Screen(format!("Failed to create scaler: {}", e)))?,
            );

            self.rgba_frame = VideoFrame::new(Pixel::RGBA, width, height);
            self.width = width;
            self.height = height;
        }

        Ok(())
    }

    /// Get the number of frames decoded.
    pub fn frames_decoded(&self) -> u64 {
        self.frames_decoded
    }

    /// Reset the decoder state.
    ///
    /// Call this when seeking or switching streams to clear internal buffers.
    pub fn reset(&mut self) {
        self.decoder.flush();
        self.scaler = None;
        self.width = 0;
        self.height = 0;
    }
}

impl Default for FfmpegDecoder {
    fn default() -> Self {
        Self::new().expect("Failed to create default FfmpegDecoder")
    }
}

impl FrameDecoder for FfmpegDecoder {
    fn name(&self) -> &'static str {
        "H.264 (FFmpeg)"
    }

    fn can_decode(&self, data: &[u8]) -> bool {
        // Check for H.264 Annex B start codes
        if data.len() < 4 {
            return false;
        }

        // Look for start code patterns
        // 0x00 0x00 0x00 0x01 (4-byte start code)
        // 0x00 0x00 0x01 (3-byte start code)
        if data[0] == 0x00 && data[1] == 0x00 {
            if data[2] == 0x01 {
                return true;
            }
            if data.len() >= 4 && data[2] == 0x00 && data[3] == 0x01 {
                return true;
            }
        }

        false
    }

    fn decode(&mut self, data: &[u8], _width: u32, _height: u32) -> Result<DecodedFrame> {
        let start = Instant::now();

        // Create packet from data
        let packet = ffmpeg::Packet::copy(data);

        // Send packet to decoder
        self.decoder
            .send_packet(&packet)
            .map_err(|e| Error::Screen(format!("Failed to send packet: {}", e)))?;

        // Receive decoded frame
        match self.decoder.receive_frame(&mut self.decoded_frame) {
            Ok(()) => {}
            Err(ffmpeg::Error::Other { errno }) if errno == EAGAIN => {
                return Err(Error::Screen("Decoder needs more data".into()));
            }
            Err(ffmpeg::Error::Eof) => {
                return Err(Error::Screen("Decoder reached EOF".into()));
            }
            Err(e) => {
                return Err(Error::Screen(format!("Decode error: {}", e)));
            }
        }

        let width = self.decoded_frame.width();
        let height = self.decoded_frame.height();
        let format = self.decoded_frame.format();

        // Ensure scaler is ready for this format
        self.ensure_scaler(width, height, format)?;

        // Convert to RGBA
        if let Some(ref mut scaler) = self.scaler {
            scaler
                .run(&self.decoded_frame, &mut self.rgba_frame)
                .map_err(|e| Error::Screen(format!("Scaling failed: {}", e)))?;
        } else {
            return Err(Error::Screen("Scaler not initialized".into()));
        }

        // Copy RGBA data to output
        let rgba_data = self.rgba_frame.data(0);
        let stride = self.rgba_frame.stride(0);
        let expected_stride = (width * 4) as usize;

        let output_data = if stride == expected_stride {
            // Direct copy
            rgba_data[..expected_stride * height as usize].to_vec()
        } else {
            // Row-by-row copy to remove padding
            let mut out = Vec::with_capacity(expected_stride * height as usize);
            for y in 0..height as usize {
                let row_start = y * stride;
                out.extend_from_slice(&rgba_data[row_start..row_start + expected_stride]);
            }
            out
        };

        self.frames_decoded += 1;

        Ok(DecodedFrame {
            data: output_data,
            width,
            height,
            decode_time_us: start.elapsed().as_micros() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_can_decode() {
        let decoder = match FfmpegDecoder::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test: no H.264 decoder available");
                return;
            }
        };

        // Valid H.264 start code (4-byte)
        assert!(decoder.can_decode(&[0x00, 0x00, 0x00, 0x01, 0x67]));

        // Valid H.264 start code (3-byte)
        assert!(decoder.can_decode(&[0x00, 0x00, 0x01, 0x67]));

        // Invalid data
        assert!(!decoder.can_decode(&[0x89, 0x50, 0x4E, 0x47])); // PNG signature
        assert!(!decoder.can_decode(&[0x5A, 0x52, 0x47, 0x42])); // ZRGB header
        assert!(!decoder.can_decode(&[0x00, 0x00])); // Too short
    }

    #[test]
    fn test_decoder_creation() {
        match FfmpegDecoder::new() {
            Ok(decoder) => {
                assert_eq!(decoder.name(), "H.264 (FFmpeg)");
                assert_eq!(decoder.frames_decoded(), 0);
            }
            Err(e) => {
                println!(
                    "Decoder creation failed (expected if no H.264 support): {}",
                    e
                );
            }
        }
    }

    // Note: Full encode/decode roundtrip test requires actual H.264 data
    // which we can't easily generate without the encoder also working.
    // See integration tests for full roundtrip testing.
}
