//! DRM/KMS screen capture backend for Linux.
//!
//! This backend reads directly from the GPU framebuffer using the Linux
//! DRM (Direct Rendering Manager) subsystem. It bypasses all compositor
//! security prompts but requires CAP_SYS_ADMIN capability.
//!
//! Based on ReFrame's approach: <https://github.com/AlynxZhou/reframe>
//!
//! ## How it works
//!
//! 1. Open `/dev/dri/card0` (or other DRM device)
//! 2. Enable universal planes API
//! 3. Drop DRM master (allows compositor to continue)
//! 4. Enumerate connectors and CRTCs
//! 5. Export framebuffer as DMA-BUF
//! 6. Memory-map and copy pixels
//!
//! ## Privileges
//!
//! Requires `CAP_SYS_ADMIN` capability:
//! ```bash
//! sudo setcap cap_sys_admin+ep /path/to/croh-daemon
//! ```

use super::{CapturedFrame, Display, PixelFormat, ScreenCapture};
use crate::error::{Error, Result};
use drm::control::{connector, crtc, framebuffer, Device as ControlDevice};
use drm::Device;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Default DRM card device path.
const DEFAULT_DRM_CARD: &str = "/dev/dri/card0";

/// DRM device wrapper that implements the drm traits.
struct DrmDevice {
    file: File,
}

impl DrmDevice {
    fn open(path: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }
}

impl AsFd for DrmDevice {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl Device for DrmDevice {}
impl ControlDevice for DrmDevice {}

/// Information about a connected display.
#[derive(Debug, Clone)]
struct ConnectorInfo {
    /// Connector handle
    #[allow(dead_code)]
    handle: connector::Handle,
    /// Human-readable name (e.g., "HDMI-A-1")
    #[allow(dead_code)]
    name: String,
    /// Associated CRTC
    crtc: Option<crtc::Handle>,
    /// Current mode width
    width: u32,
    /// Current mode height
    height: u32,
    /// Refresh rate in Hz
    refresh_rate: u32,
    /// Position X
    #[allow(dead_code)]
    x: i32,
    /// Position Y
    #[allow(dead_code)]
    y: i32,
}

/// DRM/KMS capture backend.
pub struct DrmCapture {
    /// DRM device path
    device_path: PathBuf,
    /// Whether capture is active
    active: bool,
    /// Current display being captured
    #[allow(dead_code)]
    current_display: Option<String>,
    /// Cached display list
    displays: Vec<Display>,
    /// DRM device (opened when capturing)
    device: Option<DrmDevice>,
    /// Connector info map
    connectors: HashMap<String, ConnectorInfo>,
    /// Currently selected CRTC
    current_crtc: Option<crtc::Handle>,
}

impl DrmCapture {
    /// Check if DRM capture is available (can open device).
    pub async fn is_available() -> bool {
        // Check if we can read the DRM device
        if !std::fs::metadata(DEFAULT_DRM_CARD).is_ok() {
            return false;
        }

        // Try to open the device
        match DrmDevice::open(DEFAULT_DRM_CARD) {
            Ok(device) => {
                // Check if we have the required capabilities
                // Try to get resources - this requires appropriate permissions
                device.resource_handles().is_ok()
            }
            Err(_) => false,
        }
    }

    /// Create a new DRM capture backend.
    pub async fn new() -> Result<Self> {
        Self::with_device(DEFAULT_DRM_CARD).await
    }

    /// Create with a specific DRM device.
    pub async fn with_device(device_path: &str) -> Result<Self> {
        info!("Initializing DRM capture from {}", device_path);

        // Verify device exists
        if !std::path::Path::new(device_path).exists() {
            return Err(Error::Screen(format!(
                "DRM device not found: {}",
                device_path
            )));
        }

        Ok(Self {
            device_path: PathBuf::from(device_path),
            active: false,
            current_display: None,
            displays: Vec::new(),
            device: None,
            connectors: HashMap::new(),
            current_crtc: None,
        })
    }

    /// Open the DRM device and enumerate displays.
    fn open_device(&mut self) -> Result<()> {
        let device = DrmDevice::open(self.device_path.to_str().unwrap_or(DEFAULT_DRM_CARD))
            .map_err(|e| Error::Screen(format!("Failed to open DRM device: {}", e)))?;

        // Enable universal planes to access all plane types
        if let Err(e) = device.set_client_capability(drm::ClientCapability::UniversalPlanes, true) {
            warn!("Failed to enable universal planes: {:?}", e);
        }

        // Release master to allow the compositor to continue functioning
        // This is crucial - without releasing master, the compositor would freeze
        if let Err(e) = device.release_master_lock() {
            debug!("Failed to release DRM master (may not have been master): {:?}", e);
        }

        self.device = Some(device);
        self.enumerate_connectors()?;

        Ok(())
    }

    /// Enumerate connected displays.
    fn enumerate_connectors(&mut self) -> Result<()> {
        let device = self.device.as_ref().ok_or_else(|| {
            Error::Screen("DRM device not open".into())
        })?;

        let resources = device.resource_handles().map_err(|e| {
            Error::Screen(format!("Failed to get DRM resources: {:?}", e))
        })?;

        self.connectors.clear();
        self.displays.clear();

        let mut primary_found = false;

        for conn_handle in resources.connectors() {
            let connector = match device.get_connector(*conn_handle, false) {
                Ok(c) => c,
                Err(e) => {
                    debug!("Failed to get connector {:?}: {:?}", conn_handle, e);
                    continue;
                }
            };

            // Only process connected displays
            if connector.state() != connector::State::Connected {
                continue;
            }

            // Get connector name
            let name = format!("{:?}-{}", connector.interface(), connector.interface_id());

            // Get current CRTC
            let current_encoder = connector.current_encoder();
            let crtc_handle = if let Some(enc_handle) = current_encoder {
                if let Ok(encoder) = device.get_encoder(enc_handle) {
                    encoder.crtc()
                } else {
                    None
                }
            } else {
                None
            };

            // Get current mode
            let (width, height, refresh_rate) = if let Some(crtc_h) = crtc_handle {
                if let Ok(crtc_info) = device.get_crtc(crtc_h) {
                    if let Some(mode) = crtc_info.mode() {
                        (
                            mode.size().0 as u32,
                            mode.size().1 as u32,
                            (mode.vrefresh()) as u32,
                        )
                    } else {
                        // No current mode, use first available
                        if let Some(mode) = connector.modes().first() {
                            (mode.size().0 as u32, mode.size().1 as u32, mode.vrefresh() as u32)
                        } else {
                            continue;
                        }
                    }
                } else {
                    continue;
                }
            } else {
                // No CRTC, use first available mode
                if let Some(mode) = connector.modes().first() {
                    (mode.size().0 as u32, mode.size().1 as u32, mode.vrefresh() as u32)
                } else {
                    continue;
                }
            };

            // Get position from CRTC (default to 0,0)
            let (x, y) = if let Some(crtc_h) = crtc_handle {
                if let Ok(crtc_info) = device.get_crtc(crtc_h) {
                    let pos = crtc_info.position();
                    (pos.0 as i32, pos.1 as i32)
                } else {
                    (0, 0)
                }
            } else {
                (0, 0)
            };

            let is_primary = !primary_found;
            if is_primary {
                primary_found = true;
            }

            let info = ConnectorInfo {
                handle: *conn_handle,
                name: name.clone(),
                crtc: crtc_handle,
                width,
                height,
                refresh_rate,
                x,
                y,
            };

            self.displays.push(Display {
                id: name.clone(),
                name: name.clone(),
                width,
                height,
                refresh_rate: Some(refresh_rate),
                is_primary,
                x,
                y,
            });

            self.connectors.insert(name, info);
        }

        info!("Found {} connected displays", self.displays.len());
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

    /// Get the framebuffer for a CRTC.
    fn get_framebuffer(&self, crtc_handle: crtc::Handle) -> Result<framebuffer::Handle> {
        let device = self.device.as_ref().ok_or_else(|| {
            Error::Screen("DRM device not open".into())
        })?;

        let crtc_info = device.get_crtc(crtc_handle).map_err(|e| {
            Error::Screen(format!("Failed to get CRTC info: {:?}", e))
        })?;

        crtc_info.framebuffer().ok_or_else(|| {
            Error::Screen("No framebuffer attached to CRTC".into())
        })
    }

    /// Export framebuffer as DMA-BUF and read pixels.
    fn capture_framebuffer(&self, fb_handle: framebuffer::Handle) -> Result<CapturedFrame> {
        let device = self.device.as_ref().ok_or_else(|| {
            Error::Screen("DRM device not open".into())
        })?;

        // Get framebuffer info
        let fb_info = device.get_framebuffer(fb_handle).map_err(|e| {
            Error::Screen(format!("Failed to get framebuffer info: {:?}", e))
        })?;

        let width = fb_info.size().0;
        let height = fb_info.size().1;
        let depth = fb_info.depth();
        let bpp = fb_info.bpp();
        let pitch = fb_info.pitch();

        debug!(
            "Framebuffer: {}x{}, depth={}, bpp={}, pitch={}",
            width, height, depth, bpp, pitch
        );

        // Determine pixel format based on depth/bpp
        let format = match (depth, bpp) {
            (24, 32) | (32, 32) => PixelFormat::Bgrx8, // Most common: XRGB8888 / ARGB8888
            (24, 24) => PixelFormat::Bgra8, // BGR888
            _ => {
                warn!("Unknown framebuffer format: depth={}, bpp={}", depth, bpp);
                PixelFormat::Bgrx8 // Assume common format
            }
        };

        // Get the buffer handle from the framebuffer info
        let buffer_handle = fb_info.buffer().ok_or_else(|| {
            Error::Screen("Framebuffer has no buffer handle".into())
        })?;

        // Convert buffer::Handle to u32 for the PRIME export
        let handle_u32: u32 = buffer_handle.into();

        match self.try_dma_buf_capture(device, handle_u32, width, height, pitch, format) {
            Ok(frame) => return Ok(frame),
            Err(e) => {
                debug!("DMA-BUF export failed: {:?}", e);
            }
        }

        // Fallback: try to map the framebuffer directly
        // This is more limited and may not work on all hardware
        Err(Error::Screen(
            "Failed to capture framebuffer: DMA-BUF export not supported and no fallback available".into()
        ))
    }

    /// Try to capture using DMA-BUF export (zero-copy path).
    fn try_dma_buf_capture(
        &self,
        device: &DrmDevice,
        handle: u32,
        width: u32,
        height: u32,
        pitch: u32,
        format: PixelFormat,
    ) -> Result<CapturedFrame> {
        // Export the buffer handle as a DMA-BUF fd using PRIME
        let prime_result = drm_ffi::gem::handle_to_fd(
            device.as_fd(),
            handle,
            drm_ffi::DRM_CLOEXEC | drm_ffi::DRM_RDWR,
        ).map_err(|e| Error::Screen(format!("PRIME_HANDLE_TO_FD failed: {:?}", e)))?;

        let fd = unsafe { OwnedFd::from_raw_fd(prime_result.fd) };

        // Memory-map the DMA-BUF
        let bytes_per_pixel: u32 = match format {
            PixelFormat::Rgba8 | PixelFormat::Bgra8 | PixelFormat::Rgbx8 | PixelFormat::Bgrx8 => 4,
        };
        let buffer_size = (pitch * height) as usize;

        let mapped = unsafe {
            let ptr = libc::mmap(
                std::ptr::null_mut(),
                buffer_size,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            );

            if ptr == libc::MAP_FAILED {
                return Err(Error::Screen(format!(
                    "Failed to mmap DMA-BUF: {}",
                    std::io::Error::last_os_error()
                )));
            }

            std::slice::from_raw_parts(ptr as *const u8, buffer_size)
        };

        // Copy the pixel data (we need to copy since we'll unmap)
        // Handle stride if pitch != width * bpp
        let row_bytes = (width * bytes_per_pixel) as usize;
        let mut data = Vec::with_capacity((width * height * bytes_per_pixel) as usize);

        for y in 0..height as usize {
            let row_start = y * pitch as usize;
            data.extend_from_slice(&mapped[row_start..row_start + row_bytes]);
        }

        // Unmap the buffer
        unsafe {
            libc::munmap(mapped.as_ptr() as *mut _, buffer_size);
        }

        Ok(CapturedFrame {
            width,
            height,
            format,
            data,
            stride: row_bytes as u32,
            timestamp: std::time::Instant::now(),
            dmabuf_fd: None, // We copied the data, no need to keep fd
        })
    }
}

#[async_trait::async_trait]
impl ScreenCapture for DrmCapture {
    fn name(&self) -> &'static str {
        "DRM/KMS"
    }

    async fn list_displays(&self) -> Result<Vec<Display>> {
        // If we haven't enumerated yet, do it now
        if self.displays.is_empty() {
            // Create a temporary device to enumerate
            let mut temp = Self::with_device(self.device_path.to_str().unwrap_or(DEFAULT_DRM_CARD)).await?;
            temp.open_device()?;
            return Ok(temp.displays);
        }
        Ok(self.displays.clone())
    }

    async fn start(&mut self, display_id: &str) -> Result<()> {
        info!("Starting DRM capture for display {}", display_id);

        // Open device if not already open
        if self.device.is_none() {
            self.open_device()?;
        }

        // Find the requested display
        let connector_info = self.connectors.get(display_id).ok_or_else(|| {
            Error::Screen(format!("Display not found: {}", display_id))
        })?;

        let crtc_handle = connector_info.crtc.ok_or_else(|| {
            Error::Screen(format!("Display {} has no CRTC assigned", display_id))
        })?;

        self.current_crtc = Some(crtc_handle);
        self.current_display = Some(display_id.to_string());
        self.active = true;

        info!(
            "DRM capture started for {} ({}x{} @ {}Hz)",
            display_id,
            connector_info.width,
            connector_info.height,
            connector_info.refresh_rate
        );

        Ok(())
    }

    async fn capture_frame(&mut self) -> Result<Option<CapturedFrame>> {
        if !self.active {
            return Err(Error::Screen("Capture not started".into()));
        }

        let crtc_handle = self.current_crtc.ok_or_else(|| {
            Error::Screen("No CRTC selected".into())
        })?;

        // Get current framebuffer
        let fb_handle = self.get_framebuffer(crtc_handle)?;

        // Capture the framebuffer
        let frame = self.capture_framebuffer(fb_handle)?;

        Ok(Some(frame))
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping DRM capture");
        self.active = false;
        self.current_display = None;
        self.current_crtc = None;
        // Keep device open for potential restart
        Ok(())
    }

    fn requires_privileges(&self) -> bool {
        true // Requires CAP_SYS_ADMIN
    }
}
