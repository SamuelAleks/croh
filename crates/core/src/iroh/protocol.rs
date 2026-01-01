//! Control protocol messages for trusted peer communication.
//!
//! All messages are JSON, length-prefixed (4-byte big-endian length, then JSON bytes).

use crate::peers::Permissions;
use crate::trust::PeerInfo;
use serde::{Deserialize, Serialize};

/// ALPN identifier for the control protocol.
pub const ALPN_CONTROL: &[u8] = b"croc-gui/control/1";

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
        /// Download directory path
        download_dir: String,
        /// Uptime in seconds
        uptime: u64,
        /// Software version
        version: String,
        /// Number of active transfers
        active_transfers: u32,
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
}
