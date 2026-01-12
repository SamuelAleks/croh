# FFmpeg Video Encoding for Screen Streaming

## Implementation Status

| Phase | Description | Status |
|-------|-------------|--------|
| Phase 1 | FFmpeg Integration Foundation | **Complete** |
| Phase 2 | H.264 Encoder Implementation | **Complete** |
| Phase 3 | H.264 Decoder Implementation | **Complete** |
| Phase 4 | Integration with Existing System | **Complete** |

### Completed Files

- `crates/core/Cargo.toml` - Added `ffmpeg` feature flag
- `crates/core/src/screen/ffmpeg/mod.rs` - Module root with init/version
- `crates/core/src/screen/ffmpeg/hwaccel.rs` - Hardware detection and encoder options
- `crates/core/src/screen/ffmpeg/scaler.rs` - BGRA/RGBA to YUV conversion
- `crates/core/src/screen/ffmpeg/encoder.rs` - H.264 encoder with hardware fallback
- `crates/core/src/screen/ffmpeg/decoder.rs` - H.264 decoder with YUV->RGBA conversion
- `crates/core/src/screen/encoder.rs` - Updated `create_encoder` and `auto_select_encoder`
- `crates/core/src/screen/decoder.rs` - Updated `AutoDecoder` and `create_decoder`

### Usage

```bash
# Build with FFmpeg support (requires system FFmpeg dev libraries)
cargo build -p croh-core --features ffmpeg

# Build without FFmpeg (Zstd fallback only)
cargo build -p croh-core

# Run FFmpeg-specific tests
cargo test -p croh-core --features ffmpeg --lib screen::ffmpeg
```

### Compatibility

- **FFmpeg 8.0+**: Fully supported (tested on Arch Linux)
- **FFmpeg 7.x**: Should work (uses ffmpeg-the-third v4.0.1)
- **Rust bindings**: `ffmpeg-the-third = "4.0"` (actively maintained fork)

### Verified Tests (14 passing)

```
test screen::ffmpeg::encoder::tests::test_bitrate_calculation ... ok
test screen::ffmpeg::encoder::tests::test_is_h264_keyframe ... ok
test screen::ffmpeg::encoder::tests::test_encode_frame ... ok
test screen::ffmpeg::encoder::tests::test_encoder_creation ... ok
test screen::ffmpeg::decoder::tests::test_can_decode ... ok
test screen::ffmpeg::decoder::tests::test_decoder_creation ... ok
test screen::ffmpeg::hwaccel::tests::test_detect_encoders ... ok
test screen::ffmpeg::hwaccel::tests::test_hwaccel_display ... ok
test screen::ffmpeg::hwaccel::tests::test_low_latency_options ... ok
test screen::ffmpeg::scaler::tests::test_scaler_creation ... ok
test screen::ffmpeg::scaler::tests::test_bgra_to_yuv420p ... ok
test screen::ffmpeg::scaler::tests::test_auto_scaler ... ok
test screen::ffmpeg::tests::test_ffmpeg_init ... ok
test screen::ffmpeg::tests::test_ffmpeg_version ... ok
```

---

## Overview

This document describes the implementation plan for adding proper video encoding to croh's screen streaming feature using `ffmpeg-the-third`, the actively maintained Rust FFmpeg bindings.

### Current State

The screen streaming module currently uses three encoders:
- **RawEncoder**: No compression, ~8MB/s for 1080p30 (unusable over network)
- **PngEncoder**: Lossless PNG, ~200KB-2MB per frame (high bandwidth, no temporal compression)
- **ZstdEncoder**: Lossless Zstd with custom `ZRGB` header (better than PNG, still no temporal compression)

All current encoders produce independent frames (I-frames only), missing the 10-50x bandwidth reduction possible with video codecs that exploit temporal redundancy.

### Goals

1. Add H.264 encoding with hardware acceleration where available
2. Support low-latency streaming (target: <50ms encode + decode)
3. Maintain cross-platform compatibility (Linux, Windows, macOS)
4. Graceful fallback: hardware → software → existing Zstd encoder
5. Add VP9 and AV1 support for future use

---

## Architecture

### Encoding Pipeline

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│ CapturedFrame │ → │ Format Conv │ → │  Encoder    │ → │ EncodedFrame │
│ (BGRA/RGBA) │     │ (→ NV12/I420)│     │ (H.264/etc) │     │ (NAL units) │
└─────────────┘     └─────────────┘     └─────────────┘     └─────────────┘
                           │
                    FFmpeg swscale
```

### Encoder Selection Chain

```
┌──────────────────────────────────────────────────────────────────────────┐
│                        EncoderSelector                                    │
│                                                                           │
│   ┌─────────────┐   ┌─────────────┐   ┌─────────────┐   ┌─────────────┐  │
│   │ h264_nvenc  │ → │ h264_vaapi  │ → │ h264_qsv    │ → │ h264_videotoolbox │
│   │ (NVIDIA)    │   │ (AMD/Intel) │   │ (Intel)     │   │ (macOS)     │  │
│   └─────────────┘   └─────────────┘   └─────────────┘   └─────────────┘  │
│          │                 │                 │                 │          │
│          └─────────────────┴─────────────────┴─────────────────┘          │
│                                      │                                    │
│                                      ▼                                    │
│                            ┌─────────────┐                               │
│                            │  libx264    │   (software fallback)         │
│                            └─────────────┘                               │
│                                      │                                    │
│                                      ▼                                    │
│                            ┌─────────────┐                               │
│                            │ ZstdEncoder │   (ultimate fallback)         │
│                            └─────────────┘                               │
└──────────────────────────────────────────────────────────────────────────┘
```

### Decoding Pipeline (Viewer Side)

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│ EncodedFrame │ → │  Decoder    │ → │ Format Conv │ → │ DecodedFrame │
│ (NAL units) │     │ (H.264/etc) │     │ (→ RGBA)    │     │ (for display)│
└─────────────┘     └─────────────┘     └─────────────┘     └─────────────┘
```

---

## Implementation Plan

### Phase 1: FFmpeg Integration Foundation

**Goal**: Get FFmpeg bindings compiling and accessible on all platforms.

#### 1.1 Add Dependencies

```toml
# crates/core/Cargo.toml

[features]
default = []
ffmpeg = ["dep:ffmpeg-the-third"]

[dependencies]
# Video encoding via FFmpeg (optional, for hardware acceleration)
ffmpeg-the-third = { version = "4.0", optional = true, default-features = false, features = [
    "codec",
    "software-scaling",
] }
```

#### 1.2 Create Abstraction Layer

New file: `crates/core/src/screen/ffmpeg/mod.rs`

```rust
//! FFmpeg-based video encoding.
//!
//! This module provides video encoding using FFmpeg via the `ffmpeg-the-third` crate.
//! It supports hardware acceleration (NVENC, VAAPI, VideoToolbox, QSV) with automatic
//! fallback to software encoding (libx264).

#[cfg(feature = "ffmpeg")]
mod encoder;
#[cfg(feature = "ffmpeg")]
mod decoder;
#[cfg(feature = "ffmpeg")]
mod scaler;
#[cfg(feature = "ffmpeg")]
mod hwaccel;

#[cfg(feature = "ffmpeg")]
pub use encoder::FfmpegEncoder;
#[cfg(feature = "ffmpeg")]
pub use decoder::FfmpegDecoder;
#[cfg(feature = "ffmpeg")]
pub use hwaccel::{HardwareAccel, detect_hardware_encoders};
```

#### 1.3 Hardware Detection

New file: `crates/core/src/screen/ffmpeg/hwaccel.rs`

```rust
/// Available hardware acceleration backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareAccel {
    /// NVIDIA NVENC
    Nvenc,
    /// Intel/AMD VAAPI (Linux)
    Vaapi,
    /// Intel QuickSync
    Qsv,
    /// Apple VideoToolbox
    VideoToolbox,
    /// No hardware acceleration (software only)
    None,
}

/// Detected encoder with its capabilities.
#[derive(Debug, Clone)]
pub struct DetectedEncoder {
    pub name: &'static str,
    pub codec_id: ffmpeg::codec::Id,
    pub hwaccel: HardwareAccel,
    pub supports_nv12: bool,
    pub supports_yuv420p: bool,
}

/// Detect available hardware encoders in priority order.
pub fn detect_hardware_encoders() -> Vec<DetectedEncoder> {
    let mut encoders = Vec::new();

    // Try each hardware encoder in priority order
    let candidates = [
        ("h264_nvenc", HardwareAccel::Nvenc),
        ("h264_vaapi", HardwareAccel::Vaapi),
        ("h264_videotoolbox", HardwareAccel::VideoToolbox),
        ("h264_qsv", HardwareAccel::Qsv),
        ("libx264", HardwareAccel::None),
    ];

    for (name, hwaccel) in candidates {
        if let Some(codec) = ffmpeg::encoder::find_by_name(name) {
            encoders.push(DetectedEncoder {
                name,
                codec_id: codec.id(),
                hwaccel,
                supports_nv12: true,  // Check actual support
                supports_yuv420p: true,
            });
        }
    }

    encoders
}
```

### Phase 2: H.264 Encoder Implementation

**Goal**: Working H.264 encoding with low-latency settings.

#### 2.1 Pixel Format Conversion

New file: `crates/core/src/screen/ffmpeg/scaler.rs`

```rust
use ffmpeg::software::scaling::{Context as Scaler, Flags};
use ffmpeg::util::format::Pixel;
use ffmpeg::util::frame::Video as VideoFrame;

/// Converts BGRA/RGBA frames to encoder-compatible formats (NV12/YUV420P).
pub struct FrameScaler {
    scaler: Scaler,
    output_frame: VideoFrame,
    src_format: Pixel,
    dst_format: Pixel,
    width: u32,
    height: u32,
}

impl FrameScaler {
    /// Create a scaler for the given dimensions and target format.
    ///
    /// # Arguments
    /// * `width` - Frame width
    /// * `height` - Frame height
    /// * `src_format` - Source pixel format (Bgra or Rgba)
    /// * `dst_format` - Target format (Nv12 for hardware, Yuv420P for software)
    pub fn new(
        width: u32,
        height: u32,
        src_format: Pixel,
        dst_format: Pixel,
    ) -> Result<Self, ffmpeg::Error> {
        let scaler = Scaler::get(
            src_format, width, height,
            dst_format, width, height,
            Flags::BILINEAR,
        )?;

        let mut output_frame = VideoFrame::new(dst_format, width, height);

        Ok(Self {
            scaler,
            output_frame,
            src_format,
            dst_format,
            width,
            height,
        })
    }

    /// Scale a frame from BGRA/RGBA to YUV.
    pub fn scale(&mut self, input: &CapturedFrame) -> Result<&VideoFrame, ffmpeg::Error> {
        // Create input frame wrapper
        let mut input_frame = VideoFrame::new(self.src_format, self.width, self.height);
        input_frame.data_mut(0).copy_from_slice(&input.data);

        // Run the scaler
        self.scaler.run(&input_frame, &mut self.output_frame)?;

        Ok(&self.output_frame)
    }
}
```

#### 2.2 Main Encoder

New file: `crates/core/src/screen/ffmpeg/encoder.rs`

```rust
use std::time::Instant;
use ffmpeg::codec::{self, Context as CodecContext};
use ffmpeg::encoder::Video as VideoEncoder;
use ffmpeg::util::format::Pixel;
use ffmpeg::util::frame::Video as VideoFrame;
use ffmpeg::{Dictionary, Packet};

use crate::error::{Error, Result};
use crate::iroh::protocol::{ScreenCompression, ScreenQuality};
use crate::screen::encoder::{EncodedFrame, FrameEncoder};
use crate::screen::types::CapturedFrame;

use super::scaler::FrameScaler;
use super::hwaccel::{HardwareAccel, DetectedEncoder};

/// Low-latency H.264 encoder using FFmpeg.
pub struct FfmpegEncoder {
    encoder: VideoEncoder,
    scaler: FrameScaler,
    hwaccel: HardwareAccel,
    width: u32,
    height: u32,
    frame_count: u64,
    force_keyframe: bool,
    quality: ScreenQuality,
    target_bitrate_kbps: u32,
}

impl FfmpegEncoder {
    /// Create a new FFmpeg encoder with automatic hardware detection.
    pub fn new(width: u32, height: u32, quality: ScreenQuality) -> Result<Self> {
        // Detect available encoders
        let encoders = super::hwaccel::detect_hardware_encoders();

        if encoders.is_empty() {
            return Err(Error::Screen("No H.264 encoder available".into()));
        }

        // Try each encoder until one works
        for detected in &encoders {
            match Self::try_create(detected, width, height, quality) {
                Ok(encoder) => return Ok(encoder),
                Err(e) => {
                    tracing::debug!("Encoder {} failed: {}", detected.name, e);
                    continue;
                }
            }
        }

        Err(Error::Screen("All encoder backends failed".into()))
    }

    /// Try to create an encoder with a specific backend.
    fn try_create(
        detected: &DetectedEncoder,
        width: u32,
        height: u32,
        quality: ScreenQuality,
    ) -> Result<Self> {
        let codec = ffmpeg::encoder::find_by_name(detected.name)
            .ok_or_else(|| Error::Screen(format!("Encoder {} not found", detected.name)))?;

        let mut context = CodecContext::new_with_codec(codec);
        let mut encoder = context.encoder().video()?;

        // Set basic parameters
        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_frame_rate(Some((30, 1)));
        encoder.set_time_base((1, 30));

        // Determine target bitrate based on resolution and quality
        let base_bitrate = Self::calculate_bitrate(width, height, quality);
        encoder.set_bit_rate(base_bitrate as usize);

        // Use appropriate pixel format
        let pix_fmt = if detected.hwaccel != HardwareAccel::None {
            Pixel::NV12  // Hardware encoders prefer NV12
        } else {
            Pixel::YUV420P  // Software prefers YUV420P
        };
        encoder.set_format(pix_fmt);

        // GOP size (keyframe interval) - 1 second at 30fps
        encoder.set_gop(30);

        // Build encoder options for low latency
        let opts = Self::build_options(detected.hwaccel, quality);
        let encoder = encoder.open_with(opts)?;

        // Create scaler for BGRA -> YUV conversion
        let src_format = Pixel::BGRA;  // Most capture backends use BGRA
        let scaler = FrameScaler::new(width, height, src_format, pix_fmt)?;

        tracing::info!(
            "Created {} encoder ({}x{}, {:?} quality, {} kbps)",
            detected.name, width, height, quality, base_bitrate / 1000
        );

        Ok(Self {
            encoder,
            scaler,
            hwaccel: detected.hwaccel,
            width,
            height,
            frame_count: 0,
            force_keyframe: false,
            quality,
            target_bitrate_kbps: base_bitrate / 1000,
        })
    }

    /// Calculate target bitrate based on resolution and quality.
    fn calculate_bitrate(width: u32, height: u32, quality: ScreenQuality) -> u32 {
        let pixels = width * height;

        // Base bitrate: roughly 0.1 bits per pixel at 30fps for balanced quality
        let base = (pixels as f32 * 0.1 * 30.0) as u32;

        match quality {
            ScreenQuality::Fast => base / 2,      // Lower quality, lower bandwidth
            ScreenQuality::Balanced => base,       // Standard
            ScreenQuality::Quality => base * 2,    // Higher quality
            ScreenQuality::Auto => base,           // Start with balanced
        }
    }

    /// Build encoder-specific options for low latency.
    fn build_options(hwaccel: HardwareAccel, quality: ScreenQuality) -> Dictionary<'static> {
        let mut opts = Dictionary::new();

        match hwaccel {
            HardwareAccel::Nvenc => {
                // NVENC low-latency settings
                opts.set("preset", "p4");           // Balanced preset (p1=fastest, p7=slowest)
                opts.set("tune", "ll");             // Low latency tuning
                opts.set("rc", "cbr");              // Constant bitrate for stable streaming
                opts.set("delay", "0");             // Minimize encoder delay
                opts.set("zerolatency", "1");       // Zero latency mode
                opts.set("b_ref_mode", "disabled"); // Disable B-frame references
                opts.set("bf", "0");                // No B-frames
            }
            HardwareAccel::Vaapi => {
                // VAAPI settings
                opts.set("rc_mode", "CBR");
                opts.set("quality", "7");           // Speed priority (1-8)
            }
            HardwareAccel::VideoToolbox => {
                // VideoToolbox settings
                opts.set("realtime", "1");          // Real-time encoding hint
                opts.set("allow_sw", "0");          // Prefer hardware
                opts.set("profile", "high");
            }
            HardwareAccel::Qsv => {
                // QuickSync settings
                opts.set("preset", "veryfast");
                opts.set("look_ahead", "0");        // Disable lookahead for lower latency
            }
            HardwareAccel::None => {
                // libx264 low-latency settings
                opts.set("preset", match quality {
                    ScreenQuality::Fast => "ultrafast",
                    ScreenQuality::Balanced => "veryfast",
                    ScreenQuality::Quality => "fast",
                    ScreenQuality::Auto => "veryfast",
                });
                opts.set("tune", "zerolatency");    // Disable features that add latency
                opts.set("profile", "baseline");     // Most compatible, no B-frames
                // zerolatency tune sets: bframes=0, force-cfr=1, no-mbtree=1,
                // sync-lookahead=0, sliced-threads=1, rc-lookahead=0
            }
        }

        opts
    }

    /// Encode a single frame, returning zero or more encoded packets.
    fn encode_frame_internal(&mut self, frame: &CapturedFrame) -> Result<Vec<Packet>> {
        // Convert to YUV
        let yuv_frame = self.scaler.scale(frame)?;

        // Set frame properties
        let mut video_frame = yuv_frame.clone();
        video_frame.set_pts(Some(self.frame_count as i64));

        // Force keyframe if requested
        if self.force_keyframe {
            video_frame.set_kind(ffmpeg::picture::Type::I);
            self.force_keyframe = false;
        }

        // Send frame to encoder
        self.encoder.send_frame(&video_frame)?;
        self.frame_count += 1;

        // Receive encoded packets
        let mut packets = Vec::new();
        loop {
            let mut packet = Packet::empty();
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => packets.push(packet),
                Err(ffmpeg::Error::Other { errno: ffmpeg::error::EAGAIN }) => break,
                Err(e) => return Err(Error::Screen(format!("Encode error: {}", e))),
            }
        }

        Ok(packets)
    }
}

impl FrameEncoder for FfmpegEncoder {
    fn name(&self) -> &'static str {
        match self.hwaccel {
            HardwareAccel::Nvenc => "H.264 (NVENC)",
            HardwareAccel::Vaapi => "H.264 (VAAPI)",
            HardwareAccel::VideoToolbox => "H.264 (VideoToolbox)",
            HardwareAccel::Qsv => "H.264 (QuickSync)",
            HardwareAccel::None => "H.264 (x264)",
        }
    }

    fn compression(&self) -> ScreenCompression {
        ScreenCompression::H264
    }

    fn encode(&mut self, frame: &CapturedFrame) -> Result<EncodedFrame> {
        let start = Instant::now();
        let original_size = frame.data.len();

        let packets = self.encode_frame_internal(frame)?;

        // Combine all packets into single output
        // In practice, most frames produce exactly one packet
        let mut data = Vec::new();
        let mut is_keyframe = false;

        for packet in packets {
            if packet.is_key() {
                is_keyframe = true;
            }
            data.extend_from_slice(packet.data().unwrap_or(&[]));
        }

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
        // Note: Changing quality mid-stream requires encoder reconfiguration
        // For now, just update the stored value
    }

    fn set_bitrate(&mut self, kbps: u32) {
        self.target_bitrate_kbps = kbps;
        // Note: Dynamic bitrate changes require encoder support
    }
}

impl Drop for FfmpegEncoder {
    fn drop(&mut self) {
        // Flush remaining packets
        let _ = self.encoder.send_eof();
        loop {
            let mut packet = Packet::empty();
            if self.encoder.receive_packet(&mut packet).is_err() {
                break;
            }
        }
    }
}
```

### Phase 3: H.264 Decoder Implementation

**Goal**: Decode H.264 streams on the viewer side.

#### 3.1 FFmpeg Decoder

New file: `crates/core/src/screen/ffmpeg/decoder.rs`

```rust
use std::time::Instant;
use ffmpeg::codec::{self, Context as CodecContext};
use ffmpeg::decoder::Video as VideoDecoder;
use ffmpeg::software::scaling::{Context as Scaler, Flags};
use ffmpeg::util::format::Pixel;
use ffmpeg::util::frame::Video as VideoFrame;
use ffmpeg::Packet;

use crate::error::{Error, Result};
use crate::screen::decoder::{DecodedFrame, FrameDecoder};

/// H.264 decoder using FFmpeg.
pub struct FfmpegDecoder {
    decoder: VideoDecoder,
    scaler: Option<Scaler>,
    output_frame: VideoFrame,
    width: u32,
    height: u32,
}

impl FfmpegDecoder {
    /// Create a new FFmpeg H.264 decoder.
    pub fn new() -> Result<Self> {
        let codec = ffmpeg::decoder::find(codec::Id::H264)
            .ok_or_else(|| Error::Screen("H.264 decoder not found".into()))?;

        let context = CodecContext::new_with_codec(codec);
        let decoder = context.decoder().video()?;

        Ok(Self {
            decoder,
            scaler: None,
            output_frame: VideoFrame::empty(),
            width: 0,
            height: 0,
        })
    }

    /// Ensure scaler is configured for current frame dimensions.
    fn ensure_scaler(&mut self, width: u32, height: u32, src_format: Pixel) -> Result<()> {
        if self.width != width || self.height != height {
            self.scaler = Some(Scaler::get(
                src_format, width, height,
                Pixel::RGBA, width, height,
                Flags::BILINEAR,
            )?);
            self.output_frame = VideoFrame::new(Pixel::RGBA, width, height);
            self.width = width;
            self.height = height;
        }
        Ok(())
    }
}

impl FrameDecoder for FfmpegDecoder {
    fn name(&self) -> &'static str {
        "H.264 (FFmpeg)"
    }

    fn can_decode(&self, data: &[u8]) -> bool {
        // H.264 NAL units start with 0x00 0x00 0x00 0x01 or 0x00 0x00 0x01
        if data.len() < 4 {
            return false;
        }

        // Check for Annex B start codes
        (data[0] == 0x00 && data[1] == 0x00 && data[2] == 0x00 && data[3] == 0x01)
            || (data[0] == 0x00 && data[1] == 0x00 && data[2] == 0x01)
    }

    fn decode(&mut self, data: &[u8], _width: u32, _height: u32) -> Result<DecodedFrame> {
        let start = Instant::now();

        // Create packet from data
        let mut packet = Packet::copy(data);

        // Send to decoder
        self.decoder.send_packet(&packet)?;

        // Receive decoded frame
        let mut decoded = VideoFrame::empty();
        match self.decoder.receive_frame(&mut decoded) {
            Ok(()) => {}
            Err(ffmpeg::Error::Other { errno: ffmpeg::error::EAGAIN }) => {
                return Err(Error::Screen("Decoder needs more data".into()));
            }
            Err(e) => return Err(Error::Screen(format!("Decode error: {}", e))),
        }

        let width = decoded.width();
        let height = decoded.height();
        let format = decoded.format();

        // Ensure scaler is ready
        self.ensure_scaler(width, height, format)?;

        // Convert to RGBA
        if let Some(ref mut scaler) = self.scaler {
            scaler.run(&decoded, &mut self.output_frame)?;
        }

        // Copy output data
        let rgba_data = self.output_frame.data(0).to_vec();

        Ok(DecodedFrame {
            data: rgba_data,
            width,
            height,
            decode_time_us: start.elapsed().as_micros() as u64,
        })
    }
}
```

### Phase 4: Integration with Existing System

**Goal**: Wire FFmpeg encoders into the existing encoder selection system.

#### 4.1 Update `create_encoder` Function

Modify `crates/core/src/screen/encoder.rs`:

```rust
/// Create an encoder based on the compression setting.
pub fn create_encoder(compression: ScreenCompression, quality: ScreenQuality) -> Box<dyn FrameEncoder> {
    let mut encoder: Box<dyn FrameEncoder> = match compression {
        ScreenCompression::Raw => Box::new(RawEncoder::new()),

        ScreenCompression::H264 => {
            #[cfg(feature = "ffmpeg")]
            {
                // Try FFmpeg H.264 encoder
                match super::ffmpeg::FfmpegEncoder::new(1920, 1080, quality) {
                    Ok(enc) => Box::new(enc),
                    Err(e) => {
                        tracing::warn!("FFmpeg encoder failed, falling back to Zstd: {}", e);
                        Box::new(ZstdEncoder::new())
                    }
                }
            }
            #[cfg(not(feature = "ffmpeg"))]
            {
                tracing::debug!("FFmpeg not available, using Zstd fallback");
                Box::new(ZstdEncoder::new())
            }
        }

        // Other codecs fall back to Zstd for now
        ScreenCompression::WebP |
        ScreenCompression::Vp9 |
        ScreenCompression::Av1 => Box::new(ZstdEncoder::new()),
    };

    encoder.set_quality(quality);
    encoder
}
```

#### 4.2 Update `AutoDecoder`

Modify `crates/core/src/screen/decoder.rs`:

```rust
/// Auto-detecting decoder that selects the appropriate decoder based on data.
pub struct AutoDecoder {
    raw: RawDecoder,
    png: PngDecoder,
    zstd: ZstdDecoder,
    #[cfg(feature = "ffmpeg")]
    h264: Option<super::ffmpeg::FfmpegDecoder>,
}

impl AutoDecoder {
    pub fn new() -> Self {
        Self {
            raw: RawDecoder::new(),
            png: PngDecoder::new(),
            zstd: ZstdDecoder::new(),
            #[cfg(feature = "ffmpeg")]
            h264: super::ffmpeg::FfmpegDecoder::new().ok(),
        }
    }

    pub fn decode(&mut self, data: &[u8], width: u32, height: u32) -> Result<DecodedFrame> {
        // Try H.264 first if available
        #[cfg(feature = "ffmpeg")]
        if let Some(ref mut decoder) = self.h264 {
            if decoder.can_decode(data) {
                return decoder.decode(data, width, height);
            }
        }

        // Fall back to existing decoders
        if self.zstd.can_decode(data) {
            self.zstd.decode(data, width, height)
        } else if self.png.can_decode(data) {
            self.png.decode(data, width, height)
        } else {
            self.raw.decode(data, width, height)
        }
    }
}
```

---

## Build Configuration

### System Dependencies

#### Linux (Debian/Ubuntu)

```bash
# Runtime + development libraries
sudo apt install -y \
    libavcodec-dev \
    libavformat-dev \
    libavutil-dev \
    libavfilter-dev \
    libswscale-dev \
    libswresample-dev \
    libavdevice-dev \
    pkg-config \
    clang

# For VAAPI hardware acceleration
sudo apt install -y \
    libva-dev \
    vainfo

# For NVIDIA (install NVIDIA drivers first)
# NVENC is included in the driver
```

#### macOS

```bash
brew install ffmpeg pkg-config
```

#### Windows

Option 1: vcpkg
```powershell
vcpkg install ffmpeg:x64-windows
set VCPKG_ROOT=C:\vcpkg
```

Option 2: Pre-built binaries
```powershell
# Download from https://github.com/BtbN/FFmpeg-Builds/releases
# Set environment variables:
set FFMPEG_DIR=C:\ffmpeg
set PATH=%PATH%;C:\ffmpeg\bin
```

### Cargo Features

```toml
# Enable FFmpeg support
cargo build --features ffmpeg

# Build without FFmpeg (uses Zstd fallback only)
cargo build
```

---

## Protocol Changes

### Frame Header Update

The current `FrameMetadata` already supports the `ScreenCompression` enum which includes `H264`. No protocol changes are needed.

### NAL Unit Framing

H.264 NAL units are sent directly in the frame data field. The receiver detects H.264 by checking for Annex B start codes (`0x00 0x00 0x00 0x01`).

```rust
/// Frame data format by compression type:
///
/// - Raw: RGBA pixel data (width * height * 4 bytes)
/// - PNG: Complete PNG file with header
/// - Zstd: ZRGB header (12 bytes) + zstd compressed RGBA
/// - H264: NAL units with Annex B start codes
```

---

## Performance Targets

| Metric | Target | Notes |
|--------|--------|-------|
| Encode time (1080p, NVENC) | < 5ms | Hardware encoder |
| Encode time (1080p, x264) | < 15ms | ultrafast preset |
| Decode time (1080p) | < 5ms | Hardware or software |
| Keyframe interval | 30 frames (1s) | Balance seek vs size |
| Bitrate (1080p, balanced) | 2-4 Mbps | Adjustable |
| Bitrate (1080p, fast) | 1-2 Mbps | Lower quality |
| End-to-end latency | < 100ms | Capture to display |

### Bandwidth Comparison

| Encoder | 1080p30 Bandwidth | Compression Ratio |
|---------|-------------------|-------------------|
| Raw RGBA | ~250 MB/s | 1:1 |
| Zstd (level 1) | ~15-30 MB/s | 8-17x |
| PNG (fast) | ~8-20 MB/s | 12-30x |
| H.264 (2 Mbps) | 0.25 MB/s | 1000x |

---

## Testing Strategy

### Unit Tests

```rust
#[cfg(all(test, feature = "ffmpeg"))]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_creation() {
        // Should create some encoder (hardware or software)
        let encoder = FfmpegEncoder::new(1920, 1080, ScreenQuality::Balanced);
        assert!(encoder.is_ok());
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        let mut encoder = FfmpegEncoder::new(640, 480, ScreenQuality::Fast).unwrap();
        let mut decoder = FfmpegDecoder::new().unwrap();

        // Create test frame
        let frame = create_test_frame(640, 480);

        // Encode
        let encoded = encoder.encode(&frame).unwrap();
        assert!(encoded.data.len() < frame.data.len()); // Should compress

        // Decode
        let decoded = decoder.decode(&encoded.data, 640, 480).unwrap();
        assert_eq!(decoded.width, 640);
        assert_eq!(decoded.height, 480);
        // Note: Lossy compression, can't compare pixels exactly
    }

    #[test]
    fn test_keyframe_forcing() {
        let mut encoder = FfmpegEncoder::new(640, 480, ScreenQuality::Fast).unwrap();
        let frame = create_test_frame(640, 480);

        // First frame should be keyframe
        let encoded1 = encoder.encode(&frame).unwrap();
        assert!(encoded1.is_keyframe);

        // Second frame likely not keyframe
        let encoded2 = encoder.encode(&frame).unwrap();

        // Force keyframe
        encoder.force_keyframe();
        let encoded3 = encoder.encode(&frame).unwrap();
        assert!(encoded3.is_keyframe);
    }
}
```

### Integration Tests

```rust
#[cfg(all(test, feature = "ffmpeg"))]
mod integration_tests {
    #[test]
    fn test_hardware_detection() {
        let encoders = detect_hardware_encoders();
        println!("Available encoders: {:?}", encoders);

        // Should find at least software encoder
        assert!(!encoders.is_empty());
        assert!(encoders.iter().any(|e| e.hwaccel == HardwareAccel::None));
    }

    #[test]
    fn test_streaming_simulation() {
        // Simulate 5 seconds of streaming
        let mut encoder = FfmpegEncoder::new(1920, 1080, ScreenQuality::Balanced).unwrap();
        let mut decoder = FfmpegDecoder::new().unwrap();

        let frame = create_test_frame(1920, 1080);
        let mut total_encoded_size = 0;
        let mut encode_times = Vec::new();

        for _ in 0..150 { // 5 seconds at 30fps
            let encoded = encoder.encode(&frame).unwrap();
            total_encoded_size += encoded.data.len();
            encode_times.push(encoded.encode_time_us);

            let decoded = decoder.decode(&encoded.data, 1920, 1080).unwrap();
            assert_eq!(decoded.width, 1920);
        }

        let avg_encode_time = encode_times.iter().sum::<u64>() / encode_times.len() as u64;
        let avg_frame_size = total_encoded_size / 150;
        let bitrate_mbps = (avg_frame_size * 8 * 30) as f64 / 1_000_000.0;

        println!("Average encode time: {}us", avg_encode_time);
        println!("Average frame size: {} bytes", avg_frame_size);
        println!("Average bitrate: {:.2} Mbps", bitrate_mbps);

        assert!(avg_encode_time < 20_000); // < 20ms per frame
        assert!(bitrate_mbps < 10.0); // < 10 Mbps
    }
}
```

---

## Future Work

### Phase 5: VP9 Support

Add VP9 encoder for better compression (lower bitrate at same quality):

```rust
// VP9 encoder options for real-time
opts.set("quality", "realtime");
opts.set("speed", "6");           // 0-8, higher = faster
opts.set("tile-columns", "2");    // Parallel encoding
opts.set("frame-parallel", "1");
opts.set("row-mt", "1");          // Row-based multithreading
```

### Phase 6: AV1 Support

Add AV1 for next-generation compression:

```rust
// SVT-AV1 low-latency settings
opts.set("preset", "10");         // 0-13, higher = faster
opts.set("svtav1-params", "tune=0:scd=0:enable-overlays=0");
```

### Phase 7: Adaptive Bitrate

Implement dynamic quality adjustment based on network conditions:

1. Monitor frame delivery latency
2. Track packet loss (via sequence gaps)
3. Adjust bitrate and quality dynamically
4. Request keyframes when decoder gets out of sync

### Phase 8: Hardware Decoder Support

Add hardware-accelerated decoding:
- NVDEC (NVIDIA)
- VAAPI (Linux AMD/Intel)
- VideoToolbox (macOS)
- D3D11VA (Windows)

---

## Risk Assessment

### Build Complexity

**Risk**: FFmpeg is a large dependency with complex build requirements.

**Mitigation**:
- Make FFmpeg optional via Cargo feature
- Provide clear documentation for each platform
- Fall back gracefully to Zstd when FFmpeg unavailable
- Consider bundling FFmpeg in releases

### Platform Differences

**Risk**: Hardware encoder availability varies by platform/GPU.

**Mitigation**:
- Implement robust detection and fallback chain
- Always have software (libx264) as final fallback
- Log which encoder was selected for debugging
- Test on variety of hardware configurations

### Latency

**Risk**: Video codecs may add latency compared to image compression.

**Mitigation**:
- Use zero-latency presets (disable B-frames, lookahead)
- Measure and log end-to-end latency
- Allow quality/latency tradeoff via settings
- Keep frame buffer small (1-3 frames)

---

## References

- [ffmpeg-the-third on GitHub](https://github.com/shssoichiro/ffmpeg-the-third)
- [FFmpeg Encoding Documentation](https://ffmpeg.org/ffmpeg-codecs.html)
- [FFmpeg Hardware Acceleration](https://trac.ffmpeg.org/wiki/HWAccelIntro)
- [x264 Encoding Guide](https://trac.ffmpeg.org/wiki/Encode/H.264)
- [NVIDIA Video Codec SDK](https://developer.nvidia.com/video-codec-sdk)
- [RustDesk hwcodec (reference implementation)](https://github.com/rustdesk-org/hwcodec)
