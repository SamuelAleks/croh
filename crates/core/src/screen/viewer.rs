//! Screen viewer (receiver) functionality.
//!
//! This module provides the client-side viewer for screen streaming,
//! handling frame reception, decoding, and input forwarding.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      ScreenViewer                            │
//! │  - Connection management                                     │
//! │  - Frame buffering                                          │
//! │  - Input forwarding                                          │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                    ┌─────────┴─────────┐
//!                    ▼                   ▼
//!           ┌──────────────┐    ┌──────────────┐
//!           │ FrameBuffer  │    │ InputQueue   │
//!           │ (decode+buf) │    │ (batch send) │
//!           └──────────────┘    └──────────────┘
//! ```

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::error::Result;
use crate::iroh::protocol::{DisplayInfo, ScreenCompression, ScreenQuality};

use super::adaptive::{AdaptiveBitrate, FrameAction, FrameSyncState, PacketLossTracker};
use super::decoder::AutoDecoder;
use super::events::RemoteInputEvent;
use super::time_sync::ClockSync;

/// Viewer connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewerState {
    /// Not connected to any stream.
    #[default]
    Disconnected,
    /// Connecting to the remote peer.
    Connecting,
    /// Connected, waiting for first frame.
    WaitingForFrame,
    /// Actively streaming.
    Streaming,
    /// Stream paused (local or remote).
    Paused,
    /// Disconnecting gracefully.
    Disconnecting,
    /// Error state.
    Error,
}

impl std::fmt::Display for ViewerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Note: lowercase strings match Slint UI expectations
        match self {
            Self::Disconnected => write!(f, "disconnected"),
            Self::Connecting => write!(f, "connecting"),
            Self::WaitingForFrame => write!(f, "waiting"),
            Self::Streaming => write!(f, "streaming"),
            Self::Paused => write!(f, "paused"),
            Self::Disconnecting => write!(f, "disconnecting"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// Viewer statistics.
#[derive(Debug, Clone, Default)]
pub struct ViewerStats {
    /// Frames received.
    pub frames_received: u64,
    /// Frames decoded.
    pub frames_decoded: u64,
    /// Frames dropped (decode error or buffer full).
    pub frames_dropped: u64,
    /// Current FPS (frames per second).
    pub current_fps: f32,
    /// Average decode time in microseconds.
    pub avg_decode_time_us: u64,
    /// Total bytes received.
    pub bytes_received: u64,
    /// Average bitrate in kbps.
    pub avg_bitrate_kbps: u64,
    /// End-to-end latency (capture to display) in ms.
    pub latency_ms: u32,
    /// Network RTT in ms (from clock sync).
    pub network_rtt_ms: u32,
    /// Packet loss rate (0.0-1.0).
    pub packet_loss: f32,
    /// Input events sent.
    pub input_events_sent: u64,
    /// Time connected.
    pub connected_duration: Duration,
    /// Whether clock is synchronized with host.
    pub clock_synced: bool,
    /// Clock offset from host in ms.
    pub clock_offset_ms: i64,
}

impl ViewerStats {
    /// Calculate FPS from frame timestamps.
    pub fn calculate_fps(&mut self, frame_times: &VecDeque<Instant>) {
        if frame_times.len() < 2 {
            self.current_fps = 0.0;
            return;
        }

        let oldest = frame_times.front().unwrap();
        let newest = frame_times.back().unwrap();
        let duration = newest.duration_since(*oldest);

        if duration.as_secs_f32() > 0.0 {
            self.current_fps = (frame_times.len() - 1) as f32 / duration.as_secs_f32();
        }
    }
}

/// Configuration for the viewer.
#[derive(Debug, Clone)]
pub struct ViewerConfig {
    /// Maximum frames to buffer.
    pub max_buffer_frames: usize,
    /// Input batch interval in milliseconds.
    pub input_batch_ms: u32,
    /// Maximum input events per batch.
    pub max_input_batch_size: usize,
    /// Enable input forwarding.
    pub enable_input: bool,
    /// Target quality (hint to server).
    pub preferred_quality: ScreenQuality,
    /// Target FPS (hint to server).
    pub preferred_fps: u32,
    /// Target end-to-end latency in ms.
    pub target_latency_ms: u32,
    /// Maximum latency before dropping frames in ms.
    pub max_latency_ms: u32,
    /// Enable adaptive bitrate.
    pub enable_adaptive_bitrate: bool,
    /// Minimum bitrate in kbps (for adaptive).
    pub min_bitrate_kbps: u32,
    /// Maximum bitrate in kbps (for adaptive).
    pub max_bitrate_kbps: u32,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            max_buffer_frames: 3,
            input_batch_ms: 16, // ~60Hz
            max_input_batch_size: 32,
            enable_input: true,
            preferred_quality: ScreenQuality::Balanced,
            preferred_fps: 30,
            target_latency_ms: 100,
            max_latency_ms: 500,
            enable_adaptive_bitrate: true,
            min_bitrate_kbps: 500,
            max_bitrate_kbps: 20_000,
        }
    }
}

/// A buffered frame ready for display.
#[derive(Debug, Clone)]
pub struct BufferedFrame {
    /// Decoded RGBA data.
    pub data: Vec<u8>,
    /// Frame width.
    pub width: u32,
    /// Frame height.
    pub height: u32,
    /// Frame sequence number.
    pub sequence: u64,
    /// Time frame was received.
    pub received_at: Instant,
    /// Decode time in microseconds.
    pub decode_time_us: u64,
}

/// Events emitted by the viewer.
#[derive(Debug, Clone)]
pub enum ViewerEvent {
    /// State changed.
    StateChanged {
        old_state: ViewerState,
        new_state: ViewerState,
    },
    /// New frame available for display.
    FrameReady {
        sequence: u64,
        width: u32,
        height: u32,
    },
    /// Frame was dropped (too old or decode error).
    FrameDropped {
        sequence: u64,
        reason: String,
    },
    /// Statistics updated.
    StatsUpdated(ViewerStats),
    /// Available displays received.
    DisplaysReceived(Vec<DisplayInfo>),
    /// Stream accepted by remote.
    StreamAccepted {
        stream_id: String,
        compression: ScreenCompression,
    },
    /// Stream rejected by remote.
    StreamRejected { stream_id: String, reason: String },
    /// Stream ended.
    StreamEnded { reason: String },
    /// Clock synchronized with host.
    ClockSynced {
        offset_ms: i64,
        rtt_ms: u32,
    },
    /// Quality adjustment recommended (send to host).
    QualityAdjustmentNeeded {
        suggested_quality: ScreenQuality,
        reason: String,
    },
    /// Keyframe needed (send to host).
    KeyframeNeeded {
        reason: String,
    },
    /// Bitrate adjustment recommended (send to host).
    BitrateAdjustmentNeeded {
        suggested_kbps: u32,
    },
    /// Error occurred.
    Error(String),
}

/// Sender for viewer events.
pub type ViewerEventSender = mpsc::Sender<ViewerEvent>;
/// Receiver for viewer events.
pub type ViewerEventReceiver = mpsc::Receiver<ViewerEvent>;

/// Create a viewer event channel.
pub fn viewer_event_channel(capacity: usize) -> (ViewerEventSender, ViewerEventReceiver) {
    mpsc::channel(capacity)
}

/// Commands that can be sent to the viewer.
#[derive(Debug, Clone)]
pub enum ViewerCommand {
    /// Connect to a peer's screen.
    Connect {
        peer_id: String,
        display_id: Option<String>,
    },
    /// Disconnect from the current stream.
    Disconnect,
    /// Request display list from peer.
    RequestDisplayList { peer_id: String },
    /// Switch to a different display.
    SwitchDisplay { display_id: String },
    /// Pause the stream.
    Pause,
    /// Resume the stream.
    Resume,
    /// Request quality adjustment.
    AdjustQuality {
        quality: ScreenQuality,
        fps: Option<u32>,
    },
    /// Request keyframe.
    RequestKeyframe,
    /// Forward input event.
    SendInput(RemoteInputEvent),
    /// Forward batch of input events.
    SendInputBatch(Vec<RemoteInputEvent>),
}

/// Sender for viewer commands.
pub type ViewerCommandSender = mpsc::Sender<ViewerCommand>;
/// Receiver for viewer commands.
pub type ViewerCommandReceiver = mpsc::Receiver<ViewerCommand>;

/// Create a viewer command channel.
pub fn viewer_command_channel(capacity: usize) -> (ViewerCommandSender, ViewerCommandReceiver) {
    mpsc::channel(capacity)
}

/// Frame buffer for the viewer.
pub struct FrameBuffer {
    /// Buffered frames.
    frames: VecDeque<BufferedFrame>,
    /// Maximum buffer size.
    max_size: usize,
    /// Decoder.
    decoder: AutoDecoder,
    /// Frame reception times for FPS calculation.
    frame_times: VecDeque<Instant>,
    /// Next expected sequence number.
    next_sequence: u64,
    /// Total decode time for averaging.
    total_decode_time_us: u64,
    /// Decode count for averaging.
    decode_count: u64,
}

impl FrameBuffer {
    /// Create a new frame buffer.
    pub fn new(max_size: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(max_size),
            max_size,
            decoder: AutoDecoder::new(),
            frame_times: VecDeque::with_capacity(60),
            next_sequence: 0,
            total_decode_time_us: 0,
            decode_count: 0,
        }
    }

    /// Push a raw frame into the buffer, decoding it.
    pub fn push_frame(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        sequence: u64,
    ) -> Result<()> {
        // Decode the frame
        let decoded = self.decoder.decode(data, width, height)?;

        // Track decode time
        self.total_decode_time_us += decoded.decode_time_us;
        self.decode_count += 1;

        // Track frame time for FPS
        let now = Instant::now();
        self.frame_times.push_back(now);
        if self.frame_times.len() > 60 {
            self.frame_times.pop_front();
        }

        // Create buffered frame
        let buffered = BufferedFrame {
            data: decoded.data,
            width: decoded.width,
            height: decoded.height,
            sequence,
            received_at: now,
            decode_time_us: decoded.decode_time_us,
        };

        // Remove old frames if buffer is full
        while self.frames.len() >= self.max_size {
            self.frames.pop_front();
        }

        self.frames.push_back(buffered);
        self.next_sequence = sequence + 1;

        Ok(())
    }

    /// Get the latest frame without removing it.
    pub fn latest_frame(&self) -> Option<&BufferedFrame> {
        self.frames.back()
    }

    /// Pop the oldest frame.
    pub fn pop_frame(&mut self) -> Option<BufferedFrame> {
        self.frames.pop_front()
    }

    /// Get the number of buffered frames.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        self.frames.clear();
        self.frame_times.clear();
    }

    /// Get average decode time in microseconds.
    pub fn avg_decode_time_us(&self) -> u64 {
        if self.decode_count > 0 {
            self.total_decode_time_us / self.decode_count
        } else {
            0
        }
    }

    /// Calculate current FPS.
    pub fn current_fps(&self) -> f32 {
        if self.frame_times.len() < 2 {
            return 0.0;
        }

        let oldest = self.frame_times.front().unwrap();
        let newest = self.frame_times.back().unwrap();
        let duration = newest.duration_since(*oldest);

        if duration.as_secs_f32() > 0.0 {
            (self.frame_times.len() - 1) as f32 / duration.as_secs_f32()
        } else {
            0.0
        }
    }
}

/// Input queue for batching input events.
pub struct InputQueue {
    /// Queued events.
    events: VecDeque<RemoteInputEvent>,
    /// Maximum batch size.
    max_batch_size: usize,
    /// Last batch sent time.
    last_batch_time: Instant,
    /// Batch interval.
    batch_interval: Duration,
    /// Total events sent.
    events_sent: u64,
}

impl InputQueue {
    /// Create a new input queue.
    pub fn new(max_batch_size: usize, batch_interval_ms: u32) -> Self {
        Self {
            events: VecDeque::with_capacity(max_batch_size * 2),
            max_batch_size,
            last_batch_time: Instant::now(),
            batch_interval: Duration::from_millis(batch_interval_ms as u64),
            events_sent: 0,
        }
    }

    /// Queue an input event.
    pub fn push(&mut self, event: RemoteInputEvent) {
        self.events.push_back(event);
    }

    /// Check if a batch should be sent.
    pub fn should_send(&self) -> bool {
        !self.events.is_empty()
            && (self.events.len() >= self.max_batch_size
                || self.last_batch_time.elapsed() >= self.batch_interval)
    }

    /// Get the next batch to send.
    pub fn take_batch(&mut self) -> Vec<RemoteInputEvent> {
        let batch_size = self.events.len().min(self.max_batch_size);
        let batch: Vec<_> = self.events.drain(..batch_size).collect();
        self.events_sent += batch.len() as u64;
        self.last_batch_time = Instant::now();
        batch
    }

    /// Get total events sent.
    pub fn events_sent(&self) -> u64 {
        self.events_sent
    }

    /// Clear the queue.
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

/// Screen viewer state.
pub struct ScreenViewer {
    /// Current state.
    state: ViewerState,
    /// Viewer configuration.
    config: ViewerConfig,
    /// Frame buffer.
    frame_buffer: FrameBuffer,
    /// Input queue.
    input_queue: InputQueue,
    /// Event sender.
    event_tx: ViewerEventSender,
    /// Current stream ID.
    stream_id: Option<String>,
    /// Current peer ID.
    peer_id: Option<String>,
    /// Available displays from peer.
    displays: Vec<DisplayInfo>,
    /// Current display ID.
    display_id: Option<String>,
    /// Connection start time.
    connected_at: Option<Instant>,
    /// Total bytes received.
    bytes_received: u64,
    /// Last error message.
    last_error: Option<String>,
    /// Recent latency samples for averaging (in ms).
    latency_samples: Vec<u32>,
    /// Clock synchronization state.
    clock_sync: ClockSync,
    /// Frame sync state (latency tracking and decisions).
    frame_sync: FrameSyncState,
    /// Packet loss tracker.
    loss_tracker: PacketLossTracker,
    /// Adaptive bitrate controller.
    adaptive_bitrate: AdaptiveBitrate,
    /// Current quality setting.
    current_quality: ScreenQuality,
    /// Frames dropped due to latency.
    frames_dropped_latency: u64,
}

impl ScreenViewer {
    /// Create a new screen viewer.
    pub fn new(config: ViewerConfig, event_tx: ViewerEventSender) -> Self {
        let frame_buffer = FrameBuffer::new(config.max_buffer_frames);
        let input_queue = InputQueue::new(config.max_input_batch_size, config.input_batch_ms);
        let frame_sync = FrameSyncState::new(config.target_latency_ms, config.max_latency_ms);
        let loss_tracker = PacketLossTracker::new(60); // 1 second at 60fps
        let adaptive_bitrate = AdaptiveBitrate::new(
            config.min_bitrate_kbps,
            config.max_bitrate_kbps,
            (config.min_bitrate_kbps + config.max_bitrate_kbps) / 2, // Start at midpoint
        );

        Self {
            state: ViewerState::Disconnected,
            config: config.clone(),
            frame_buffer,
            input_queue,
            event_tx,
            stream_id: None,
            peer_id: None,
            displays: Vec::new(),
            display_id: None,
            connected_at: None,
            bytes_received: 0,
            last_error: None,
            latency_samples: Vec::with_capacity(30),
            clock_sync: ClockSync::new(),
            frame_sync,
            loss_tracker,
            adaptive_bitrate,
            current_quality: config.preferred_quality,
            frames_dropped_latency: 0,
        }
    }

    /// Get current state.
    pub fn state(&self) -> ViewerState {
        self.state
    }

    /// Get current peer ID.
    pub fn peer_id(&self) -> Option<&str> {
        self.peer_id.as_deref()
    }

    /// Get current stream ID.
    pub fn stream_id(&self) -> Option<&str> {
        self.stream_id.as_deref()
    }

    /// Get available displays.
    pub fn displays(&self) -> &[DisplayInfo] {
        &self.displays
    }

    /// Get current display ID.
    pub fn display_id(&self) -> Option<&str> {
        self.display_id.as_deref()
    }

    /// Get the latest frame.
    pub fn latest_frame(&self) -> Option<&BufferedFrame> {
        self.frame_buffer.latest_frame()
    }

    /// Get last error.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Check if input is enabled.
    pub fn input_enabled(&self) -> bool {
        self.config.enable_input
    }

    /// Set state and emit event.
    fn set_state(&mut self, new_state: ViewerState) {
        let old_state = self.state;
        if old_state != new_state {
            self.state = new_state;
            let _ = self.event_tx.try_send(ViewerEvent::StateChanged {
                old_state,
                new_state,
            });
        }
    }

    /// Start connecting to a peer.
    pub fn start_connect(&mut self, peer_id: String, display_id: Option<String>) {
        info!("Starting connection to peer {}", peer_id);
        self.peer_id = Some(peer_id);
        self.display_id = display_id;
        self.stream_id = Some(uuid::Uuid::new_v4().to_string());
        self.bytes_received = 0;
        self.last_error = None;
        self.frame_buffer.clear();
        self.input_queue.clear();
        self.set_state(ViewerState::Connecting);
    }

    /// Handle connection established.
    pub fn on_connected(&mut self, displays: Vec<DisplayInfo>) {
        info!("Connected to peer, {} displays available", displays.len());
        self.displays = displays.clone();
        self.connected_at = Some(Instant::now());
        self.set_state(ViewerState::WaitingForFrame);

        let _ = self
            .event_tx
            .try_send(ViewerEvent::DisplaysReceived(displays));
    }

    /// Handle stream accepted.
    pub fn on_stream_accepted(&mut self, compression: ScreenCompression) {
        if let Some(stream_id) = &self.stream_id {
            let _ = self.event_tx.try_send(ViewerEvent::StreamAccepted {
                stream_id: stream_id.clone(),
                compression,
            });
        }
    }

    /// Handle stream rejected.
    pub fn on_stream_rejected(&mut self, reason: String) {
        warn!("Stream rejected: {}", reason);
        self.last_error = Some(reason.clone());
        self.set_state(ViewerState::Error);

        if let Some(stream_id) = &self.stream_id {
            let _ = self.event_tx.try_send(ViewerEvent::StreamRejected {
                stream_id: stream_id.clone(),
                reason,
            });
        }
    }

    /// Handle incoming frame with full metadata.
    ///
    /// This is the main frame reception method that handles:
    /// - Clock-adjusted latency calculation
    /// - Frame sync decisions (display/drop/request adjustment)
    /// - Packet loss tracking
    /// - Adaptive bitrate updates
    ///
    /// # Arguments
    /// * `data` - Encoded frame data
    /// * `width` - Frame width
    /// * `height` - Frame height
    /// * `sequence` - Frame sequence number
    /// * `captured_at_ms` - Host's capture timestamp (Unix millis)
    ///
    /// # Returns
    /// The action taken for this frame
    pub fn on_frame_received_with_metadata(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        sequence: u64,
        captured_at_ms: i64,
    ) -> Result<FrameAction> {
        self.bytes_received += data.len() as u64;

        // Track packet loss
        let packet_loss = self.loss_tracker.record(sequence);

        // Calculate latency, adjusting for clock offset if synced
        let now_ms = chrono::Utc::now().timestamp_millis();
        let adjusted_captured_at = if self.clock_sync.is_synced() {
            self.clock_sync.to_local_time(captured_at_ms)
        } else {
            captured_at_ms
        };
        let latency_ms = (now_ms - adjusted_captured_at).max(0);

        // Record in legacy latency samples for backwards compatibility
        if self.latency_samples.len() >= 30 {
            self.latency_samples.remove(0);
        }
        self.latency_samples.push(latency_ms as u32);

        // Evaluate frame timing
        let action = self.frame_sync.evaluate_frame(adjusted_captured_at, now_ms);

        match action {
            FrameAction::Drop => {
                self.frames_dropped_latency += 1;
                let _ = self.event_tx.try_send(ViewerEvent::FrameDropped {
                    sequence,
                    reason: format!("Latency too high: {}ms", latency_ms),
                });
                return Ok(FrameAction::Drop);
            }
            FrameAction::RequestQualityReduction => {
                let suggested = self.frame_sync.suggest_quality(self.current_quality);
                let _ = self.event_tx.try_send(ViewerEvent::QualityAdjustmentNeeded {
                    suggested_quality: suggested,
                    reason: format!("Falling behind: {}ms latency", latency_ms),
                });
            }
            FrameAction::RequestKeyframeFlush => {
                self.frame_buffer.clear();
                self.frame_sync.reset_after_flush();
                let _ = self.event_tx.try_send(ViewerEvent::KeyframeNeeded {
                    reason: format!("Severely behind: {}ms latency, flushing", latency_ms),
                });
                return Ok(FrameAction::RequestKeyframeFlush);
            }
            FrameAction::Display => {}
        }

        // Update adaptive bitrate if enabled
        if self.config.enable_adaptive_bitrate {
            let rtt = self.clock_sync.avg_rtt_ms();
            if let Some(new_bitrate) = self.adaptive_bitrate.update(rtt, packet_loss) {
                let _ = self.event_tx.try_send(ViewerEvent::BitrateAdjustmentNeeded {
                    suggested_kbps: new_bitrate,
                });
            }
        }

        // Decode and buffer the frame
        match self.frame_buffer.push_frame(data, width, height, sequence) {
            Ok(()) => {
                // Transition to streaming on first frame
                if self.state == ViewerState::WaitingForFrame {
                    self.set_state(ViewerState::Streaming);
                }

                let _ = self.event_tx.try_send(ViewerEvent::FrameReady {
                    sequence,
                    width,
                    height,
                });

                Ok(action)
            }
            Err(e) => {
                debug!("Frame decode error: {}", e);
                let _ = self.event_tx.try_send(ViewerEvent::FrameDropped {
                    sequence,
                    reason: format!("Decode error: {}", e),
                });
                Err(e)
            }
        }
    }

    /// Handle incoming frame (legacy method without timestamp).
    ///
    /// Use `on_frame_received_with_metadata` when capture timestamp is available.
    pub fn on_frame_received(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        sequence: u64,
    ) -> Result<()> {
        // Use current time as captured_at (no latency calculation possible)
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.on_frame_received_with_metadata(data, width, height, sequence, now_ms)?;
        Ok(())
    }

    /// Process a time sync response from the host.
    ///
    /// # Arguments
    /// * `client_time` - Original client timestamp from request
    /// * `server_receive_time` - Host's time when it received request
    /// * `server_send_time` - Host's time when it sent response
    ///
    /// # Returns
    /// `true` if clock sync is now complete
    pub fn on_time_sync_response(
        &mut self,
        client_time: i64,
        server_receive_time: i64,
        server_send_time: i64,
    ) -> bool {
        let response_received_time = chrono::Utc::now().timestamp_millis();

        let synced = self.clock_sync.process_response(
            client_time,
            server_receive_time,
            server_send_time,
            response_received_time,
        );

        if synced {
            let _ = self.event_tx.try_send(ViewerEvent::ClockSynced {
                offset_ms: self.clock_sync.offset_ms(),
                rtt_ms: self.clock_sync.avg_rtt_ms(),
            });
        }

        synced
    }

    /// Check if clock sync is complete.
    pub fn is_clock_synced(&self) -> bool {
        self.clock_sync.is_synced()
    }

    /// Apply externally computed clock sync values.
    ///
    /// This is used when clock sync is performed externally (e.g., by the transfer layer)
    /// and the results need to be passed to the viewer for accurate latency calculation.
    ///
    /// # Arguments
    /// * `offset_ms` - Clock offset in milliseconds (host_time = local_time + offset)
    /// * `rtt_ms` - Round-trip time in milliseconds
    pub fn apply_clock_sync(&mut self, offset_ms: i64, rtt_ms: u32) {
        self.clock_sync.apply_sync(offset_ms, rtt_ms);
        info!(
            "External clock sync applied: offset={}ms, rtt={}ms",
            offset_ms, rtt_ms
        );
    }

    /// Check if more time sync exchanges are needed.
    pub fn needs_time_sync(&self) -> bool {
        self.clock_sync.needs_more_samples()
    }

    /// Get the next time sync request sequence.
    pub fn next_time_sync_sequence(&mut self) -> u8 {
        self.clock_sync.next_request_sequence()
    }

    /// Get current packet loss rate.
    pub fn packet_loss_rate(&self) -> f32 {
        self.loss_tracker.current_rate()
    }

    /// Get current adaptive bitrate.
    pub fn current_bitrate(&self) -> u32 {
        self.adaptive_bitrate.current_bitrate()
    }

    /// Record latency sample from frame capture timestamp (legacy).
    pub fn record_latency(&mut self, captured_at_ms: i64) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let adjusted = if self.clock_sync.is_synced() {
            self.clock_sync.to_local_time(captured_at_ms)
        } else {
            captured_at_ms
        };
        let latency = (now_ms - adjusted).max(0) as u32;

        if self.latency_samples.len() >= 30 {
            self.latency_samples.remove(0);
        }
        self.latency_samples.push(latency);
    }

    /// Get average latency in ms.
    pub fn avg_latency_ms(&self) -> u32 {
        if self.latency_samples.is_empty() {
            return 0;
        }
        let sum: u32 = self.latency_samples.iter().sum();
        sum / self.latency_samples.len() as u32
    }

    /// Get frame sync state for inspection.
    pub fn frame_sync(&self) -> &FrameSyncState {
        &self.frame_sync
    }

    /// Handle disconnect request.
    pub fn disconnect(&mut self, reason: String) {
        info!("Disconnecting: {}", reason);
        self.set_state(ViewerState::Disconnecting);

        let _ = self.event_tx.try_send(ViewerEvent::StreamEnded {
            reason: reason.clone(),
        });

        // Clean up
        self.stream_id = None;
        self.peer_id = None;
        self.display_id = None;
        self.displays.clear();
        self.frame_buffer.clear();
        self.input_queue.clear();
        self.connected_at = None;

        // Reset sync state
        self.clock_sync.reset();
        self.loss_tracker.reset();
        self.adaptive_bitrate.reset(
            (self.config.min_bitrate_kbps + self.config.max_bitrate_kbps) / 2,
        );
        self.frame_sync = FrameSyncState::new(
            self.config.target_latency_ms,
            self.config.max_latency_ms,
        );
        self.frames_dropped_latency = 0;
        self.latency_samples.clear();

        self.set_state(ViewerState::Disconnected);
    }

    /// Handle error.
    pub fn on_error(&mut self, error: String) {
        warn!("Viewer error: {}", error);
        self.last_error = Some(error.clone());
        self.set_state(ViewerState::Error);
        let _ = self.event_tx.try_send(ViewerEvent::Error(error));
    }

    /// Queue an input event.
    pub fn queue_input(&mut self, event: RemoteInputEvent) {
        if self.config.enable_input && self.state == ViewerState::Streaming {
            self.input_queue.push(event);
        }
    }

    /// Check if input batch is ready to send.
    pub fn input_batch_ready(&self) -> bool {
        self.input_queue.should_send()
    }

    /// Get input batch to send.
    pub fn take_input_batch(&mut self) -> Vec<RemoteInputEvent> {
        self.input_queue.take_batch()
    }

    /// Get current statistics.
    pub fn stats(&self) -> ViewerStats {
        let connected_duration = self.connected_at.map(|t| t.elapsed()).unwrap_or_default();

        let avg_bitrate_kbps = if connected_duration.as_secs() > 0 {
            (self.bytes_received * 8 / 1000) / connected_duration.as_secs()
        } else {
            0
        };

        ViewerStats {
            frames_received: self.frame_buffer.decode_count,
            frames_decoded: self.frame_buffer.decode_count,
            frames_dropped: self.frames_dropped_latency,
            current_fps: self.frame_buffer.current_fps(),
            avg_decode_time_us: self.frame_buffer.avg_decode_time_us(),
            bytes_received: self.bytes_received,
            avg_bitrate_kbps,
            latency_ms: self.frame_sync.avg_latency_ms(),
            network_rtt_ms: self.clock_sync.avg_rtt_ms(),
            packet_loss: self.loss_tracker.current_rate(),
            input_events_sent: self.input_queue.events_sent(),
            connected_duration,
            clock_synced: self.clock_sync.is_synced(),
            clock_offset_ms: self.clock_sync.offset_ms(),
        }
    }

    /// Emit stats update event.
    pub fn emit_stats(&self) {
        let _ = self
            .event_tx
            .try_send(ViewerEvent::StatsUpdated(self.stats()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_viewer_state_transitions() {
        let (tx, _rx) = viewer_event_channel(32);
        let mut viewer = ScreenViewer::new(ViewerConfig::default(), tx);

        assert_eq!(viewer.state(), ViewerState::Disconnected);

        viewer.start_connect("peer-1".into(), None);
        assert_eq!(viewer.state(), ViewerState::Connecting);
        assert_eq!(viewer.peer_id(), Some("peer-1"));

        viewer.on_connected(vec![]);
        assert_eq!(viewer.state(), ViewerState::WaitingForFrame);

        // Simulate a simple raw frame
        let width = 10u32;
        let height = 10u32;
        let frame_data = vec![0u8; (width * height * 4) as usize];
        viewer
            .on_frame_received(&frame_data, width, height, 0)
            .unwrap();
        assert_eq!(viewer.state(), ViewerState::Streaming);

        viewer.disconnect("user request".into());
        assert_eq!(viewer.state(), ViewerState::Disconnected);
    }

    #[test]
    fn test_frame_buffer() {
        let mut buffer = FrameBuffer::new(3);

        // Push some frames
        for i in 0..5 {
            let data = vec![0u8; 40]; // 10x1 RGBA
            buffer.push_frame(&data, 10, 1, i).unwrap();
        }

        // Should only have 3 frames (buffer size)
        assert_eq!(buffer.len(), 3);

        // Latest should be sequence 4
        assert_eq!(buffer.latest_frame().unwrap().sequence, 4);
    }

    #[test]
    fn test_input_queue() {
        let mut queue = InputQueue::new(5, 100);

        // Push some events
        for _ in 0..3 {
            queue.push(RemoteInputEvent::MouseMove {
                x: 100,
                y: 100,
                absolute: true,
            });
        }

        // Not ready yet (below batch size)
        assert!(!queue.should_send());

        // Push more to hit batch size
        for _ in 0..2 {
            queue.push(RemoteInputEvent::MouseMove {
                x: 100,
                y: 100,
                absolute: true,
            });
        }

        // Now ready
        assert!(queue.should_send());

        let batch = queue.take_batch();
        assert_eq!(batch.len(), 5);
        assert_eq!(queue.events_sent(), 5);
    }

    #[test]
    fn test_viewer_stats() {
        let (tx, _rx) = viewer_event_channel(32);
        let viewer = ScreenViewer::new(ViewerConfig::default(), tx);

        let stats = viewer.stats();
        assert_eq!(stats.frames_received, 0);
        assert_eq!(stats.current_fps, 0.0);
    }
}
