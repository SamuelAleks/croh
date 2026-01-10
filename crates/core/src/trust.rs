//! Trust bundle management for peer establishment.
//!
//! A trust bundle is a JSON file sent via croc to bootstrap a trusted peer connection.
//! It contains the sender's Iroh endpoint ID and capabilities, allowing the receiver
//! to connect back via Iroh and complete the trust handshake.

use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Version of the trust bundle format.
pub const TRUST_BUNDLE_VERSION: u32 = 1;

/// How long a trust bundle is valid (5 minutes).
const BUNDLE_VALIDITY_MINUTES: i64 = 5;

/// Prefix for trust bundle filenames.
pub const TRUST_BUNDLE_PREFIX: &str = "croh-trust-";

/// Information about a peer for trust bundles and handshakes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerInfo {
    /// The Iroh endpoint ID (NodeId as string)
    pub endpoint_id: String,

    /// User-provided device name
    pub name: String,

    /// Software version
    pub version: String,

    /// Relay URL for connectivity (optional, but needed for NAT traversal)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
}

/// Capabilities that can be offered in a trust relationship.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    /// Can push files to this peer
    Push,
    /// Can pull files from this peer
    Pull,
    /// Can browse filesystem on this peer
    Browse,
    /// Can query status of this peer
    Status,
}

impl Capability {
    /// Get all capabilities.
    pub fn all() -> Vec<Capability> {
        vec![
            Capability::Push,
            Capability::Pull,
            Capability::Browse,
            Capability::Status,
        ]
    }
}

/// A trust bundle sent via croc to initiate a trusted peer connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustBundle {
    /// Version marker (always TRUST_BUNDLE_VERSION)
    pub croh_trust: u32,

    /// Information about the sender
    pub sender: PeerInfo,

    /// Capabilities the sender is offering
    pub capabilities_offered: Vec<Capability>,

    /// Random nonce for handshake verification
    pub nonce: String,

    /// When this bundle was created
    pub created_at: DateTime<Utc>,

    /// When this bundle expires
    pub expires_at: DateTime<Utc>,
}

impl TrustBundle {
    /// Create a new trust bundle from an identity.
    pub fn new(identity: &crate::iroh::Identity) -> Self {
        Self::new_with_relay(identity, None)
    }

    /// Create a new trust bundle from an identity with a relay URL.
    pub fn new_with_relay(identity: &crate::iroh::Identity, relay_url: Option<String>) -> Self {
        let now = Utc::now();
        let expires_at = now + Duration::minutes(BUNDLE_VALIDITY_MINUTES);

        Self {
            croh_trust: TRUST_BUNDLE_VERSION,
            sender: identity.to_peer_info_with_relay(relay_url),
            capabilities_offered: Capability::all(),
            nonce: generate_nonce(),
            created_at: now,
            expires_at,
        }
    }

    /// Check if this bundle is still valid (not expired).
    pub fn is_valid(&self) -> bool {
        Utc::now() < self.expires_at
    }

    /// Check if a string contains a trust bundle by looking for the marker key.
    pub fn is_trust_bundle(content: &str) -> bool {
        // Quick check for the marker key
        if !content.contains("\"croh_trust\"") {
            return false;
        }

        // Try to parse and verify
        serde_json::from_str::<TrustBundle>(content).is_ok()
    }

    /// Load a trust bundle from a file.
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let bundle: TrustBundle = serde_json::from_str(&contents)?;
        Ok(bundle)
    }

    /// Save this bundle to a temporary file for sending via croc.
    /// Returns the path to the created file.
    pub fn save_to_temp(&self) -> Result<PathBuf> {
        let temp_dir = std::env::temp_dir();
        let filename = format!("{}{}.json", TRUST_BUNDLE_PREFIX, &self.nonce[..8]);
        let path = temp_dir.join(filename);

        let contents = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;

        Ok(path)
    }

    /// Check if a filename matches the trust bundle pattern.
    pub fn is_trust_bundle_filename(filename: &str) -> bool {
        filename.starts_with(TRUST_BUNDLE_PREFIX) && filename.ends_with(".json")
    }
}

/// Generate a random nonce for handshake verification.
fn generate_nonce() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 16] = rng.gen();
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iroh::Identity;

    #[test]
    fn test_trust_bundle_creation() {
        let identity = Identity::generate("Test Device".to_string()).unwrap();
        let bundle = TrustBundle::new(&identity);

        assert_eq!(bundle.croh_trust, TRUST_BUNDLE_VERSION);
        assert_eq!(bundle.sender.endpoint_id, identity.endpoint_id);
        assert!(!bundle.nonce.is_empty());
        assert!(bundle.is_valid());
    }

    #[test]
    fn test_trust_bundle_detection() {
        let identity = Identity::generate("Test Device".to_string()).unwrap();
        let bundle = TrustBundle::new(&identity);
        let json = serde_json::to_string(&bundle).unwrap();

        assert!(TrustBundle::is_trust_bundle(&json));
        assert!(!TrustBundle::is_trust_bundle("Hello world"));
        assert!(!TrustBundle::is_trust_bundle(r#"{"foo": "bar"}"#));
    }

    #[test]
    fn test_trust_bundle_filename_detection() {
        assert!(TrustBundle::is_trust_bundle_filename(
            "croh-trust-abc123.json"
        ));
        assert!(!TrustBundle::is_trust_bundle_filename("other-file.json"));
        assert!(!TrustBundle::is_trust_bundle_filename("croh-trust-abc123.txt"));
    }

    #[test]
    fn test_trust_bundle_serialization() {
        let identity = Identity::generate("Test Device".to_string()).unwrap();
        let bundle = TrustBundle::new(&identity);

        let json = serde_json::to_string_pretty(&bundle).unwrap();
        let parsed: TrustBundle = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.nonce, bundle.nonce);
        assert_eq!(parsed.sender.endpoint_id, bundle.sender.endpoint_id);
    }
}
