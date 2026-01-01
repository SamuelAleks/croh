//! Identity management for Iroh endpoints.
//!
//! Handles keypair generation, persistence, and loading for the Iroh networking layer.

use crate::error::{Error, Result};
use crate::platform;
use chrono::{DateTime, Utc};
use iroh::SecretKey;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Identity data that is persisted to disk.
/// The secret key is stored separately for security.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityData {
    /// The public endpoint ID (NodeId as hex string)
    pub endpoint_id: String,

    /// User-provided device name
    pub name: String,

    /// When this identity was created
    pub created_at: DateTime<Utc>,

    /// The secret key in hex format (for persistence)
    secret_key_hex: String,
}

/// Runtime identity with parsed secret key.
#[derive(Debug, Clone)]
pub struct Identity {
    /// The public endpoint ID (NodeId as hex string)
    pub endpoint_id: String,

    /// User-provided device name
    pub name: String,

    /// When this identity was created
    pub created_at: DateTime<Utc>,

    /// The secret key for signing and encryption
    secret_key: SecretKey,
}

impl Identity {
    /// Get the secret key for use with Iroh endpoints.
    pub fn secret_key(&self) -> &SecretKey {
        &self.secret_key
    }

    /// Load an existing identity or create a new one.
    ///
    /// If no identity exists, creates a new keypair and saves it.
    /// The device name defaults to the hostname if not provided.
    pub fn load_or_create() -> Result<Self> {
        let identity_path = platform::identity_file_path();

        if identity_path.exists() {
            Self::load_from_file(&identity_path)
        } else {
            let name = get_default_device_name();
            let identity = Self::generate(name)?;
            identity.save()?;
            Ok(identity)
        }
    }

    /// Load an existing identity or create with a custom name.
    pub fn load_or_create_with_name(name: String) -> Result<Self> {
        let identity_path = platform::identity_file_path();

        if identity_path.exists() {
            Self::load_from_file(&identity_path)
        } else {
            let identity = Self::generate(name)?;
            identity.save()?;
            Ok(identity)
        }
    }

    /// Generate a new identity with the given device name.
    pub fn generate(name: String) -> Result<Self> {
        let secret_key = SecretKey::generate(rand::rngs::OsRng);
        let public_key = secret_key.public();
        let endpoint_id = public_key.to_string();

        Ok(Self {
            endpoint_id,
            name,
            created_at: Utc::now(),
            secret_key,
        })
    }

    /// Load identity from a file.
    fn load_from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let data: IdentityData = serde_json::from_str(&contents)?;

        // Parse the secret key from hex
        let secret_key_bytes = hex::decode(&data.secret_key_hex)
            .map_err(|e| Error::Identity(format!("invalid secret key hex: {}", e)))?;

        let secret_key_array: [u8; 32] = secret_key_bytes
            .try_into()
            .map_err(|_| Error::Identity("secret key must be 32 bytes".to_string()))?;

        let secret_key = SecretKey::from_bytes(&secret_key_array);

        // Verify that the endpoint_id matches the public key
        let public_key = secret_key.public();
        let expected_id = public_key.to_string();
        if expected_id != data.endpoint_id {
            return Err(Error::Identity(
                "endpoint_id does not match secret key".to_string(),
            ));
        }

        Ok(Self {
            endpoint_id: data.endpoint_id,
            name: data.name,
            created_at: data.created_at,
            secret_key,
        })
    }

    /// Save identity to the default location.
    pub fn save(&self) -> Result<()> {
        let identity_path = platform::identity_file_path();
        self.save_to_file(&identity_path)
    }

    /// Save identity to a specific file.
    fn save_to_file(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let data = IdentityData {
            endpoint_id: self.endpoint_id.clone(),
            name: self.name.clone(),
            created_at: self.created_at,
            secret_key_hex: hex::encode(self.secret_key.to_bytes()),
        };

        let contents = serde_json::to_string_pretty(&data)?;
        std::fs::write(path, contents)?;

        Ok(())
    }

    /// Update the device name and save.
    pub fn set_name(&mut self, name: String) -> Result<()> {
        self.name = name;
        self.save()
    }

    /// Convert to PeerInfo for use in trust bundles.
    pub fn to_peer_info(&self) -> crate::trust::PeerInfo {
        crate::trust::PeerInfo {
            endpoint_id: self.endpoint_id.clone(),
            name: self.name.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// Get the default device name (hostname or fallback).
fn get_default_device_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "Unknown Device".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_identity() {
        let identity = Identity::generate("Test Device".to_string()).unwrap();
        assert!(!identity.endpoint_id.is_empty());
        assert_eq!(identity.name, "Test Device");
    }

    #[test]
    fn test_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let identity_path = temp_dir.path().join("identity.json");

        // Generate and save
        let original = Identity::generate("Test Device".to_string()).unwrap();
        original.save_to_file(&identity_path).unwrap();

        // Load and verify
        let loaded = Identity::load_from_file(&identity_path).unwrap();
        assert_eq!(loaded.endpoint_id, original.endpoint_id);
        assert_eq!(loaded.name, original.name);
    }

    #[test]
    fn test_secret_key_persistence() {
        let temp_dir = TempDir::new().unwrap();
        let identity_path = temp_dir.path().join("identity.json");

        // Generate and save
        let original = Identity::generate("Test Device".to_string()).unwrap();
        let original_key_bytes = original.secret_key.to_bytes();
        original.save_to_file(&identity_path).unwrap();

        // Load and verify secret key matches
        let loaded = Identity::load_from_file(&identity_path).unwrap();
        let loaded_key_bytes = loaded.secret_key.to_bytes();
        assert_eq!(original_key_bytes, loaded_key_bytes);
    }
}
