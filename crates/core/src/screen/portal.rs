//! XDG Desktop Portal screen capture backend for Wayland.
//!
//! This module implements screen capture via the XDG Desktop Portal ScreenCast
//! interface, which works on Wayland compositors like KDE Plasma and GNOME.
//!
//! ## Features
//!
//! - Works on Wayland without elevated privileges
//! - Supports restore tokens for persistent unattended access after initial approval
//! - Uses PipeWire for efficient frame streaming
//!
//! ## Restore Token Flow
//!
//! 1. First use: No token → User dialog shown → Token received → Saved to config
//! 2. Subsequent use: Token loaded → Session restored silently → New token saved
//! 3. Token invalid: Load fails → User dialog shown → New token saved
//!
//! ## Requirements
//!
//! - `xdg-desktop-portal` service running (1.14+ for restore tokens)
//! - `xdg-desktop-portal-kde` or `xdg-desktop-portal-gnome` backend
//! - PipeWire running
//!
//! ## Architecture
//!
//! The Portal capture uses a multi-threaded architecture:
//! - Portal session is created and started in the async context
//! - PipeWire mainloop runs on a dedicated thread
//! - Frames are sent from PipeWire thread to async context via channel
//! - The portal session must stay alive to keep the PipeWire stream active

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use ashpd::enumflags2::BitFlags;

use crate::error::{Error, Result};
use crate::screen::token_manager::TokenManager;
use crate::screen::types::{BackendInfo, CapturedFrame, Display, PixelFormat, ScreenCapture};
use crate::screen::wayland::{CompositorCapabilities, WaylandCompositor};

/// Portal session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionState {
    /// Initial state, no session created
    Created,
    /// Session started, PipeWire stream active
    Streaming,
    /// Session stopped
    Stopped,
}

/// Commands sent to the PipeWire thread
enum PipeWireCommand {
    /// Stop the PipeWire mainloop and exit the thread
    Stop,
}

/// A captured video frame from PipeWire
struct PipeWireFrame {
    /// Frame width
    width: u32,
    /// Frame height
    height: u32,
    /// Pixel format
    format: PixelFormat,
    /// Raw pixel data
    data: Vec<u8>,
    /// Stride (bytes per row)
    stride: u32,
    /// Capture timestamp
    timestamp: Instant,
}

/// Action to take after session error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Retry with existing token
    Retry,
    /// Need user to grant permission again
    ReauthorizeRequired,
    /// User explicitly cancelled
    UserCancelled,
    /// Unrecoverable error
    Fatal,
}

/// Status of unattended access capability.
#[derive(Debug, Clone)]
pub struct UnattendedStatus {
    /// Whether unattended is currently possible
    pub available: bool,
    /// Human-readable explanation
    pub message: String,
    /// Action user can take to enable (if any)
    pub action_hint: Option<String>,
    /// Whether a dialog will be shown on next capture
    pub dialog_expected: bool,
}

/// XDG Desktop Portal screen capture backend.
///
/// Uses the ScreenCast portal interface for Wayland screen capture with
/// optional restore token support for unattended access.
pub struct PortalCapture {
    /// Current session state
    state: SessionState,
    /// Restore token for persistent access (loaded from/saved to config)
    restore_token: Option<String>,
    /// Currently selected display ID (PipeWire node ID as string)
    current_display: Option<String>,
    /// Cached display list from last session
    displays: Vec<Display>,
    /// PipeWire node ID from portal
    pipewire_node_id: Option<u32>,
    /// Channel to receive frames from PipeWire thread (wrapped in Mutex for Sync)
    frame_receiver: Option<Arc<Mutex<Receiver<PipeWireFrame>>>>,
    /// Channel to send commands to PipeWire thread
    command_sender: Option<Sender<PipeWireCommand>>,
    /// PipeWire thread handle
    pipewire_thread: Option<JoinHandle<()>>,
    /// Last captured frame dimensions
    frame_width: u32,
    frame_height: u32,
    /// Token manager for persistent restore tokens (optional)
    token_manager: Option<Arc<TokenManager>>,
    /// Detected compositor
    compositor: WaylandCompositor,
    /// Whether last session used restore (no dialog shown)
    last_session_was_silent: bool,
    /// Number of consecutive restore failures
    restore_failures: u32,
}

impl PortalCapture {
    /// Create a new Portal capture backend.
    ///
    /// # Arguments
    /// * `restore_token` - Optional restore token from previous session
    pub async fn new(restore_token: Option<String>) -> Result<Self> {
        // Verify Portal is available before creating
        if !Self::is_available().await {
            return Err(Error::Screen(
                "XDG Desktop Portal ScreenCast not available".into(),
            ));
        }

        let compositor = WaylandCompositor::detect();

        Ok(Self {
            state: SessionState::Created,
            restore_token,
            current_display: None,
            displays: Vec::new(),
            pipewire_node_id: None,
            frame_receiver: None,
            command_sender: None,
            pipewire_thread: None,
            frame_width: 0,
            frame_height: 0,
            token_manager: None,
            compositor,
            last_session_was_silent: false,
            restore_failures: 0,
        })
    }

    /// Create with token manager for persistent tokens.
    ///
    /// This constructor uses a TokenManager to automatically persist restore tokens
    /// to the config file after each session.
    pub async fn new_with_token_manager(token_manager: Arc<TokenManager>) -> Result<Self> {
        if !Self::is_available().await {
            return Err(Error::Screen(
                "XDG Desktop Portal ScreenCast not available".into(),
            ));
        }

        let compositor = WaylandCompositor::detect();
        let caps = compositor.capabilities();

        tracing::info!(
            "Portal capture initializing on {} (restore_tokens: {}, unattended: {})",
            compositor.name(),
            caps.supports_restore_tokens,
            caps.supports_unattended
        );

        Ok(Self {
            state: SessionState::Created,
            restore_token: token_manager.get_token(),
            current_display: None,
            displays: Vec::new(),
            pipewire_node_id: None,
            frame_receiver: None,
            command_sender: None,
            pipewire_thread: None,
            frame_width: 0,
            frame_height: 0,
            token_manager: Some(token_manager),
            compositor,
            last_session_was_silent: false,
            restore_failures: 0,
        })
    }

    /// Check if XDG Desktop Portal ScreenCast is available on this system.
    pub async fn is_available() -> bool {
        // Check if we're on Wayland first
        if let Ok(session_type) = std::env::var("XDG_SESSION_TYPE") {
            if session_type != "wayland" {
                return false;
            }
        } else {
            return false;
        }

        // Try to create a Screencast proxy to verify D-Bus availability
        match Screencast::new().await {
            Ok(_) => {
                tracing::debug!("XDG Desktop Portal ScreenCast is available");
                true
            }
            Err(e) => {
                tracing::debug!("XDG Desktop Portal ScreenCast not available: {}", e);
                false
            }
        }
    }

    /// Get the restore token to save to config.
    #[allow(dead_code)]
    pub fn get_restore_token(&self) -> Option<&str> {
        self.restore_token.as_deref()
    }

    /// Create a portal session, start it, and get PipeWire stream info.
    ///
    /// This method implements smart restore handling:
    /// - Attempts to use restore token if available
    /// - Retries without token on failure (up to 2 failures)
    /// - Saves new token via token manager if available
    async fn create_and_start_portal_session(&mut self) -> Result<(u32, OwnedFd)> {
        // Get token from manager if available, otherwise use stored token
        let restore_token = if let Some(ref tm) = self.token_manager {
            tm.get_token()
        } else {
            self.restore_token.clone()
        };
        let attempting_restore = restore_token.is_some();

        tracing::info!(
            "Creating Portal session (attempting_restore: {}, failures: {})",
            attempting_restore,
            self.restore_failures
        );

        // If we've failed to restore multiple times, clear token and force dialog
        let use_token = if self.restore_failures >= 2 {
            tracing::warn!("Multiple restore failures, clearing token and forcing dialog");
            if let Some(ref tm) = self.token_manager {
                tm.invalidate_token();
            }
            self.restore_token = None;
            None
        } else {
            restore_token
        };

        let screencast = Screencast::new()
            .await
            .map_err(|e| Error::Screen(format!("Failed to create Screencast proxy: {}", e)))?;

        let session = screencast
            .create_session()
            .await
            .map_err(|e| Error::Screen(format!("Failed to create Portal session: {}", e)))?;

        let restore_token_ref = use_token.as_deref();
        tracing::debug!(
            "Selecting sources with restore_token: {}",
            restore_token_ref.is_some()
        );

        let source_types: BitFlags<SourceType> = SourceType::Monitor.into();

        // Handle select_sources failure (token may be invalid)
        let select_result = screencast
            .select_sources(
                &session,
                CursorMode::Embedded,
                source_types,
                false,
                restore_token_ref,
                PersistMode::ExplicitlyRevoked,
            )
            .await;

        if let Err(e) = select_result {
            if attempting_restore && self.restore_failures < 2 {
                tracing::warn!("Restore failed ({}), will retry without token", e);
                self.restore_failures += 1;
                if let Some(ref tm) = self.token_manager {
                    tm.invalidate_token();
                }
                self.restore_token = None;

                // Retry without token
                screencast
                    .select_sources(
                        &session,
                        CursorMode::Embedded,
                        source_types,
                        false,
                        None,
                        PersistMode::ExplicitlyRevoked,
                    )
                    .await
                    .map_err(|e| Error::Screen(format!("Failed to select sources: {}", e)))?;
            } else {
                return Err(Error::Screen(format!("Failed to select sources: {}", e)));
            }
        }

        tracing::info!("Starting Portal session...");

        let response = screencast
            .start(&session, None)
            .await
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("cancelled") || msg.contains("denied") {
                    Error::Screen("User cancelled screen capture permission".into())
                } else {
                    Error::Screen(format!("Failed to start Portal session: {}", e))
                }
            })?
            .response()
            .map_err(|e| Error::Screen(format!("Portal start response error: {}", e)))?;

        // Success! Save new token immediately
        if let Some(token) = response.restore_token() {
            tracing::info!("Received new restore token from Portal");
            let token_str = token.to_string();
            self.restore_token = Some(token_str.clone());

            // Persist via token manager if available
            if let Some(ref tm) = self.token_manager {
                if let Err(e) = tm.update_token(Some(token_str)) {
                    tracing::warn!("Failed to persist token: {}", e);
                }
            }

            // Reset failure count on success
            self.restore_failures = 0;

            // Track if this was a silent restore (no user interaction)
            self.last_session_was_silent = attempting_restore && self.restore_failures == 0;
        }

        // Get stream info
        let streams = response.streams();
        if streams.is_empty() {
            return Err(Error::Screen("No streams available from Portal".into()));
        }

        let stream = &streams[0];
        let node_id = stream.pipe_wire_node_id();
        let size = stream.size();
        let position = stream.position();

        tracing::info!(
            "Portal stream: node_id={}, size={:?}, position={:?}",
            node_id,
            size,
            position
        );

        // Populate display info
        self.displays.clear();
        let (width, height) = size.unwrap_or((1920, 1080));
        let (x, y) = position.unwrap_or((0, 0));

        self.displays.push(Display {
            id: node_id.to_string(),
            name: "Display 0 (Portal)".to_string(),
            width: width as u32,
            height: height as u32,
            refresh_rate: Some(60),
            is_primary: true,
            x,
            y,
        });

        self.frame_width = width as u32;
        self.frame_height = height as u32;

        // Get PipeWire fd
        let pipewire_fd = screencast
            .open_pipe_wire_remote(&session)
            .await
            .map_err(|e| Error::Screen(format!("Failed to open PipeWire remote: {}", e)))?;

        tracing::info!("Got PipeWire fd: {}", pipewire_fd.as_raw_fd());

        Ok((node_id, pipewire_fd))
    }

    /// Start the PipeWire capture thread
    fn start_pipewire_thread(&mut self, node_id: u32, pipewire_fd: OwnedFd) -> Result<()> {
        let (frame_tx, frame_rx) = mpsc::channel();
        let (cmd_tx, cmd_rx) = mpsc::channel();

        let thread_handle = thread::Builder::new()
            .name("portal-pipewire".into())
            .spawn(move || {
                if let Err(e) = run_pipewire_capture(node_id, pipewire_fd, frame_tx, cmd_rx) {
                    tracing::error!("PipeWire capture thread error: {}", e);
                }
            })
            .map_err(|e| Error::Screen(format!("Failed to spawn PipeWire thread: {}", e)))?;

        self.frame_receiver = Some(Arc::new(Mutex::new(frame_rx)));
        self.command_sender = Some(cmd_tx);
        self.pipewire_thread = Some(thread_handle);
        self.pipewire_node_id = Some(node_id);

        Ok(())
    }

    /// Stop the PipeWire thread
    fn stop_pipewire_thread(&mut self) {
        // Send stop command
        if let Some(cmd_tx) = self.command_sender.take() {
            let _ = cmd_tx.send(PipeWireCommand::Stop);
        }

        // Wait for thread to finish
        if let Some(handle) = self.pipewire_thread.take() {
            let _ = handle.join();
        }

        self.frame_receiver = None;
        self.pipewire_node_id = None;
    }

    /// Check if the last session started silently (no user dialog).
    pub fn was_silent_restore(&self) -> bool {
        self.last_session_was_silent
    }

    /// Get compositor capabilities for UI hints.
    pub fn compositor_capabilities(&self) -> CompositorCapabilities {
        self.compositor.capabilities()
    }

    /// Get the detected compositor.
    pub fn compositor(&self) -> WaylandCompositor {
        self.compositor
    }

    /// Attempt to recover a broken session.
    pub async fn recover_session(&mut self) -> Result<()> {
        tracing::info!("Attempting session recovery");

        // Stop existing PipeWire thread
        self.stop_pipewire_thread();

        // Clear session state
        self.state = SessionState::Stopped;

        // Try to restart - this will attempt restore first
        let display_id = self
            .current_display
            .clone()
            .unwrap_or_else(|| "portal-default".to_string());

        self.start(&display_id).await
    }

    /// Handle session errors with smart recovery.
    pub fn handle_session_error(&mut self, error: &str) -> RecoveryAction {
        tracing::warn!("Session error: {}", error);

        // Check if this is a recoverable error
        if error.contains("stream ended")
            || error.contains("pipewire")
            || error.contains("timeout")
        {
            // Transient error - try to recover
            self.restore_failures = 0; // Don't penalize restore attempts
            return RecoveryAction::Retry;
        }

        if error.contains("permission")
            || error.contains("denied")
            || error.contains("revoked")
        {
            // Permission revoked - need new authorization
            if let Some(ref tm) = self.token_manager {
                tm.invalidate_token();
            }
            self.restore_token = None;
            return RecoveryAction::ReauthorizeRequired;
        }

        if error.contains("cancelled") {
            // User cancelled - don't auto-retry
            return RecoveryAction::UserCancelled;
        }

        // Unknown error
        RecoveryAction::Fatal
    }

    /// Get status of unattended access capability.
    pub fn unattended_status(&self) -> UnattendedStatus {
        let caps = self.compositor.capabilities();
        let has_token = if let Some(ref tm) = self.token_manager {
            tm.has_token()
        } else {
            self.restore_token.is_some()
        };

        if !caps.supports_restore_tokens {
            return UnattendedStatus {
                available: false,
                message: format!(
                    "{} doesn't support session restore tokens",
                    caps.compositor.name()
                ),
                action_hint: Some("Consider using DRM backend with CAP_SYS_ADMIN".into()),
                dialog_expected: true,
            };
        }

        if !has_token {
            return UnattendedStatus {
                available: false,
                message: "No restore token saved. Permission dialog will be shown.".into(),
                action_hint: Some(
                    "Grant permission once - subsequent sessions will be automatic.".into(),
                ),
                dialog_expected: true,
            };
        }

        if self.restore_failures >= 2 {
            return UnattendedStatus {
                available: false,
                message: "Restore token appears invalid. Re-authorization required.".into(),
                action_hint: None,
                dialog_expected: true,
            };
        }

        UnattendedStatus {
            available: true,
            message: format!(
                "Restore token available for {}. Should connect without dialog.",
                caps.compositor.name()
            ),
            action_hint: None,
            dialog_expected: false,
        }
    }
}

#[async_trait::async_trait]
impl ScreenCapture for PortalCapture {
    fn name(&self) -> &'static str {
        "Portal"
    }

    async fn list_displays(&self) -> Result<Vec<Display>> {
        // Return cached displays if we have them from a previous session
        if !self.displays.is_empty() {
            return Ok(self.displays.clone());
        }

        // Return placeholder before session starts (Portal can't enumerate without dialog)
        Ok(vec![Display {
            id: "portal-default".to_string(),
            name: "Screen (Portal)".to_string(),
            width: 1920,
            height: 1080,
            refresh_rate: Some(60),
            is_primary: true,
            x: 0,
            y: 0,
        }])
    }

    async fn start(&mut self, display_id: &str) -> Result<()> {
        if self.state == SessionState::Streaming {
            return Err(Error::Screen("Already streaming".into()));
        }

        // If stopped, reset state to allow restart with restore token
        if self.state == SessionState::Stopped {
            tracing::info!("Restarting Portal capture from stopped state");
            self.state = SessionState::Created;
        }

        tracing::info!("Portal start requested for display: {}", display_id);

        // Create portal session and get PipeWire info
        let (node_id, pipewire_fd) = self.create_and_start_portal_session().await?;

        // Start PipeWire capture thread
        self.start_pipewire_thread(node_id, pipewire_fd)?;

        self.current_display = Some(display_id.to_string());
        self.state = SessionState::Streaming;

        tracing::info!("Portal capture started, PipeWire node: {}", node_id);
        Ok(())
    }

    async fn capture_frame(&mut self) -> Result<Option<CapturedFrame>> {
        if self.state != SessionState::Streaming {
            return Err(Error::Screen("Not streaming".into()));
        }

        let receiver = self
            .frame_receiver
            .as_ref()
            .ok_or_else(|| Error::Screen("No frame receiver available".into()))?;

        // Lock the receiver and try to get the latest frame (non-blocking)
        let rx = receiver
            .lock()
            .map_err(|e| Error::Screen(format!("Failed to lock frame receiver: {}", e)))?;

        match rx.try_recv() {
            Ok(frame) => {
                // Update dimensions if they changed
                self.frame_width = frame.width;
                self.frame_height = frame.height;

                Ok(Some(CapturedFrame {
                    width: frame.width,
                    height: frame.height,
                    format: frame.format,
                    data: frame.data,
                    stride: frame.stride,
                    timestamp: frame.timestamp,
                    dmabuf_fd: None,
                }))
            }
            Err(mpsc::TryRecvError::Empty) => {
                // No new frame available
                Ok(None)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                tracing::warn!("PipeWire frame channel disconnected");
                Err(Error::Screen("PipeWire stream disconnected".into()))
            }
        }
    }

    async fn stop(&mut self) -> Result<()> {
        if self.state == SessionState::Stopped {
            return Ok(());
        }

        tracing::info!("Stopping Portal session");

        self.stop_pipewire_thread();
        self.displays.clear();

        self.state = SessionState::Stopped;
        self.current_display = None;

        tracing::info!("Portal session stopped");
        Ok(())
    }

    fn requires_privileges(&self) -> bool {
        false
    }

    fn info(&self) -> BackendInfo {
        BackendInfo {
            name: self.name(),
            requires_privileges: false,
            supports_cursor: true,
            supports_region: false,
        }
    }
}

impl Drop for PortalCapture {
    fn drop(&mut self) {
        if self.state == SessionState::Streaming {
            tracing::debug!("PortalCapture dropped while streaming - cleaning up");
            self.stop_pipewire_thread();
        }
    }
}

/// Video format info captured from PipeWire
struct VideoFormatInfo {
    width: u32,
    height: u32,
    format: PixelFormat,
}

impl Default for VideoFormatInfo {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            format: PixelFormat::Bgra8,
        }
    }
}

/// Run the PipeWire capture loop on a dedicated thread
fn run_pipewire_capture(
    node_id: u32,
    pipewire_fd: OwnedFd,
    frame_tx: Sender<PipeWireFrame>,
    cmd_rx: Receiver<PipeWireCommand>,
) -> Result<()> {
    use pipewire as pw;
    use pw::spa;
    use pw::spa::param::format::{MediaSubtype, MediaType};
    use pw::spa::param::format_utils;
    use pw::spa::pod::Pod;

    // Initialize PipeWire
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| Error::Screen(format!("Failed to create PipeWire mainloop: {}", e)))?;

    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| Error::Screen(format!("Failed to create PipeWire context: {}", e)))?;

    // Connect using the portal's fd
    let core = context
        .connect_fd_rc(pipewire_fd, None)
        .map_err(|e| Error::Screen(format!("Failed to connect to PipeWire: {}", e)))?;

    // Create video stream with properties
    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Video",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Screen",
    };

    let stream = pw::stream::StreamBox::new(&core, "portal-video-capture", props)
        .map_err(|e| Error::Screen(format!("Failed to create PipeWire stream: {}", e)))?;

    // Store raw pointer for trigger_process() calls from timer
    // Safety: The stream lives for the duration of the mainloop, and we only
    // call trigger_process() while the stream is valid
    let stream_ptr = stream.as_raw_ptr();

    // Track video format - shared between callbacks via Arc
    let format_info = Arc::new(std::sync::Mutex::new(VideoFormatInfo::default()));
    let format_info_for_param = format_info.clone();
    let format_info_for_process = format_info.clone();
    let frame_tx_clone = frame_tx.clone();

    // Track stream error state
    let stream_error = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stream_error_for_state = stream_error.clone();

    // Set up stream listener (user data not needed since we use Arc for sharing)
    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_stream, _user_data, old, new| {
            tracing::info!("PipeWire stream state: {:?} -> {:?}", old, new);
            // Check for error state
            if let pw::stream::StreamState::Error(ref msg) = new {
                tracing::error!("PipeWire stream error: {}", msg);
                stream_error_for_state.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            // Log when we reach streaming state
            if matches!(new, pw::stream::StreamState::Streaming) {
                tracing::info!("PipeWire stream is now STREAMING - frames should start arriving");
            }
        })
        .param_changed(move |_stream, _user_data, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }

            let Some(param) = param else { return };

            let (media_type, media_subtype) = match format_utils::parse_format(param) {
                Ok(v) => v,
                Err(_) => return,
            };

            if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
                return;
            }

            // Parse the video format
            let mut video_info = spa::param::video::VideoInfoRaw::new();
            if video_info.parse(param).is_ok() {
                let size = video_info.size();
                let format = spa_format_to_pixel_format(video_info.format());

                let mut info = format_info_for_param.lock().unwrap();
                info.width = size.width;
                info.height = size.height;
                info.format = format;

                tracing::info!(
                    "PipeWire video format: {:?} {}x{} @ {}/{}fps",
                    format,
                    size.width,
                    size.height,
                    video_info.framerate().num,
                    video_info.framerate().denom
                );
            }
        })
        .process(move |stream, _user_data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };

            let datas = buffer.datas_mut();
            if datas.is_empty() {
                tracing::warn!("PipeWire: buffer has no data planes");
                return;
            }

            let data = &mut datas[0];
            let chunk = data.chunk();
            let chunk_size = chunk.size() as usize;
            let chunk_offset = chunk.offset() as usize;
            let chunk_stride = chunk.stride() as u32;

            let Some(slice) = data.data() else {
                tracing::warn!("PipeWire: data plane has no mapped memory");
                return;
            };

            let info = format_info_for_process.lock().unwrap();
            let width = info.width;
            let height = info.height;
            let format = info.format;
            drop(info);

            if width == 0 || height == 0 {
                return;
            }

            // Calculate stride (bytes per row) - prefer chunk stride if available
            let bpp = 4; // All our formats are 4 bytes per pixel
            let stride = if chunk_stride > 0 { chunk_stride } else { width * bpp };

            // Copy frame data - use chunk_size if available, otherwise calculate
            let frame_size = if chunk_size > 0 {
                chunk_size
            } else {
                (stride * height) as usize
            };

            // Apply chunk offset to get the actual data start
            let data_start = chunk_offset;
            let data_end = data_start + frame_size;

            if data_end <= slice.len() && frame_size > 0 {
                let frame = PipeWireFrame {
                    width,
                    height,
                    format,
                    data: slice[data_start..data_end].to_vec(),
                    stride,
                    timestamp: Instant::now(),
                };

                // Send frame (blocking send is fine since we're on dedicated thread)
                if frame_tx_clone.send(frame).is_err() {
                    tracing::warn!("PipeWire: frame channel closed");
                }
            } else {
                tracing::warn!(
                    "PipeWire: buffer too small: have {} bytes, need {} (offset {} + size {}) for {}x{}",
                    slice.len(), data_end, data_start, frame_size, width, height
                );
            }
        })
        .register()
        .map_err(|e| Error::Screen(format!("Failed to register stream listener: {}", e)))?;

    // Build format pod for negotiation - request common video formats
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::RGBx,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::RGBA,
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            spa::utils::Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 60, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction {
                num: 1000,
                denom: 1
            }
        ),
    );

    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| Error::Screen(format!("Failed to serialize format pod: {}", e)))?
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).unwrap()];

    // Connect to the portal's PipeWire node
    // We try without DRIVER first - let the portal source push frames when screen changes.
    // If that doesn't work, we may need to be the driver and call trigger_process().
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| Error::Screen(format!("Failed to connect PipeWire stream: {}", e)))?;

    tracing::info!("PipeWire stream connected to node {}", node_id);

    // Set up a timer to trigger frame processing and check for stop command
    let mainloop_weak = mainloop.downgrade();
    let stream_error_for_timer = stream_error.clone();
    let timer_count = std::sync::atomic::AtomicU64::new(0);
    let timer = mainloop.loop_().add_timer(move |_| {
        let count = timer_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if count.is_multiple_of(1800) {
            // Log every ~60 seconds at debug level (not spammy)
            tracing::debug!("PipeWire timer tick #{}", count + 1);
        }

        // Check for stream error
        if stream_error_for_timer.load(std::sync::atomic::Ordering::SeqCst) {
            tracing::warn!("Stream error detected, quitting PipeWire mainloop");
            if let Some(ml) = mainloop_weak.upgrade() {
                ml.quit();
            }
            return;
        }

        // Check for stop command
        match cmd_rx.try_recv() {
            Ok(PipeWireCommand::Stop) => {
                tracing::info!("Received stop command, quitting PipeWire mainloop");
                if let Some(ml) = mainloop_weak.upgrade() {
                    ml.quit();
                }
                return;
            }
            Err(mpsc::TryRecvError::Empty) => {
                // No command, continue
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                tracing::info!("Command channel disconnected, quitting PipeWire mainloop");
                if let Some(ml) = mainloop_weak.upgrade() {
                    ml.quit();
                }
                return;
            }
        }

        // Trigger frame processing - this is required when using DRIVER flag
        // Safety: stream_ptr is valid for the lifetime of the mainloop
        let result = unsafe { pw::sys::pw_stream_trigger_process(stream_ptr) };
        if result < 0 {
            tracing::warn!("trigger_process failed: {}", result);
        } else {
            tracing::trace!("trigger_process called successfully");
        }
    });

    // Update timer to fire at ~30fps (33ms) for responsive capture
    use std::time::Duration;
    timer
        .update_timer(
            Some(Duration::from_millis(33)),
            Some(Duration::from_millis(33)),
        )
        .into_result()
        .map_err(|e| Error::Screen(format!("Failed to set timer: {}", e)))?;

    // Run the mainloop
    mainloop.run();

    tracing::info!("PipeWire capture thread exiting");
    Ok(())
}

/// Convert SPA video format to our PixelFormat
fn spa_format_to_pixel_format(format: pipewire::spa::param::video::VideoFormat) -> PixelFormat {
    use pipewire::spa::param::video::VideoFormat;
    match format {
        VideoFormat::RGBA => PixelFormat::Rgba8,
        VideoFormat::BGRA => PixelFormat::Bgra8,
        VideoFormat::RGBx => PixelFormat::Rgbx8,
        VideoFormat::BGRx => PixelFormat::Bgrx8,
        _ => PixelFormat::Bgra8, // Default fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_portal_is_available_check() {
        let available = PortalCapture::is_available().await;
        println!("Portal available: {}", available);
    }

    #[tokio::test]
    async fn test_portal_capture_creation() {
        if std::env::var("XDG_SESSION_TYPE").unwrap_or_default() != "wayland" {
            println!("Skipping test - not on Wayland");
            return;
        }

        let result = PortalCapture::new(None).await;
        match result {
            Ok(capture) => {
                assert_eq!(capture.name(), "Portal");
                assert!(!capture.requires_privileges());
            }
            Err(e) => {
                println!("Portal not available: {}", e);
            }
        }
    }

    #[test]
    fn test_spa_format_to_pixel_format() {
        use pipewire::spa::param::video::VideoFormat;

        assert_eq!(
            spa_format_to_pixel_format(VideoFormat::RGBA),
            PixelFormat::Rgba8
        );
        assert_eq!(
            spa_format_to_pixel_format(VideoFormat::BGRA),
            PixelFormat::Bgra8
        );
        assert_eq!(
            spa_format_to_pixel_format(VideoFormat::RGBx),
            PixelFormat::Rgbx8
        );
        assert_eq!(
            spa_format_to_pixel_format(VideoFormat::BGRx),
            PixelFormat::Bgrx8
        );
        // Unknown formats should default to Bgra8
        assert_eq!(
            spa_format_to_pixel_format(VideoFormat::YUY2),
            PixelFormat::Bgra8
        );
    }

    #[test]
    fn test_session_state_transitions() {
        // Test state enum equality
        assert_eq!(SessionState::Created, SessionState::Created);
        assert_ne!(SessionState::Created, SessionState::Streaming);
        assert_ne!(SessionState::Streaming, SessionState::Stopped);
    }

    #[test]
    fn test_video_format_info_default() {
        let info = VideoFormatInfo::default();
        assert_eq!(info.width, 0);
        assert_eq!(info.height, 0);
        assert_eq!(info.format, PixelFormat::Bgra8);
    }

    #[test]
    fn test_backend_info() {
        // Can't create PortalCapture without async, but we can test BackendInfo
        let info = BackendInfo {
            name: "Portal",
            requires_privileges: false,
            supports_cursor: true,
            supports_region: false,
        };
        assert_eq!(info.name, "Portal");
        assert!(!info.requires_privileges);
        assert!(info.supports_cursor);
        assert!(!info.supports_region);
    }

    #[tokio::test]
    async fn test_list_displays_returns_placeholder() {
        // Skip if not on Wayland (can't create PortalCapture)
        if std::env::var("XDG_SESSION_TYPE").unwrap_or_default() != "wayland" {
            println!("Skipping test - not on Wayland");
            return;
        }

        let result = PortalCapture::new(None).await;
        if let Ok(capture) = result {
            let displays = capture.list_displays().await.unwrap();
            assert!(!displays.is_empty());
            // Before start(), should return placeholder
            assert_eq!(displays[0].id, "portal-default");
            assert_eq!(displays[0].name, "Screen (Portal)");
        }
    }

    #[tokio::test]
    async fn test_capture_frame_without_start_fails() {
        if std::env::var("XDG_SESSION_TYPE").unwrap_or_default() != "wayland" {
            println!("Skipping test - not on Wayland");
            return;
        }

        let result = PortalCapture::new(None).await;
        if let Ok(mut capture) = result {
            // Should fail because we haven't started
            let frame_result = capture.capture_frame().await;
            assert!(frame_result.is_err());
        }
    }
}
