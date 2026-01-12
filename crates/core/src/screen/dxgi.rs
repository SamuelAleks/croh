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
use super::{CapturedFrame, Display, PixelFormat, ScreenCapture};
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
};

#[cfg(not(target_os = "windows"))]
use super::{CapturedFrame, Display, ScreenCapture};
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

            debug!("Output duplication setup: {}x{}", width, height);
        }
        Ok(())
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

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping DXGI capture");

        #[cfg(target_os = "windows")]
        {
            self.release_resources();
        }

        self.active = false;
        self.current_display = None;
        Ok(())
    }

    fn requires_privileges(&self) -> bool {
        false // DXGI doesn't need admin privileges
    }
}
