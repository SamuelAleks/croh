//! Screen capture and streaming functionality.
//!
//! This module provides cross-platform screen capture with multiple backend
//! support and a streaming manager for remote desktop functionality.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    ScreenStreamManager                       │
//! │  - Session lifecycle                                        │
//! │  - Frame dispatch                                           │
//! │  - Flow control                                             │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    ScreenCapture (trait)                     │
//! │  - list_displays()                                          │
//! │  - capture_frame()                                          │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!          ┌───────────────────┼───────────────────┐
//!          ▼                   ▼                   ▼
//!    ┌──────────┐       ┌──────────┐       ┌──────────┐
//!    │   DRM    │       │  X11     │       │  DXGI    │
//!    │ (Linux)  │       │ (Linux)  │       │(Windows) │
//!    └──────────┘       └──────────┘       └──────────┘
//! ```
//!
//! ## Capture Backends
//!
//! | Backend | Platform | Privileges | Wayland Prompts |
//! |---------|----------|------------|-----------------|
//! | DRM/KMS | Linux    | CAP_SYS_ADMIN | None |
//! | wlroots | Linux (Sway/Hyprland) | None | None |
//! | X11 SHM | Linux (X11) | None | N/A |
//! | Portal  | Linux    | None | Yes (first use) |
//! | DXGI    | Windows  | None | None |
//!
//! ## Usage
//!
//! ```rust,ignore
//! use croh_core::screen::{create_capture_backend, ScreenCapture};
//! use croh_core::config::ScreenStreamSettings;
//!
//! let settings = ScreenStreamSettings::default();
//! let mut capture = create_capture_backend(&settings).await?;
//!
//! // List available displays
//! let displays = capture.list_displays().await?;
//!
//! // Start capturing the primary display
//! capture.start(&displays[0].id).await?;
//!
//! // Capture frames in a loop
//! loop {
//!     if let Some(frame) = capture.capture_frame().await? {
//!         // Process frame...
//!     }
//! }
//! ```

mod adaptive;
mod cursor;
mod decoder;
mod encoder;
mod events;
mod input;
mod manager;
mod session;
mod time_sync;
mod types;
mod viewer;

// FFmpeg video encoding (optional feature)
#[cfg(feature = "ffmpeg")]
pub mod ffmpeg;

// Backend modules - conditionally compiled
#[cfg(target_os = "linux")]
mod drm;

#[cfg(target_os = "linux")]
mod x11;

#[cfg(target_os = "linux")]
mod portal;

#[cfg(target_os = "linux")]
mod uinput;

#[cfg(target_os = "windows")]
mod dxgi;

#[cfg(target_os = "windows")]
mod windows_input;

// Re-export public types
pub use adaptive::*;
pub use cursor::*;
pub use decoder::*;
pub use encoder::*;
pub use events::*;
pub use input::*;
pub use manager::*;
pub use session::*;
pub use time_sync::*;
pub use types::*;
pub use viewer::*;

use crate::config::{CaptureBackend, ScreenStreamSettings};
use crate::error::{Error, Result};

/// Create the appropriate capture backend for the current platform and settings.
///
/// This function auto-detects the best available backend based on:
/// 1. User preference in settings
/// 2. Platform capabilities
/// 3. Available privileges
///
/// # Backend Selection (Linux)
///
/// 1. **DRM/KMS**: Best option, works on all compositors, but requires `CAP_SYS_ADMIN`
/// 2. **wlroots screencopy**: Works on Sway/Hyprland without prompts
/// 3. **X11 SHM**: Works on X11 sessions
/// 4. **XDG Portal**: Fallback, may prompt user
///
/// # Backend Selection (Windows)
///
/// - **DXGI Desktop Duplication**: Only option, no special privileges needed
pub async fn create_capture_backend(
    settings: &ScreenStreamSettings,
) -> Result<Box<dyn ScreenCapture>> {
    match settings.capture_backend {
        CaptureBackend::Auto => auto_detect_backend().await,
        CaptureBackend::Drm => {
            #[cfg(target_os = "linux")]
            {
                Ok(Box::new(drm::DrmCapture::new().await?))
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(Error::Screen("DRM capture only available on Linux".into()))
            }
        }
        CaptureBackend::WlrScreencopy => Err(Error::Screen(
            "wlroots screencopy not yet implemented".into(),
        )),
        CaptureBackend::Portal => {
            #[cfg(target_os = "linux")]
            {
                let token = settings.portal_restore_token.clone();
                Ok(Box::new(portal::PortalCapture::new(token).await?))
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(Error::Screen(
                    "Portal capture only available on Linux".into(),
                ))
            }
        }
        CaptureBackend::X11 => {
            #[cfg(target_os = "linux")]
            {
                Ok(Box::new(x11::X11Capture::new().await?))
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(Error::Screen("X11 capture only available on Linux".into()))
            }
        }
        CaptureBackend::Dxgi => {
            #[cfg(target_os = "windows")]
            {
                Ok(Box::new(dxgi::DxgiCapture::new().await?))
            }
            #[cfg(not(target_os = "windows"))]
            {
                Err(Error::Screen(
                    "DXGI capture only available on Windows".into(),
                ))
            }
        }
    }
}

/// Auto-detect the best available capture backend.
///
/// This function tries each backend in order and verifies it can actually
/// capture a frame before returning it. This ensures we don't select a backend
/// that initializes fine but fails at capture time (common with DRM on compositors
/// that use modifiers or atomic modesetting).
async fn auto_detect_backend() -> Result<Box<dyn ScreenCapture>> {
    #[cfg(target_os = "linux")]
    {
        // Try backends in order of preference

        // 1. DRM/KMS (best, but needs CAP_SYS_ADMIN)
        if drm::DrmCapture::is_available().await {
            match drm::DrmCapture::new().await {
                Ok(mut capture) => {
                    // Try to actually capture a frame to verify it works
                    let displays = capture.list_displays().await.unwrap_or_default();
                    if let Some(display) = displays.first() {
                        if capture.start(&display.id).await.is_ok() {
                            match capture.capture_frame().await {
                                Ok(Some(_)) => {
                                    tracing::info!("Using DRM/KMS capture backend");
                                    return Ok(Box::new(capture));
                                }
                                Ok(None) => {
                                    tracing::debug!(
                                        "DRM capture returned no frame, trying next backend"
                                    );
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "DRM capture failed test frame: {}, trying next backend",
                                        e
                                    );
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("DRM capture not available: {}", e);
                }
            }
        }

        // 2. XDG Portal (Wayland - may need one-time user approval)
        if portal::PortalCapture::is_available().await {
            match portal::PortalCapture::new(None).await {
                Ok(capture) => {
                    tracing::info!("Using XDG Portal capture backend (Wayland)");
                    // Note: Portal may require user approval on first use
                    // We don't test-capture here to avoid triggering the dialog during auto-detect
                    return Ok(Box::new(capture));
                }
                Err(e) => {
                    tracing::debug!("Portal capture not available: {}", e);
                }
            }
        }

        // 3. X11 SHM (if X11 session)
        if x11::X11Capture::is_available().await {
            match x11::X11Capture::new().await {
                Ok(capture) => {
                    tracing::info!("Using X11 SHM capture backend");
                    return Ok(Box::new(capture));
                }
                Err(e) => {
                    tracing::debug!("X11 capture not available: {}", e);
                }
            }
        }

        // No backend available
        Err(Error::Screen(
            "No capture backend available. DRM requires CAP_SYS_ADMIN, Portal requires Wayland+xdg-desktop-portal, X11 requires X11 session.".into()
        ))
    }

    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(dxgi::DxgiCapture::new().await?))
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(Error::Screen(
            "Screen capture not supported on this platform".into(),
        ))
    }
}

/// Check if any capture backend is available on the current system.
pub async fn is_capture_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        drm::DrmCapture::is_available().await
            || portal::PortalCapture::is_available().await
            || x11::X11Capture::is_available().await
    }

    #[cfg(target_os = "windows")]
    {
        true // DXGI is always available on Windows 8+
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        false
    }
}
