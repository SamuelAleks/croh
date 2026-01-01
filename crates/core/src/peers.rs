//! Trusted peer management and storage.
//!
//! This module handles the persistence and management of trusted peers
//! that have been established via the trust handshake protocol.

use crate::error::{Error, Result};
use crate::platform;
use crate::trust::Capability;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Permissions granted to/from a peer.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Permissions {
    /// Can push files to this peer
    pub push: bool,
    /// Can pull files from this peer
    pub pull: bool,
    /// Can browse filesystem on this peer
    pub browse: bool,
    /// Can query status of this peer
    pub status: bool,
}

impl Permissions {
    /// Create permissions with all capabilities enabled.
    pub fn all() -> Self {
        Self {
            push: true,
            pull: true,
            browse: true,
            status: true,
        }
    }

    /// Create permissions from a list of capabilities.
    pub fn from_capabilities(capabilities: &[Capability]) -> Self {
        let mut perms = Self::default();
        for cap in capabilities {
            match cap {
                Capability::Push => perms.push = true,
                Capability::Pull => perms.pull = true,
                Capability::Browse => perms.browse = true,
                Capability::Status => perms.status = true,
            }
        }
        perms
    }

    /// Convert to a list of enabled capabilities.
    pub fn to_capabilities(&self) -> Vec<Capability> {
        let mut caps = Vec::new();
        if self.push {
            caps.push(Capability::Push);
        }
        if self.pull {
            caps.push(Capability::Pull);
        }
        if self.browse {
            caps.push(Capability::Browse);
        }
        if self.status {
            caps.push(Capability::Status);
        }
        caps
    }
}

/// A trusted peer that has completed the trust handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedPeer {
    /// Local UUID for this peer
    pub id: String,

    /// The Iroh endpoint ID (NodeId as string)
    pub endpoint_id: String,

    /// User-provided device name
    pub name: String,

    /// When this peer was added
    pub added_at: DateTime<Utc>,

    /// When this peer was last seen online
    #[serde(default)]
    pub last_seen: Option<DateTime<Utc>>,

    /// Permissions we have granted to this peer
    pub permissions_granted: Permissions,

    /// Permissions this peer has granted to us
    pub their_permissions: Permissions,

    /// Operating system info (optional)
    #[serde(default)]
    pub os: Option<String>,

    /// Software version (optional)
    #[serde(default)]
    pub version: Option<String>,

    /// User notes about this peer (optional)
    #[serde(default)]
    pub notes: Option<String>,
}

impl TrustedPeer {
    /// Create a new trusted peer from handshake data.
    pub fn new(
        endpoint_id: String,
        name: String,
        permissions_granted: Permissions,
        their_permissions: Permissions,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            endpoint_id,
            name,
            added_at: Utc::now(),
            last_seen: Some(Utc::now()),
            permissions_granted,
            their_permissions,
            os: None,
            version: None,
            notes: None,
        }
    }

    /// Update the last seen timestamp to now.
    pub fn touch(&mut self) {
        self.last_seen = Some(Utc::now());
    }
}

/// Storage container for trusted peers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeerStore {
    /// List of trusted peers
    peers: Vec<TrustedPeer>,
}

impl PeerStore {
    /// Load the peer store from the default location.
    pub fn load() -> Result<Self> {
        let peers_path = platform::peers_file_path();

        if peers_path.exists() {
            Self::load_from_file(&peers_path)
        } else {
            Ok(Self::default())
        }
    }

    /// Load the peer store from a specific file.
    fn load_from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let store: PeerStore = serde_json::from_str(&contents)?;
        Ok(store)
    }

    /// Save the peer store to the default location.
    pub fn save(&self) -> Result<()> {
        let peers_path = platform::peers_file_path();
        self.save_to_file(&peers_path)
    }

    /// Save the peer store to a specific file.
    fn save_to_file(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(self)?;
        std::fs::write(path, contents)?;

        Ok(())
    }

    /// Add a new trusted peer.
    pub fn add(&mut self, peer: TrustedPeer) -> Result<()> {
        // Check for duplicate endpoint_id
        if self.find_by_endpoint_id(&peer.endpoint_id).is_some() {
            return Err(Error::Peer(format!(
                "peer with endpoint_id {} already exists",
                peer.endpoint_id
            )));
        }

        self.peers.push(peer);
        self.save()
    }

    /// Remove a peer by local ID.
    pub fn remove(&mut self, id: &str) -> Result<()> {
        let original_len = self.peers.len();
        self.peers.retain(|p| p.id != id);

        if self.peers.len() == original_len {
            return Err(Error::Peer(format!("peer with id {} not found", id)));
        }

        self.save()
    }

    /// Remove a peer by endpoint ID.
    pub fn remove_by_endpoint_id(&mut self, endpoint_id: &str) -> Result<()> {
        let original_len = self.peers.len();
        self.peers.retain(|p| p.endpoint_id != endpoint_id);

        if self.peers.len() == original_len {
            return Err(Error::Peer(format!(
                "peer with endpoint_id {} not found",
                endpoint_id
            )));
        }

        self.save()
    }

    /// Find a peer by endpoint ID.
    pub fn find_by_endpoint_id(&self, endpoint_id: &str) -> Option<&TrustedPeer> {
        self.peers.iter().find(|p| p.endpoint_id == endpoint_id)
    }

    /// Find a peer by endpoint ID (mutable).
    pub fn find_by_endpoint_id_mut(&mut self, endpoint_id: &str) -> Option<&mut TrustedPeer> {
        self.peers.iter_mut().find(|p| p.endpoint_id == endpoint_id)
    }

    /// Find a peer by local ID.
    pub fn find_by_id(&self, id: &str) -> Option<&TrustedPeer> {
        self.peers.iter().find(|p| p.id == id)
    }

    /// Get all peers.
    pub fn list(&self) -> &[TrustedPeer] {
        &self.peers
    }

    /// Get the number of peers.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Update a peer's last seen timestamp.
    pub fn touch_peer(&mut self, endpoint_id: &str) -> Result<()> {
        if let Some(peer) = self.find_by_endpoint_id_mut(endpoint_id) {
            peer.touch();
            self.save()
        } else {
            Err(Error::Peer(format!(
                "peer with endpoint_id {} not found",
                endpoint_id
            )))
        }
    }

    /// Update a peer and save.
    pub fn update<F>(&mut self, id: &str, f: F) -> Result<()>
    where
        F: FnOnce(&mut TrustedPeer),
    {
        if let Some(peer) = self.peers.iter_mut().find(|p| p.id == id) {
            f(peer);
            self.save()
        } else {
            Err(Error::Peer(format!("peer with id {} not found", id)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_permissions_all() {
        let perms = Permissions::all();
        assert!(perms.push && perms.pull && perms.browse && perms.status);
    }

    #[test]
    fn test_permissions_from_capabilities() {
        let caps = vec![Capability::Push, Capability::Status];
        let perms = Permissions::from_capabilities(&caps);

        assert!(perms.push);
        assert!(!perms.pull);
        assert!(!perms.browse);
        assert!(perms.status);
    }

    #[test]
    fn test_trusted_peer_creation() {
        let peer = TrustedPeer::new(
            "abc123".to_string(),
            "Test Peer".to_string(),
            Permissions::all(),
            Permissions::all(),
        );

        assert!(!peer.id.is_empty());
        assert_eq!(peer.endpoint_id, "abc123");
        assert_eq!(peer.name, "Test Peer");
        assert!(peer.last_seen.is_some());
    }

    #[test]
    fn test_peer_store_crud() {
        let temp_dir = TempDir::new().unwrap();
        let peers_path = temp_dir.path().join("peers.json");

        let mut store = PeerStore::default();

        // Add a peer
        let peer = TrustedPeer::new(
            "endpoint1".to_string(),
            "Peer 1".to_string(),
            Permissions::all(),
            Permissions::all(),
        );
        let peer_id = peer.id.clone();
        store.add(peer).unwrap();
        store.save_to_file(&peers_path).unwrap();

        // Reload and verify
        let loaded = PeerStore::load_from_file(&peers_path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded.find_by_endpoint_id("endpoint1").is_some());

        // Remove peer
        let mut store = loaded;
        store.remove(&peer_id).unwrap();
        store.save_to_file(&peers_path).unwrap();

        let loaded = PeerStore::load_from_file(&peers_path).unwrap();
        assert_eq!(loaded.len(), 0);
    }

    #[test]
    fn test_duplicate_endpoint_id_rejected() {
        let mut store = PeerStore::default();

        let peer1 = TrustedPeer::new(
            "same_endpoint".to_string(),
            "Peer 1".to_string(),
            Permissions::all(),
            Permissions::all(),
        );
        store.peers.push(peer1); // Bypass save for test

        let peer2 = TrustedPeer::new(
            "same_endpoint".to_string(),
            "Peer 2".to_string(),
            Permissions::all(),
            Permissions::all(),
        );

        let result = store.add(peer2);
        assert!(result.is_err());
    }
}
