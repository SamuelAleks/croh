//! X11 SHM screen capture backend for Linux.
//!
//! This backend uses the X11 Shared Memory extension (XShm) for efficient
//! screen capture on X11 sessions. It does not work on pure Wayland.
//!
//! ## How it works
//!
//! 1. Connect to X server via DISPLAY env var
//! 2. Query SHM extension availability
//! 3. Enumerate screens via RandR
//! 4. Create shared memory segment for capture
//! 5. Use XShmGetImage to capture screen
//!
//! ## Limitations
//!
//! - Only works on X11 sessions (not pure Wayland)
//! - May work on XWayland but with limitations
//! - No cursor capture by default

use super::{CapturedFrame, Display, PixelFormat, ScreenCapture};
use crate::error::{Error, Result};
use std::sync::Arc;
use tracing::{debug, info, warn};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::randr::{self, ConnectionExt as RandrExt};
use x11rb::protocol::shm::{self, ConnectionExt as ShmExt};
use x11rb::protocol::xproto::{self, ImageFormat, Screen};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;

/// X11 SHM capture backend.
pub struct X11Capture {
    /// X11 connection
    conn: Option<Arc<RustConnection>>,
    /// Root window
    root: Option<xproto::Window>,
    /// Screen info
    screen: Option<Screen>,
    /// SHM segment
    shm_seg: Option<shm::Seg>,
    /// Shared memory id
    shm_id: Option<i32>,
    /// Shared memory pointer
    shm_ptr: Option<*mut u8>,
    /// Shared memory size
    shm_size: usize,
    /// Whether capture is active
    active: bool,
    /// Current display being captured (screen number or output name)
    current_display: Option<String>,
    /// Cached display list
    displays: Vec<Display>,
    /// Current capture dimensions
    capture_width: u32,
    capture_height: u32,
    capture_x: i32,
    capture_y: i32,
    /// Depth of the display
    depth: u8,
}

// Safety: X11Capture manages shared memory safely
unsafe impl Send for X11Capture {}
unsafe impl Sync for X11Capture {}

impl Drop for X11Capture {
    fn drop(&mut self) {
        self.cleanup_shm();
    }
}

impl X11Capture {
    /// Check if X11 capture is available.
    pub async fn is_available() -> bool {
        // Check if DISPLAY is set (indicates X11 session)
        if std::env::var("DISPLAY").is_err() {
            return false;
        }

        // Try to connect to X server
        match RustConnection::connect(None) {
            Ok((conn, _)) => {
                // Check if SHM extension is available
                conn.extension_information(shm::X11_EXTENSION_NAME)
                    .ok()
                    .flatten()
                    .is_some()
            }
            Err(_) => false,
        }
    }

    /// Create a new X11 capture backend.
    pub async fn new() -> Result<Self> {
        // Check for DISPLAY
        if std::env::var("DISPLAY").is_err() {
            return Err(Error::Screen("DISPLAY not set - not an X11 session".into()));
        }

        info!("Initializing X11 SHM capture");

        Ok(Self {
            conn: None,
            root: None,
            screen: None,
            shm_seg: None,
            shm_id: None,
            shm_ptr: None,
            shm_size: 0,
            active: false,
            current_display: None,
            displays: Vec::new(),
            capture_width: 0,
            capture_height: 0,
            capture_x: 0,
            capture_y: 0,
            depth: 24,
        })
    }

    /// Connect to the X server.
    fn connect(&mut self) -> Result<()> {
        let (conn, screen_num) = RustConnection::connect(None)
            .map_err(|e| Error::Screen(format!("Failed to connect to X server: {:?}", e)))?;

        // Verify SHM extension
        let shm_info = conn
            .extension_information(shm::X11_EXTENSION_NAME)
            .map_err(|e| Error::Screen(format!("Failed to query SHM extension: {:?}", e)))?
            .ok_or_else(|| Error::Screen("SHM extension not available".into()))?;

        debug!("SHM extension opcode: {}", shm_info.major_opcode);

        // Query SHM version
        let shm_version = conn
            .shm_query_version()
            .map_err(|e| Error::Screen(format!("Failed to query SHM version: {:?}", e)))?
            .reply()
            .map_err(|e| Error::Screen(format!("Failed to get SHM version reply: {:?}", e)))?;

        info!(
            "SHM version {}.{}, shared_pixmaps: {}",
            shm_version.major_version, shm_version.minor_version, shm_version.shared_pixmaps
        );

        // Get the screen
        let screen = conn.setup().roots[screen_num].clone();
        let root = screen.root;
        let depth = screen.root_depth;

        self.conn = Some(Arc::new(conn));
        self.root = Some(root);
        self.screen = Some(screen);
        self.depth = depth;

        // Enumerate displays using RandR
        self.enumerate_displays()?;

        Ok(())
    }

    /// Enumerate displays using RandR extension.
    fn enumerate_displays(&mut self) -> Result<()> {
        let conn = self
            .conn
            .as_ref()
            .ok_or_else(|| Error::Screen("Not connected to X server".into()))?;
        let root = self
            .root
            .ok_or_else(|| Error::Screen("No root window".into()))?;
        let screen = self
            .screen
            .as_ref()
            .ok_or_else(|| Error::Screen("No screen info".into()))?;

        self.displays.clear();

        // Check if RandR is available
        let randr_info = conn
            .extension_information(randr::X11_EXTENSION_NAME)
            .map_err(|e| Error::Screen(format!("Failed to query RandR: {:?}", e)))?;

        if randr_info.is_none() {
            // Fallback: use the whole screen as one display
            warn!("RandR extension not available, using default screen");
            self.displays.push(Display {
                id: "default".to_string(),
                name: "Default Screen".to_string(),
                width: screen.width_in_pixels as u32,
                height: screen.height_in_pixels as u32,
                refresh_rate: None,
                is_primary: true,
                x: 0,
                y: 0,
            });
            return Ok(());
        }

        // Query RandR screen resources
        let resources = conn
            .randr_get_screen_resources_current(root)
            .map_err(|e| Error::Screen(format!("Failed to get screen resources: {:?}", e)))?
            .reply()
            .map_err(|e| Error::Screen(format!("Failed to get screen resources reply: {:?}", e)))?;

        let mut primary_found = false;

        // Enumerate CRTCs (each CRTC represents an active output)
        for crtc in &resources.crtcs {
            let crtc_info = match conn.randr_get_crtc_info(*crtc, resources.config_timestamp) {
                Ok(cookie) => match cookie.reply() {
                    Ok(info) => info,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };

            // Skip inactive CRTCs
            if crtc_info.width == 0 || crtc_info.height == 0 {
                continue;
            }

            // Get output name for the first output connected to this CRTC
            let output_name = if let Some(output) = crtc_info.outputs.first() {
                match conn.randr_get_output_info(*output, resources.config_timestamp) {
                    Ok(cookie) => match cookie.reply() {
                        Ok(info) => String::from_utf8_lossy(&info.name).to_string(),
                        Err(_) => format!("CRTC-{}", crtc),
                    },
                    Err(_) => format!("CRTC-{}", crtc),
                }
            } else {
                format!("CRTC-{}", crtc)
            };

            // Get refresh rate from current mode
            let refresh_rate = if crtc_info.mode != 0 {
                resources
                    .modes
                    .iter()
                    .find(|m| m.id == crtc_info.mode)
                    .map(|mode| {
                        if mode.htotal > 0 && mode.vtotal > 0 {
                            let rate =
                                (mode.dot_clock as f64) / (mode.htotal as f64 * mode.vtotal as f64);
                            rate.round() as u32
                        } else {
                            60 // Default
                        }
                    })
            } else {
                None
            };

            let is_primary = !primary_found;
            if is_primary {
                primary_found = true;
            }

            self.displays.push(Display {
                id: output_name.clone(),
                name: output_name,
                width: crtc_info.width as u32,
                height: crtc_info.height as u32,
                refresh_rate,
                is_primary,
                x: crtc_info.x as i32,
                y: crtc_info.y as i32,
            });
        }

        // If no displays found, use the whole screen
        if self.displays.is_empty() {
            warn!("No RandR outputs found, using default screen");
            self.displays.push(Display {
                id: "default".to_string(),
                name: "Default Screen".to_string(),
                width: screen.width_in_pixels as u32,
                height: screen.height_in_pixels as u32,
                refresh_rate: None,
                is_primary: true,
                x: 0,
                y: 0,
            });
        }

        info!("Found {} X11 displays", self.displays.len());
        for disp in &self.displays {
            info!(
                "  {} ({}x{} @ {}Hz) at ({}, {})",
                disp.name,
                disp.width,
                disp.height,
                disp.refresh_rate.unwrap_or(0),
                disp.x,
                disp.y
            );
        }

        Ok(())
    }

    /// Setup shared memory segment for capture.
    fn setup_shm(&mut self, width: u32, height: u32) -> Result<()> {
        // Check connection exists
        if self.conn.is_none() {
            return Err(Error::Screen("Not connected to X server".into()));
        }

        // Clean up any existing SHM
        self.cleanup_shm();

        let conn = self.conn.as_ref().unwrap();

        // Calculate buffer size (4 bytes per pixel for 32-bit depth)
        let bytes_per_pixel = 4u32;
        let size = (width * height * bytes_per_pixel) as usize;

        // Create System V shared memory segment
        let shm_id = unsafe { libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600) };

        if shm_id < 0 {
            return Err(Error::Screen(format!(
                "Failed to create shared memory segment: {}",
                std::io::Error::last_os_error()
            )));
        }

        // Attach shared memory
        let shm_ptr = unsafe { libc::shmat(shm_id, std::ptr::null(), 0) };
        if shm_ptr == (-1isize) as *mut libc::c_void {
            unsafe { libc::shmctl(shm_id, libc::IPC_RMID, std::ptr::null_mut()) };
            return Err(Error::Screen(format!(
                "Failed to attach shared memory: {}",
                std::io::Error::last_os_error()
            )));
        }

        // Create SHM segment ID for X11
        let shm_seg = conn
            .generate_id()
            .map_err(|e| Error::Screen(format!("Failed to generate SHM segment ID: {:?}", e)))?;

        // Attach SHM to X server
        conn.shm_attach(shm_seg, shm_id as u32, false)
            .map_err(|e| Error::Screen(format!("Failed to attach SHM to X server: {:?}", e)))?;

        // Mark the segment for deletion (will be deleted when all processes detach)
        unsafe { libc::shmctl(shm_id, libc::IPC_RMID, std::ptr::null_mut()) };

        // Sync to ensure attach is complete
        conn.sync()
            .map_err(|e| Error::Screen(format!("Failed to sync X connection: {:?}", e)))?;

        self.shm_seg = Some(shm_seg);
        self.shm_id = Some(shm_id);
        self.shm_ptr = Some(shm_ptr as *mut u8);
        self.shm_size = size;
        self.capture_width = width;
        self.capture_height = height;

        debug!(
            "Created SHM segment: id={}, seg={}, size={} bytes ({}x{})",
            shm_id, shm_seg, size, width, height
        );

        Ok(())
    }

    /// Clean up shared memory segment.
    fn cleanup_shm(&mut self) {
        if let (Some(conn), Some(shm_seg)) = (self.conn.as_ref(), self.shm_seg) {
            let _ = conn.shm_detach(shm_seg);
            let _ = conn.sync();
        }

        if let Some(shm_ptr) = self.shm_ptr {
            unsafe { libc::shmdt(shm_ptr as *const _) };
        }

        self.shm_seg = None;
        self.shm_id = None;
        self.shm_ptr = None;
        self.shm_size = 0;
    }

    /// Capture a frame using SHM.
    fn capture_shm(&self) -> Result<CapturedFrame> {
        let conn = self
            .conn
            .as_ref()
            .ok_or_else(|| Error::Screen("Not connected to X server".into()))?;
        let root = self
            .root
            .ok_or_else(|| Error::Screen("No root window".into()))?;
        let shm_seg = self
            .shm_seg
            .ok_or_else(|| Error::Screen("SHM not initialized".into()))?;
        let shm_ptr = self
            .shm_ptr
            .ok_or_else(|| Error::Screen("SHM pointer not set".into()))?;

        // Use shm_get_image to capture the screen
        let reply = conn
            .shm_get_image(
                root,
                self.capture_x as i16,
                self.capture_y as i16,
                self.capture_width as u16,
                self.capture_height as u16,
                !0, // plane_mask: all planes
                ImageFormat::Z_PIXMAP.into(),
                shm_seg,
                0, // offset
            )
            .map_err(|e| Error::Screen(format!("Failed to send SHM get_image: {:?}", e)))?
            .reply()
            .map_err(|e| Error::Screen(format!("Failed to get SHM image reply: {:?}", e)))?;

        debug!(
            "Captured frame: depth={}, visual={}, size={}",
            reply.depth, reply.visual, reply.size
        );

        // Copy the image data from shared memory
        let bytes_per_pixel = 4usize;
        let row_bytes = self.capture_width as usize * bytes_per_pixel;
        let total_bytes = row_bytes * self.capture_height as usize;

        let data = unsafe { std::slice::from_raw_parts(shm_ptr, total_bytes).to_vec() };

        // X11 typically uses BGRA format
        Ok(CapturedFrame {
            width: self.capture_width,
            height: self.capture_height,
            format: PixelFormat::Bgrx8,
            data,
            stride: row_bytes as u32,
            timestamp: std::time::Instant::now(),
            dmabuf_fd: None,
        })
    }
}

#[async_trait::async_trait]
impl ScreenCapture for X11Capture {
    fn name(&self) -> &'static str {
        "X11 SHM"
    }

    async fn list_displays(&self) -> Result<Vec<Display>> {
        if self.displays.is_empty() {
            // Create a temporary instance to enumerate
            let mut temp = Self::new().await?;
            temp.connect()?;
            return Ok(std::mem::take(&mut temp.displays));
        }
        Ok(self.displays.clone())
    }

    async fn start(&mut self, display_id: &str) -> Result<()> {
        info!("Starting X11 capture for display {}", display_id);

        // Connect if not already connected
        if self.conn.is_none() {
            self.connect()?;
        }

        // Find the requested display
        let display = self
            .displays
            .iter()
            .find(|d| d.id == display_id)
            .ok_or_else(|| Error::Screen(format!("Display not found: {}", display_id)))?
            .clone();

        // Setup SHM for the display size
        self.setup_shm(display.width, display.height)?;

        self.capture_x = display.x;
        self.capture_y = display.y;
        self.current_display = Some(display_id.to_string());
        self.active = true;

        info!(
            "X11 capture started for {} ({}x{}) at ({}, {})",
            display_id, self.capture_width, self.capture_height, self.capture_x, self.capture_y
        );

        Ok(())
    }

    async fn capture_frame(&mut self) -> Result<Option<CapturedFrame>> {
        if !self.active {
            return Err(Error::Screen("Capture not started".into()));
        }

        let frame = self.capture_shm()?;
        Ok(Some(frame))
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping X11 capture");
        self.cleanup_shm();
        self.active = false;
        self.current_display = None;
        Ok(())
    }

    fn requires_privileges(&self) -> bool {
        false // X11 capture doesn't need special privileges
    }
}
