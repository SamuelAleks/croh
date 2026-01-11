//! Trusted peer management and storage.
//!
//! This module handles the persistence and management of trusted peers
//! that have been established via the trust handshake protocol.
//!
//! ## Trust Tiers
//!
//! Peers can be either **Trusted** (permanent) or **Guest** (temporary):
//!
//! - **Trusted peers** have no expiry and can join networks and be introduced
//!   to other peers.
//! - **Guest peers** have a time-limited access that auto-expires. They cannot
//!   join networks or be introduced to other peers, but can request extensions
//!   or promotion to trusted status.

use crate::error::{Error, Result};
use crate::iroh::PeerAddress;
use crate::platform;
use crate::trust::Capability;
use chrono::{DateTime, Duration, Utc};
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
    /// Can send/receive chat messages with this peer
    #[serde(default = "default_chat_permission")]
    pub chat: bool,
}

/// Default value for chat permission (true for backwards compatibility).
fn default_chat_permission() -> bool {
    true
}

impl Permissions {
    /// Create permissions with all capabilities enabled.
    pub fn all() -> Self {
        Self {
            push: true,
            pull: true,
            browse: true,
            status: true,
            chat: true,
        }
    }

    /// Alias for `all()` - create permissions with all capabilities enabled.
    pub fn full() -> Self {
        Self::all()
    }

    /// Create default guest permissions (push, status, and chat).
    pub fn guest_default() -> Self {
        Self {
            push: true,
            pull: false,
            browse: false,
            status: true,
            chat: true,
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
                Capability::Chat => perms.chat = true,
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
        if self.chat {
            caps.push(Capability::Chat);
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

    /// How to reach this peer (replaces legacy relay_url).
    /// Supports multiple transport types for future Tor integration.
    #[serde(default)]
    pub address: PeerAddress,

    // === Guest peer fields ===

    /// Is this a temporary guest peer?
    /// Guest peers have limited permissions and auto-expire.
    #[serde(default)]
    pub is_guest: bool,

    /// When guest access expires (None = permanent trusted peer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,

    /// How many times has this guest been extended?
    #[serde(default)]
    pub extension_count: u32,

    /// Networks this peer belongs to (network IDs).
    /// Guest peers cannot join networks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<String>,

    /// Who introduced us to this peer? (endpoint_id for audit trail).
    /// Used for security review if an introducer is compromised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introduced_by: Option<String>,

    /// Has this peer requested promotion from guest to trusted?
    #[serde(default)]
    pub promotion_pending: bool,

    // === Legacy field for backwards compatibility ===

    /// Legacy relay URL field - migrated to `address` on load.
    /// This field is kept for backwards compatibility with existing peers.json files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay_url: Option<String>,
}

impl TrustedPeer {
    /// Create a new trusted peer from handshake data.
    pub fn new(
        endpoint_id: String,
        name: String,
        permissions_granted: Permissions,
        their_permissions: Permissions,
    ) -> Self {
        Self::new_with_address(
            endpoint_id,
            name,
            permissions_granted,
            their_permissions,
            PeerAddress::default(),
        )
    }

    /// Create a new trusted peer from handshake data with relay URL.
    /// This is a convenience method that wraps the relay URL in a PeerAddress.
    pub fn new_with_relay(
        endpoint_id: String,
        name: String,
        permissions_granted: Permissions,
        their_permissions: Permissions,
        relay_url: Option<String>,
    ) -> Self {
        Self::new_with_address(
            endpoint_id,
            name,
            permissions_granted,
            their_permissions,
            PeerAddress::iroh(relay_url),
        )
    }

    /// Create a new trusted peer from handshake data with a full address.
    pub fn new_with_address(
        endpoint_id: String,
        name: String,
        permissions_granted: Permissions,
        their_permissions: Permissions,
        address: PeerAddress,
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
            address,
            is_guest: false,
            expires_at: None,
            extension_count: 0,
            networks: Vec::new(),
            introduced_by: None,
            promotion_pending: false,
            relay_url: None, // Legacy field, not used for new peers
        }
    }

    /// Create a new guest peer with temporary access.
    ///
    /// Guest peers have limited permissions and auto-expire after the specified duration.
    /// They cannot join networks or be introduced to other peers.
    pub fn new_guest(
        endpoint_id: String,
        name: String,
        permissions_granted: Permissions,
        their_permissions: Permissions,
        address: PeerAddress,
        duration: Duration,
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
            address,
            is_guest: true,
            expires_at: Some(Utc::now() + duration),
            extension_count: 0,
            networks: Vec::new(),
            introduced_by: None,
            promotion_pending: false,
            relay_url: None,
        }
    }

    /// Create a guest peer with default 24-hour duration and guest permissions.
    pub fn new_guest_default(
        endpoint_id: String,
        name: String,
        address: PeerAddress,
    ) -> Self {
        Self::new_guest(
            endpoint_id,
            name,
            Permissions::guest_default(),
            Permissions::guest_default(),
            address,
            Duration::hours(24),
        )
    }

    /// Update the last seen timestamp to now.
    pub fn touch(&mut self) {
        self.last_seen = Some(Utc::now());
    }

    /// Check if this peer's access has expired.
    pub fn is_expired(&self) -> bool {
        self.expires_at.map(|e| Utc::now() > e).unwrap_or(false)
    }

    /// Check if this peer can be shared via introductions.
    /// Guest peers and expired peers cannot be introduced to others.
    pub fn is_shareable(&self) -> bool {
        !self.is_guest && !self.is_expired()
    }

    /// Get the remaining time until expiry, if any.
    pub fn time_remaining(&self) -> Option<Duration> {
        self.expires_at.and_then(|e| {
            let now = Utc::now();
            if e > now {
                Some(e - now)
            } else {
                None
            }
        })
    }

    /// Extend guest access by the specified duration.
    ///
    /// Returns the new expiry time, or None if this is not a guest peer.
    pub fn extend(&mut self, duration: Duration) -> Option<DateTime<Utc>> {
        if !self.is_guest {
            return None;
        }

        let new_expiry = match self.expires_at {
            Some(current) if current > Utc::now() => current + duration,
            _ => Utc::now() + duration,
        };

        self.expires_at = Some(new_expiry);
        self.extension_count += 1;

        Some(new_expiry)
    }

    /// Promote a guest peer to trusted status.
    ///
    /// This removes the expiry and guest status, making the peer permanent.
    /// Returns false if this was not a guest peer.
    pub fn promote_to_trusted(&mut self) -> bool {
        if !self.is_guest {
            return false;
        }

        self.is_guest = false;
        self.expires_at = None;
        self.extension_count = 0;
        self.promotion_pending = false;

        true
    }

    /// Get the relay URL for this peer (convenience method).
    /// This is a shortcut for `self.address.relay_url()`.
    pub fn relay_url(&self) -> Option<&str> {
        self.address.relay_url()
    }

    /// Migrate from legacy relay_url field to address field.
    /// Called automatically when loading from JSON.
    fn migrate_legacy_fields(&mut self) {
        if let Some(relay_url) = self.relay_url.take() {
            if self.address.is_empty() {
                self.address = PeerAddress::iroh(Some(relay_url));
            }
        }
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
        let mut store: PeerStore = serde_json::from_str(&contents)?;

        // Migrate legacy fields (relay_url -> address)
        for peer in &mut store.peers {
            peer.migrate_legacy_fields();
        }

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

    /// Add a new trusted peer, or update if it already exists.
    /// Returns true if the peer was updated, false if newly added.
    pub fn add_or_update(&mut self, peer: TrustedPeer) -> Result<bool> {
        if let Some(existing) = self.find_by_endpoint_id_mut(&peer.endpoint_id) {
            // Update existing peer's fields
            existing.name = peer.name;
            existing.last_seen = peer.last_seen;
            existing.permissions_granted = peer.permissions_granted;
            existing.their_permissions = peer.their_permissions;
            existing.address.merge(&peer.address);
            if peer.os.is_some() {
                existing.os = peer.os;
            }
            if peer.version.is_some() {
                existing.version = peer.version;
            }
            self.save()?;
            Ok(true)
        } else {
            self.peers.push(peer);
            self.save()?;
            Ok(false)
        }
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

    // === Guest peer management ===

    /// Get all guest peers.
    pub fn list_guests(&self) -> Vec<&TrustedPeer> {
        self.peers.iter().filter(|p| p.is_guest).collect()
    }

    /// Get all expired guest peers.
    pub fn list_expired(&self) -> Vec<&TrustedPeer> {
        self.peers.iter().filter(|p| p.is_expired()).collect()
    }

    /// Remove all expired guest peers.
    /// Returns the number of peers removed.
    pub fn cleanup_expired(&mut self) -> Result<usize> {
        let original_len = self.peers.len();
        self.peers.retain(|p| !p.is_expired());
        let removed = original_len - self.peers.len();

        if removed > 0 {
            self.save()?;
        }

        Ok(removed)
    }

    /// Extend a guest peer's access.
    /// Returns the new expiry time, or None if the peer is not a guest.
    pub fn extend_guest(&mut self, id: &str, duration: Duration) -> Result<Option<DateTime<Utc>>> {
        if let Some(peer) = self.peers.iter_mut().find(|p| p.id == id) {
            if let Some(new_expiry) = peer.extend(duration) {
                self.save()?;
                Ok(Some(new_expiry))
            } else {
                Ok(None) // Not a guest peer
            }
        } else {
            Err(Error::Peer(format!("peer with id {} not found", id)))
        }
    }

    /// Promote a guest peer to trusted status.
    /// Returns true if the peer was promoted, false if it wasn't a guest.
    pub fn promote_guest(&mut self, id: &str) -> Result<bool> {
        if let Some(peer) = self.peers.iter_mut().find(|p| p.id == id) {
            if peer.promote_to_trusted() {
                self.save()?;
                Ok(true)
            } else {
                Ok(false) // Not a guest peer
            }
        } else {
            Err(Error::Peer(format!("peer with id {} not found", id)))
        }
    }

    /// Set the promotion_pending flag for a guest peer.
    pub fn set_promotion_pending(&mut self, id: &str, pending: bool) -> Result<()> {
        if let Some(peer) = self.peers.iter_mut().find(|p| p.id == id) {
            peer.promotion_pending = pending;
            self.save()
        } else {
            Err(Error::Peer(format!("peer with id {} not found", id)))
        }
    }

    /// Get all peers with pending promotion requests.
    pub fn list_promotion_pending(&self) -> Vec<&TrustedPeer> {
        self.peers.iter().filter(|p| p.promotion_pending).collect()
    }

    /// Check if a peer can be added to a network.
    /// Guests and expired peers cannot join networks.
    pub fn can_join_network(&self, id: &str) -> bool {
        self.peers
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.is_shareable())
            .unwrap_or(false)
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
    fn test_permissions_full_alias() {
        let all = Permissions::all();
        let full = Permissions::full();
        assert_eq!(all, full);
    }

    #[test]
    fn test_permissions_guest_default() {
        let perms = Permissions::guest_default();
        assert!(perms.push);
        assert!(!perms.pull);
        assert!(!perms.browse);
        assert!(perms.status);
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
        assert!(!peer.is_guest);
        assert!(peer.expires_at.is_none());
        assert!(peer.is_shareable());
    }

    #[test]
    fn test_guest_peer_creation() {
        let peer = TrustedPeer::new_guest_default(
            "guest123".to_string(),
            "Guest Peer".to_string(),
            PeerAddress::default(),
        );

        assert!(peer.is_guest);
        assert!(peer.expires_at.is_some());
        assert!(!peer.is_expired());
        assert!(!peer.is_shareable()); // Guests are not shareable
        assert!(peer.time_remaining().is_some());
    }

    #[test]
    fn test_guest_peer_expiry() {
        let mut peer = TrustedPeer::new_guest(
            "guest123".to_string(),
            "Guest Peer".to_string(),
            Permissions::guest_default(),
            Permissions::guest_default(),
            PeerAddress::default(),
            Duration::seconds(-1), // Already expired
        );

        assert!(peer.is_expired());
        assert!(!peer.is_shareable());
        assert!(peer.time_remaining().is_none());

        // Extending should work from now
        let new_expiry = peer.extend(Duration::hours(1));
        assert!(new_expiry.is_some());
        assert!(!peer.is_expired());
        assert_eq!(peer.extension_count, 1);
    }

    #[test]
    fn test_guest_promotion() {
        let mut peer = TrustedPeer::new_guest_default(
            "guest123".to_string(),
            "Guest Peer".to_string(),
            PeerAddress::default(),
        );

        assert!(peer.is_guest);
        assert!(peer.expires_at.is_some());

        let promoted = peer.promote_to_trusted();
        assert!(promoted);
        assert!(!peer.is_guest);
        assert!(peer.expires_at.is_none());
        assert!(peer.is_shareable());

        // Can't promote again
        let promoted_again = peer.promote_to_trusted();
        assert!(!promoted_again);
    }

    #[test]
    fn test_peer_address_integration() {
        let peer = TrustedPeer::new_with_relay(
            "abc123".to_string(),
            "Test Peer".to_string(),
            Permissions::all(),
            Permissions::all(),
            Some("https://relay.example.com".to_string()),
        );

        assert_eq!(peer.relay_url(), Some("https://relay.example.com"));
        assert!(!peer.address.is_empty());
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

    #[test]
    fn test_peer_store_guest_management() {
        let mut store = PeerStore::default();

        // Add a regular peer
        let regular = TrustedPeer::new(
            "regular".to_string(),
            "Regular Peer".to_string(),
            Permissions::all(),
            Permissions::all(),
        );
        store.peers.push(regular);

        // Add a guest peer
        let guest = TrustedPeer::new_guest_default(
            "guest".to_string(),
            "Guest Peer".to_string(),
            PeerAddress::default(),
        );
        let guest_id = guest.id.clone();
        store.peers.push(guest);

        // Add an expired guest
        let expired = TrustedPeer::new_guest(
            "expired".to_string(),
            "Expired Guest".to_string(),
            Permissions::guest_default(),
            Permissions::guest_default(),
            PeerAddress::default(),
            Duration::seconds(-1),
        );
        store.peers.push(expired);

        assert_eq!(store.len(), 3);
        assert_eq!(store.list_guests().len(), 2);
        assert_eq!(store.list_expired().len(), 1);

        // Cleanup expired
        let removed = store.cleanup_expired().unwrap();
        assert_eq!(removed, 1);
        assert_eq!(store.len(), 2);

        // Check network eligibility
        assert!(store.can_join_network(&store.peers[0].id)); // Regular peer
        assert!(!store.can_join_network(&guest_id)); // Guest peer
    }

    #[test]
    fn test_legacy_relay_url_migration() {
        // Simulate loading a peer with old relay_url format
        let json = r#"{
            "peers": [{
                "id": "test-id",
                "endpoint_id": "abc123",
                "name": "Legacy Peer",
                "added_at": "2024-01-01T00:00:00Z",
                "permissions_granted": {"push": true, "pull": true, "browse": true, "status": true},
                "their_permissions": {"push": true, "pull": true, "browse": true, "status": true},
                "relay_url": "https://old-relay.example.com"
            }]
        }"#;

        let mut store: PeerStore = serde_json::from_str(json).unwrap();

        // Migrate legacy fields
        for peer in &mut store.peers {
            peer.migrate_legacy_fields();
        }

        let peer = &store.peers[0];
        assert_eq!(peer.relay_url(), Some("https://old-relay.example.com"));
        assert!(matches!(peer.address, PeerAddress::Iroh { .. }));
    }
}
