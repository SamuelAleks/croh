# Screen Streaming Design Document

## Overview

This document outlines the architecture for adding remote desktop/screen streaming functionality to croh, leveraging the existing Iroh P2P infrastructure. The design follows ReFrame's approach for privileged DRM/KMS capture on Linux while providing a multi-backend abstraction for cross-platform support.

## Goals

1. **Prompt-free capture on Linux** - Bypass Wayland compositor restrictions using DRM/KMS
2. **Cross-platform support** - Linux (X11/Wayland/DRM) and Windows (DXGI)
3. **Low latency** - Target <100ms end-to-end for local network
4. **Seamless integration** - Reuse existing Iroh connection infrastructure
5. **Security** - Honor existing trust/permission model

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                          croh-daemon                                 │
│  ┌─────────────────┐  ┌──────────────────┐  ┌───────────────────┐  │
│  │  Screen Capture │  │  Frame Encoder   │  │  Stream Manager   │  │
│  │  (Multi-Backend)│──│  (H.264/VP9)     │──│  (Iroh Protocol)  │  │
│  └─────────────────┘  └──────────────────┘  └───────────────────┘  │
│          │                     │                      │             │
│          ▼                     ▼                      ▼             │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │              Existing Iroh Infrastructure                    │   │
│  │  • ControlConnection (send/recv, send_raw/recv_raw)         │   │
│  │  • Persistent peer connections                               │   │
│  │  • NAT traversal via relay                                   │   │
│  └─────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼ QUIC
┌─────────────────────────────────────────────────────────────────────┐
│                          croh (viewer)                              │
│  ┌───────────────────┐  ┌──────────────────┐  ┌─────────────────┐  │
│  │  Frame Decoder    │  │  Display Renderer│  │  Input Handler  │  │
│  │  (H.264/VP9)      │──│  (Slint/wgpu)    │──│  (kbd/mouse)    │  │
│  └───────────────────┘  └──────────────────┘  └─────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

## Part 1: Protocol Messages

Add to `crates/core/src/iroh/protocol.rs`:

```rust
// ==================== Screen Streaming Messages ====================

/// Display information for screen streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    /// Display identifier (e.g., "HDMI-1", "eDP-1", ":0.0")
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Refresh rate in Hz (if known)
    pub refresh_rate: Option<u32>,
    /// Whether this is the primary display
    pub is_primary: bool,
}

/// Compression format for screen frames.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScreenCompression {
    /// Raw RGBA frames (testing only, very high bandwidth)
    Raw,
    /// WebP for low-motion screens (good for text/desktop)
    WebP,
    /// H.264/AVC (best compatibility, hardware acceleration)
    H264,
    /// VP9 (better compression, software only)
    Vp9,
    /// AV1 (best compression, newer hardware only)
    Av1,
}

/// Quality preset for screen streaming.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScreenQuality {
    /// Prioritize low latency (lower quality)
    Fast,
    /// Balanced latency and quality
    Balanced,
    /// Prioritize quality (higher latency)
    Quality,
    /// Auto-adjust based on network conditions
    Auto,
}

/// Frame metadata for a single screen frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameMetadata {
    /// Sequential frame number (monotonically increasing)
    pub sequence: u64,
    /// Frame width in pixels
    pub width: u32,
    /// Frame height in pixels
    pub height: u32,
    /// Capture timestamp (Unix millis)
    pub captured_at: i64,
    /// Compression format used
    pub compression: ScreenCompression,
    /// Whether this is a keyframe (can be decoded independently)
    pub is_keyframe: bool,
    /// Compressed frame size in bytes
    pub size: u32,
}

/// Input event from viewer to host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputEvent {
    /// Mouse movement (absolute coordinates)
    MouseMove {
        x: i32,
        y: i32,
    },
    /// Mouse button press/release
    MouseButton {
        button: u8,  // 0=left, 1=right, 2=middle
        pressed: bool,
    },
    /// Mouse scroll
    MouseScroll {
        delta_x: i32,
        delta_y: i32,
    },
    /// Keyboard key press/release
    Key {
        /// Platform-independent key code (USB HID usage codes)
        code: u16,
        pressed: bool,
        /// Modifier state at time of event
        modifiers: u8,  // bits: shift, ctrl, alt, meta
    },
}

// Add these variants to ControlMessage enum:

/// Request to start screen streaming.
ScreenStreamRequest {
    /// Unique stream ID
    stream_id: String,
    /// Which display to stream (None = primary)
    display_id: Option<String>,
    /// Requested compression format
    compression: ScreenCompression,
    /// Quality preset
    quality: ScreenQuality,
    /// Target frame rate (0 = max available)
    target_fps: u32,
},

/// Response to screen stream request.
ScreenStreamResponse {
    /// Stream ID from request
    stream_id: String,
    /// Whether streaming is accepted
    accepted: bool,
    /// Reason if rejected
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// Available displays (for selection)
    #[serde(default)]
    displays: Vec<DisplayInfo>,
    /// Actual compression that will be used
    #[serde(skip_serializing_if = "Option::is_none")]
    compression: Option<ScreenCompression>,
},

/// Screen frame header (followed by raw frame data).
/// After receiving this, read `metadata.size` bytes of frame data.
ScreenFrame {
    /// Stream ID
    stream_id: String,
    /// Frame metadata
    metadata: FrameMetadata,
},

/// Acknowledge receipt of frames (for flow control).
ScreenFrameAck {
    /// Stream ID
    stream_id: String,
    /// Highest sequence number received
    up_to_sequence: u64,
    /// Estimated available bandwidth (bytes/sec)
    estimated_bandwidth: Option<u64>,
    /// Suggested quality adjustment
    #[serde(skip_serializing_if = "Option::is_none")]
    quality_hint: Option<ScreenQuality>,
},

/// Request quality/settings change mid-stream.
ScreenStreamAdjust {
    /// Stream ID
    stream_id: String,
    /// New quality setting
    #[serde(skip_serializing_if = "Option::is_none")]
    quality: Option<ScreenQuality>,
    /// New target FPS
    #[serde(skip_serializing_if = "Option::is_none")]
    target_fps: Option<u32>,
    /// Request keyframe (for recovery after packet loss)
    request_keyframe: bool,
},

/// Stop screen streaming.
ScreenStreamStop {
    /// Stream ID
    stream_id: String,
    /// Reason for stopping
    reason: String,
},

/// Input events from viewer (batched for efficiency).
ScreenInput {
    /// Stream ID
    stream_id: String,
    /// Batch of input events
    events: Vec<InputEvent>,
},

/// Query available displays without starting a stream.
DisplayListRequest,

/// Response with available displays.
DisplayListResponse {
    /// Available displays
    displays: Vec<DisplayInfo>,
    /// Current capture backend
    backend: String,
},
```

## Part 2: Permissions Extension

Add to `crates/core/src/peers.rs`:

```rust
/// Permissions granted to/from a peer.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Permissions {
    /// Can push files to this peer
    pub push: bool,
    /// Can pull files from this peer
    pub pull: bool,
    /// Can browse filesystem on this peer
    pub browse: bool,
    /// Can query status of this peer
    pub status: bool,
    /// Can send/receive chat messages with this peer
    #[serde(default = "default_chat_permission")]
    pub chat: bool,

    // === NEW SCREEN STREAMING PERMISSIONS ===

    /// Can view this peer's screen (receive stream)
    #[serde(default)]
    pub screen_view: bool,
    /// Can control this peer's screen (send input)
    #[serde(default)]
    pub screen_control: bool,
}

impl Permissions {
    /// Create permissions for remote desktop (view + control).
    pub fn remote_desktop() -> Self {
        Self {
            screen_view: true,
            screen_control: true,
            status: true,
            ..Default::default()
        }
    }
}
```

Add to `crates/core/src/trust.rs`:

```rust
/// Capability offered in a trust bundle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Push,
    Pull,
    Browse,
    Status,
    Chat,
    ScreenView,     // NEW
    ScreenControl,  // NEW
}
```

## Part 3: Configuration Extension

Add to `crates/core/src/config.rs`:

```rust
/// Screen streaming settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenStreamSettings {
    /// Enable screen streaming functionality
    #[serde(default)]
    pub enabled: bool,

    /// Default quality preset
    #[serde(default)]
    pub default_quality: ScreenQuality,

    /// Default compression format
    #[serde(default)]
    pub default_compression: ScreenCompression,

    /// Maximum FPS to stream (0 = unlimited)
    #[serde(default = "default_max_fps")]
    pub max_fps: u32,

    /// Minimum bitrate in Kbps (for quality floor)
    #[serde(default = "default_min_bitrate")]
    pub min_bitrate_kbps: u32,

    /// Maximum bitrate in Kbps (for bandwidth cap)
    #[serde(default = "default_max_bitrate")]
    pub max_bitrate_kbps: u32,

    /// Allow input control (keyboard/mouse)
    #[serde(default = "default_true")]
    pub allow_input: bool,

    /// Require confirmation for each stream request
    #[serde(default)]
    pub require_confirmation: bool,

    /// Auto-lock screen when streaming ends (security)
    #[serde(default)]
    pub lock_on_disconnect: bool,

    /// Capture backend preference
    #[serde(default)]
    pub capture_backend: CaptureBackend,
}

/// Capture backend preference.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureBackend {
    /// Auto-detect best backend for platform
    #[default]
    Auto,
    /// DRM/KMS direct (Linux, requires CAP_SYS_ADMIN)
    Drm,
    /// wlroots screencopy protocol (Sway, Hyprland)
    WlrScreencopy,
    /// XDG Desktop Portal (prompts on first use)
    Portal,
    /// X11 SHM (X11 only)
    X11,
    /// DXGI Desktop Duplication (Windows)
    Dxgi,
}

fn default_max_fps() -> u32 { 60 }
fn default_min_bitrate() -> u32 { 500 }   // 500 Kbps
fn default_max_bitrate() -> u32 { 20000 } // 20 Mbps
fn default_true() -> bool { true }

impl Default for ScreenStreamSettings {
    fn default() -> Self {
        Self {
            enabled: false,  // Opt-in by default
            default_quality: ScreenQuality::Auto,
            default_compression: ScreenCompression::H264,
            max_fps: 60,
            min_bitrate_kbps: 500,
            max_bitrate_kbps: 20000,
            allow_input: true,
            require_confirmation: false,
            lock_on_disconnect: false,
            capture_backend: CaptureBackend::Auto,
        }
    }
}
```

## Part 4: Multi-Backend Capture Abstraction

Create `crates/core/src/screen/mod.rs`:

```rust
//! Screen capture abstraction layer.
//!
//! Provides a unified interface for screen capture across different
//! platforms and backends.

mod backend;
mod drm;
mod wlroots;
mod portal;
mod x11;
mod dxgi;

pub use backend::*;

use crate::config::{CaptureBackend, ScreenStreamSettings};
use crate::error::Result;

/// A captured frame ready for encoding.
pub struct CapturedFrame {
    /// Frame width
    pub width: u32,
    /// Frame height
    pub height: u32,
    /// Pixel format (BGRA, RGBA, etc.)
    pub format: PixelFormat,
    /// Raw pixel data
    pub data: Vec<u8>,
    /// Stride (bytes per row)
    pub stride: u32,
    /// Capture timestamp
    pub timestamp: std::time::Instant,
    /// DMA-BUF file descriptor (for zero-copy path)
    pub dmabuf_fd: Option<std::os::fd::RawFd>,
}

/// Pixel format of captured frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra8,
    Rgba8,
    Bgrx8,
    Rgbx8,
}

/// Display information from the capture backend.
#[derive(Debug, Clone)]
pub struct Display {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_rate: Option<u32>,
    pub is_primary: bool,
}

/// Screen capture backend trait.
#[async_trait::async_trait]
pub trait ScreenCapture: Send + Sync {
    /// Get the backend name for logging.
    fn name(&self) -> &'static str;

    /// List available displays.
    async fn list_displays(&self) -> Result<Vec<Display>>;

    /// Start capturing a display.
    async fn start(&mut self, display_id: &str) -> Result<()>;

    /// Capture the next frame.
    /// Returns None if no new frame is available yet.
    async fn capture_frame(&mut self) -> Result<Option<CapturedFrame>>;

    /// Stop capturing.
    async fn stop(&mut self) -> Result<()>;

    /// Check if this backend requires elevated privileges.
    fn requires_privileges(&self) -> bool;

    /// Get the DRM device path (if applicable).
    fn drm_device(&self) -> Option<&str> { None }
}

/// Create the appropriate capture backend for the current platform.
pub async fn create_capture_backend(
    settings: &ScreenStreamSettings,
) -> Result<Box<dyn ScreenCapture>> {
    let backend = match settings.capture_backend {
        CaptureBackend::Auto => auto_detect_backend().await?,
        CaptureBackend::Drm => Box::new(drm::DrmCapture::new().await?),
        CaptureBackend::WlrScreencopy => Box::new(wlroots::WlrootsCapture::new().await?),
        CaptureBackend::Portal => Box::new(portal::PortalCapture::new().await?),
        CaptureBackend::X11 => Box::new(x11::X11Capture::new().await?),
        CaptureBackend::Dxgi => {
            #[cfg(target_os = "windows")]
            { Box::new(dxgi::DxgiCapture::new().await?) }
            #[cfg(not(target_os = "windows"))]
            { return Err(crate::error::Error::Screen("DXGI only available on Windows".into())) }
        }
    };

    Ok(backend)
}

/// Auto-detect the best available capture backend.
async fn auto_detect_backend() -> Result<Box<dyn ScreenCapture>> {
    #[cfg(target_os = "linux")]
    {
        // Priority order for Linux:
        // 1. DRM/KMS (if we have CAP_SYS_ADMIN)
        // 2. wlroots screencopy (if running on wlroots compositor)
        // 3. X11 SHM (if X11 session)
        // 4. XDG Portal (fallback, may prompt)

        if drm::DrmCapture::is_available().await {
            return Ok(Box::new(drm::DrmCapture::new().await?));
        }

        if wlroots::WlrootsCapture::is_available().await {
            return Ok(Box::new(wlroots::WlrootsCapture::new().await?));
        }

        if x11::X11Capture::is_available().await {
            return Ok(Box::new(x11::X11Capture::new().await?));
        }

        // Fallback to portal
        Ok(Box::new(portal::PortalCapture::new().await?))
    }

    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(dxgi::DxgiCapture::new().await?))
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(crate::error::Error::Screen("Unsupported platform".into()))
    }
}
```

## Part 5: DRM/KMS Backend (Linux - ReFrame Style)

Create `crates/core/src/screen/drm.rs`:

```rust
//! DRM/KMS screen capture backend.
//!
//! This backend reads directly from the GPU framebuffer using the Linux
//! DRM (Direct Rendering Manager) subsystem. It bypasses all compositor
//! security prompts but requires CAP_SYS_ADMIN capability.
//!
//! Based on ReFrame's approach: https://github.com/AlynxZhou/reframe

use super::{CapturedFrame, Display, PixelFormat, ScreenCapture};
use crate::error::{Error, Result};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// DRM card device path.
const DRM_CARD_PATH: &str = "/dev/dri/card0";

/// DRM/KMS capture backend.
pub struct DrmCapture {
    /// DRM device file descriptor
    card_fd: Option<OwnedFd>,
    /// Current connector ID
    connector_id: Option<u32>,
    /// Current CRTC ID
    crtc_id: Option<u32>,
    /// Current framebuffer ID
    fb_id: Option<u32>,
    /// Frame dimensions
    width: u32,
    height: u32,
    /// Pixel format
    format: PixelFormat,
    /// DRM device path
    device_path: PathBuf,
}

impl DrmCapture {
    /// Check if DRM capture is available (have permissions).
    pub async fn is_available() -> bool {
        // Check if we can open the DRM device
        match std::fs::File::open(DRM_CARD_PATH) {
            Ok(file) => {
                // Try to get DRM resources to verify access
                let fd = file.as_raw_fd();
                unsafe {
                    let res = drm_sys::drmModeGetResources(fd);
                    if !res.is_null() {
                        drm_sys::drmModeFreeResources(res);
                        return true;
                    }
                }
                false
            }
            Err(_) => false,
        }
    }

    /// Create a new DRM capture backend.
    pub async fn new() -> Result<Self> {
        Self::new_with_device(DRM_CARD_PATH).await
    }

    /// Create with a specific DRM device.
    pub async fn new_with_device(device_path: &str) -> Result<Self> {
        info!("Initializing DRM capture from {}", device_path);

        // Open DRM device
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(device_path)
            .map_err(|e| Error::Screen(format!("Failed to open DRM device: {}", e)))?;

        let fd = file.as_raw_fd();

        // Enable universal planes
        unsafe {
            if drm_sys::drmSetClientCap(fd, drm_sys::DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1) != 0 {
                warn!("Failed to enable universal planes");
            }

            // Enable atomic mode setting
            if drm_sys::drmSetClientCap(fd, drm_sys::DRM_CLIENT_CAP_ATOMIC, 1) != 0 {
                debug!("Atomic mode setting not available");
            }

            // Drop master immediately - we only need to read framebuffers
            drm_sys::drmDropMaster(fd);
        }

        // Convert to OwnedFd to take ownership
        let card_fd = unsafe { OwnedFd::from_raw_fd(file.into_raw_fd()) };

        Ok(Self {
            card_fd: Some(card_fd),
            connector_id: None,
            crtc_id: None,
            fb_id: None,
            width: 0,
            height: 0,
            format: PixelFormat::Bgrx8,
            device_path: PathBuf::from(device_path),
        })
    }

    /// Find the CRTC and framebuffer for a connector.
    fn find_crtc_for_connector(&self, connector_id: u32) -> Result<(u32, u32, u32, u32)> {
        let fd = self.card_fd.as_ref()
            .ok_or_else(|| Error::Screen("DRM device not open".into()))?
            .as_raw_fd();

        unsafe {
            let connector = drm_sys::drmModeGetConnector(fd, connector_id);
            if connector.is_null() {
                return Err(Error::Screen("Failed to get connector".into()));
            }

            // Get the current CRTC
            let encoder_id = (*connector).encoder_id;
            drm_sys::drmModeFreeConnector(connector);

            if encoder_id == 0 {
                return Err(Error::Screen("No encoder attached".into()));
            }

            let encoder = drm_sys::drmModeGetEncoder(fd, encoder_id);
            if encoder.is_null() {
                return Err(Error::Screen("Failed to get encoder".into()));
            }

            let crtc_id = (*encoder).crtc_id;
            drm_sys::drmModeFreeEncoder(encoder);

            if crtc_id == 0 {
                return Err(Error::Screen("No CRTC attached".into()));
            }

            let crtc = drm_sys::drmModeGetCrtc(fd, crtc_id);
            if crtc.is_null() {
                return Err(Error::Screen("Failed to get CRTC".into()));
            }

            let fb_id = (*crtc).buffer_id;
            let width = (*crtc).width;
            let height = (*crtc).height;
            drm_sys::drmModeFreeCrtc(crtc);

            if fb_id == 0 {
                return Err(Error::Screen("No framebuffer attached".into()));
            }

            Ok((crtc_id, fb_id, width, height))
        }
    }

    /// Export framebuffer as DMA-BUF.
    fn export_framebuffer(&self, fb_id: u32) -> Result<(OwnedFd, u32, u32, PixelFormat)> {
        let fd = self.card_fd.as_ref()
            .ok_or_else(|| Error::Screen("DRM device not open".into()))?
            .as_raw_fd();

        unsafe {
            // Try FB2 first (multi-planar, modern)
            let fb2 = drm_sys::drmModeGetFB2(fd, fb_id);
            if !fb2.is_null() {
                let handle = (*fb2).handles[0];
                let width = (*fb2).width;
                let height = (*fb2).height;
                let fourcc = (*fb2).pixel_format;
                drm_sys::drmModeFreeFB2(fb2);

                // Convert handle to DMA-BUF FD
                let mut dmabuf_fd: RawFd = -1;
                if drm_sys::drmPrimeHandleToFD(fd, handle, 0, &mut dmabuf_fd) != 0 {
                    return Err(Error::Screen("Failed to export DMA-BUF".into()));
                }

                let format = match fourcc {
                    drm_sys::DRM_FORMAT_XRGB8888 | drm_sys::DRM_FORMAT_ARGB8888 => PixelFormat::Bgrx8,
                    drm_sys::DRM_FORMAT_XBGR8888 | drm_sys::DRM_FORMAT_ABGR8888 => PixelFormat::Rgbx8,
                    _ => PixelFormat::Bgrx8, // Assume common format
                };

                return Ok((OwnedFd::from_raw_fd(dmabuf_fd), width, height, format));
            }

            // Fall back to FB (legacy, single-plane)
            let fb = drm_sys::drmModeGetFB(fd, fb_id);
            if fb.is_null() {
                return Err(Error::Screen("Failed to get framebuffer".into()));
            }

            let handle = (*fb).handle;
            let width = (*fb).width;
            let height = (*fb).height;
            drm_sys::drmModeFreeFB(fb);

            let mut dmabuf_fd: RawFd = -1;
            if drm_sys::drmPrimeHandleToFD(fd, handle, 0, &mut dmabuf_fd) != 0 {
                return Err(Error::Screen("Failed to export DMA-BUF".into()));
            }

            Ok((OwnedFd::from_raw_fd(dmabuf_fd), width, height, PixelFormat::Bgrx8))
        }
    }

    /// Read pixels from DMA-BUF (software path).
    fn read_dmabuf(&self, dmabuf_fd: RawFd, width: u32, height: u32) -> Result<Vec<u8>> {
        use std::os::unix::io::AsRawFd;

        let stride = width * 4; // 4 bytes per pixel (BGRX/RGBA)
        let size = (stride * height) as usize;

        // Memory-map the DMA-BUF
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ,
                libc::MAP_SHARED,
                dmabuf_fd,
                0,
            )
        };

        if ptr == libc::MAP_FAILED {
            return Err(Error::Screen("Failed to mmap DMA-BUF".into()));
        }

        // Copy the data
        let data = unsafe {
            std::slice::from_raw_parts(ptr as *const u8, size).to_vec()
        };

        // Unmap
        unsafe {
            libc::munmap(ptr, size);
        }

        Ok(data)
    }
}

#[async_trait::async_trait]
impl ScreenCapture for DrmCapture {
    fn name(&self) -> &'static str {
        "DRM/KMS"
    }

    async fn list_displays(&self) -> Result<Vec<Display>> {
        let fd = self.card_fd.as_ref()
            .ok_or_else(|| Error::Screen("DRM device not open".into()))?
            .as_raw_fd();

        let mut displays = Vec::new();

        unsafe {
            let resources = drm_sys::drmModeGetResources(fd);
            if resources.is_null() {
                return Err(Error::Screen("Failed to get DRM resources".into()));
            }

            let connectors = std::slice::from_raw_parts(
                (*resources).connectors,
                (*resources).count_connectors as usize,
            );

            for (i, &connector_id) in connectors.iter().enumerate() {
                let connector = drm_sys::drmModeGetConnector(fd, connector_id);
                if connector.is_null() {
                    continue;
                }

                if (*connector).connection == drm_sys::DRM_MODE_CONNECTED {
                    // Get connector name
                    let connector_type = (*connector).connector_type;
                    let connector_type_id = (*connector).connector_type_id;

                    let type_name = match connector_type {
                        drm_sys::DRM_MODE_CONNECTOR_HDMIA => "HDMI",
                        drm_sys::DRM_MODE_CONNECTOR_HDMIB => "HDMI",
                        drm_sys::DRM_MODE_CONNECTOR_DisplayPort => "DP",
                        drm_sys::DRM_MODE_CONNECTOR_eDP => "eDP",
                        drm_sys::DRM_MODE_CONNECTOR_VGA => "VGA",
                        drm_sys::DRM_MODE_CONNECTOR_LVDS => "LVDS",
                        _ => "Unknown",
                    };

                    let name = format!("{}-{}", type_name, connector_type_id);

                    // Get current mode
                    let modes = std::slice::from_raw_parts(
                        (*connector).modes,
                        (*connector).count_modes as usize,
                    );

                    let (width, height, refresh) = if !modes.is_empty() {
                        let mode = &modes[0];
                        (mode.hdisplay as u32, mode.vdisplay as u32, Some(mode.vrefresh))
                    } else {
                        (0, 0, None)
                    };

                    displays.push(Display {
                        id: connector_id.to_string(),
                        name,
                        width,
                        height,
                        refresh_rate: refresh,
                        is_primary: i == 0,
                    });
                }

                drm_sys::drmModeFreeConnector(connector);
            }

            drm_sys::drmModeFreeResources(resources);
        }

        Ok(displays)
    }

    async fn start(&mut self, display_id: &str) -> Result<()> {
        let connector_id: u32 = display_id.parse()
            .map_err(|_| Error::Screen("Invalid display ID".into()))?;

        let (crtc_id, fb_id, width, height) = self.find_crtc_for_connector(connector_id)?;

        self.connector_id = Some(connector_id);
        self.crtc_id = Some(crtc_id);
        self.fb_id = Some(fb_id);
        self.width = width;
        self.height = height;

        info!("DRM capture started: {}x{} on connector {}", width, height, connector_id);

        Ok(())
    }

    async fn capture_frame(&mut self) -> Result<Option<CapturedFrame>> {
        let fb_id = self.fb_id
            .ok_or_else(|| Error::Screen("Capture not started".into()))?;

        // Get current framebuffer (may have changed)
        if let Some(crtc_id) = self.crtc_id {
            let fd = self.card_fd.as_ref()
                .ok_or_else(|| Error::Screen("DRM device not open".into()))?
                .as_raw_fd();

            unsafe {
                let crtc = drm_sys::drmModeGetCrtc(fd, crtc_id);
                if !crtc.is_null() {
                    let new_fb_id = (*crtc).buffer_id;
                    drm_sys::drmModeFreeCrtc(crtc);

                    if new_fb_id != 0 && new_fb_id != fb_id {
                        self.fb_id = Some(new_fb_id);
                    }
                }
            }
        }

        // Export framebuffer as DMA-BUF
        let (dmabuf_fd, width, height, format) = self.export_framebuffer(
            self.fb_id.ok_or_else(|| Error::Screen("No framebuffer".into()))?
        )?;

        // Read pixels (software path for now)
        let data = self.read_dmabuf(dmabuf_fd.as_raw_fd(), width, height)?;

        Ok(Some(CapturedFrame {
            width,
            height,
            format,
            data,
            stride: width * 4,
            timestamp: std::time::Instant::now(),
            dmabuf_fd: Some(dmabuf_fd.into_raw_fd()),
        }))
    }

    async fn stop(&mut self) -> Result<()> {
        self.connector_id = None;
        self.crtc_id = None;
        self.fb_id = None;
        info!("DRM capture stopped");
        Ok(())
    }

    fn requires_privileges(&self) -> bool {
        true // Requires CAP_SYS_ADMIN
    }

    fn drm_device(&self) -> Option<&str> {
        self.device_path.to_str()
    }
}
```

## Part 6: Windows DXGI Backend

Create `crates/core/src/screen/dxgi.rs`:

```rust
//! DXGI Desktop Duplication capture backend for Windows.
//!
//! Uses the Desktop Duplication API introduced in Windows 8 for
//! efficient screen capture with GPU acceleration.

#![cfg(target_os = "windows")]

use super::{CapturedFrame, Display, PixelFormat, ScreenCapture};
use crate::error::{Error, Result};
use std::ptr;
use tracing::{debug, info};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;

/// DXGI Desktop Duplication capture backend.
pub struct DxgiCapture {
    device: Option<ID3D11Device>,
    context: Option<ID3D11DeviceContext>,
    duplication: Option<IDXGIOutputDuplication>,
    staging_texture: Option<ID3D11Texture2D>,
    width: u32,
    height: u32,
}

impl DxgiCapture {
    /// Check if DXGI capture is available.
    pub async fn is_available() -> bool {
        // DXGI Desktop Duplication requires Windows 8+
        true
    }

    /// Create a new DXGI capture backend.
    pub async fn new() -> Result<Self> {
        info!("Initializing DXGI capture");

        // Create D3D11 device
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;

        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            ).map_err(|e| Error::Screen(format!("Failed to create D3D11 device: {}", e)))?;
        }

        Ok(Self {
            device,
            context,
            duplication: None,
            staging_texture: None,
            width: 0,
            height: 0,
        })
    }

    /// Create staging texture for CPU readback.
    fn create_staging_texture(&mut self, width: u32, height: u32) -> Result<()> {
        let device = self.device.as_ref()
            .ok_or_else(|| Error::Screen("D3D11 device not initialized".into()))?;

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: D3D11_BIND_FLAG(0),
            CPUAccessFlags: D3D11_CPU_ACCESS_READ,
            MiscFlags: D3D11_RESOURCE_MISC_FLAG(0),
        };

        let texture = unsafe {
            let mut texture: Option<ID3D11Texture2D> = None;
            device.CreateTexture2D(&desc, None, Some(&mut texture))
                .map_err(|e| Error::Screen(format!("Failed to create staging texture: {}", e)))?;
            texture
        };

        self.staging_texture = texture;
        Ok(())
    }
}

#[async_trait::async_trait]
impl ScreenCapture for DxgiCapture {
    fn name(&self) -> &'static str {
        "DXGI"
    }

    async fn list_displays(&self) -> Result<Vec<Display>> {
        let device = self.device.as_ref()
            .ok_or_else(|| Error::Screen("D3D11 device not initialized".into()))?;

        let mut displays = Vec::new();

        unsafe {
            let dxgi_device: IDXGIDevice = device.cast()
                .map_err(|e| Error::Screen(format!("Failed to get DXGI device: {}", e)))?;

            let adapter = dxgi_device.GetAdapter()
                .map_err(|e| Error::Screen(format!("Failed to get adapter: {}", e)))?;

            let mut output_index = 0u32;
            while let Ok(output) = adapter.EnumOutputs(output_index) {
                let desc = output.GetDesc()
                    .map_err(|e| Error::Screen(format!("Failed to get output desc: {}", e)))?;

                let rect = desc.DesktopCoordinates;
                let width = (rect.right - rect.left) as u32;
                let height = (rect.bottom - rect.top) as u32;

                let name = String::from_utf16_lossy(&desc.DeviceName)
                    .trim_end_matches('\0')
                    .to_string();

                displays.push(Display {
                    id: output_index.to_string(),
                    name,
                    width,
                    height,
                    refresh_rate: None,
                    is_primary: output_index == 0,
                });

                output_index += 1;
            }
        }

        Ok(displays)
    }

    async fn start(&mut self, display_id: &str) -> Result<()> {
        let output_index: u32 = display_id.parse()
            .map_err(|_| Error::Screen("Invalid display ID".into()))?;

        let device = self.device.as_ref()
            .ok_or_else(|| Error::Screen("D3D11 device not initialized".into()))?;

        unsafe {
            let dxgi_device: IDXGIDevice = device.cast()
                .map_err(|e| Error::Screen(format!("Failed to get DXGI device: {}", e)))?;

            let adapter = dxgi_device.GetAdapter()
                .map_err(|e| Error::Screen(format!("Failed to get adapter: {}", e)))?;

            let output = adapter.EnumOutputs(output_index)
                .map_err(|e| Error::Screen(format!("Failed to get output: {}", e)))?;

            let output1: IDXGIOutput1 = output.cast()
                .map_err(|e| Error::Screen(format!("Failed to get Output1: {}", e)))?;

            let duplication = output1.DuplicateOutput(device)
                .map_err(|e| Error::Screen(format!("Failed to duplicate output: {}", e)))?;

            // Get output dimensions
            let desc = output.GetDesc()
                .map_err(|e| Error::Screen(format!("Failed to get output desc: {}", e)))?;

            let rect = desc.DesktopCoordinates;
            self.width = (rect.right - rect.left) as u32;
            self.height = (rect.bottom - rect.top) as u32;

            self.duplication = Some(duplication);
            self.create_staging_texture(self.width, self.height)?;
        }

        info!("DXGI capture started: {}x{} on output {}", self.width, self.height, output_index);

        Ok(())
    }

    async fn capture_frame(&mut self) -> Result<Option<CapturedFrame>> {
        let duplication = self.duplication.as_ref()
            .ok_or_else(|| Error::Screen("Capture not started".into()))?;
        let context = self.context.as_ref()
            .ok_or_else(|| Error::Screen("D3D11 context not initialized".into()))?;
        let staging = self.staging_texture.as_ref()
            .ok_or_else(|| Error::Screen("Staging texture not created".into()))?;

        unsafe {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;

            // Try to acquire next frame (100ms timeout)
            match duplication.AcquireNextFrame(100, &mut frame_info, &mut resource) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    return Ok(None); // No new frame
                }
                Err(e) => {
                    return Err(Error::Screen(format!("Failed to acquire frame: {}", e)));
                }
            }

            let resource = resource.ok_or_else(|| Error::Screen("No resource".into()))?;
            let texture: ID3D11Texture2D = resource.cast()
                .map_err(|e| Error::Screen(format!("Failed to get texture: {}", e)))?;

            // Copy to staging texture
            context.CopyResource(staging, &texture);

            // Release frame
            duplication.ReleaseFrame()
                .map_err(|e| Error::Screen(format!("Failed to release frame: {}", e)))?;

            // Map staging texture
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| Error::Screen(format!("Failed to map texture: {}", e)))?;

            // Copy pixel data
            let row_pitch = mapped.RowPitch as usize;
            let data_size = self.height as usize * self.width as usize * 4;
            let mut data = vec![0u8; data_size];

            for y in 0..self.height as usize {
                let src_offset = y * row_pitch;
                let dst_offset = y * self.width as usize * 4;
                let src_slice = std::slice::from_raw_parts(
                    (mapped.pData as *const u8).add(src_offset),
                    self.width as usize * 4,
                );
                data[dst_offset..dst_offset + self.width as usize * 4]
                    .copy_from_slice(src_slice);
            }

            context.Unmap(staging, 0);

            Ok(Some(CapturedFrame {
                width: self.width,
                height: self.height,
                format: PixelFormat::Bgra8,
                data,
                stride: self.width * 4,
                timestamp: std::time::Instant::now(),
                dmabuf_fd: None,
            }))
        }
    }

    async fn stop(&mut self) -> Result<()> {
        self.duplication = None;
        self.staging_texture = None;
        info!("DXGI capture stopped");
        Ok(())
    }

    fn requires_privileges(&self) -> bool {
        false // DXGI doesn't require admin
    }
}
```

## Part 7: Stream Manager

Create `crates/core/src/screen/stream.rs`:

```rust
//! Screen stream manager.
//!
//! Handles the lifecycle of screen streaming sessions, including
//! frame capture, encoding, and network transmission.

use super::{CapturedFrame, ScreenCapture};
use crate::config::ScreenStreamSettings;
use crate::error::{Error, Result};
use crate::iroh::endpoint::ControlConnection;
use crate::iroh::protocol::{
    ControlMessage, FrameMetadata, ScreenCompression, ScreenQuality,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

/// Stream state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamState {
    /// Stream is initializing
    Starting,
    /// Stream is active
    Active,
    /// Stream is paused (viewer requested)
    Paused,
    /// Stream is stopping
    Stopping,
    /// Stream has stopped
    Stopped,
}

/// Active stream session.
pub struct StreamSession {
    /// Unique stream ID
    pub stream_id: String,
    /// Peer endpoint ID
    pub peer_id: String,
    /// Current state
    pub state: StreamState,
    /// Compression format
    pub compression: ScreenCompression,
    /// Quality preset
    pub quality: ScreenQuality,
    /// Target FPS
    pub target_fps: u32,
    /// Last frame sequence number
    pub sequence: u64,
    /// Frames sent
    pub frames_sent: u64,
    /// Bytes sent
    pub bytes_sent: u64,
    /// Last ACK sequence
    pub last_ack_sequence: u64,
    /// Start time
    pub started_at: Instant,
    /// Display being captured
    pub display_id: String,
}

/// Screen stream events.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Stream started
    Started { stream_id: String, display_id: String },
    /// Frame sent
    FrameSent { stream_id: String, sequence: u64, size: usize },
    /// Quality adjusted
    QualityAdjusted { stream_id: String, new_quality: ScreenQuality },
    /// Stream stopped
    Stopped { stream_id: String, reason: String },
    /// Error occurred
    Error { stream_id: String, error: String },
}

/// Screen stream manager.
pub struct ScreenStreamManager {
    /// Active stream sessions
    sessions: Arc<RwLock<HashMap<String, StreamSession>>>,
    /// Settings
    settings: ScreenStreamSettings,
    /// Event channel
    event_tx: mpsc::Sender<StreamEvent>,
    /// Shutdown signal
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl ScreenStreamManager {
    /// Create a new stream manager.
    pub fn new(
        settings: ScreenStreamSettings,
        event_tx: mpsc::Sender<StreamEvent>,
    ) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            settings,
            event_tx,
            shutdown_tx: None,
        }
    }

    /// Start a new stream session.
    pub async fn start_stream(
        &mut self,
        stream_id: String,
        peer_id: String,
        display_id: String,
        compression: ScreenCompression,
        quality: ScreenQuality,
        target_fps: u32,
        conn: ControlConnection,
        capture: Box<dyn ScreenCapture>,
    ) -> Result<()> {
        let session = StreamSession {
            stream_id: stream_id.clone(),
            peer_id: peer_id.clone(),
            state: StreamState::Starting,
            compression,
            quality,
            target_fps: if target_fps == 0 { self.settings.max_fps } else { target_fps },
            sequence: 0,
            frames_sent: 0,
            bytes_sent: 0,
            last_ack_sequence: 0,
            started_at: Instant::now(),
            display_id: display_id.clone(),
        };

        self.sessions.write().await.insert(stream_id.clone(), session);

        // Start capture loop in background
        let sessions = self.sessions.clone();
        let event_tx = self.event_tx.clone();
        let settings = self.settings.clone();
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        tokio::spawn(async move {
            if let Err(e) = Self::capture_loop(
                stream_id.clone(),
                conn,
                capture,
                sessions.clone(),
                event_tx.clone(),
                settings,
                &mut shutdown_rx,
            ).await {
                error!("Stream {} capture loop error: {}", stream_id, e);
                let _ = event_tx.send(StreamEvent::Error {
                    stream_id: stream_id.clone(),
                    error: e.to_string(),
                }).await;
            }

            // Clean up session
            sessions.write().await.remove(&stream_id);
        });

        let _ = self.event_tx.send(StreamEvent::Started {
            stream_id,
            display_id,
        }).await;

        Ok(())
    }

    /// Stop a stream session.
    pub async fn stop_stream(&mut self, stream_id: &str, reason: &str) -> Result<()> {
        if let Some(session) = self.sessions.write().await.get_mut(stream_id) {
            session.state = StreamState::Stopping;
        }

        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }

        let _ = self.event_tx.send(StreamEvent::Stopped {
            stream_id: stream_id.to_string(),
            reason: reason.to_string(),
        }).await;

        Ok(())
    }

    /// Get stream statistics.
    pub async fn get_stats(&self, stream_id: &str) -> Option<StreamStats> {
        let sessions = self.sessions.read().await;
        sessions.get(stream_id).map(|s| StreamStats {
            frames_sent: s.frames_sent,
            bytes_sent: s.bytes_sent,
            duration_secs: s.started_at.elapsed().as_secs_f64(),
            current_fps: s.frames_sent as f64 / s.started_at.elapsed().as_secs_f64().max(1.0),
            bitrate_kbps: (s.bytes_sent as f64 * 8.0 / 1000.0) / s.started_at.elapsed().as_secs_f64().max(1.0),
        })
    }

    /// Capture loop for a stream.
    async fn capture_loop(
        stream_id: String,
        mut conn: ControlConnection,
        mut capture: Box<dyn ScreenCapture>,
        sessions: Arc<RwLock<HashMap<String, StreamSession>>>,
        event_tx: mpsc::Sender<StreamEvent>,
        settings: ScreenStreamSettings,
        shutdown_rx: &mut mpsc::Receiver<()>,
    ) -> Result<()> {
        // Get session info
        let (display_id, target_fps, compression) = {
            let sessions = sessions.read().await;
            let session = sessions.get(&stream_id)
                .ok_or_else(|| Error::Screen("Session not found".into()))?;
            (session.display_id.clone(), session.target_fps, session.compression)
        };

        // Start capture
        capture.start(&display_id).await?;

        // Update state to active
        {
            let mut sessions = sessions.write().await;
            if let Some(session) = sessions.get_mut(&stream_id) {
                session.state = StreamState::Active;
            }
        }

        let frame_interval = Duration::from_secs_f64(1.0 / target_fps as f64);
        let mut last_frame_time = Instant::now();
        let mut sequence: u64 = 0;

        loop {
            // Check for shutdown
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("Stream {} received shutdown signal", stream_id);
                    break;
                }
                _ = tokio::time::sleep(frame_interval.saturating_sub(last_frame_time.elapsed())) => {}
            }

            last_frame_time = Instant::now();

            // Capture frame
            let frame = match capture.capture_frame().await? {
                Some(f) => f,
                None => continue, // No new frame
            };

            // Encode frame
            let encoded = encode_frame(&frame, compression, &settings)?;

            sequence += 1;

            // Send frame header
            let metadata = FrameMetadata {
                sequence,
                width: frame.width,
                height: frame.height,
                captured_at: chrono::Utc::now().timestamp_millis(),
                compression,
                is_keyframe: sequence % 60 == 1, // Keyframe every 60 frames
                size: encoded.len() as u32,
            };

            conn.send(&ControlMessage::ScreenFrame {
                stream_id: stream_id.clone(),
                metadata: metadata.clone(),
            }).await?;

            // Send frame data in chunks
            const CHUNK_SIZE: usize = 64 * 1024;
            for chunk in encoded.chunks(CHUNK_SIZE) {
                let len = chunk.len() as u32;
                conn.send_raw(&len.to_be_bytes()).await?;
                conn.send_raw(chunk).await?;
            }

            // Signal end of frame
            conn.send_raw(&0u32.to_be_bytes()).await?;

            // Update stats
            {
                let mut sessions = sessions.write().await;
                if let Some(session) = sessions.get_mut(&stream_id) {
                    session.sequence = sequence;
                    session.frames_sent += 1;
                    session.bytes_sent += encoded.len() as u64;
                }
            }

            let _ = event_tx.send(StreamEvent::FrameSent {
                stream_id: stream_id.clone(),
                sequence,
                size: encoded.len(),
            }).await;
        }

        // Stop capture
        capture.stop().await?;

        // Send stop message
        let _ = conn.send(&ControlMessage::ScreenStreamStop {
            stream_id: stream_id.clone(),
            reason: "Stream ended".to_string(),
        }).await;

        Ok(())
    }
}

/// Stream statistics.
#[derive(Debug, Clone)]
pub struct StreamStats {
    pub frames_sent: u64,
    pub bytes_sent: u64,
    pub duration_secs: f64,
    pub current_fps: f64,
    pub bitrate_kbps: f64,
}

/// Encode a frame with the specified compression.
fn encode_frame(
    frame: &CapturedFrame,
    compression: ScreenCompression,
    settings: &ScreenStreamSettings,
) -> Result<Vec<u8>> {
    match compression {
        ScreenCompression::Raw => {
            // Just return raw RGBA data
            Ok(frame.data.clone())
        }
        ScreenCompression::WebP => {
            // TODO: Use webp crate
            unimplemented!("WebP encoding not yet implemented")
        }
        ScreenCompression::H264 => {
            // TODO: Use x264 or openh264 crate
            unimplemented!("H.264 encoding not yet implemented")
        }
        ScreenCompression::Vp9 => {
            // TODO: Use vpx crate
            unimplemented!("VP9 encoding not yet implemented")
        }
        ScreenCompression::Av1 => {
            // TODO: Use rav1e crate
            unimplemented!("AV1 encoding not yet implemented")
        }
    }
}
```

## Part 8: Input Injection (Linux uinput)

Create `crates/core/src/screen/input.rs`:

```rust
//! Input event injection for remote control.
//!
//! Uses uinput on Linux and SendInput on Windows.

use crate::error::{Error, Result};
use crate::iroh::protocol::InputEvent;
use tracing::{debug, info};

/// Input injector trait.
pub trait InputInjector: Send + Sync {
    /// Inject an input event.
    fn inject(&mut self, event: &InputEvent) -> Result<()>;

    /// Inject a batch of events.
    fn inject_batch(&mut self, events: &[InputEvent]) -> Result<()> {
        for event in events {
            self.inject(event)?;
        }
        Ok(())
    }
}

/// Create the appropriate input injector for the platform.
pub fn create_input_injector() -> Result<Box<dyn InputInjector>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(UinputInjector::new()?))
    }

    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(WindowsInjector::new()?))
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(Error::Screen("Input injection not supported on this platform".into()))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::os::fd::AsRawFd;

    /// uinput-based input injector for Linux.
    pub struct UinputInjector {
        device: File,
    }

    impl UinputInjector {
        /// Create a new uinput injector.
        pub fn new() -> Result<Self> {
            let device = File::options()
                .write(true)
                .open("/dev/uinput")
                .map_err(|e| Error::Screen(format!("Failed to open /dev/uinput: {}", e)))?;

            let fd = device.as_raw_fd();

            unsafe {
                // Enable event types
                Self::ioctl(fd, uinput_sys::UI_SET_EVBIT, libc::EV_KEY as u64)?;
                Self::ioctl(fd, uinput_sys::UI_SET_EVBIT, libc::EV_REL as u64)?;
                Self::ioctl(fd, uinput_sys::UI_SET_EVBIT, libc::EV_ABS as u64)?;

                // Enable all keys
                for key in 0..256 {
                    let _ = Self::ioctl(fd, uinput_sys::UI_SET_KEYBIT, key);
                }

                // Enable mouse buttons
                Self::ioctl(fd, uinput_sys::UI_SET_KEYBIT, libc::BTN_LEFT as u64)?;
                Self::ioctl(fd, uinput_sys::UI_SET_KEYBIT, libc::BTN_RIGHT as u64)?;
                Self::ioctl(fd, uinput_sys::UI_SET_KEYBIT, libc::BTN_MIDDLE as u64)?;

                // Enable relative axes (mouse movement)
                Self::ioctl(fd, uinput_sys::UI_SET_RELBIT, libc::REL_X as u64)?;
                Self::ioctl(fd, uinput_sys::UI_SET_RELBIT, libc::REL_Y as u64)?;
                Self::ioctl(fd, uinput_sys::UI_SET_RELBIT, libc::REL_WHEEL as u64)?;
                Self::ioctl(fd, uinput_sys::UI_SET_RELBIT, libc::REL_HWHEEL as u64)?;

                // Enable absolute axes (for absolute positioning)
                Self::ioctl(fd, uinput_sys::UI_SET_ABSBIT, libc::ABS_X as u64)?;
                Self::ioctl(fd, uinput_sys::UI_SET_ABSBIT, libc::ABS_Y as u64)?;

                // Set up absolute axis parameters
                let abs_setup = uinput_sys::uinput_abs_setup {
                    code: libc::ABS_X as u16,
                    absinfo: libc::input_absinfo {
                        minimum: 0,
                        maximum: 65535,
                        fuzz: 0,
                        flat: 0,
                        resolution: 0,
                        value: 0,
                    },
                };
                Self::ioctl_ptr(fd, uinput_sys::UI_ABS_SETUP, &abs_setup)?;

                let abs_setup_y = uinput_sys::uinput_abs_setup {
                    code: libc::ABS_Y as u16,
                    ..abs_setup
                };
                Self::ioctl_ptr(fd, uinput_sys::UI_ABS_SETUP, &abs_setup_y)?;

                // Create device
                let setup = uinput_sys::uinput_setup {
                    id: libc::input_id {
                        bustype: libc::BUS_USB as u16,
                        vendor: 0xCB0D,  // "croh" in hex-ish
                        product: 0x0001,
                        version: 1,
                    },
                    name: {
                        let mut name = [0i8; 80];
                        let s = b"croh-remote";
                        for (i, &byte) in s.iter().enumerate() {
                            name[i] = byte as i8;
                        }
                        name
                    },
                    ff_effects_max: 0,
                };
                Self::ioctl_ptr(fd, uinput_sys::UI_DEV_SETUP, &setup)?;
                Self::ioctl(fd, uinput_sys::UI_DEV_CREATE, 0)?;
            }

            info!("uinput device created");
            Ok(Self { device })
        }

        unsafe fn ioctl(fd: i32, request: u64, arg: u64) -> Result<()> {
            if libc::ioctl(fd, request as libc::c_ulong, arg) < 0 {
                return Err(Error::Screen(format!("ioctl failed: {}", std::io::Error::last_os_error())));
            }
            Ok(())
        }

        unsafe fn ioctl_ptr<T>(fd: i32, request: u64, arg: &T) -> Result<()> {
            if libc::ioctl(fd, request as libc::c_ulong, arg as *const T) < 0 {
                return Err(Error::Screen(format!("ioctl failed: {}", std::io::Error::last_os_error())));
            }
            Ok(())
        }

        fn write_event(&mut self, type_: u16, code: u16, value: i32) -> Result<()> {
            let event = libc::input_event {
                time: libc::timeval { tv_sec: 0, tv_usec: 0 },
                type_,
                code,
                value,
            };

            let bytes = unsafe {
                std::slice::from_raw_parts(
                    &event as *const _ as *const u8,
                    std::mem::size_of::<libc::input_event>(),
                )
            };

            self.device.write_all(bytes)
                .map_err(|e| Error::Screen(format!("Failed to write event: {}", e)))?;

            Ok(())
        }

        fn sync(&mut self) -> Result<()> {
            self.write_event(libc::EV_SYN as u16, libc::SYN_REPORT as u16, 0)
        }
    }

    impl InputInjector for UinputInjector {
        fn inject(&mut self, event: &InputEvent) -> Result<()> {
            match event {
                InputEvent::MouseMove { x, y } => {
                    // Use absolute positioning (scaled to 0-65535)
                    self.write_event(libc::EV_ABS as u16, libc::ABS_X as u16, *x)?;
                    self.write_event(libc::EV_ABS as u16, libc::ABS_Y as u16, *y)?;
                    self.sync()?;
                }
                InputEvent::MouseButton { button, pressed } => {
                    let code = match button {
                        0 => libc::BTN_LEFT,
                        1 => libc::BTN_RIGHT,
                        2 => libc::BTN_MIDDLE,
                        _ => return Ok(()), // Ignore unknown buttons
                    };
                    self.write_event(libc::EV_KEY as u16, code as u16, if *pressed { 1 } else { 0 })?;
                    self.sync()?;
                }
                InputEvent::MouseScroll { delta_x, delta_y } => {
                    if *delta_y != 0 {
                        self.write_event(libc::EV_REL as u16, libc::REL_WHEEL as u16, *delta_y)?;
                    }
                    if *delta_x != 0 {
                        self.write_event(libc::EV_REL as u16, libc::REL_HWHEEL as u16, *delta_x)?;
                    }
                    self.sync()?;
                }
                InputEvent::Key { code, pressed, .. } => {
                    // Convert USB HID code to Linux key code
                    let linux_code = hid_to_linux_keycode(*code);
                    if linux_code != 0 {
                        self.write_event(libc::EV_KEY as u16, linux_code, if *pressed { 1 } else { 0 })?;
                        self.sync()?;
                    }
                }
            }
            Ok(())
        }
    }

    impl Drop for UinputInjector {
        fn drop(&mut self) {
            unsafe {
                let fd = self.device.as_raw_fd();
                let _ = libc::ioctl(fd, uinput_sys::UI_DEV_DESTROY as libc::c_ulong);
            }
            info!("uinput device destroyed");
        }
    }

    /// Convert USB HID usage code to Linux key code.
    fn hid_to_linux_keycode(hid: u16) -> u16 {
        // This is a simplified mapping - full mapping would be larger
        match hid {
            0x04 => libc::KEY_A as u16,
            0x05 => libc::KEY_B as u16,
            // ... more mappings
            0x28 => libc::KEY_ENTER as u16,
            0x29 => libc::KEY_ESC as u16,
            0x2A => libc::KEY_BACKSPACE as u16,
            0x2B => libc::KEY_TAB as u16,
            0x2C => libc::KEY_SPACE as u16,
            _ => 0,
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::UinputInjector;

#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::*;

    /// Windows SendInput-based injector.
    pub struct WindowsInjector;

    impl WindowsInjector {
        pub fn new() -> Result<Self> {
            Ok(Self)
        }
    }

    impl InputInjector for WindowsInjector {
        fn inject(&mut self, event: &InputEvent) -> Result<()> {
            match event {
                InputEvent::MouseMove { x, y } => {
                    let input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: *x,
                                dy: *y,
                                dwFlags: MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE,
                                ..Default::default()
                            }
                        }
                    };
                    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32); }
                }
                InputEvent::MouseButton { button, pressed } => {
                    let flags = match (button, pressed) {
                        (0, true) => MOUSEEVENTF_LEFTDOWN,
                        (0, false) => MOUSEEVENTF_LEFTUP,
                        (1, true) => MOUSEEVENTF_RIGHTDOWN,
                        (1, false) => MOUSEEVENTF_RIGHTUP,
                        (2, true) => MOUSEEVENTF_MIDDLEDOWN,
                        (2, false) => MOUSEEVENTF_MIDDLEUP,
                        _ => return Ok(()),
                    };
                    let input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dwFlags: flags,
                                ..Default::default()
                            }
                        }
                    };
                    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32); }
                }
                InputEvent::MouseScroll { delta_y, .. } => {
                    let input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                mouseData: (*delta_y * 120) as u32,
                                dwFlags: MOUSEEVENTF_WHEEL,
                                ..Default::default()
                            }
                        }
                    };
                    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32); }
                }
                InputEvent::Key { code, pressed, .. } => {
                    let vk = hid_to_windows_vk(*code);
                    if vk != 0 {
                        let input = INPUT {
                            r#type: INPUT_KEYBOARD,
                            Anonymous: INPUT_0 {
                                ki: KEYBDINPUT {
                                    wVk: VIRTUAL_KEY(vk),
                                    dwFlags: if *pressed { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP },
                                    ..Default::default()
                                }
                            }
                        };
                        unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32); }
                    }
                }
            }
            Ok(())
        }
    }

    fn hid_to_windows_vk(hid: u16) -> u16 {
        // Simplified mapping
        match hid {
            0x04 => 'A' as u16,
            0x05 => 'B' as u16,
            // ... more mappings
            0x28 => VK_RETURN.0,
            0x29 => VK_ESCAPE.0,
            _ => 0,
        }
    }
}

#[cfg(target_os = "windows")]
pub use windows::WindowsInjector;
```

## Part 9: Daemon Integration

Add to `crates/daemon/src/main.rs` or create `crates/daemon/src/screen.rs`:

```rust
//! Screen streaming daemon integration.

use croh_core::config::ScreenStreamSettings;
use croh_core::iroh::endpoint::{ControlConnection, IrohEndpoint};
use croh_core::iroh::protocol::{ControlMessage, ScreenStreamRequest, ScreenStreamResponse};
use croh_core::peers::PeerStore;
use croh_core::screen::{create_capture_backend, ScreenStreamManager, StreamEvent};
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Handle an incoming screen stream request.
pub async fn handle_screen_stream_request(
    conn: &mut ControlConnection,
    peer_id: &str,
    request: ScreenStreamRequest,
    peer_store: &PeerStore,
    settings: &ScreenStreamSettings,
) -> croh_core::error::Result<()> {
    // Check if screen streaming is enabled
    if !settings.enabled {
        let response = ControlMessage::ScreenStreamResponse {
            stream_id: request.stream_id,
            accepted: false,
            reason: Some("Screen streaming is disabled".to_string()),
            displays: vec![],
            compression: None,
        };
        conn.send(&response).await?;
        return Ok(());
    }

    // Check peer permissions
    if let Some(peer) = peer_store.find_by_endpoint_id(peer_id) {
        if !peer.permissions_granted.screen_view {
            let response = ControlMessage::ScreenStreamResponse {
                stream_id: request.stream_id,
                accepted: false,
                reason: Some("Screen view permission not granted".to_string()),
                displays: vec![],
                compression: None,
            };
            conn.send(&response).await?;
            return Ok(());
        }
    } else {
        let response = ControlMessage::ScreenStreamResponse {
            stream_id: request.stream_id,
            accepted: false,
            reason: Some("Unknown peer".to_string()),
            displays: vec![],
            compression: None,
        };
        conn.send(&response).await?;
        return Ok(());
    }

    // Create capture backend
    let capture = match create_capture_backend(settings).await {
        Ok(c) => c,
        Err(e) => {
            let response = ControlMessage::ScreenStreamResponse {
                stream_id: request.stream_id.clone(),
                accepted: false,
                reason: Some(format!("Failed to initialize capture: {}", e)),
                displays: vec![],
                compression: None,
            };
            conn.send(&response).await?;
            return Ok(());
        }
    };

    // List displays
    let displays = capture.list_displays().await?;
    let display_infos: Vec<_> = displays.iter().map(|d| croh_core::iroh::protocol::DisplayInfo {
        id: d.id.clone(),
        name: d.name.clone(),
        width: d.width,
        height: d.height,
        refresh_rate: d.refresh_rate,
        is_primary: d.is_primary,
    }).collect();

    // Determine which display to capture
    let display_id = request.display_id.unwrap_or_else(|| {
        displays.iter()
            .find(|d| d.is_primary)
            .or_else(|| displays.first())
            .map(|d| d.id.clone())
            .unwrap_or_default()
    });

    // Send acceptance
    let response = ControlMessage::ScreenStreamResponse {
        stream_id: request.stream_id.clone(),
        accepted: true,
        reason: None,
        displays: display_infos,
        compression: Some(request.compression),
    };
    conn.send(&response).await?;

    info!("Starting screen stream {} for peer {}", request.stream_id, peer_id);

    // Create event channel
    let (event_tx, mut event_rx) = mpsc::channel::<StreamEvent>(100);

    // Create stream manager
    let mut manager = ScreenStreamManager::new(settings.clone(), event_tx);

    // Start streaming (this takes ownership of the connection for the stream)
    // In practice, you'd want a separate stream connection
    // For now, we'll handle this by cloning the connection or using a new one

    // Note: Full implementation would:
    // 1. Create a separate QUIC stream for video data
    // 2. Keep the control connection for commands
    // 3. Handle graceful shutdown

    Ok(())
}

/// Handle display list request.
pub async fn handle_display_list_request(
    conn: &mut ControlConnection,
    settings: &ScreenStreamSettings,
) -> croh_core::error::Result<()> {
    let capture = create_capture_backend(settings).await?;
    let displays = capture.list_displays().await?;

    let display_infos: Vec<_> = displays.iter().map(|d| croh_core::iroh::protocol::DisplayInfo {
        id: d.id.clone(),
        name: d.name.clone(),
        width: d.width,
        height: d.height,
        refresh_rate: d.refresh_rate,
        is_primary: d.is_primary,
    }).collect();

    let response = ControlMessage::DisplayListResponse {
        displays: display_infos,
        backend: capture.name().to_string(),
    };

    conn.send(&response).await?;

    Ok(())
}
```

## Part 10: Crate Dependencies

Add to `crates/core/Cargo.toml`:

```toml
[dependencies]
# ... existing deps ...

# Screen capture (conditional)
async-trait = "0.1"

[target.'cfg(target_os = "linux")'.dependencies]
drm = "0.12"           # DRM/KMS bindings
drm-ffi = "0.9"        # Low-level DRM FFI
libc = "0.2"           # System calls
nix = { version = "0.29", features = ["ioctl"] }

[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Graphics_Direct3D11",
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Dxgi_Common",
    "Win32_UI_Input_KeyboardAndMouse",
]}

[features]
default = []
screen-capture = []           # Enable screen capture
screen-drm = ["screen-capture"]  # DRM/KMS backend
screen-all = ["screen-capture"]  # All backends
```

## Security Considerations

### Privilege Requirements

1. **DRM/KMS (Linux)**: Requires `CAP_SYS_ADMIN` capability
   ```bash
   sudo setcap cap_sys_admin+ep target/release/croh-daemon
   ```

2. **uinput (Linux)**: Requires `/dev/uinput` access
   - Add user to `input` group, OR
   - Run daemon as root, OR
   - Use udev rule to grant access

3. **DXGI (Windows)**: No special privileges needed

### Permission Model

- `screen_view` permission: View-only, no input injection
- `screen_control` permission: Full control (requires `screen_view`)
- Separate from file permissions (push/pull/browse)

### Trust Flow

Screen streaming follows the existing trust model:
1. Peer must be trusted (completed trust handshake)
2. Peer must have `screen_view` and/or `screen_control` permission
3. Host can revoke permission at any time
4. Optional: Require confirmation for each stream request

## Implementation Phases

### Phase 1: Protocol & Types (1-2 days)
- Add protocol messages to `protocol.rs`
- Extend `Permissions` and `Capability`
- Add `ScreenStreamSettings` to config

### Phase 2: Capture Abstraction (3-5 days)
- Create `screen/mod.rs` with traits
- Implement DRM backend (Linux)
- Implement DXGI backend (Windows)
- Add auto-detection logic

### Phase 3: Stream Manager (2-3 days)
- Create `stream.rs` with session management
- Implement frame capture loop
- Add flow control (ACKs, backpressure)

### Phase 4: Encoding (3-5 days)
- Add raw frame support (for testing)
- Integrate H.264 encoder (x264 or openh264)
- Add quality adjustment logic

### Phase 5: Input Injection (2-3 days)
- Create `input.rs` with uinput (Linux)
- Add SendInput (Windows)
- Implement key code translation

### Phase 6: Daemon Integration (2-3 days)
- Add message handlers to daemon
- Integrate with peer store
- Add configuration options

### Phase 7: Viewer (GUI) (3-5 days)
- Add stream viewer widget (Slint)
- Implement frame decoder
- Add input capture and forwarding

### Phase 8: Testing & Polish (ongoing)
- Unit tests for all components
- Integration tests with test endpoints
- Performance optimization
- Documentation

## Total Estimated Effort

- **MVP (raw frames, single backend)**: 2-3 weeks
- **Production (H.264, both platforms)**: 6-8 weeks
- **Polished (all features)**: 3-4 months
