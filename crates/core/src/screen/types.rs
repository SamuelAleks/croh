//! Common types for screen capture backends.

use crate::error::Result;

/// A captured frame ready for encoding or transmission.
#[derive(Debug)]
pub struct CapturedFrame {
    /// Frame width in pixels
    pub width: u32,
    /// Frame height in pixels
    pub height: u32,
    /// Pixel format
    pub format: PixelFormat,
    /// Raw pixel data
    pub data: Vec<u8>,
    /// Stride (bytes per row, may include padding)
    pub stride: u32,
    /// Capture timestamp
    pub timestamp: std::time::Instant,
    /// DMA-BUF file descriptor (for zero-copy path on Linux)
    #[cfg(target_os = "linux")]
    pub dmabuf_fd: Option<std::os::fd::RawFd>,
}

impl CapturedFrame {
    /// Create a new captured frame.
    pub fn new(width: u32, height: u32, format: PixelFormat, data: Vec<u8>, stride: u32) -> Self {
        Self {
            width,
            height,
            format,
            data,
            stride,
            timestamp: std::time::Instant::now(),
            #[cfg(target_os = "linux")]
            dmabuf_fd: None,
        }
    }

    /// Get the expected data size based on dimensions and format.
    pub fn expected_size(&self) -> usize {
        (self.stride * self.height) as usize
    }

    /// Convert pixel data to RGBA format (if not already).
    pub fn to_rgba(&self) -> Vec<u8> {
        match self.format {
            PixelFormat::Rgba8 => self.data.clone(),
            PixelFormat::Bgra8 => {
                // Swap R and B channels
                let mut rgba = self.data.clone();
                for chunk in rgba.chunks_exact_mut(4) {
                    chunk.swap(0, 2);
                }
                rgba
            }
            PixelFormat::Rgbx8 => {
                // Set alpha to 255
                let mut rgba = self.data.clone();
                for chunk in rgba.chunks_exact_mut(4) {
                    chunk[3] = 255;
                }
                rgba
            }
            PixelFormat::Bgrx8 => {
                // Swap R and B, set alpha to 255
                let mut rgba = self.data.clone();
                for chunk in rgba.chunks_exact_mut(4) {
                    chunk.swap(0, 2);
                    chunk[3] = 255;
                }
                rgba
            }
        }
    }
}

/// Pixel format of captured frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// RGBA (red, green, blue, alpha) - standard format
    Rgba8,
    /// BGRA (blue, green, red, alpha) - Windows/DirectX native
    Bgra8,
    /// RGBX (red, green, blue, padding) - no alpha
    Rgbx8,
    /// BGRX (blue, green, red, padding) - DRM native
    Bgrx8,
}

impl PixelFormat {
    /// Bytes per pixel for this format.
    pub fn bytes_per_pixel(&self) -> u32 {
        4 // All supported formats are 4 bytes per pixel
    }
}

/// Display information from the capture backend.
#[derive(Debug, Clone)]
pub struct Display {
    /// Unique display identifier
    pub id: String,
    /// Human-readable name (e.g., "HDMI-1", "eDP-1")
    pub name: String,
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Refresh rate in Hz (if known)
    pub refresh_rate: Option<u32>,
    /// Whether this is the primary display
    pub is_primary: bool,
    /// X offset in virtual screen space (for multi-monitor)
    pub x: i32,
    /// Y offset in virtual screen space (for multi-monitor)
    pub y: i32,
}

impl Display {
    /// Create a simple display info.
    pub fn new(id: String, name: String, width: u32, height: u32) -> Self {
        Self {
            id,
            name,
            width,
            height,
            refresh_rate: None,
            is_primary: false,
            x: 0,
            y: 0,
        }
    }
}

/// Screen capture backend trait.
///
/// Implementations provide platform-specific screen capture functionality.
/// Each backend should handle its own initialization and cleanup.
#[async_trait::async_trait]
pub trait ScreenCapture: Send + Sync {
    /// Get the backend name for logging and display.
    fn name(&self) -> &'static str;

    /// List available displays.
    async fn list_displays(&self) -> Result<Vec<Display>>;

    /// Start capturing a specific display.
    ///
    /// # Arguments
    /// * `display_id` - The display identifier from `list_displays()`
    async fn start(&mut self, display_id: &str) -> Result<()>;

    /// Capture the next frame.
    ///
    /// Returns `None` if no new frame is available (e.g., screen hasn't changed).
    /// This method should be called in a loop at the desired frame rate.
    async fn capture_frame(&mut self) -> Result<Option<CapturedFrame>>;

    /// Capture the current cursor state.
    ///
    /// Returns cursor position, visibility, and optionally the shape if it changed.
    /// Default implementation returns None (cursor capture not supported).
    async fn capture_cursor(&mut self) -> Result<Option<CapturedCursor>> {
        Ok(None)
    }

    /// Stop capturing and release resources.
    async fn stop(&mut self) -> Result<()>;

    /// Check if this backend requires elevated privileges.
    fn requires_privileges(&self) -> bool;

    /// Get additional backend-specific information.
    fn info(&self) -> BackendInfo {
        BackendInfo {
            name: self.name(),
            requires_privileges: self.requires_privileges(),
            supports_cursor: false,
            supports_region: false,
        }
    }
}

/// Additional information about a capture backend.
#[derive(Debug, Clone)]
pub struct BackendInfo {
    /// Backend name
    pub name: &'static str,
    /// Whether elevated privileges are required
    pub requires_privileges: bool,
    /// Whether cursor capture is supported
    pub supports_cursor: bool,
    /// Whether region capture is supported (vs full display)
    pub supports_region: bool,
}

// ==================== Cursor Types ====================

/// Captured cursor information.
#[derive(Debug, Clone)]
pub struct CapturedCursor {
    /// X position in screen coordinates
    pub x: i32,
    /// Y position in screen coordinates
    pub y: i32,
    /// Whether cursor is visible
    pub visible: bool,
    /// Cursor shape (only if changed or first capture)
    pub shape: Option<CursorShape>,
}

/// Cursor shape/image.
#[derive(Debug, Clone)]
pub struct CursorShape {
    /// Unique ID for this shape (based on content hash)
    pub shape_id: u64,
    /// Cursor width in pixels
    pub width: u32,
    /// Cursor height in pixels
    pub height: u32,
    /// X hotspot (click point within image)
    pub hotspot_x: u32,
    /// Y hotspot (click point within image)
    pub hotspot_y: u32,
    /// RGBA pixel data (width * height * 4 bytes)
    pub data: Vec<u8>,
}

impl CursorShape {
    /// Calculate a hash-based ID from the cursor data.
    pub fn calculate_id(data: &[u8]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut hasher);
        hasher.finish()
    }

    /// Create a new cursor shape with auto-calculated ID.
    pub fn new(width: u32, height: u32, hotspot_x: u32, hotspot_y: u32, data: Vec<u8>) -> Self {
        let shape_id = Self::calculate_id(&data);
        Self {
            shape_id,
            width,
            height,
            hotspot_x,
            hotspot_y,
            data,
        }
    }
}
