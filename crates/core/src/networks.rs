//! Peer network management and storage.
//!
//! Networks are groups of trusted peers that can share introductions.
//! Only trusted (non-guest) peers can join networks.
//!
//! ## Introduction Flow
//!
//! When peer A wants to introduce peer B to peer C in a network:
//! 1. A sends IntroductionOffer to B with C's info
//! 2. B accepts the offer
//! 3. A sends IntroductionRequest to C with B's info
//! 4. C accepts the request
//! 5. A forwards C's connection details to B
//! 6. B connects directly to C and completes a fresh trust handshake

use crate::error::{Error, Result};
use crate::platform;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Settings for a peer network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkSettings {
    /// Whether members can introduce new peers to the network.
    #[serde(default = "default_true")]
    pub allow_introductions: bool,

    /// Whether to auto-accept introductions from network members.
    /// If false, each introduction requires manual approval.
    #[serde(default)]
    pub auto_accept_introductions: bool,

    /// Maximum network size (0 = unlimited).
    #[serde(default)]
    pub max_members: u32,

    /// Whether to share the member list with other members.
    #[serde(default = "default_true")]
    pub share_member_list: bool,
}

fn default_true() -> bool {
    true
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            allow_introductions: true,
            auto_accept_introductions: false,
            max_members: 0, // Unlimited
            share_member_list: true,
        }
    }
}

/// A network of trusted peers that can share introductions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerNetwork {
    /// Unique identifier for this network.
    pub id: String,

    /// Human-readable name for the network.
    pub name: String,

    /// Optional description of the network's purpose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// When the network was created.
    pub created_at: DateTime<Utc>,

    /// Member peer IDs (must be trusted, not guest).
    /// These are local peer IDs from the PeerStore.
    #[serde(default)]
    pub members: Vec<String>,

    /// Network settings.
    #[serde(default)]
    pub settings: NetworkSettings,

    /// The peer ID of the network owner (creator).
    /// The owner can modify network settings and remove members.
    /// This is "self" for networks we created.
    pub owner_id: String,
}

impl PeerNetwork {
    /// Create a new network owned by the local user.
    pub fn new(name: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            description: None,
            created_at: Utc::now(),
            members: Vec::new(),
            settings: NetworkSettings::default(),
            owner_id: "self".to_string(),
        }
    }

    /// Create a new network with a description.
    pub fn new_with_description(name: String, description: String) -> Self {
        let mut network = Self::new(name);
        network.description = Some(description);
        network
    }

    /// Check if the local user owns this network.
    pub fn is_owner(&self) -> bool {
        self.owner_id == "self"
    }

    /// Add a member to the network.
    /// Returns an error if the member is already in the network or max size reached.
    pub fn add_member(&mut self, peer_id: &str) -> Result<()> {
        // Check if already a member
        if self.members.contains(&peer_id.to_string()) {
            return Err(Error::Network(format!(
                "Peer {} is already a member of network {}",
                peer_id, self.name
            )));
        }

        // Check max size
        if self.settings.max_members > 0 && self.members.len() >= self.settings.max_members as usize
        {
            return Err(Error::Network(format!(
                "Network {} has reached maximum size of {}",
                self.name, self.settings.max_members
            )));
        }

        self.members.push(peer_id.to_string());
        Ok(())
    }

    /// Remove a member from the network.
    pub fn remove_member(&mut self, peer_id: &str) -> bool {
        let original_len = self.members.len();
        self.members.retain(|id| id != peer_id);
        self.members.len() < original_len
    }

    /// Check if a peer is a member of this network.
    pub fn has_member(&self, peer_id: &str) -> bool {
        self.members.iter().any(|id| id == peer_id)
    }

    /// Get the number of members.
    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    /// Check if introductions are allowed.
    pub fn can_introduce(&self) -> bool {
        self.settings.allow_introductions
    }
}

/// Storage container for peer networks.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetworkStore {
    /// List of networks.
    networks: Vec<PeerNetwork>,
}

impl NetworkStore {
    /// Load the network store from the default location.
    pub fn load() -> Result<Self> {
        let networks_path = platform::networks_file_path();

        if networks_path.exists() {
            Self::load_from_file(&networks_path)
        } else {
            Ok(Self::default())
        }
    }

    /// Load the network store from a specific file.
    fn load_from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let store: NetworkStore = serde_json::from_str(&contents)?;
        Ok(store)
    }

    /// Save the network store to the default location.
    pub fn save(&self) -> Result<()> {
        let networks_path = platform::networks_file_path();
        self.save_to_file(&networks_path)
    }

    /// Save the network store to a specific file.
    fn save_to_file(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(self)?;
        std::fs::write(path, contents)?;

        Ok(())
    }

    /// Create a new network.
    pub fn create(&mut self, name: String) -> Result<&PeerNetwork> {
        // Check for duplicate name
        if self.find_by_name(&name).is_some() {
            return Err(Error::Network(format!(
                "Network with name '{}' already exists",
                name
            )));
        }

        let network = PeerNetwork::new(name);
        self.networks.push(network);
        self.save()?;

        Ok(self.networks.last().unwrap())
    }

    /// Create a new network with description.
    pub fn create_with_description(
        &mut self,
        name: String,
        description: String,
    ) -> Result<&PeerNetwork> {
        // Check for duplicate name
        if self.find_by_name(&name).is_some() {
            return Err(Error::Network(format!(
                "Network with name '{}' already exists",
                name
            )));
        }

        let network = PeerNetwork::new_with_description(name, description);
        self.networks.push(network);
        self.save()?;

        Ok(self.networks.last().unwrap())
    }

    /// Delete a network by ID.
    pub fn delete(&mut self, id: &str) -> Result<()> {
        let original_len = self.networks.len();
        self.networks.retain(|n| n.id != id);

        if self.networks.len() == original_len {
            return Err(Error::Network(format!("Network with id {} not found", id)));
        }

        self.save()
    }

    /// Find a network by ID.
    pub fn find_by_id(&self, id: &str) -> Option<&PeerNetwork> {
        self.networks.iter().find(|n| n.id == id)
    }

    /// Find a network by ID (mutable).
    pub fn find_by_id_mut(&mut self, id: &str) -> Option<&mut PeerNetwork> {
        self.networks.iter_mut().find(|n| n.id == id)
    }

    /// Find a network by name.
    pub fn find_by_name(&self, name: &str) -> Option<&PeerNetwork> {
        self.networks.iter().find(|n| n.name == name)
    }

    /// Get all networks.
    pub fn list(&self) -> &[PeerNetwork] {
        &self.networks
    }

    /// Get all networks a peer belongs to.
    pub fn networks_for_peer(&self, peer_id: &str) -> Vec<&PeerNetwork> {
        self.networks
            .iter()
            .filter(|n| n.has_member(peer_id))
            .collect()
    }

    /// Add a member to a network.
    pub fn add_member(&mut self, network_id: &str, peer_id: &str) -> Result<()> {
        let network = self
            .find_by_id_mut(network_id)
            .ok_or_else(|| Error::Network(format!("Network {} not found", network_id)))?;

        network.add_member(peer_id)?;
        self.save()
    }

    /// Remove a member from a network.
    pub fn remove_member(&mut self, network_id: &str, peer_id: &str) -> Result<bool> {
        let network = self
            .find_by_id_mut(network_id)
            .ok_or_else(|| Error::Network(format!("Network {} not found", network_id)))?;

        let removed = network.remove_member(peer_id);
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Update network settings.
    pub fn update_settings(&mut self, network_id: &str, settings: NetworkSettings) -> Result<()> {
        let network = self
            .find_by_id_mut(network_id)
            .ok_or_else(|| Error::Network(format!("Network {} not found", network_id)))?;

        network.settings = settings;
        self.save()
    }

    /// Rename a network.
    pub fn rename(&mut self, network_id: &str, new_name: String) -> Result<()> {
        // Check for duplicate name
        if self
            .networks
            .iter()
            .any(|n| n.name == new_name && n.id != network_id)
        {
            return Err(Error::Network(format!(
                "Network with name '{}' already exists",
                new_name
            )));
        }

        let network = self
            .find_by_id_mut(network_id)
            .ok_or_else(|| Error::Network(format!("Network {} not found", network_id)))?;

        network.name = new_name;
        self.save()
    }

    /// Get the number of networks.
    pub fn len(&self) -> usize {
        self.networks.len()
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.networks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_network_creation() {
        let network = PeerNetwork::new("Friends".to_string());
        assert_eq!(network.name, "Friends");
        assert!(network.is_owner());
        assert!(network.members.is_empty());
        assert!(network.can_introduce());
    }

    #[test]
    fn test_network_with_description() {
        let network =
            PeerNetwork::new_with_description("Work".to_string(), "Colleagues at work".to_string());
        assert_eq!(network.name, "Work");
        assert_eq!(network.description, Some("Colleagues at work".to_string()));
    }

    #[test]
    fn test_add_remove_members() {
        let mut network = PeerNetwork::new("Test".to_string());

        // Add member
        assert!(network.add_member("peer1").is_ok());
        assert!(network.has_member("peer1"));
        assert_eq!(network.member_count(), 1);

        // Duplicate add should fail
        assert!(network.add_member("peer1").is_err());

        // Add another
        assert!(network.add_member("peer2").is_ok());
        assert_eq!(network.member_count(), 2);

        // Remove
        assert!(network.remove_member("peer1"));
        assert!(!network.has_member("peer1"));
        assert_eq!(network.member_count(), 1);

        // Remove non-existent
        assert!(!network.remove_member("peer3"));
    }

    #[test]
    fn test_max_members() {
        let mut network = PeerNetwork::new("Limited".to_string());
        network.settings.max_members = 2;

        assert!(network.add_member("peer1").is_ok());
        assert!(network.add_member("peer2").is_ok());
        assert!(network.add_member("peer3").is_err()); // Should fail
    }

    #[test]
    fn test_network_store_crud() {
        let temp_dir = TempDir::new().unwrap();
        let networks_path = temp_dir.path().join("networks.json");

        let mut store = NetworkStore::default();

        // Create network
        store.create("Friends".to_string()).unwrap();
        store.save_to_file(&networks_path).unwrap();

        // Reload and verify
        let loaded = NetworkStore::load_from_file(&networks_path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded.find_by_name("Friends").is_some());

        // Add member
        let mut store = loaded;
        let network_id = store.find_by_name("Friends").unwrap().id.clone();
        store.add_member(&network_id, "peer1").unwrap();
        store.save_to_file(&networks_path).unwrap();

        // Verify member
        let loaded = NetworkStore::load_from_file(&networks_path).unwrap();
        let network = loaded.find_by_id(&network_id).unwrap();
        assert!(network.has_member("peer1"));

        // Delete network
        let mut store = loaded;
        store.delete(&network_id).unwrap();
        store.save_to_file(&networks_path).unwrap();

        let loaded = NetworkStore::load_from_file(&networks_path).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_duplicate_network_name() {
        let mut store = NetworkStore::default();

        store.create("Friends".to_string()).unwrap();
        assert!(store.create("Friends".to_string()).is_err());
    }

    #[test]
    fn test_networks_for_peer() {
        let mut store = NetworkStore::default();

        store.create("Network1".to_string()).unwrap();
        store.create("Network2".to_string()).unwrap();
        store.create("Network3".to_string()).unwrap();

        let id1 = store.find_by_name("Network1").unwrap().id.clone();
        let id2 = store.find_by_name("Network2").unwrap().id.clone();

        store.add_member(&id1, "peer1").unwrap();
        store.add_member(&id2, "peer1").unwrap();

        let networks = store.networks_for_peer("peer1");
        assert_eq!(networks.len(), 2);
    }
}
