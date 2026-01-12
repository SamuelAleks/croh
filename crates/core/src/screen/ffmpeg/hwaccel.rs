//! Hardware acceleration detection and configuration.
//!
//! This module provides detection of available hardware video encoders
//! and configuration helpers for each backend.

use ffmpeg::codec::Id;
use ffmpeg_the_third as ffmpeg;
use tracing::{debug, info};

/// Available hardware acceleration backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareAccel {
    /// NVIDIA NVENC (requires NVIDIA GPU with encoding support)
    Nvenc,
    /// Intel/AMD VAAPI (Linux only, requires VA-API drivers)
    Vaapi,
    /// Intel QuickSync Video (requires Intel GPU)
    Qsv,
    /// Apple VideoToolbox (macOS only)
    VideoToolbox,
    /// AMD AMF (Windows only, requires AMD GPU)
    Amf,
    /// No hardware acceleration (software encoding via libx264)
    None,
}

impl HardwareAccel {
    /// Get a human-readable name for this acceleration backend.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Nvenc => "NVIDIA NVENC",
            Self::Vaapi => "VA-API",
            Self::Qsv => "Intel QuickSync",
            Self::VideoToolbox => "VideoToolbox",
            Self::Amf => "AMD AMF",
            Self::None => "Software (x264)",
        }
    }

    /// Check if this is a hardware encoder.
    pub fn is_hardware(&self) -> bool {
        !matches!(self, Self::None)
    }
}

impl std::fmt::Display for HardwareAccel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Detected encoder with its capabilities.
#[derive(Debug, Clone)]
pub struct DetectedEncoder {
    /// FFmpeg encoder name (e.g., "h264_nvenc", "libx264")
    pub name: &'static str,
    /// Codec ID
    pub codec_id: Id,
    /// Hardware acceleration backend
    pub hwaccel: HardwareAccel,
    /// Whether this encoder supports NV12 pixel format (preferred for hardware)
    pub supports_nv12: bool,
    /// Whether this encoder supports YUV420P pixel format (preferred for software)
    pub supports_yuv420p: bool,
}

impl DetectedEncoder {
    /// Get a human-readable description.
    pub fn description(&self) -> String {
        if self.hwaccel.is_hardware() {
            format!("H.264 ({})", self.hwaccel.name())
        } else {
            "H.264 (Software)".to_string()
        }
    }
}

/// H.264 encoder candidates in priority order.
///
/// Hardware encoders are tried first, then software fallback.
const H264_ENCODER_CANDIDATES: &[(&str, HardwareAccel)] = &[
    // NVIDIA - best performance, widely available
    ("h264_nvenc", HardwareAccel::Nvenc),
    // AMD on Windows
    ("h264_amf", HardwareAccel::Amf),
    // Intel/AMD on Linux
    ("h264_vaapi", HardwareAccel::Vaapi),
    // Apple Silicon / Intel Mac
    ("h264_videotoolbox", HardwareAccel::VideoToolbox),
    // Intel QuickSync
    ("h264_qsv", HardwareAccel::Qsv),
    // Software fallback - always available if FFmpeg built with libx264
    ("libx264", HardwareAccel::None),
];

/// HEVC/H.265 encoder candidates in priority order.
const HEVC_ENCODER_CANDIDATES: &[(&str, HardwareAccel)] = &[
    ("hevc_nvenc", HardwareAccel::Nvenc),
    ("hevc_amf", HardwareAccel::Amf),
    ("hevc_vaapi", HardwareAccel::Vaapi),
    ("hevc_videotoolbox", HardwareAccel::VideoToolbox),
    ("hevc_qsv", HardwareAccel::Qsv),
    ("libx265", HardwareAccel::None),
];

/// Detect available H.264 encoders in priority order.
///
/// Returns a list of encoders that are available on this system,
/// sorted by preference (hardware first, then software).
pub fn detect_hardware_encoders() -> Vec<DetectedEncoder> {
    detect_encoders_for_codec(Id::H264, H264_ENCODER_CANDIDATES)
}

/// Detect available HEVC/H.265 encoders in priority order.
pub fn detect_hevc_encoders() -> Vec<DetectedEncoder> {
    detect_encoders_for_codec(Id::HEVC, HEVC_ENCODER_CANDIDATES)
}

/// Detect encoders for a specific codec.
fn detect_encoders_for_codec(
    codec_id: Id,
    candidates: &[(&'static str, HardwareAccel)],
) -> Vec<DetectedEncoder> {
    let mut encoders = Vec::new();

    for (name, hwaccel) in candidates {
        if let Some(_codec) = ffmpeg::encoder::find_by_name(name) {
            debug!("Found encoder: {} ({:?})", name, hwaccel);

            // Determine supported pixel formats
            // Hardware encoders typically prefer NV12, software prefers YUV420P
            let (supports_nv12, supports_yuv420p) = match hwaccel {
                HardwareAccel::None => (false, true),
                _ => (true, true), // Most hardware encoders support both
            };

            encoders.push(DetectedEncoder {
                name,
                codec_id,
                hwaccel: *hwaccel,
                supports_nv12,
                supports_yuv420p,
            });
        }
    }

    if encoders.is_empty() {
        info!("No H.264 encoders found");
    } else {
        info!(
            "Found {} H.264 encoder(s): {:?}",
            encoders.len(),
            encoders.iter().map(|e| e.name).collect::<Vec<_>>()
        );
    }

    encoders
}

/// Get the best available H.264 encoder.
///
/// Returns the highest priority encoder that's available,
/// or None if no encoders are found.
pub fn get_best_h264_encoder() -> Option<DetectedEncoder> {
    detect_hardware_encoders().into_iter().next()
}

/// Check if hardware encoding is available.
pub fn has_hardware_encoding() -> bool {
    detect_hardware_encoders()
        .iter()
        .any(|e| e.hwaccel.is_hardware())
}

/// Get encoder options for low-latency streaming.
///
/// Returns a dictionary of FFmpeg options optimized for real-time
/// screen streaming with minimal latency.
pub fn get_low_latency_options(hwaccel: HardwareAccel) -> ffmpeg::Dictionary<'static> {
    let mut opts = ffmpeg::Dictionary::new();

    match hwaccel {
        HardwareAccel::Nvenc => {
            // NVENC low-latency settings
            // Preset: p1 (fastest) to p7 (slowest/best quality)
            opts.set("preset", "p4");
            // Tuning: ll (low latency), ull (ultra low latency), hq (high quality)
            opts.set("tune", "ll");
            // Rate control: cbr gives most consistent bitrate
            opts.set("rc", "cbr");
            // Minimize encoder delay
            opts.set("delay", "0");
            // Enable zero latency mode
            opts.set("zerolatency", "1");
            // Disable B-frames for lower latency
            opts.set("bf", "0");
        }
        HardwareAccel::Amf => {
            // AMD AMF settings
            opts.set("quality", "speed");
            opts.set("rc", "cbr");
            // Disable B-frames
            opts.set("bf", "0");
        }
        HardwareAccel::Vaapi => {
            // VA-API settings
            opts.set("rc_mode", "CBR");
            // Quality level 1-8 (lower = faster)
            opts.set("quality", "4");
        }
        HardwareAccel::VideoToolbox => {
            // VideoToolbox settings
            opts.set("realtime", "1");
            // Allow software fallback if hardware busy
            opts.set("allow_sw", "1");
            opts.set("profile", "high");
        }
        HardwareAccel::Qsv => {
            // Intel QuickSync settings
            opts.set("preset", "veryfast");
            // Disable lookahead for lower latency
            opts.set("look_ahead", "0");
        }
        HardwareAccel::None => {
            // libx264 low-latency settings
            // Preset: ultrafast, superfast, veryfast, faster, fast, medium, slow, slower, veryslow
            opts.set("preset", "veryfast");
            // Tune for zero latency (disables B-frames, lookahead, etc.)
            opts.set("tune", "zerolatency");
            // Use baseline profile for maximum compatibility
            opts.set("profile", "baseline");
        }
    }

    opts
}

/// Get encoder options for balanced quality/speed.
pub fn get_balanced_options(hwaccel: HardwareAccel) -> ffmpeg::Dictionary<'static> {
    let mut opts = ffmpeg::Dictionary::new();

    match hwaccel {
        HardwareAccel::Nvenc => {
            opts.set("preset", "p5");
            opts.set("tune", "hq");
            opts.set("rc", "vbr");
            opts.set("bf", "0"); // Still no B-frames for streaming
        }
        HardwareAccel::Amf => {
            opts.set("quality", "balanced");
            opts.set("rc", "vbr_latency");
            opts.set("bf", "0");
        }
        HardwareAccel::Vaapi => {
            opts.set("rc_mode", "VBR");
            opts.set("quality", "5");
        }
        HardwareAccel::VideoToolbox => {
            opts.set("realtime", "0");
            opts.set("allow_sw", "1");
            opts.set("profile", "high");
        }
        HardwareAccel::Qsv => {
            opts.set("preset", "fast");
            opts.set("look_ahead", "0");
        }
        HardwareAccel::None => {
            opts.set("preset", "fast");
            opts.set("tune", "zerolatency");
            opts.set("profile", "main");
        }
    }

    opts
}

/// Get encoder options for high quality (higher latency acceptable).
pub fn get_quality_options(hwaccel: HardwareAccel) -> ffmpeg::Dictionary<'static> {
    let mut opts = ffmpeg::Dictionary::new();

    match hwaccel {
        HardwareAccel::Nvenc => {
            opts.set("preset", "p6");
            opts.set("tune", "hq");
            opts.set("rc", "vbr");
            opts.set("bf", "2"); // Allow some B-frames for quality
            opts.set("temporal-aq", "1");
        }
        HardwareAccel::Amf => {
            opts.set("quality", "quality");
            opts.set("rc", "vbr_peak");
            opts.set("bf", "2");
        }
        HardwareAccel::Vaapi => {
            opts.set("rc_mode", "VBR");
            opts.set("quality", "7");
        }
        HardwareAccel::VideoToolbox => {
            opts.set("realtime", "0");
            opts.set("allow_sw", "1");
            opts.set("profile", "high");
        }
        HardwareAccel::Qsv => {
            opts.set("preset", "medium");
            opts.set("look_ahead", "1");
        }
        HardwareAccel::None => {
            opts.set("preset", "medium");
            opts.set("profile", "high");
            // Don't use zerolatency tune for quality mode
        }
    }

    opts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_encoders() {
        ffmpeg::init().unwrap();

        let encoders = detect_hardware_encoders();
        println!("Detected encoders:");
        for enc in &encoders {
            println!("  - {} ({:?})", enc.name, enc.hwaccel);
        }

        // We should find at least libx264 if FFmpeg is properly configured
        // But this might fail in minimal FFmpeg builds, so we just log
        if encoders.is_empty() {
            println!("Warning: No H.264 encoders found. FFmpeg may not have codec support.");
        }
    }

    #[test]
    fn test_hwaccel_display() {
        assert_eq!(HardwareAccel::Nvenc.name(), "NVIDIA NVENC");
        assert_eq!(HardwareAccel::None.name(), "Software (x264)");
        assert!(HardwareAccel::Nvenc.is_hardware());
        assert!(!HardwareAccel::None.is_hardware());
    }

    #[test]
    fn test_low_latency_options() {
        let opts = get_low_latency_options(HardwareAccel::None);
        // Verify key settings are present
        assert!(opts.get("preset").is_some());
        assert!(opts.get("tune").is_some());
    }
}
