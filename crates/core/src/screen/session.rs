//! Stream session management types.
//!
//! This module defines the core types for tracking screen streaming sessions,
//! including session state, statistics, and configuration.

use std::time::{Duration, Instant};

use crate::config::ScreenStreamSettings;

/// Unique identifier for a streaming session.
pub type SessionId = String;

/// State of a streaming session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    /// Session is initializing (setting up capture backend)
    Starting,
    /// Session is actively streaming frames
    Active,
    /// Session is paused (no frames being sent, but resources held)
    Paused,
    /// Session is shutting down gracefully
    Stopping,
    /// Session has stopped and resources are released
    Stopped,
    /// Session encountered an error
    Error,
}

impl StreamState {
    /// Check if the session is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, StreamState::Stopped | StreamState::Error)
    }

    /// Check if the session is actively streaming.
    pub fn is_active(&self) -> bool {
        matches!(self, StreamState::Active)
    }

    /// Check if the session can transition to active state.
    pub fn can_activate(&self) -> bool {
        matches!(self, StreamState::Starting | StreamState::Paused)
    }
}

impl std::fmt::Display for StreamState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamState::Starting => write!(f, "Starting"),
            StreamState::Active => write!(f, "Active"),
            StreamState::Paused => write!(f, "Paused"),
            StreamState::Stopping => write!(f, "Stopping"),
            StreamState::Stopped => write!(f, "Stopped"),
            StreamState::Error => write!(f, "Error"),
        }
    }
}

/// Statistics for a streaming session.
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    /// Total frames captured
    pub frames_captured: u64,
    /// Total frames sent to peer
    pub frames_sent: u64,
    /// Total frames dropped (due to backpressure, encoding errors, etc.)
    pub frames_dropped: u64,
    /// Total bytes sent (encoded frame data)
    pub bytes_sent: u64,
    /// Total bytes received (ACKs, input events)
    pub bytes_received: u64,
    /// Current frames per second (measured)
    pub current_fps: f32,
    /// Current bitrate in kilobits per second
    pub current_bitrate_kbps: u32,
    /// Round-trip time estimate (from ACKs)
    pub rtt_ms: Option<u32>,
    /// Last sequence number acknowledged by peer
    pub last_ack_seq: u64,
    /// Current sequence number being sent
    pub current_seq: u64,
    /// Time of last frame capture
    pub last_capture_time: Option<Instant>,
    /// Time of last ACK received
    pub last_ack_time: Option<Instant>,
    /// Encoding time for last frame (microseconds)
    pub last_encode_time_us: u64,
    /// Capture time for last frame (microseconds)
    pub last_capture_time_us: u64,
}

impl StreamStats {
    /// Create new empty stats.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the number of frames in flight (sent but not ACKed).
    pub fn frames_in_flight(&self) -> u64 {
        self.current_seq.saturating_sub(self.last_ack_seq)
    }

    /// Get the drop rate as a percentage.
    pub fn drop_rate(&self) -> f32 {
        if self.frames_captured == 0 {
            return 0.0;
        }
        (self.frames_dropped as f32 / self.frames_captured as f32) * 100.0
    }

    /// Record a frame capture.
    pub fn record_capture(&mut self, capture_time_us: u64) {
        self.frames_captured += 1;
        self.last_capture_time = Some(Instant::now());
        self.last_capture_time_us = capture_time_us;
    }

    /// Record a frame sent.
    pub fn record_send(&mut self, bytes: u64, encode_time_us: u64) {
        self.frames_sent += 1;
        self.bytes_sent += bytes;
        self.current_seq += 1;
        self.last_encode_time_us = encode_time_us;
    }

    /// Record a frame dropped.
    pub fn record_drop(&mut self) {
        self.frames_dropped += 1;
    }

    /// Record an ACK received.
    pub fn record_ack(&mut self, seq: u64, bytes_received: u64) {
        if seq > self.last_ack_seq {
            self.last_ack_seq = seq;
            self.last_ack_time = Some(Instant::now());
        }
        self.bytes_received += bytes_received;
    }

    /// Update the current FPS measurement.
    pub fn update_fps(&mut self, fps: f32) {
        self.current_fps = fps;
    }

    /// Update the current bitrate measurement.
    pub fn update_bitrate(&mut self, bitrate_kbps: u32) {
        self.current_bitrate_kbps = bitrate_kbps;
    }

    /// Update RTT estimate.
    pub fn update_rtt(&mut self, rtt_ms: u32) {
        self.rtt_ms = Some(rtt_ms);
    }
}

/// A screen streaming session.
#[derive(Debug)]
pub struct StreamSession {
    /// Unique session identifier
    pub id: SessionId,
    /// Current session state
    pub state: StreamState,
    /// Session statistics
    pub stats: StreamStats,
    /// Session configuration (snapshot at creation time)
    pub settings: ScreenStreamSettings,
    /// The display being captured
    pub display_id: String,
    /// The peer receiving the stream
    pub peer_id: String,
    /// When the session was created
    pub created_at: Instant,
    /// When the session started streaming (if active)
    pub started_at: Option<Instant>,
    /// When the session stopped (if stopped)
    pub stopped_at: Option<Instant>,
    /// Error message if state is Error
    pub error_message: Option<String>,
    /// Whether remote input is allowed
    pub allow_input: bool,
    /// Target frame interval (1/fps)
    frame_interval: Duration,
    /// Last time FPS was calculated
    last_fps_calc: Instant,
    /// Frame count at last FPS calculation
    frames_at_last_calc: u64,
}

impl StreamSession {
    /// Create a new streaming session.
    pub fn new(
        id: SessionId,
        display_id: String,
        peer_id: String,
        settings: ScreenStreamSettings,
        allow_input: bool,
    ) -> Self {
        let frame_interval = Duration::from_secs_f64(1.0 / settings.max_fps as f64);

        Self {
            id,
            state: StreamState::Starting,
            stats: StreamStats::new(),
            settings,
            display_id,
            peer_id,
            created_at: Instant::now(),
            started_at: None,
            stopped_at: None,
            error_message: None,
            allow_input,
            frame_interval,
            last_fps_calc: Instant::now(),
            frames_at_last_calc: 0,
        }
    }

    /// Get the target frame interval.
    pub fn frame_interval(&self) -> Duration {
        self.frame_interval
    }

    /// Update the target FPS.
    pub fn set_target_fps(&mut self, fps: u32) {
        self.frame_interval = Duration::from_secs_f64(1.0 / fps as f64);
    }

    /// Transition to active state.
    pub fn activate(&mut self) -> bool {
        if self.state.can_activate() {
            self.state = StreamState::Active;
            if self.started_at.is_none() {
                self.started_at = Some(Instant::now());
            }
            true
        } else {
            false
        }
    }

    /// Transition to paused state.
    pub fn pause(&mut self) -> bool {
        if self.state == StreamState::Active {
            self.state = StreamState::Paused;
            true
        } else {
            false
        }
    }

    /// Transition to stopping state.
    pub fn begin_stop(&mut self) -> bool {
        if !self.state.is_terminal() {
            self.state = StreamState::Stopping;
            true
        } else {
            false
        }
    }

    /// Transition to stopped state.
    pub fn complete_stop(&mut self) {
        self.state = StreamState::Stopped;
        self.stopped_at = Some(Instant::now());
    }

    /// Transition to error state.
    pub fn set_error(&mut self, message: String) {
        self.state = StreamState::Error;
        self.error_message = Some(message);
        self.stopped_at = Some(Instant::now());
    }

    /// Get session duration.
    pub fn duration(&self) -> Duration {
        let end_time = self.stopped_at.unwrap_or_else(Instant::now);
        if let Some(start) = self.started_at {
            end_time.duration_since(start)
        } else {
            Duration::ZERO
        }
    }

    /// Update FPS calculation periodically.
    pub fn update_fps_calc(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_fps_calc);

        // Update every second
        if elapsed >= Duration::from_secs(1) {
            let frames_delta = self.stats.frames_sent - self.frames_at_last_calc;
            let fps = frames_delta as f32 / elapsed.as_secs_f32();
            self.stats.update_fps(fps);

            self.last_fps_calc = now;
            self.frames_at_last_calc = self.stats.frames_sent;
        }
    }

    /// Check if we should capture the next frame based on timing.
    pub fn should_capture(&self) -> bool {
        if let Some(last) = self.stats.last_capture_time {
            last.elapsed() >= self.frame_interval
        } else {
            true
        }
    }

    /// Check if there's too much backpressure (too many frames in flight).
    pub fn has_backpressure(&self) -> bool {
        // If we have more than max_buffer_frames in flight, pause
        self.stats.frames_in_flight() > self.settings.max_buffer_frames as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScreenStreamSettings;

    #[test]
    fn test_stream_state_transitions() {
        assert!(StreamState::Starting.can_activate());
        assert!(StreamState::Paused.can_activate());
        assert!(!StreamState::Active.can_activate());
        assert!(!StreamState::Stopped.can_activate());

        assert!(StreamState::Stopped.is_terminal());
        assert!(StreamState::Error.is_terminal());
        assert!(!StreamState::Active.is_terminal());
    }

    #[test]
    fn test_stream_stats() {
        let mut stats = StreamStats::new();

        stats.record_capture(1000);
        assert_eq!(stats.frames_captured, 1);
        assert!(stats.last_capture_time.is_some());

        stats.record_send(1024, 500);
        assert_eq!(stats.frames_sent, 1);
        assert_eq!(stats.bytes_sent, 1024);
        assert_eq!(stats.current_seq, 1);

        stats.record_ack(1, 32);
        assert_eq!(stats.last_ack_seq, 1);
        assert_eq!(stats.bytes_received, 32);
        assert_eq!(stats.frames_in_flight(), 0);

        stats.record_send(2048, 600);
        assert_eq!(stats.frames_in_flight(), 1);

        stats.record_drop();
        assert_eq!(stats.frames_dropped, 1);
    }

    #[test]
    fn test_session_lifecycle() {
        let settings = ScreenStreamSettings::default();
        let mut session = StreamSession::new(
            "test-session".to_string(),
            "display-0".to_string(),
            "peer-123".to_string(),
            settings,
            true,
        );

        assert_eq!(session.state, StreamState::Starting);

        // Can activate from starting
        assert!(session.activate());
        assert_eq!(session.state, StreamState::Active);
        assert!(session.started_at.is_some());

        // Can pause from active
        assert!(session.pause());
        assert_eq!(session.state, StreamState::Paused);

        // Can reactivate from paused
        assert!(session.activate());
        assert_eq!(session.state, StreamState::Active);

        // Can stop
        assert!(session.begin_stop());
        assert_eq!(session.state, StreamState::Stopping);

        session.complete_stop();
        assert_eq!(session.state, StreamState::Stopped);
        assert!(session.stopped_at.is_some());

        // Cannot activate from stopped
        assert!(!session.activate());
    }

    #[test]
    fn test_session_error() {
        let settings = ScreenStreamSettings::default();
        let mut session = StreamSession::new(
            "test-session".to_string(),
            "display-0".to_string(),
            "peer-123".to_string(),
            settings,
            false,
        );

        session.set_error("Test error".to_string());
        assert_eq!(session.state, StreamState::Error);
        assert_eq!(session.error_message, Some("Test error".to_string()));
        assert!(session.stopped_at.is_some());
    }
}
