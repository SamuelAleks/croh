//! Transfer state management.

use crate::error::{Error, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Unique identifier for a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransferId(pub String);

impl TransferId {
    /// Generate a new random transfer ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for TransferId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TransferId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Type of transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferType {
    /// Sending files via croc.
    Send,
    /// Receiving files via croc.
    Receive,
    /// Push files to trusted peer via Iroh.
    IrohPush,
    /// Pull files from trusted peer via Iroh.
    IrohPull,
}

/// Status of a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferStatus {
    /// Transfer is pending (waiting to start).
    Pending,
    /// Transfer is running.
    Running,
    /// Transfer completed successfully.
    Completed,
    /// Transfer failed.
    Failed,
    /// Transfer was cancelled.
    Cancelled,
}

impl TransferStatus {
    /// Check if the transfer is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TransferStatus::Completed | TransferStatus::Failed | TransferStatus::Cancelled
        )
    }
}

/// A file transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transfer {
    /// Unique identifier.
    pub id: TransferId,

    /// Type of transfer (send or receive).
    pub transfer_type: TransferType,

    /// Current status.
    pub status: TransferStatus,

    /// Croc code (for sharing or receiving).
    pub code: Option<String>,

    /// Files being transferred.
    pub files: Vec<String>,

    /// Progress percentage (0.0 - 100.0).
    pub progress: f64,

    /// Transfer speed (human-readable).
    pub speed: String,

    /// Error message if failed.
    pub error: Option<String>,

    /// When the transfer started.
    pub started_at: DateTime<Utc>,

    /// When the transfer completed (if applicable).
    pub completed_at: Option<DateTime<Utc>>,

    /// Total size in bytes.
    pub total_size: u64,

    /// Bytes transferred so far.
    pub transferred: u64,

    /// Peer endpoint ID (for Iroh transfers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_endpoint_id: Option<String>,

    /// Peer name (for display, Iroh transfers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_name: Option<String>,

    /// File hashes (for Iroh transfers, BLAKE3 hex).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_hashes: Vec<String>,
}

impl Transfer {
    /// Create a new send transfer (croc).
    pub fn new_send(files: Vec<String>) -> Self {
        Self {
            id: TransferId::new(),
            transfer_type: TransferType::Send,
            status: TransferStatus::Pending,
            code: None,
            files,
            progress: 0.0,
            speed: String::new(),
            error: None,
            started_at: Utc::now(),
            completed_at: None,
            total_size: 0,
            transferred: 0,
            peer_endpoint_id: None,
            peer_name: None,
            file_hashes: Vec::new(),
        }
    }

    /// Create a new receive transfer (croc).
    pub fn new_receive(code: String) -> Self {
        Self {
            id: TransferId::new(),
            transfer_type: TransferType::Receive,
            status: TransferStatus::Pending,
            code: Some(code),
            files: Vec::new(),
            progress: 0.0,
            speed: String::new(),
            error: None,
            started_at: Utc::now(),
            completed_at: None,
            total_size: 0,
            transferred: 0,
            peer_endpoint_id: None,
            peer_name: None,
            file_hashes: Vec::new(),
        }
    }

    /// Create a new Iroh push transfer (send to trusted peer).
    pub fn new_iroh_push(files: Vec<String>, peer_endpoint_id: String, peer_name: String) -> Self {
        Self {
            id: TransferId::new(),
            transfer_type: TransferType::IrohPush,
            status: TransferStatus::Pending,
            code: None,
            files,
            progress: 0.0,
            speed: String::new(),
            error: None,
            started_at: Utc::now(),
            completed_at: None,
            total_size: 0,
            transferred: 0,
            peer_endpoint_id: Some(peer_endpoint_id),
            peer_name: Some(peer_name),
            file_hashes: Vec::new(),
        }
    }

    /// Create a new Iroh pull transfer (receive from trusted peer).
    pub fn new_iroh_pull(files: Vec<String>, peer_endpoint_id: String, peer_name: String) -> Self {
        Self {
            id: TransferId::new(),
            transfer_type: TransferType::IrohPull,
            status: TransferStatus::Pending,
            code: None,
            files,
            progress: 0.0,
            speed: String::new(),
            error: None,
            started_at: Utc::now(),
            completed_at: None,
            total_size: 0,
            transferred: 0,
            peer_endpoint_id: Some(peer_endpoint_id),
            peer_name: Some(peer_name),
            file_hashes: Vec::new(),
        }
    }

    /// Check if this is an Iroh-based transfer.
    pub fn is_iroh_transfer(&self) -> bool {
        matches!(
            self.transfer_type,
            TransferType::IrohPush | TransferType::IrohPull
        )
    }
}

/// Manages active transfers.
#[derive(Debug, Clone)]
pub struct TransferManager {
    transfers: Arc<RwLock<HashMap<TransferId, Transfer>>>,
}

impl Default for TransferManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TransferManager {
    /// Create a new transfer manager.
    pub fn new() -> Self {
        Self {
            transfers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add a new transfer.
    pub async fn add(&self, transfer: Transfer) -> Result<TransferId> {
        let id = transfer.id.clone();
        let mut transfers = self.transfers.write().await;

        if transfers.contains_key(&id) {
            return Err(Error::TransferExists(id.to_string()));
        }

        transfers.insert(id.clone(), transfer);
        Ok(id)
    }

    /// Get a transfer by ID.
    pub async fn get(&self, id: &TransferId) -> Option<Transfer> {
        let transfers = self.transfers.read().await;
        transfers.get(id).cloned()
    }

    /// Update a transfer.
    pub async fn update<F>(&self, id: &TransferId, f: F) -> Result<()>
    where
        F: FnOnce(&mut Transfer),
    {
        let mut transfers = self.transfers.write().await;
        let transfer = transfers
            .get_mut(id)
            .ok_or_else(|| Error::TransferNotFound(id.to_string()))?;

        f(transfer);
        Ok(())
    }

    /// Remove a transfer.
    pub async fn remove(&self, id: &TransferId) -> Option<Transfer> {
        let mut transfers = self.transfers.write().await;
        transfers.remove(id)
    }

    /// Get all transfers.
    pub async fn list(&self) -> Vec<Transfer> {
        let transfers = self.transfers.read().await;
        transfers.values().cloned().collect()
    }

    /// Get all active (non-terminal) transfers.
    pub async fn list_active(&self) -> Vec<Transfer> {
        let transfers = self.transfers.read().await;
        transfers
            .values()
            .filter(|t| !t.status.is_terminal())
            .cloned()
            .collect()
    }

    /// Clean up old completed/failed/cancelled transfers.
    pub async fn cleanup_expired(&self, max_age: std::time::Duration) {
        let cutoff = Utc::now() - chrono::Duration::from_std(max_age).unwrap_or_default();

        let mut transfers = self.transfers.write().await;
        transfers.retain(|_, t| {
            if t.status.is_terminal() {
                t.completed_at.is_none_or(|completed| completed > cutoff)
            } else {
                true
            }
        });
    }
}
