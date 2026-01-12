//! Pixel format conversion for video encoding.
//!
//! Screen capture typically produces BGRA or RGBA frames, but video encoders
//! require YUV formats (NV12 for hardware, YUV420P for software).
//!
//! This module provides efficient conversion using FFmpeg's swscale library.

use ffmpeg_the_third as ffmpeg;
use ffmpeg::software::scaling::{Context as ScalingContext, Flags};
use ffmpeg::util::format::Pixel;
use ffmpeg::util::frame::video::Video as VideoFrame;

use crate::error::{Error, Result};
use crate::screen::types::{CapturedFrame, PixelFormat};

/// Converts captured frames to encoder-compatible pixel formats.
///
/// Screen capture produces BGRA/RGBA frames, but video encoders need YUV.
/// This scaler handles the conversion efficiently using FFmpeg's swscale.
pub struct FrameScaler {
    /// The swscale context
    context: ScalingContext,
    /// Source pixel format
    src_format: Pixel,
    /// Destination pixel format
    dst_format: Pixel,
    /// Frame width
    width: u32,
    /// Frame height
    height: u32,
    /// Pre-allocated output frame
    output_frame: VideoFrame,
}

// Safety: The swscale context is only accessed from a single thread at a time
// through &mut self methods. FFmpeg's swscale is thread-safe for concurrent
// contexts, and we don't share the context across threads.
unsafe impl Send for FrameScaler {}

impl FrameScaler {
    /// Create a new scaler for the given dimensions and formats.
    pub fn new(
        width: u32,
        height: u32,
        src_format: Pixel,
        dst_format: Pixel,
    ) -> Result<Self> {
        let context = ScalingContext::get(
            src_format,
            width,
            height,
            dst_format,
            width,
            height,
            Flags::BILINEAR,
        )
        .map_err(|e| Error::Screen(format!("Failed to create scaler: {}", e)))?;

        let output_frame = VideoFrame::new(dst_format, width, height);

        Ok(Self {
            context,
            src_format,
            dst_format,
            width,
            height,
            output_frame,
        })
    }

    /// Create a scaler optimized for hardware encoding (BGRA -> NV12).
    pub fn for_hardware(width: u32, height: u32) -> Result<Self> {
        Self::new(width, height, Pixel::BGRA, Pixel::NV12)
    }

    /// Create a scaler optimized for software encoding (BGRA -> YUV420P).
    pub fn for_software(width: u32, height: u32) -> Result<Self> {
        Self::new(width, height, Pixel::BGRA, Pixel::YUV420P)
    }

    /// Get the destination pixel format.
    pub fn dst_format(&self) -> Pixel {
        self.dst_format
    }

    /// Get the frame dimensions.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Scale a captured frame to the encoder format.
    pub fn scale(&mut self, frame: &CapturedFrame) -> Result<&VideoFrame> {
        if frame.width != self.width || frame.height != self.height {
            return Err(Error::Screen(format!(
                "Frame size mismatch: expected {}x{}, got {}x{}",
                self.width, self.height, frame.width, frame.height
            )));
        }

        let actual_src_format = pixel_format_to_ffmpeg(frame.format);
        if actual_src_format != self.src_format {
            return Err(Error::Screen(format!(
                "Pixel format mismatch: scaler expects {:?}, got {:?}",
                self.src_format, frame.format
            )));
        }

        // Create input frame from captured data
        let mut input_frame = VideoFrame::new(self.src_format, self.width, self.height);

        // Get stride first before mutable borrow
        let ffmpeg_stride = input_frame.stride(0);
        let frame_stride = frame.stride as usize;

        // Copy data into the input frame
        {
            let input_data = input_frame.data_mut(0);
            if frame_stride == ffmpeg_stride {
                input_data[..frame.data.len()].copy_from_slice(&frame.data);
            } else {
                let row_bytes = (self.width * 4) as usize;
                for y in 0..self.height as usize {
                    let src_offset = y * frame_stride;
                    let dst_offset = y * ffmpeg_stride;
                    input_data[dst_offset..dst_offset + row_bytes]
                        .copy_from_slice(&frame.data[src_offset..src_offset + row_bytes]);
                }
            }
        }

        // Run the scaler
        self.context
            .run(&input_frame, &mut self.output_frame)
            .map_err(|e| Error::Screen(format!("Scaling failed: {}", e)))?;

        Ok(&self.output_frame)
    }

    /// Scale raw BGRA/RGBA data to the encoder format.
    pub fn scale_raw(&mut self, data: &[u8], stride: u32) -> Result<&VideoFrame> {
        let expected_size = (stride * self.height) as usize;
        if data.len() < expected_size {
            return Err(Error::Screen(format!(
                "Data too small: expected {} bytes, got {}",
                expected_size,
                data.len()
            )));
        }

        let mut input_frame = VideoFrame::new(self.src_format, self.width, self.height);
        let ffmpeg_stride = input_frame.stride(0);
        let stride = stride as usize;

        {
            let input_data = input_frame.data_mut(0);
            if stride == ffmpeg_stride {
                input_data[..expected_size].copy_from_slice(&data[..expected_size]);
            } else {
                let row_bytes = (self.width * 4) as usize;
                for y in 0..self.height as usize {
                    let src_offset = y * stride;
                    let dst_offset = y * ffmpeg_stride;
                    input_data[dst_offset..dst_offset + row_bytes]
                        .copy_from_slice(&data[src_offset..src_offset + row_bytes]);
                }
            }
        }

        self.context
            .run(&input_frame, &mut self.output_frame)
            .map_err(|e| Error::Screen(format!("Scaling failed: {}", e)))?;

        Ok(&self.output_frame)
    }
}

/// Convert our PixelFormat to FFmpeg's Pixel enum.
fn pixel_format_to_ffmpeg(format: PixelFormat) -> Pixel {
    match format {
        PixelFormat::Rgba8 => Pixel::RGBA,
        PixelFormat::Bgra8 => Pixel::BGRA,
        // RGBX/BGRX don't have direct equivalents in newer FFmpeg, use RGB/BGR
        PixelFormat::Rgbx8 => Pixel::RGBA, // Treat as RGBA, alpha will be ignored
        PixelFormat::Bgrx8 => Pixel::BGRA, // Treat as BGRA, alpha will be ignored
    }
}

/// Create a scaler that auto-detects source format from CapturedFrame.
pub struct AutoScaler {
    scaler: Option<FrameScaler>,
    dst_format: Pixel,
}

// Safety: AutoScaler only contains an Option<FrameScaler>, which is Send
unsafe impl Send for AutoScaler {}

impl AutoScaler {
    /// Create a new auto-detecting scaler.
    pub fn new(dst_format: Pixel) -> Self {
        Self {
            scaler: None,
            dst_format,
        }
    }

    /// Create for hardware encoding (target: NV12).
    pub fn for_hardware() -> Self {
        Self::new(Pixel::NV12)
    }

    /// Create for software encoding (target: YUV420P).
    pub fn for_software() -> Self {
        Self::new(Pixel::YUV420P)
    }

    /// Scale a frame, auto-detecting format and recreating scaler if needed.
    pub fn scale(&mut self, frame: &CapturedFrame) -> Result<&VideoFrame> {
        let src_format = pixel_format_to_ffmpeg(frame.format);

        let needs_recreate = match &self.scaler {
            None => true,
            Some(s) => {
                s.width != frame.width || s.height != frame.height || s.src_format != src_format
            }
        };

        if needs_recreate {
            self.scaler = Some(FrameScaler::new(
                frame.width,
                frame.height,
                src_format,
                self.dst_format,
            )?);
        }

        self.scaler.as_mut().unwrap().scale(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_frame(width: u32, height: u32, format: PixelFormat) -> CapturedFrame {
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

        CapturedFrame::new(width, height, format, data, stride)
    }

    #[test]
    fn test_scaler_creation() {
        ffmpeg::init().unwrap();

        let scaler = FrameScaler::for_software(640, 480);
        assert!(scaler.is_ok());

        let scaler = scaler.unwrap();
        assert_eq!(scaler.dimensions(), (640, 480));
        assert_eq!(scaler.dst_format(), Pixel::YUV420P);
    }

    #[test]
    fn test_bgra_to_yuv420p() {
        ffmpeg::init().unwrap();

        let mut scaler =
            FrameScaler::new(100, 100, Pixel::BGRA, Pixel::YUV420P).unwrap();

        let frame = create_test_frame(100, 100, PixelFormat::Bgra8);
        let result = scaler.scale(&frame);

        assert!(result.is_ok());
        let yuv = result.unwrap();
        assert_eq!(yuv.width(), 100);
        assert_eq!(yuv.height(), 100);
        assert_eq!(yuv.format(), Pixel::YUV420P);
    }

    #[test]
    fn test_auto_scaler() {
        ffmpeg::init().unwrap();

        let mut scaler = AutoScaler::for_software();

        let frame1 = create_test_frame(640, 480, PixelFormat::Bgra8);
        let result = scaler.scale(&frame1);
        assert!(result.is_ok());

        let frame2 = create_test_frame(640, 480, PixelFormat::Bgra8);
        let result = scaler.scale(&frame2);
        assert!(result.is_ok());

        let frame3 = create_test_frame(1280, 720, PixelFormat::Bgra8);
        let result = scaler.scale(&frame3);
        assert!(result.is_ok());
    }
}
