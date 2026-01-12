//! Stream events for communication between components.
//!
//! This module defines events that flow between the stream manager,
//! capture backend, encoder, and network layer.

use crate::iroh::protocol::ScreenQuality;

use super::session::{SessionId, StreamState, StreamStats};
use super::types::Display;

/// Events emitted by the streaming system.
#[derive(Debug)]
pub enum StreamEvent {
    /// Session state changed
    StateChanged {
        session_id: SessionId,
        old_state: StreamState,
        new_state: StreamState,
    },

    /// A frame was captured
    FrameCaptured {
        session_id: SessionId,
        sequence: u64,
        width: u32,
        height: u32,
        capture_time_us: u64,
    },

    /// A frame was encoded and ready for transmission
    FrameEncoded {
        session_id: SessionId,
        sequence: u64,
        encoded_size: usize,
        encode_time_us: u64,
    },

    /// A frame was sent to the peer
    FrameSent {
        session_id: SessionId,
        sequence: u64,
        bytes_sent: u64,
    },

    /// A frame was dropped (due to backpressure or error)
    FrameDropped {
        session_id: SessionId,
        sequence: u64,
        reason: FrameDropReason,
    },

    /// Received ACK from peer
    AckReceived {
        session_id: SessionId,
        sequence: u64,
        rtt_ms: Option<u32>,
    },

    /// Statistics updated
    StatsUpdated {
        session_id: SessionId,
        stats: StreamStats,
    },

    /// Quality adjustment suggested
    QualityAdjustment {
        session_id: SessionId,
        suggested_fps: Option<u32>,
        suggested_quality: Option<ScreenQuality>,
        suggested_bitrate_kbps: Option<u32>,
    },

    /// Input event received from remote peer
    InputReceived {
        session_id: SessionId,
        event: RemoteInputEvent,
    },

    /// Error occurred
    Error {
        session_id: SessionId,
        error: String,
    },

    /// Session ended
    SessionEnded {
        session_id: SessionId,
        reason: SessionEndReason,
    },

    /// Available displays changed
    DisplaysChanged {
        displays: Vec<Display>,
    },
}

/// Reason for dropping a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDropReason {
    /// Too many frames in flight, waiting for ACKs
    Backpressure,
    /// Encoding failed
    EncodingError,
    /// Network send failed
    NetworkError,
    /// Frame rate limiting (skipped to maintain target FPS)
    RateLimited,
    /// Session is paused
    Paused,
    /// Session is stopping
    Stopping,
}

impl std::fmt::Display for FrameDropReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameDropReason::Backpressure => write!(f, "backpressure"),
            FrameDropReason::EncodingError => write!(f, "encoding error"),
            FrameDropReason::NetworkError => write!(f, "network error"),
            FrameDropReason::RateLimited => write!(f, "rate limited"),
            FrameDropReason::Paused => write!(f, "paused"),
            FrameDropReason::Stopping => write!(f, "stopping"),
        }
    }
}

/// Reason for session ending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEndReason {
    /// Normal shutdown requested
    Requested,
    /// Peer disconnected
    PeerDisconnected,
    /// Error occurred
    Error(String),
    /// Timeout (no ACKs received)
    Timeout,
    /// Permission revoked
    PermissionRevoked,
}

impl std::fmt::Display for SessionEndReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionEndReason::Requested => write!(f, "requested"),
            SessionEndReason::PeerDisconnected => write!(f, "peer disconnected"),
            SessionEndReason::Error(e) => write!(f, "error: {}", e),
            SessionEndReason::Timeout => write!(f, "timeout"),
            SessionEndReason::PermissionRevoked => write!(f, "permission revoked"),
        }
    }
}

/// Input event from remote peer.
#[derive(Debug, Clone)]
pub enum RemoteInputEvent {
    /// Mouse movement
    MouseMove {
        x: i32,
        y: i32,
        /// Whether coordinates are absolute or relative
        absolute: bool,
    },

    /// Mouse button press/release
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },

    /// Mouse wheel scroll
    MouseScroll {
        /// Horizontal scroll delta
        dx: i32,
        /// Vertical scroll delta
        dy: i32,
    },

    /// Key press/release
    Key {
        /// Platform-independent key code
        key: KeyCode,
        /// Whether the key was pressed (true) or released (false)
        pressed: bool,
        /// Modifier keys held
        modifiers: KeyModifiers,
    },

    /// Text input (for IME and text fields)
    TextInput {
        text: String,
    },
}

/// Mouse button identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    Other(u8),
}

/// Key code - platform-independent key identifier.
/// Uses a subset of common keys; can be extended as needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    // Letters
    A, B, C, D, E, F, G, H, I, J, K, L, M,
    N, O, P, Q, R, S, T, U, V, W, X, Y, Z,

    // Numbers
    Key0, Key1, Key2, Key3, Key4, Key5, Key6, Key7, Key8, Key9,

    // Function keys
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,

    // Modifiers
    LeftShift, RightShift,
    LeftControl, RightControl,
    LeftAlt, RightAlt,
    LeftSuper, RightSuper,

    // Navigation
    Up, Down, Left, Right,
    Home, End, PageUp, PageDown,
    Insert, Delete,

    // Editing
    Backspace, Tab, Return, Escape, Space,

    // Punctuation
    Comma, Period, Slash, Backslash,
    Semicolon, Apostrophe,
    LeftBracket, RightBracket,
    Minus, Equals, Grave,

    // Numpad
    Numpad0, Numpad1, Numpad2, Numpad3, Numpad4,
    Numpad5, Numpad6, Numpad7, Numpad8, Numpad9,
    NumpadAdd, NumpadSubtract, NumpadMultiply, NumpadDivide,
    NumpadDecimal, NumpadEnter,
    NumLock,

    // Other
    CapsLock, ScrollLock, PrintScreen, Pause,
    Menu,

    /// Unknown or platform-specific key
    Unknown(u32),
}

/// Key modifier flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub super_key: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

impl KeyModifiers {
    /// No modifiers.
    pub const NONE: Self = Self {
        shift: false,
        control: false,
        alt: false,
        super_key: false,
        caps_lock: false,
        num_lock: false,
    };

    /// Check if any modifier is held.
    pub fn any(&self) -> bool {
        self.shift || self.control || self.alt || self.super_key
    }
}

/// Channel types for stream events.
pub type StreamEventSender = tokio::sync::mpsc::Sender<StreamEvent>;
pub type StreamEventReceiver = tokio::sync::mpsc::Receiver<StreamEvent>;

/// Create a new stream event channel.
pub fn stream_event_channel(buffer: usize) -> (StreamEventSender, StreamEventReceiver) {
    tokio::sync::mpsc::channel(buffer)
}

/// Commands sent to the streaming task.
#[derive(Debug)]
pub enum StreamCommand {
    /// Start streaming
    Start,
    /// Pause streaming
    Pause,
    /// Resume streaming (same as Start after pause)
    Resume,
    /// Stop streaming
    Stop,
    /// Force a keyframe (for video codecs)
    ForceKeyframe,
    /// Adjust quality settings
    AdjustQuality {
        fps: Option<u32>,
        quality: Option<ScreenQuality>,
        bitrate_kbps: Option<u32>,
    },
    /// Inject input event
    InjectInput(RemoteInputEvent),
}

/// Channel types for stream commands.
pub type StreamCommandSender = tokio::sync::mpsc::Sender<StreamCommand>;
pub type StreamCommandReceiver = tokio::sync::mpsc::Receiver<StreamCommand>;

/// Create a new stream command channel.
pub fn stream_command_channel(buffer: usize) -> (StreamCommandSender, StreamCommandReceiver) {
    tokio::sync::mpsc::channel(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_drop_reason_display() {
        assert_eq!(FrameDropReason::Backpressure.to_string(), "backpressure");
        assert_eq!(FrameDropReason::EncodingError.to_string(), "encoding error");
    }

    #[test]
    fn test_session_end_reason_display() {
        assert_eq!(SessionEndReason::Requested.to_string(), "requested");
        assert_eq!(
            SessionEndReason::Error("test".to_string()).to_string(),
            "error: test"
        );
    }

    #[test]
    fn test_key_modifiers() {
        let mods = KeyModifiers::NONE;
        assert!(!mods.any());

        let mods = KeyModifiers {
            shift: true,
            ..Default::default()
        };
        assert!(mods.any());
    }
}
