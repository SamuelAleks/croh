//! FFmpeg-based video encoding and decoding.
//!
//! This module provides video encoding using FFmpeg via the `ffmpeg-the-third` crate.
//! It supports hardware acceleration (NVENC, VAAPI, VideoToolbox, QSV) with automatic
//! fallback to software encoding (libx264).
//!
//! ## Feature Flag
//!
//! This module is only available when the `ffmpeg` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! croh-core = { version = "0.1", features = ["ffmpeg"] }
//! ```
//!
//! ## Hardware Acceleration
//!
//! The encoder automatically detects and uses available hardware encoders:
//!
//! 1. **NVENC** (NVIDIA GPUs) - Fastest, best quality
//! 2. **VAAPI** (AMD/Intel on Linux) - Good performance
//! 3. **VideoToolbox** (macOS) - Native Apple silicon support
//! 4. **QuickSync** (Intel) - Good for integrated graphics
//! 5. **libx264** (Software) - Universal fallback
//!
//! ## Low Latency
//!
//! All encoders are configured for low-latency streaming:
//! - No B-frames (reduces latency by ~2 frame times)
//! - Zero lookahead (no buffering for rate control)
//! - Sliced threading (encode parts of frame in parallel)

mod hwaccel;
mod scaler;
mod encoder;
mod decoder;

pub use hwaccel::{detect_hardware_encoders, DetectedEncoder, HardwareAccel};
pub use scaler::{AutoScaler, FrameScaler};
pub use encoder::FfmpegEncoder;
pub use decoder::FfmpegDecoder;

use ffmpeg_the_third as ffmpeg;

/// Initialize FFmpeg library.
///
/// This should be called once at application startup.
/// It's safe to call multiple times.
pub fn init() {
    ffmpeg::init().expect("Failed to initialize FFmpeg");
}

/// Check if FFmpeg is available and working.
pub fn is_available() -> bool {
    // Try to find the H.264 decoder as a basic sanity check
    ffmpeg::decoder::find(ffmpeg::codec::Id::H264).is_some()
}

/// Get FFmpeg version string.
pub fn version() -> String {
    // ffmpeg-the-third v4.0 doesn't expose version directly
    // Just return a generic string indicating FFmpeg is available
    "FFmpeg (ffmpeg-the-third)".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ffmpeg_init() {
        init();
        assert!(is_available());
    }

    #[test]
    fn test_ffmpeg_version() {
        init();
        let ver = version();
        assert!(ver.contains("FFmpeg"));
        println!("FFmpeg: {}", ver);
    }
}
