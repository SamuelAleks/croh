//! DXGI Desktop Duplication capture backend for Windows.
//!
//! This backend uses the Desktop Duplication API introduced in Windows 8
//! for efficient screen capture with GPU acceleration.
//!
//! ## How it works
//!
//! 1. Create D3D11 device
//! 2. Get DXGI output (display)
//! 3. Duplicate output for capture
//! 4. Acquire frames with timeout
//! 5. Copy to staging texture for CPU readback
//!
//! ## Requirements
//!
//! - Windows 8 or later
//! - No special privileges needed

#[cfg(target_os = "windows")]
use super::{CapturedCursor, CapturedFrame, CursorShape, Display, PixelFormat, ScreenCapture};
#[cfg(target_os = "windows")]
use crate::error::{Error, Result};
#[cfg(target_os = "windows")]
use tracing::{debug, info};
#[cfg(target_os = "windows")]
use windows::{
    core::Interface,
    Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE,
    Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_FLAG, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
        D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    },
    Win32::Graphics::Dxgi::{
        Common::DXGI_FORMAT_B8G8R8A8_UNORM, CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1,
        IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication, DXGI_OUTDUPL_FRAME_INFO,
    },
    Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetBitmapBits, GetDIBits, GetObjectW,
        SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
    },
    Win32::UI::WindowsAndMessaging::{
        CopyIcon, DestroyIcon, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, ICONINFO,
    },
};

#[cfg(not(target_os = "windows"))]
use super::{CapturedCursor, CapturedFrame, Display, ScreenCapture};
#[cfg(not(target_os = "windows"))]
use crate::error::{Error, Result};
#[cfg(not(target_os = "windows"))]
use tracing::{info, warn};

/// DXGI Desktop Duplication capture backend.
pub struct DxgiCapture {
    /// Whether capture is active
    active: bool,
    /// Current display being captured
    #[allow(dead_code)]
    current_display: Option<String>,
    /// Cached display list
    displays: Vec<Display>,
    #[cfg(target_os = "windows")]
    /// D3D11 device
    device: Option<ID3D11Device>,
    #[cfg(target_os = "windows")]
    /// D3D11 device context
    context: Option<ID3D11DeviceContext>,
    #[cfg(target_os = "windows")]
    /// Output duplication
    duplication: Option<IDXGIOutputDuplication>,
    #[cfg(target_os = "windows")]
    /// Staging texture for CPU readback
    staging_texture: Option<ID3D11Texture2D>,
    #[cfg(target_os = "windows")]
    /// Current capture dimensions
    capture_width: u32,
    #[cfg(target_os = "windows")]
    capture_height: u32,
    #[cfg(target_os = "windows")]
    /// Display offset for cursor position calculation
    display_x: i32,
    #[cfg(target_os = "windows")]
    display_y: i32,
    #[cfg(target_os = "windows")]
    /// Last captured cursor shape ID (to detect changes)
    last_cursor_shape_id: u64,
}

impl DxgiCapture {
    /// Check if DXGI capture is available.
    pub async fn is_available() -> bool {
        #[cfg(target_os = "windows")]
        {
            // Try to create a DXGI factory to verify DXGI is available
            unsafe { CreateDXGIFactory1::<IDXGIFactory1>().is_ok() }
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    /// Create a new DXGI capture backend.
    pub async fn new() -> Result<Self> {
        info!("Initializing DXGI capture");

        #[cfg(target_os = "windows")]
        {
            Ok(Self {
                active: false,
                current_display: None,
                displays: Vec::new(),
                device: None,
                context: None,
                duplication: None,
                staging_texture: None,
                capture_width: 0,
                capture_height: 0,
                display_x: 0,
                display_y: 0,
                last_cursor_shape_id: 0,
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(Self {
                active: false,
                current_display: None,
                displays: Vec::new(),
            })
        }
    }

    #[cfg(target_os = "windows")]
    /// Initialize D3D11 device.
    fn init_d3d11(&mut self) -> Result<()> {
        unsafe {
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;

            D3D11CreateDevice(
                None, // Default adapter
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_FLAG(0),
                None, // Feature levels (use default)
                D3D11_SDK_VERSION,
                Some(&mut device),
                None, // Actual feature level
                Some(&mut context),
            )
            .map_err(|e| Error::Screen(format!("Failed to create D3D11 device: {:?}", e)))?;

            self.device = device;
            self.context = context;

            debug!("D3D11 device created successfully");
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    /// Enumerate DXGI outputs (displays).
    fn enumerate_outputs(&mut self) -> Result<()> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()
                .map_err(|e| Error::Screen(format!("Failed to create DXGI factory: {:?}", e)))?;

            self.displays.clear();
            let mut adapter_index = 0u32;

            // Enumerate adapters
            while let Ok(adapter) = factory.EnumAdapters1(adapter_index) {
                let mut output_index = 0u32;

                // Enumerate outputs for this adapter
                while let Ok(output) = adapter.EnumOutputs(output_index) {
                    let desc = output.GetDesc().map_err(|e| {
                        Error::Screen(format!("Failed to get output desc: {:?}", e))
                    })?;

                    // Get display name
                    let name_end = desc
                        .DeviceName
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(desc.DeviceName.len());
                    let name = String::from_utf16_lossy(&desc.DeviceName[..name_end]);

                    let rect = desc.DesktopCoordinates;
                    let width = (rect.right - rect.left) as u32;
                    let height = (rect.bottom - rect.top) as u32;

                    let is_primary = adapter_index == 0 && output_index == 0;

                    self.displays.push(Display {
                        id: format!("{}:{}", adapter_index, output_index),
                        name,
                        width,
                        height,
                        refresh_rate: None, // Would need to query mode list
                        is_primary,
                        x: rect.left,
                        y: rect.top,
                    });

                    output_index += 1;
                }
                adapter_index += 1;
            }

            info!("Found {} DXGI outputs", self.displays.len());
            for disp in &self.displays {
                info!(
                    "  {} ({}x{}) at ({}, {})",
                    disp.name, disp.width, disp.height, disp.x, disp.y
                );
            }
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    /// Setup output duplication for a specific display.
    fn setup_duplication(&mut self, adapter_idx: u32, output_idx: u32) -> Result<()> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()
                .map_err(|e| Error::Screen(format!("Failed to create DXGI factory: {:?}", e)))?;

            let adapter: IDXGIAdapter1 = factory.EnumAdapters1(adapter_idx).map_err(|e| {
                Error::Screen(format!("Adapter {} not found: {:?}", adapter_idx, e))
            })?;

            let output: IDXGIOutput = adapter
                .EnumOutputs(output_idx)
                .map_err(|e| Error::Screen(format!("Output {} not found: {:?}", output_idx, e)))?;

            // Get output1 interface for DuplicateOutput
            let output1: IDXGIOutput1 = output
                .cast()
                .map_err(|e| Error::Screen(format!("Failed to get IDXGIOutput1: {:?}", e)))?;

            let device = self
                .device
                .as_ref()
                .ok_or_else(|| Error::Screen("D3D11 device not initialized".into()))?;

            // Create output duplication
            let duplication = output1
                .DuplicateOutput(device)
                .map_err(|e| Error::Screen(format!("Failed to duplicate output: {:?}", e)))?;

            // Get output description for dimensions
            let desc = output
                .GetDesc()
                .map_err(|e| Error::Screen(format!("Failed to get output desc: {:?}", e)))?;

            let width = (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left) as u32;
            let height = (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top) as u32;

            // Create staging texture for CPU readback
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let mut staging: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(|e| Error::Screen(format!("Failed to create staging texture: {:?}", e)))?;

            self.duplication = Some(duplication);
            self.staging_texture = staging;
            self.capture_width = width;
            self.capture_height = height;
            self.display_x = desc.DesktopCoordinates.left;
            self.display_y = desc.DesktopCoordinates.top;

            debug!("Output duplication setup: {}x{}", width, height);
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    /// Capture cursor using Win32 API.
    fn capture_cursor_win32(&mut self) -> Result<Option<CapturedCursor>> {
        unsafe {
            // Get cursor info
            let mut cursor_info = CURSORINFO {
                cbSize: std::mem::size_of::<CURSORINFO>() as u32,
                ..Default::default()
            };

            if GetCursorInfo(&mut cursor_info).is_err() {
                return Ok(None);
            }

            let visible = cursor_info.flags == CURSOR_SHOWING;

            // Calculate position relative to captured display
            let x = cursor_info.ptScreenPos.x - self.display_x;
            let y = cursor_info.ptScreenPos.y - self.display_y;

            // Check if cursor is within the capture area
            let in_bounds = x >= 0
                && y >= 0
                && x < self.capture_width as i32
                && y < self.capture_height as i32;

            // Get cursor shape - check if cursor handle is valid
            let shape = if visible && !cursor_info.hCursor.is_invalid() {
                self.get_cursor_shape(cursor_info.hCursor)?
            } else {
                None
            };

            Ok(Some(CapturedCursor {
                x,
                y,
                visible: visible && in_bounds,
                shape,
            }))
        }
    }

    #[cfg(target_os = "windows")]
    /// Get cursor shape from HCURSOR.
    fn get_cursor_shape(
        &mut self,
        hcursor: windows::Win32::UI::WindowsAndMessaging::HCURSOR,
    ) -> Result<Option<CursorShape>> {
        unsafe {
            // Copy the cursor to get our own handle
            let hicon = CopyIcon(hcursor)
                .map_err(|e| Error::Screen(format!("Failed to copy cursor: {:?}", e)))?;

            let mut icon_info = ICONINFO::default();
            if GetIconInfo(hicon, &mut icon_info).is_err() {
                let _ = DestroyIcon(hicon);
                return Ok(None);
            }

            // Get bitmap info
            let hbm_color = icon_info.hbmColor;
            let hbm_mask = icon_info.hbmMask;

            let result = if !hbm_color.is_invalid() {
                // Color cursor
                self.extract_color_cursor(hbm_color, icon_info.xHotspot, icon_info.yHotspot)
            } else if !hbm_mask.is_invalid() {
                // Monochrome cursor (mask only)
                self.extract_mono_cursor(hbm_mask, icon_info.xHotspot, icon_info.yHotspot)
            } else {
                Ok(None)
            };

            // Cleanup
            if !hbm_color.is_invalid() {
                let _ = DeleteObject(hbm_color);
            }
            if !hbm_mask.is_invalid() {
                let _ = DeleteObject(hbm_mask);
            }
            let _ = DestroyIcon(hicon);

            result
        }
    }

    #[cfg(target_os = "windows")]
    /// Extract RGBA data from a color cursor bitmap.
    fn extract_color_cursor(
        &mut self,
        hbitmap: HBITMAP,
        hotspot_x: u32,
        hotspot_y: u32,
    ) -> Result<Option<CursorShape>> {
        unsafe {
            // Get bitmap dimensions
            let mut bm = BITMAP::default();
            if GetObjectW(
                hbitmap,
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bm as *mut _ as *mut _),
            ) == 0
            {
                return Ok(None);
            }

            let width = bm.bmWidth as u32;
            let height = bm.bmHeight as u32;

            if width == 0 || height == 0 || width > 256 || height > 256 {
                return Ok(None);
            }

            // Create compatible DC
            let hdc = CreateCompatibleDC(None);
            if hdc.is_invalid() {
                return Ok(None);
            }

            // Select bitmap into DC
            let old_bmp = SelectObject(hdc, hbitmap);

            // Setup BITMAPINFO for 32-bit BGRA
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32), // Negative for top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [Default::default()],
            };

            // Allocate buffer for bitmap bits
            let mut bits = vec![0u8; (width * height * 4) as usize];
            let result = GetDIBits(
                hdc,
                hbitmap,
                0,
                height,
                Some(bits.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            );

            // Cleanup
            SelectObject(hdc, old_bmp);
            let _ = DeleteDC(hdc);

            if result == 0 {
                return Ok(None);
            }

            // Convert BGRA to RGBA
            for chunk in bits.chunks_exact_mut(4) {
                chunk.swap(0, 2); // Swap B and R
            }

            // Check if shape changed
            let shape_id = CursorShape::calculate_id(&bits);
            if shape_id == self.last_cursor_shape_id {
                return Ok(None);
            }
            self.last_cursor_shape_id = shape_id;

            Ok(Some(CursorShape::new(width, height, hotspot_x, hotspot_y, bits)))
        }
    }

    #[cfg(target_os = "windows")]
    /// Extract RGBA data from a monochrome cursor bitmap.
    fn extract_mono_cursor(
        &mut self,
        hbitmap: HBITMAP,
        hotspot_x: u32,
        hotspot_y: u32,
    ) -> Result<Option<CursorShape>> {
        unsafe {
            // Get bitmap dimensions
            let mut bm = BITMAP::default();
            if GetObjectW(
                hbitmap,
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bm as *mut _ as *mut _),
            ) == 0
            {
                return Ok(None);
            }

            let width = bm.bmWidth as u32;
            // Monochrome cursors have AND mask + XOR mask stacked vertically
            let height = (bm.bmHeight / 2) as u32;

            if width == 0 || height == 0 || width > 256 || height > 256 {
                return Ok(None);
            }

            // Get the raw bits
            let row_bytes = ((width + 31) / 32) * 4; // 1-bit per pixel, DWORD aligned
            let mask_size = (row_bytes * height) as usize;
            let mut mask_bits = vec![0u8; mask_size * 2]; // AND + XOR masks

            let bytes_read = GetBitmapBits(hbitmap, mask_bits.len() as i32, mask_bits.as_mut_ptr() as *mut _);
            if bytes_read == 0 {
                return Ok(None);
            }

            // Convert monochrome to RGBA
            let mut rgba = vec![0u8; (width * height * 4) as usize];
            let and_mask = &mask_bits[..mask_size];
            let xor_mask = &mask_bits[mask_size..];

            for y in 0..height {
                for x in 0..width {
                    let byte_idx = (y * row_bytes + x / 8) as usize;
                    let bit_idx = 7 - (x % 8);
                    let and_bit = (and_mask.get(byte_idx).copied().unwrap_or(0) >> bit_idx) & 1;
                    let xor_bit = (xor_mask.get(byte_idx).copied().unwrap_or(0) >> bit_idx) & 1;

                    let pixel_idx = ((y * width + x) * 4) as usize;
                    // AND=0, XOR=0 -> Black
                    // AND=0, XOR=1 -> White
                    // AND=1, XOR=0 -> Transparent
                    // AND=1, XOR=1 -> Inverted (render as semi-transparent)
                    let (r, g, b, a) = match (and_bit, xor_bit) {
                        (0, 0) => (0, 0, 0, 255),       // Black
                        (0, 1) => (255, 255, 255, 255), // White
                        (1, 0) => (0, 0, 0, 0),         // Transparent
                        (1, 1) => (128, 128, 128, 128), // Inverted (show as gray)
                        _ => (0, 0, 0, 0),
                    };
                    rgba[pixel_idx] = r;
                    rgba[pixel_idx + 1] = g;
                    rgba[pixel_idx + 2] = b;
                    rgba[pixel_idx + 3] = a;
                }
            }

            // Check if shape changed
            let shape_id = CursorShape::calculate_id(&rgba);
            if shape_id == self.last_cursor_shape_id {
                return Ok(None);
            }
            self.last_cursor_shape_id = shape_id;

            Ok(Some(CursorShape::new(width, height, hotspot_x, hotspot_y, rgba)))
        }
    }

    #[cfg(target_os = "windows")]
    /// Capture a frame using DXGI Desktop Duplication.
    fn capture_dxgi(&mut self) -> Result<CapturedFrame> {
        unsafe {
            let duplication = self
                .duplication
                .as_ref()
                .ok_or_else(|| Error::Screen("Output duplication not initialized".into()))?;
            let context = self
                .context
                .as_ref()
                .ok_or_else(|| Error::Screen("D3D11 context not initialized".into()))?;
            let staging = self
                .staging_texture
                .as_ref()
                .ok_or_else(|| Error::Screen("Staging texture not initialized".into()))?;

            // Acquire next frame (100ms timeout)
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut desktop_resource = None;

            let result = duplication.AcquireNextFrame(100, &mut frame_info, &mut desktop_resource);
            if let Err(e) = result {
                // Timeout or other error - might need to recreate duplication
                return Err(Error::Screen(format!("Failed to acquire frame: {:?}", e)));
            }

            let desktop_resource = desktop_resource
                .ok_or_else(|| Error::Screen("No desktop resource acquired".into()))?;

            // Get the texture from the resource
            let desktop_texture: ID3D11Texture2D = desktop_resource
                .cast()
                .map_err(|e| Error::Screen(format!("Failed to get desktop texture: {:?}", e)))?;

            // Copy to staging texture
            context.CopyResource(staging, &desktop_texture);

            // Map the staging texture for CPU read
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| Error::Screen(format!("Failed to map staging texture: {:?}", e)))?;

            // Copy the data
            let bytes_per_pixel = 4usize;
            let row_bytes = self.capture_width as usize * bytes_per_pixel;
            let total_bytes = row_bytes * self.capture_height as usize;
            let mut data = Vec::with_capacity(total_bytes);

            // Handle row pitch (stride might be larger than width * bpp)
            for y in 0..self.capture_height as usize {
                let src = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize);
                let row = std::slice::from_raw_parts(src, row_bytes);
                data.extend_from_slice(row);
            }

            // Unmap and release frame
            context.Unmap(staging, 0);
            let _ = duplication.ReleaseFrame();

            Ok(CapturedFrame::new(
                self.capture_width,
                self.capture_height,
                PixelFormat::Bgra8,
                data,
                row_bytes as u32,
            ))
        }
    }

    #[cfg(target_os = "windows")]
    /// Release DXGI resources.
    fn release_resources(&mut self) {
        self.duplication = None;
        self.staging_texture = None;
    }
}

#[async_trait::async_trait]
impl ScreenCapture for DxgiCapture {
    fn name(&self) -> &'static str {
        "DXGI"
    }

    async fn list_displays(&self) -> Result<Vec<Display>> {
        #[cfg(target_os = "windows")]
        {
            if self.displays.is_empty() {
                let mut temp = Self::new().await?;
                temp.init_d3d11()?;
                temp.enumerate_outputs()?;
                return Ok(std::mem::take(&mut temp.displays));
            }
            Ok(self.displays.clone())
        }
        #[cfg(not(target_os = "windows"))]
        {
            warn!("DXGI is only available on Windows");
            Ok(vec![])
        }
    }

    async fn start(&mut self, display_id: &str) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            info!("Starting DXGI capture for display {}", display_id);

            // Parse display ID (format: "adapter:output")
            let parts: Vec<&str> = display_id.split(':').collect();
            if parts.len() != 2 {
                return Err(Error::Screen(format!(
                    "Invalid display ID format: {}",
                    display_id
                )));
            }

            let adapter_idx: u32 = parts[0]
                .parse()
                .map_err(|_| Error::Screen("Invalid adapter index".into()))?;
            let output_idx: u32 = parts[1]
                .parse()
                .map_err(|_| Error::Screen("Invalid output index".into()))?;

            // Initialize D3D11 if not already done
            if self.device.is_none() {
                self.init_d3d11()?;
            }

            // Enumerate outputs if not already done
            if self.displays.is_empty() {
                self.enumerate_outputs()?;
            }

            // Setup output duplication
            self.setup_duplication(adapter_idx, output_idx)?;

            self.current_display = Some(display_id.to_string());
            self.active = true;

            info!(
                "DXGI capture started for {} ({}x{})",
                display_id, self.capture_width, self.capture_height
            );

            Ok(())
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = display_id;
            Err(Error::Screen("DXGI is only available on Windows".into()))
        }
    }

    async fn capture_frame(&mut self) -> Result<Option<CapturedFrame>> {
        if !self.active {
            return Err(Error::Screen("Capture not started".into()));
        }

        #[cfg(target_os = "windows")]
        {
            let frame = self.capture_dxgi()?;
            Ok(Some(frame))
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(Error::Screen("DXGI is only available on Windows".into()))
        }
    }

    async fn capture_cursor(&mut self) -> Result<Option<CapturedCursor>> {
        if !self.active {
            return Err(Error::Screen("Capture not started".into()));
        }

        #[cfg(target_os = "windows")]
        {
            self.capture_cursor_win32()
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(None)
        }
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping DXGI capture");

        #[cfg(target_os = "windows")]
        {
            self.release_resources();
            self.last_cursor_shape_id = 0;
        }

        self.active = false;
        self.current_display = None;
        Ok(())
    }

    fn requires_privileges(&self) -> bool {
        false // DXGI doesn't need admin privileges
    }

    fn info(&self) -> super::BackendInfo {
        super::BackendInfo {
            name: self.name(),
            requires_privileges: self.requires_privileges(),
            supports_cursor: true, // Windows cursor capture always available
            supports_region: false,
        }
    }
}
