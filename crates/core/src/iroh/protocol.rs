//! Control protocol messages for trusted peer communication.
//!
//! All messages are JSON, length-prefixed (4-byte big-endian length, then JSON bytes).

use crate::chat::ChatSyncMessage;
use crate::peers::Permissions;
use crate::trust::PeerInfo;
use serde::{Deserialize, Serialize};

/// ALPN identifier for the control protocol.
pub const ALPN_CONTROL: &[u8] = b"croh/control/1";

/// ALPN identifier for the blob transfer protocol.
pub const ALPN_BLOBS: &[u8] = b"croh/blobs/1";

/// Information about a file for transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    /// Relative path (from share root)
    pub path: String,
    /// File name
    pub name: String,
    /// Size in bytes
    pub size: u64,
    /// BLAKE3 hash (hex-encoded)
    pub hash: String,
}

/// Request for a specific file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRequest {
    /// Path to request
    pub path: String,
    /// Optional hash to verify
    pub hash: Option<String>,
}

/// Directory entry for browsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    /// Entry name
    pub name: String,
    /// Whether this is a directory
    pub is_dir: bool,
    /// Size in bytes (0 for directories)
    pub size: u64,
    /// Last modified timestamp (Unix timestamp, optional)
    pub modified: Option<i64>,
}

// ==================== Screen Streaming Types ====================

/// Display information for screen streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    /// Display identifier (e.g., "HDMI-1", "eDP-1", "0")
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Refresh rate in Hz (if known)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_rate: Option<u32>,
    /// Whether this is the primary display
    pub is_primary: bool,
}

/// Compression format for screen frames.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScreenCompression {
    /// Raw RGBA frames (testing only, very high bandwidth)
    Raw,
    /// WebP for low-motion screens (good for text/desktop)
    WebP,
    /// H.264/AVC (best compatibility, hardware acceleration)
    #[default]
    H264,
    /// VP9 (better compression, software only)
    Vp9,
    /// AV1 (best compression, newer hardware only)
    Av1,
}

/// Quality preset for screen streaming.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScreenQuality {
    /// Prioritize low latency (lower quality)
    Fast,
    /// Balanced latency and quality
    #[default]
    Balanced,
    /// Prioritize quality (higher latency)
    Quality,
    /// Auto-adjust based on network conditions
    Auto,
}

/// Frame metadata for a single screen frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameMetadata {
    /// Sequential frame number (monotonically increasing)
    pub sequence: u64,
    /// Frame width in pixels
    pub width: u32,
    /// Frame height in pixels
    pub height: u32,
    /// Capture timestamp (Unix millis)
    pub captured_at: i64,
    /// Compression format used
    pub compression: ScreenCompression,
    /// Whether this is a keyframe (can be decoded independently)
    pub is_keyframe: bool,
    /// Compressed frame size in bytes
    pub size: u32,
}

/// Input event from viewer to host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputEvent {
    /// Mouse movement (absolute coordinates, scaled 0-65535)
    MouseMove {
        /// X position (0-65535, scaled to display width)
        x: i32,
        /// Y position (0-65535, scaled to display height)
        y: i32,
    },
    /// Mouse button press/release
    MouseButton {
        /// Button: 0=left, 1=right, 2=middle, 3+=extended
        button: u8,
        /// Whether the button is pressed (true) or released (false)
        pressed: bool,
    },
    /// Mouse scroll
    MouseScroll {
        /// Horizontal scroll delta (positive = right)
        delta_x: i32,
        /// Vertical scroll delta (positive = down)
        delta_y: i32,
    },
    /// Keyboard key press/release
    Key {
        /// Platform-independent key code (USB HID usage codes)
        code: u16,
        /// Whether the key is pressed (true) or released (false)
        pressed: bool,
        /// Modifier state: bit 0=shift, 1=ctrl, 2=alt, 3=meta
        modifiers: u8,
    },
}

/// Control messages exchanged between trusted peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Trust confirmation during handshake.
    TrustConfirm {
        /// Information about the sender
        peer: PeerInfo,
        /// Nonce from the trust bundle for verification
        nonce: String,
        /// Permissions being offered
        permissions: Permissions,
    },

    /// Trust handshake completed successfully.
    TrustComplete,

    /// Trust revocation.
    TrustRevoke {
        /// Reason for revocation
        reason: String,
    },

    /// Permissions update notification.
    /// Sent when we change the permissions we grant to a peer.
    PermissionsUpdate {
        /// Updated permissions we grant to this peer
        permissions: Permissions,
    },

    /// Ping for keepalive.
    Ping {
        /// Unix timestamp
        timestamp: u64,
    },

    /// Pong response to ping.
    Pong {
        /// Echo back the timestamp from ping
        timestamp: u64,
    },

    /// Status request.
    StatusRequest,

    /// Status response.
    StatusResponse {
        /// Hostname of the peer
        hostname: String,
        /// Operating system info
        os: String,
        /// Free disk space in bytes
        free_space: u64,
        /// Total disk space in bytes
        total_space: u64,
        /// Download directory path
        download_dir: String,
        /// Uptime in seconds
        uptime: u64,
        /// Software version
        version: String,
        /// Number of active transfers
        active_transfers: u32,
    },

    // ==================== File Transfer Messages ====================

    /// Push offer: sender wants to push files to receiver.
    PushOffer {
        /// Unique transfer ID
        transfer_id: String,
        /// Files being offered with their hashes and sizes
        files: Vec<FileInfo>,
        /// Total size in bytes
        total_size: u64,
    },

    /// Push response from receiver.
    PushResponse {
        /// Transfer ID from the offer
        transfer_id: String,
        /// Whether to accept the push
        accepted: bool,
        /// Reason if rejected
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Pull request: requester wants to pull files from provider.
    PullRequest {
        /// Unique transfer ID
        transfer_id: String,
        /// Files to pull (by path)
        files: Vec<FileRequest>,
    },

    /// Pull response with file info.
    PullResponse {
        /// Transfer ID from the request
        transfer_id: String,
        /// Files available with their hashes (if granted)
        #[serde(default)]
        files: Vec<FileInfo>,
        /// Whether request is granted
        granted: bool,
        /// Reason if denied
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Transfer progress update.
    TransferProgress {
        /// Transfer ID
        transfer_id: String,
        /// Bytes transferred so far
        transferred: u64,
        /// Total bytes
        total: u64,
        /// Current file being transferred
        #[serde(skip_serializing_if = "Option::is_none")]
        current_file: Option<String>,
    },

    /// Transfer completed successfully.
    TransferComplete {
        /// Transfer ID
        transfer_id: String,
    },

    /// Transfer failed.
    TransferFailed {
        /// Transfer ID
        transfer_id: String,
        /// Error message
        error: String,
    },

    /// Cancel a transfer.
    TransferCancel {
        /// Transfer ID
        transfer_id: String,
        /// Reason for cancellation
        reason: String,
    },

    // ==================== File Browsing Messages ====================

    /// Browse request: list files in a directory.
    BrowseRequest {
        /// Path to browse (None for root/allowed paths)
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },

    /// Browse response.
    BrowseResponse {
        /// Path that was browsed
        path: String,
        /// Directory entries
        entries: Vec<DirectoryEntry>,
        /// Error if any
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    // ==================== Status Messages ====================

    /// Do Not Disturb status update.
    /// Sent when a peer's DND status changes.
    DndStatus {
        /// Whether DND is enabled
        enabled: bool,
        /// DND mode: "available", "silent", "busy"
        mode: String,
        /// When DND expires (Unix timestamp), None = indefinite
        #[serde(skip_serializing_if = "Option::is_none")]
        until: Option<i64>,
        /// Optional custom status message
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    // ==================== Speed Test Messages ====================

    /// Request to start a speed test.
    SpeedTestRequest {
        /// Unique test ID
        test_id: String,
        /// Size of data to transfer in bytes (default 10MB)
        size: u64,
    },

    /// Accept a speed test request.
    SpeedTestAccept {
        /// Test ID from the request
        test_id: String,
    },

    /// Reject a speed test request.
    SpeedTestReject {
        /// Test ID from the request
        test_id: String,
        /// Reason for rejection
        reason: String,
    },

    /// Speed test data chunk (sent during test).
    SpeedTestData {
        /// Test ID
        test_id: String,
        /// Sequence number
        sequence: u32,
        /// Data payload (base64 encoded random bytes)
        data: String,
    },

    /// Speed test completed with results.
    SpeedTestComplete {
        /// Test ID
        test_id: String,
        /// Upload speed in bytes per second (initiator -> responder)
        upload_speed: u64,
        /// Download speed in bytes per second (responder -> initiator)
        download_speed: u64,
        /// Round-trip latency in milliseconds
        latency_ms: u64,
        /// Total bytes transferred
        bytes_transferred: u64,
        /// Duration in milliseconds
        duration_ms: u64,
    },

    // ==================== Guest Peer Messages ====================

    /// Guest requests time extension.
    ExtensionRequest {
        /// Requested additional hours
        requested_hours: u32,
        /// Optional reason for the request
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Response to extension request.
    ExtensionResponse {
        /// Whether the extension was approved
        approved: bool,
        /// New expiry time if approved (Unix timestamp)
        #[serde(skip_serializing_if = "Option::is_none")]
        new_expires_at: Option<i64>,
        /// Reason if denied
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Guest requests promotion to trusted peer.
    PromotionRequest {
        /// Optional reason for the request
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Response to promotion request.
    PromotionResponse {
        /// Whether the promotion was approved
        approved: bool,
        /// Reason if denied
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        // Note: If approved, the owner will initiate a new trust handshake
        // with a fresh TrustBundle for security. The guest must complete
        // this handshake to finalize the promotion.
    },

    // ==================== Network Introduction Messages ====================
    // Three-way consent flow for peer introductions within a network.
    // 1. Introducer (A) sends IntroductionOffer to peer B with C's info
    // 2. B accepts or rejects the offer
    // 3. A sends IntroductionRequest to peer C with B's info
    // 4. C accepts or rejects the request
    // 5. If both accept, A sends IntroductionComplete to both with connection details
    // 6. B and C connect directly and complete a fresh trust handshake

    /// Introduction offer: sent to one peer (B) offering to introduce them to another (C).
    IntroductionOffer {
        /// Unique introduction ID
        introduction_id: String,
        /// Network ID where the introduction is happening
        network_id: String,
        /// Information about the peer being introduced (C)
        peer: PeerInfo,
        /// Optional message from the introducer
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Response to an introduction offer.
    IntroductionOfferResponse {
        /// Introduction ID from the offer
        introduction_id: String,
        /// Whether the offer is accepted
        accepted: bool,
        /// Reason if rejected
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Introduction request: sent to the other peer (C) asking if they accept introduction to B.
    IntroductionRequest {
        /// Unique introduction ID
        introduction_id: String,
        /// Network ID where the introduction is happening
        network_id: String,
        /// Information about the peer requesting introduction (B)
        peer: PeerInfo,
        /// Optional message from the introducer
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Response to an introduction request.
    IntroductionRequestResponse {
        /// Introduction ID from the request
        introduction_id: String,
        /// Whether the request is accepted
        accepted: bool,
        /// Reason if rejected
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Introduction complete: sent to both peers after both have accepted.
    /// Contains connection details for direct peer-to-peer connection.
    IntroductionComplete {
        /// Introduction ID
        introduction_id: String,
        /// Network ID
        network_id: String,
        /// The peer to connect to
        peer: PeerInfo,
        /// Trust bundle for the peer (for establishing trust)
        trust_bundle_json: String,
    },

    /// Introduction failed notification.
    IntroductionFailed {
        /// Introduction ID
        introduction_id: String,
        /// Reason for failure
        reason: String,
    },

    // ==================== Chat Messages ====================

    /// Send a chat message to peer.
    ChatMessage {
        /// Unique message ID
        message_id: String,
        /// Message content (UTF-8 text, max 10KB)
        content: String,
        /// Sender-side sequence number for ordering
        sequence: u64,
        /// Timestamp when sent (Unix timestamp millis)
        sent_at: i64,
    },

    /// Acknowledge receipt of chat messages (delivery receipt).
    ChatDelivered {
        /// Message IDs that were delivered
        message_ids: Vec<String>,
    },

    /// Mark messages as read (read receipt).
    ChatRead {
        /// Message IDs that were read
        message_ids: Vec<String>,
        /// Highest sequence number read (for batch acknowledgment)
        up_to_sequence: u64,
    },

    /// Typing indicator.
    ChatTyping {
        /// Whether user is currently typing
        is_typing: bool,
    },

    /// Request message history sync.
    ChatSyncRequest {
        /// Request messages after this sequence number
        after_sequence: u64,
        /// Maximum number of messages to retrieve
        limit: u32,
    },

    /// Response with message history.
    ChatSyncResponse {
        /// Messages in chronological order
        messages: Vec<ChatSyncMessage>,
        /// Whether there are more messages available
        has_more: bool,
    },

    // ==================== Screen Streaming Messages ====================

    /// Request to start screen streaming.
    ScreenStreamRequest {
        /// Unique stream ID
        stream_id: String,
        /// Which display to stream (None = primary)
        #[serde(skip_serializing_if = "Option::is_none")]
        display_id: Option<String>,
        /// Requested compression format
        compression: ScreenCompression,
        /// Quality preset
        quality: ScreenQuality,
        /// Target frame rate (0 = max available)
        target_fps: u32,
    },

    /// Response to screen stream request.
    ScreenStreamResponse {
        /// Stream ID from request
        stream_id: String,
        /// Whether streaming is accepted
        accepted: bool,
        /// Reason if rejected
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Available displays (for selection)
        #[serde(default)]
        displays: Vec<DisplayInfo>,
        /// Actual compression that will be used
        #[serde(skip_serializing_if = "Option::is_none")]
        compression: Option<ScreenCompression>,
    },

    /// Screen frame header (followed by raw frame data via send_raw).
    /// After receiving this, read chunks until a zero-length chunk.
    ScreenFrame {
        /// Stream ID
        stream_id: String,
        /// Frame metadata
        metadata: FrameMetadata,
    },

    /// Acknowledge receipt of frames (for flow control).
    ScreenFrameAck {
        /// Stream ID
        stream_id: String,
        /// Highest sequence number received
        up_to_sequence: u64,
        /// Estimated available bandwidth (bytes/sec)
        #[serde(skip_serializing_if = "Option::is_none")]
        estimated_bandwidth: Option<u64>,
        /// Suggested quality adjustment
        #[serde(skip_serializing_if = "Option::is_none")]
        quality_hint: Option<ScreenQuality>,
    },

    /// Request quality/settings change mid-stream.
    ScreenStreamAdjust {
        /// Stream ID
        stream_id: String,
        /// New quality setting
        #[serde(skip_serializing_if = "Option::is_none")]
        quality: Option<ScreenQuality>,
        /// New target FPS
        #[serde(skip_serializing_if = "Option::is_none")]
        target_fps: Option<u32>,
        /// Request keyframe (for recovery after packet loss)
        #[serde(default)]
        request_keyframe: bool,
    },

    /// Stop screen streaming.
    ScreenStreamStop {
        /// Stream ID
        stream_id: String,
        /// Reason for stopping
        reason: String,
    },

    /// Input events from viewer (batched for efficiency).
    ScreenInput {
        /// Stream ID
        stream_id: String,
        /// Batch of input events
        events: Vec<InputEvent>,
    },

    /// Query available displays without starting a stream.
    DisplayListRequest,

    /// Response with available displays.
    DisplayListResponse {
        /// Available displays
        displays: Vec<DisplayInfo>,
        /// Current capture backend name
        backend: String,
    },
}

impl ControlMessage {
    /// Encode a message for sending (length-prefixed JSON).
    pub fn encode(&self) -> crate::error::Result<Vec<u8>> {
        let json = serde_json::to_vec(self)?;
        let len = json.len() as u32;
        let mut buf = Vec::with_capacity(4 + json.len());
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&json);
        Ok(buf)
    }

    /// Decode a message from bytes (expects length prefix already stripped).
    pub fn decode(data: &[u8]) -> crate::error::Result<Self> {
        let msg = serde_json::from_slice(data)?;
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization() {
        let msg = ControlMessage::Ping { timestamp: 12345 };
        let encoded = msg.encode().unwrap();

        // First 4 bytes are length
        let len = u32::from_be_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
        assert_eq!(len as usize, encoded.len() - 4);

        // Decode the JSON part
        let decoded = ControlMessage::decode(&encoded[4..]).unwrap();
        match decoded {
            ControlMessage::Ping { timestamp } => assert_eq!(timestamp, 12345),
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_trust_confirm_serialization() {
        let msg = ControlMessage::TrustConfirm {
            peer: PeerInfo {
                endpoint_id: "abc123".to_string(),
                name: "Test Device".to_string(),
                version: "0.1.0".to_string(),
                relay_url: None,
            },
            nonce: "nonce123".to_string(),
            permissions: Permissions::all(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"trust_confirm\""));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::TrustConfirm { peer, nonce, .. } => {
                assert_eq!(peer.endpoint_id, "abc123");
                assert_eq!(nonce, "nonce123");
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_extension_request_serialization() {
        let msg = ControlMessage::ExtensionRequest {
            requested_hours: 24,
            reason: Some("Need more time".to_string()),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"extension_request\""));
        assert!(json.contains("\"requested_hours\":24"));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::ExtensionRequest {
                requested_hours,
                reason,
            } => {
                assert_eq!(requested_hours, 24);
                assert_eq!(reason, Some("Need more time".to_string()));
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_extension_response_serialization() {
        let msg = ControlMessage::ExtensionResponse {
            approved: true,
            new_expires_at: Some(1704067200), // Example Unix timestamp
            reason: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"extension_response\""));
        assert!(json.contains("\"approved\":true"));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::ExtensionResponse {
                approved,
                new_expires_at,
                ..
            } => {
                assert!(approved);
                assert_eq!(new_expires_at, Some(1704067200));
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_promotion_request_serialization() {
        let msg = ControlMessage::PromotionRequest {
            reason: Some("Would like permanent access".to_string()),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"promotion_request\""));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::PromotionRequest { reason } => {
                assert_eq!(reason, Some("Would like permanent access".to_string()));
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_promotion_response_serialization() {
        let msg = ControlMessage::PromotionResponse {
            approved: false,
            reason: Some("Not approved at this time".to_string()),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"promotion_response\""));
        assert!(json.contains("\"approved\":false"));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::PromotionResponse { approved, reason } => {
                assert!(!approved);
                assert_eq!(reason, Some("Not approved at this time".to_string()));
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_introduction_offer_serialization() {
        let msg = ControlMessage::IntroductionOffer {
            introduction_id: "intro123".to_string(),
            network_id: "network456".to_string(),
            peer: PeerInfo {
                endpoint_id: "peer789".to_string(),
                name: "Bob's Device".to_string(),
                version: "0.1.0".to_string(),
                relay_url: None,
            },
            message: Some("Meet my friend Bob!".to_string()),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"introduction_offer\""));
        assert!(json.contains("\"introduction_id\":\"intro123\""));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::IntroductionOffer {
                introduction_id,
                network_id,
                peer,
                message,
            } => {
                assert_eq!(introduction_id, "intro123");
                assert_eq!(network_id, "network456");
                assert_eq!(peer.name, "Bob's Device");
                assert_eq!(message, Some("Meet my friend Bob!".to_string()));
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_introduction_complete_serialization() {
        let msg = ControlMessage::IntroductionComplete {
            introduction_id: "intro123".to_string(),
            network_id: "network456".to_string(),
            peer: PeerInfo {
                endpoint_id: "peer789".to_string(),
                name: "Carol's Device".to_string(),
                version: "0.1.0".to_string(),
                relay_url: Some("https://relay.example.com".to_string()),
            },
            trust_bundle_json: "{\"nonce\":\"abc\"}".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"introduction_complete\""));
        assert!(json.contains("\"trust_bundle_json\""));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::IntroductionComplete {
                introduction_id,
                peer,
                trust_bundle_json,
                ..
            } => {
                assert_eq!(introduction_id, "intro123");
                assert_eq!(peer.name, "Carol's Device");
                assert!(trust_bundle_json.contains("nonce"));
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_screen_stream_request_serialization() {
        let msg = ControlMessage::ScreenStreamRequest {
            stream_id: "stream123".to_string(),
            display_id: Some("HDMI-1".to_string()),
            compression: ScreenCompression::H264,
            quality: ScreenQuality::Balanced,
            target_fps: 30,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"screen_stream_request\""));
        assert!(json.contains("\"stream_id\":\"stream123\""));
        assert!(json.contains("\"compression\":\"h264\""));
        assert!(json.contains("\"quality\":\"balanced\""));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::ScreenStreamRequest {
                stream_id,
                display_id,
                compression,
                quality,
                target_fps,
            } => {
                assert_eq!(stream_id, "stream123");
                assert_eq!(display_id, Some("HDMI-1".to_string()));
                assert_eq!(compression, ScreenCompression::H264);
                assert_eq!(quality, ScreenQuality::Balanced);
                assert_eq!(target_fps, 30);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_screen_stream_response_serialization() {
        let msg = ControlMessage::ScreenStreamResponse {
            stream_id: "stream123".to_string(),
            accepted: true,
            reason: None,
            displays: vec![
                DisplayInfo {
                    id: "0".to_string(),
                    name: "Primary Display".to_string(),
                    width: 1920,
                    height: 1080,
                    refresh_rate: Some(60),
                    is_primary: true,
                },
            ],
            compression: Some(ScreenCompression::H264),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"screen_stream_response\""));
        assert!(json.contains("\"accepted\":true"));
        assert!(json.contains("\"width\":1920"));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::ScreenStreamResponse {
                stream_id,
                accepted,
                displays,
                ..
            } => {
                assert_eq!(stream_id, "stream123");
                assert!(accepted);
                assert_eq!(displays.len(), 1);
                assert_eq!(displays[0].width, 1920);
                assert!(displays[0].is_primary);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_screen_frame_serialization() {
        let msg = ControlMessage::ScreenFrame {
            stream_id: "stream123".to_string(),
            metadata: FrameMetadata {
                sequence: 42,
                width: 1920,
                height: 1080,
                captured_at: 1704067200000,
                compression: ScreenCompression::H264,
                is_keyframe: true,
                size: 65536,
            },
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"screen_frame\""));
        assert!(json.contains("\"sequence\":42"));
        assert!(json.contains("\"is_keyframe\":true"));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::ScreenFrame { stream_id, metadata } => {
                assert_eq!(stream_id, "stream123");
                assert_eq!(metadata.sequence, 42);
                assert!(metadata.is_keyframe);
                assert_eq!(metadata.size, 65536);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_screen_input_serialization() {
        let msg = ControlMessage::ScreenInput {
            stream_id: "stream123".to_string(),
            events: vec![
                InputEvent::MouseMove { x: 1000, y: 500 },
                InputEvent::MouseButton { button: 0, pressed: true },
                InputEvent::Key { code: 0x04, pressed: true, modifiers: 0 },
            ],
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"screen_input\""));
        assert!(json.contains("\"kind\":\"mouse_move\""));
        assert!(json.contains("\"kind\":\"mouse_button\""));
        assert!(json.contains("\"kind\":\"key\""));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::ScreenInput { stream_id, events } => {
                assert_eq!(stream_id, "stream123");
                assert_eq!(events.len(), 3);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_display_list_response_serialization() {
        let msg = ControlMessage::DisplayListResponse {
            displays: vec![
                DisplayInfo {
                    id: "0".to_string(),
                    name: "eDP-1".to_string(),
                    width: 2560,
                    height: 1440,
                    refresh_rate: Some(144),
                    is_primary: true,
                },
                DisplayInfo {
                    id: "1".to_string(),
                    name: "HDMI-1".to_string(),
                    width: 1920,
                    height: 1080,
                    refresh_rate: Some(60),
                    is_primary: false,
                },
            ],
            backend: "drm".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"display_list_response\""));
        assert!(json.contains("\"backend\":\"drm\""));

        let parsed: ControlMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMessage::DisplayListResponse { displays, backend } => {
                assert_eq!(displays.len(), 2);
                assert_eq!(backend, "drm");
                assert_eq!(displays[0].refresh_rate, Some(144));
            }
            _ => panic!("wrong message type"),
        }
    }
}
