//! Screen stream manager - orchestrates capture sessions.
//!
//! The manager handles:
//! - Session lifecycle (start, pause, resume, stop)
//! - Capture backend management
//! - Frame dispatch and flow control
//! - Statistics aggregation

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, warn};

use super::encoder::create_encoder;
use super::events::{
    FrameDropReason, RemoteInputEvent, SessionEndReason, StreamCommand, StreamCommandReceiver,
    StreamCommandSender, StreamEvent, StreamEventSender, stream_command_channel,
};
use super::input::{create_input_injector, InputSecuritySettings, SecureInputHandler};
use super::session::{SessionId, StreamSession, StreamState, StreamStats};
use super::types::Display;
use super::{create_capture_backend, is_capture_available};
use crate::config::ScreenStreamSettings;
use crate::error::{Error, Result};
use crate::iroh::protocol::{ScreenCompression, ScreenQuality};

/// Frame chunk size for transmission (64KB).
pub const FRAME_CHUNK_SIZE: usize = 64 * 1024;

/// Maximum frames to buffer before applying backpressure.
pub const DEFAULT_MAX_BUFFER_FRAMES: u32 = 10;

/// Default ACK timeout before considering connection stale.
pub const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// Handle to a running stream session.
#[derive(Debug)]
pub struct StreamHandle {
    /// Session ID
    pub session_id: SessionId,
    /// Command sender to control the session
    command_tx: StreamCommandSender,
    /// Task join handle
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl StreamHandle {
    /// Send a command to the streaming task.
    pub async fn send_command(&self, command: StreamCommand) -> Result<()> {
        self.command_tx
            .send(command)
            .await
            .map_err(|_| Error::Screen("Session task not running".into()))
    }

    /// Pause the stream.
    pub async fn pause(&self) -> Result<()> {
        self.send_command(StreamCommand::Pause).await
    }

    /// Resume the stream.
    pub async fn resume(&self) -> Result<()> {
        self.send_command(StreamCommand::Resume).await
    }

    /// Stop the stream.
    pub async fn stop(&self) -> Result<()> {
        self.send_command(StreamCommand::Stop).await
    }

    /// Request a keyframe.
    pub async fn force_keyframe(&self) -> Result<()> {
        self.send_command(StreamCommand::ForceKeyframe).await
    }

    /// Adjust quality settings.
    pub async fn adjust_quality(
        &self,
        fps: Option<u32>,
        quality: Option<ScreenQuality>,
        bitrate_kbps: Option<u32>,
    ) -> Result<()> {
        self.send_command(StreamCommand::AdjustQuality {
            fps,
            quality,
            bitrate_kbps,
        })
        .await
    }

    /// Inject an input event.
    pub async fn inject_input(&self, event: RemoteInputEvent) -> Result<()> {
        self.send_command(StreamCommand::InjectInput(event)).await
    }
}

/// Callback for sending encoded frames to the network.
pub type FrameSender = Arc<dyn Fn(SessionId, u64, Vec<u8>) -> Result<()> + Send + Sync>;

/// Manages screen streaming sessions.
pub struct ScreenStreamManager {
    /// Active sessions (session_id -> session state)
    sessions: RwLock<HashMap<SessionId, Arc<Mutex<StreamSession>>>>,
    /// Stream handles for controlling sessions
    handles: RwLock<HashMap<SessionId, StreamHandle>>,
    /// Event sender for broadcasting events
    event_tx: StreamEventSender,
    /// Global settings
    settings: RwLock<ScreenStreamSettings>,
    /// Cached display list
    displays: RwLock<Vec<Display>>,
    /// Frame sender callback (set by integrator)
    frame_sender: RwLock<Option<FrameSender>>,
}

impl ScreenStreamManager {
    /// Create a new stream manager.
    pub fn new(event_tx: StreamEventSender, settings: ScreenStreamSettings) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            handles: RwLock::new(HashMap::new()),
            event_tx,
            settings: RwLock::new(settings),
            displays: RwLock::new(Vec::new()),
            frame_sender: RwLock::new(None),
        }
    }

    /// Set the frame sender callback.
    pub async fn set_frame_sender(&self, sender: FrameSender) {
        let mut fs = self.frame_sender.write().await;
        *fs = Some(sender);
    }

    /// Check if screen capture is available on this system.
    pub async fn is_available(&self) -> bool {
        is_capture_available().await
    }

    /// List available displays.
    pub async fn list_displays(&self) -> Result<Vec<Display>> {
        let settings = self.settings.read().await;
        let capture = create_capture_backend(&settings).await?;
        let displays = capture.list_displays().await?;

        // Cache the display list
        let mut cached = self.displays.write().await;
        *cached = displays.clone();

        Ok(displays)
    }

    /// Get cached display list.
    pub async fn get_cached_displays(&self) -> Vec<Display> {
        self.displays.read().await.clone()
    }

    /// Start a new streaming session.
    pub async fn start_stream(
        &self,
        session_id: SessionId,
        display_id: String,
        peer_id: String,
        allow_input: bool,
    ) -> Result<()> {
        // Check if session already exists
        {
            let sessions = self.sessions.read().await;
            if sessions.contains_key(&session_id) {
                return Err(Error::Screen(format!(
                    "Session {} already exists",
                    session_id
                )));
            }
        }

        let settings = self.settings.read().await.clone();

        // Create session
        let session = StreamSession::new(
            session_id.clone(),
            display_id.clone(),
            peer_id.clone(),
            settings.clone(),
            allow_input,
        );

        let session = Arc::new(Mutex::new(session));

        // Store session
        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(session_id.clone(), session.clone());
        }

        // Create command channel
        let (command_tx, command_rx) = stream_command_channel(32);

        // Clone what we need for the task
        let event_tx = self.event_tx.clone();
        let frame_sender = self.frame_sender.read().await.clone();
        let task_session_id = session_id.clone();
        let task_display_id = display_id.clone();

        // Spawn capture task
        let task_handle = tokio::spawn(async move {
            if let Err(e) = run_capture_loop(
                task_session_id.clone(),
                task_display_id,
                session,
                command_rx,
                event_tx.clone(),
                frame_sender,
                settings,
            )
            .await
            {
                error!("Capture loop error for session {}: {}", task_session_id, e);
                let _ = event_tx
                    .send(StreamEvent::Error {
                        session_id: task_session_id.clone(),
                        error: e.to_string(),
                    })
                    .await;
                let _ = event_tx
                    .send(StreamEvent::SessionEnded {
                        session_id: task_session_id,
                        reason: SessionEndReason::Error(e.to_string()),
                    })
                    .await;
            }
        });

        // Store handle
        let handle = StreamHandle {
            session_id: session_id.clone(),
            command_tx,
            task_handle: Some(task_handle),
        };

        {
            let mut handles = self.handles.write().await;
            handles.insert(session_id.clone(), handle);
        }

        info!("Started streaming session {} for display {}", session_id, display_id);

        Ok(())
    }

    /// Stop a streaming session.
    pub async fn stop_stream(&self, session_id: &str) -> Result<()> {
        let handle = {
            let handles = self.handles.read().await;
            handles.get(session_id).map(|h| h.command_tx.clone())
        };

        if let Some(tx) = handle {
            tx.send(StreamCommand::Stop)
                .await
                .map_err(|_| Error::Screen("Failed to send stop command".into()))?;

            // Give the task time to clean up
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Remove from maps
            {
                let mut sessions = self.sessions.write().await;
                sessions.remove(session_id);
            }
            {
                let mut handles = self.handles.write().await;
                handles.remove(session_id);
            }

            info!("Stopped streaming session {}", session_id);
            Ok(())
        } else {
            Err(Error::Screen(format!("Session {} not found", session_id)))
        }
    }

    /// Get session statistics.
    pub async fn get_stats(&self, session_id: &str) -> Result<StreamStats> {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_id) {
            let session = session.lock().await;
            Ok(session.stats.clone())
        } else {
            Err(Error::Screen(format!("Session {} not found", session_id)))
        }
    }

    /// Get session state.
    pub async fn get_state(&self, session_id: &str) -> Result<StreamState> {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_id) {
            let session = session.lock().await;
            Ok(session.state)
        } else {
            Err(Error::Screen(format!("Session {} not found", session_id)))
        }
    }

    /// List all active session IDs.
    pub async fn list_sessions(&self) -> Vec<SessionId> {
        let sessions = self.sessions.read().await;
        sessions.keys().cloned().collect()
    }

    /// Process an ACK from the remote peer.
    pub async fn process_ack(&self, session_id: &str, sequence: u64) -> Result<()> {
        let sessions = self.sessions.read().await;
        if let Some(session) = sessions.get(session_id) {
            let mut session = session.lock().await;

            // Calculate RTT if we have timing info
            let rtt_ms = session.stats.last_capture_time.map(|t| t.elapsed().as_millis() as u32);

            session.stats.record_ack(sequence, 0);
            if let Some(rtt) = rtt_ms {
                session.stats.update_rtt(rtt);
            }

            // Emit ACK event
            let _ = self
                .event_tx
                .send(StreamEvent::AckReceived {
                    session_id: session_id.to_string(),
                    sequence,
                    rtt_ms,
                })
                .await;

            Ok(())
        } else {
            Err(Error::Screen(format!("Session {} not found", session_id)))
        }
    }

    /// Update global settings.
    pub async fn update_settings(&self, settings: ScreenStreamSettings) {
        let mut s = self.settings.write().await;
        *s = settings;
    }

    /// Get a handle to control a session.
    pub async fn get_handle(&self, session_id: &str) -> Option<StreamCommandSender> {
        let handles = self.handles.read().await;
        handles.get(session_id).map(|h| h.command_tx.clone())
    }
}

/// Run the capture loop for a session.
async fn run_capture_loop(
    session_id: SessionId,
    display_id: String,
    session: Arc<Mutex<StreamSession>>,
    mut command_rx: StreamCommandReceiver,
    event_tx: StreamEventSender,
    frame_sender: Option<FrameSender>,
    settings: ScreenStreamSettings,
) -> Result<()> {
    info!("Starting capture loop for session {}", session_id);

    // Create capture backend
    let mut capture = create_capture_backend(&settings).await?;

    // Start capturing the display
    capture.start(&display_id).await?;

    // Create encoder (use Zstd by default for good compression/speed balance)
    // TODO: Make compression configurable via settings or stream request
    let mut encoder = create_encoder(ScreenCompression::Raw, ScreenQuality::Balanced);
    info!("Using {} encoder for session {}", encoder.name(), session_id);

    // Initialize input injector if input is allowed
    let allow_input = {
        let s = session.lock().await;
        s.allow_input
    };

    let mut input_handler: Option<SecureInputHandler> = if allow_input {
        match create_input_injector() {
            Ok(injector) => {
                let security_settings = InputSecuritySettings::default();
                let mut handler = SecureInputHandler::new(injector, security_settings);
                match handler.init() {
                    Ok(()) => {
                        info!("Input injection enabled for session {} using {}", session_id, handler.name());
                        Some(handler)
                    }
                    Err(e) => {
                        warn!("Failed to initialize input injector for session {}: {}", session_id, e);
                        None
                    }
                }
            }
            Err(e) => {
                warn!("Input injection not available for session {}: {}", session_id, e);
                None
            }
        }
    } else {
        debug!("Input injection disabled for session {}", session_id);
        None
    };

    // Activate session
    {
        let mut s = session.lock().await;
        let old_state = s.state;
        s.activate();
        let _ = event_tx
            .send(StreamEvent::StateChanged {
                session_id: session_id.clone(),
                old_state,
                new_state: s.state,
            })
            .await;
    }

    let mut sequence: u64 = 0;
    let mut last_stats_update = Instant::now();
    let stats_interval = Duration::from_secs(1);
    let mut _force_keyframe = false; // Used with video codecs

    loop {
        // Check for commands (non-blocking)
        match command_rx.try_recv() {
            Ok(command) => {
                match command {
                    StreamCommand::Stop => {
                        info!("Received stop command for session {}", session_id);
                        break;
                    }
                    StreamCommand::Pause => {
                        let mut s = session.lock().await;
                        let old_state = s.state;
                        if s.pause() {
                            let _ = event_tx
                                .send(StreamEvent::StateChanged {
                                    session_id: session_id.clone(),
                                    old_state,
                                    new_state: s.state,
                                })
                                .await;
                        }
                    }
                    StreamCommand::Resume | StreamCommand::Start => {
                        let mut s = session.lock().await;
                        let old_state = s.state;
                        if s.activate() {
                            let _ = event_tx
                                .send(StreamEvent::StateChanged {
                                    session_id: session_id.clone(),
                                    old_state,
                                    new_state: s.state,
                                })
                                .await;
                        }
                    }
                    StreamCommand::ForceKeyframe => {
                        debug!("Force keyframe requested for session {}", session_id);
                        _force_keyframe = true;
                        encoder.force_keyframe();
                    }
                    StreamCommand::AdjustQuality { fps, quality, bitrate_kbps } => {
                        let mut s = session.lock().await;
                        if let Some(new_fps) = fps {
                            s.set_target_fps(new_fps);
                        }
                        // Apply quality and bitrate to encoder
                        if let Some(q) = quality {
                            encoder.set_quality(q);
                        }
                        if let Some(br) = bitrate_kbps {
                            encoder.set_bitrate(br);
                        }
                        let _ = event_tx
                            .send(StreamEvent::QualityAdjustment {
                                session_id: session_id.clone(),
                                suggested_fps: fps,
                                suggested_quality: quality,
                                suggested_bitrate_kbps: bitrate_kbps,
                            })
                            .await;
                    }
                    StreamCommand::InjectInput(event) => {
                        // Handle input injection
                        if let Some(ref mut handler) = input_handler {
                            match handler.handle_input(&event) {
                                Ok(()) => {
                                    debug!("Injected input event for session {}: {:?}", session_id, event);
                                    let _ = event_tx
                                        .send(StreamEvent::InputReceived {
                                            session_id: session_id.clone(),
                                            event,
                                        })
                                        .await;
                                }
                                Err(e) => {
                                    warn!("Failed to inject input for session {}: {}", session_id, e);
                                    let _ = event_tx
                                        .send(StreamEvent::Error {
                                            session_id: session_id.clone(),
                                            error: format!("Input injection failed: {}", e),
                                        })
                                        .await;
                                }
                            }
                        } else {
                            warn!("Input injection not available for session {}", session_id);
                        }
                    }
                }
            }
            Err(mpsc::error::TryRecvError::Empty) => {
                // No commands, continue
            }
            Err(mpsc::error::TryRecvError::Disconnected) => {
                warn!("Command channel disconnected for session {}", session_id);
                break;
            }
        }

        // Check session state
        let (should_capture, has_backpressure, frame_interval) = {
            let mut s = session.lock().await;
            s.update_fps_calc();
            (
                s.state.is_active() && s.should_capture(),
                s.has_backpressure(),
                s.frame_interval(),
            )
        };

        if !should_capture {
            // Sleep a bit and check again
            tokio::time::sleep(Duration::from_millis(1)).await;
            continue;
        }

        // Check for backpressure
        if has_backpressure {
            let mut s = session.lock().await;
            s.stats.record_drop();
            let _ = event_tx
                .send(StreamEvent::FrameDropped {
                    session_id: session_id.clone(),
                    sequence,
                    reason: FrameDropReason::Backpressure,
                })
                .await;
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        }

        // Capture frame
        let capture_start = Instant::now();
        match capture.capture_frame().await {
            Ok(Some(frame)) => {
                let capture_time_us = capture_start.elapsed().as_micros() as u64;

                // Update stats
                {
                    let mut s = session.lock().await;
                    s.stats.record_capture(capture_time_us);
                }

                let _ = event_tx
                    .send(StreamEvent::FrameCaptured {
                        session_id: session_id.clone(),
                        sequence,
                        width: frame.width,
                        height: frame.height,
                        capture_time_us,
                    })
                    .await;

                // Encode the frame
                let encode_result = encoder.encode(&frame);

                match encode_result {
                    Ok(encoded) => {
                        let encode_time_us = encoded.encode_time_us;

                        // Emit encoded event
                        let _ = event_tx
                            .send(StreamEvent::FrameEncoded {
                                session_id: session_id.clone(),
                                sequence,
                                encoded_size: encoded.data.len(),
                                encode_time_us,
                            })
                            .await;

                        // Send frame
                        if let Some(ref sender) = frame_sender {
                            let bytes_sent = encoded.data.len() as u64;

                            match sender(session_id.clone(), sequence, encoded.data) {
                                Ok(()) => {
                                    let mut s = session.lock().await;
                                    s.stats.record_send(bytes_sent, encode_time_us);

                                    let _ = event_tx
                                        .send(StreamEvent::FrameSent {
                                            session_id: session_id.clone(),
                                            sequence,
                                            bytes_sent,
                                        })
                                        .await;
                                }
                                Err(e) => {
                                    warn!("Failed to send frame {}: {}", sequence, e);
                                    let mut s = session.lock().await;
                                    s.stats.record_drop();

                                    let _ = event_tx
                                        .send(StreamEvent::FrameDropped {
                                            session_id: session_id.clone(),
                                            sequence,
                                            reason: FrameDropReason::NetworkError,
                                        })
                                        .await;
                                }
                            }
                        } else {
                            // No sender configured, just track stats
                            let mut s = session.lock().await;
                            s.stats.record_send(encoded.data.len() as u64, encode_time_us);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to encode frame {}: {}", sequence, e);
                        let mut s = session.lock().await;
                        s.stats.record_drop();

                        let _ = event_tx
                            .send(StreamEvent::FrameDropped {
                                session_id: session_id.clone(),
                                sequence,
                                reason: FrameDropReason::EncodingError,
                            })
                            .await;
                    }
                }

                // Reset force_keyframe flag after use
                _force_keyframe = false;
                sequence += 1;
            }
            Ok(None) => {
                // No new frame available (screen unchanged)
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Err(e) => {
                error!("Capture error for session {}: {}", session_id, e);
                // Don't break immediately, try to recover
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        // Emit periodic stats
        if last_stats_update.elapsed() >= stats_interval {
            let stats = {
                let s = session.lock().await;
                s.stats.clone()
            };
            let _ = event_tx
                .send(StreamEvent::StatsUpdated {
                    session_id: session_id.clone(),
                    stats,
                })
                .await;
            last_stats_update = Instant::now();
        }

        // Frame rate limiting - sleep until next frame is due
        let sleep_duration = frame_interval.saturating_sub(capture_start.elapsed());
        if !sleep_duration.is_zero() {
            tokio::time::sleep(sleep_duration).await;
        }
    }

    // Clean up
    capture.stop().await?;

    // Shutdown input handler
    if let Some(ref mut handler) = input_handler {
        if let Err(e) = handler.shutdown() {
            warn!("Error shutting down input handler for session {}: {}", session_id, e);
        }
    }

    // Mark session as stopped
    {
        let mut s = session.lock().await;
        let old_state = s.state;
        s.complete_stop();
        let _ = event_tx
            .send(StreamEvent::StateChanged {
                session_id: session_id.clone(),
                old_state,
                new_state: s.state,
            })
            .await;
    }

    let _ = event_tx
        .send(StreamEvent::SessionEnded {
            session_id,
            reason: SessionEndReason::Requested,
        })
        .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScreenStreamSettings;

    #[tokio::test]
    async fn test_manager_creation() {
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(32);
        let settings = ScreenStreamSettings::default();
        let manager = ScreenStreamManager::new(event_tx, settings);

        let sessions = manager.list_sessions().await;
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn test_display_caching() {
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(32);
        let settings = ScreenStreamSettings::default();
        let manager = ScreenStreamManager::new(event_tx, settings);

        let cached = manager.get_cached_displays().await;
        assert!(cached.is_empty());
    }
}
