//! Control protocol messages for trusted peer communication.
//!
//! All messages are JSON, length-prefixed (4-byte big-endian length, then JSON bytes).

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
}
