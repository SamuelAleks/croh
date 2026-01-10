//! Transfer history persistence.
//!
//! This module handles saving and loading completed transfer history
//! for viewing past transfers.

use crate::error::Result;
use crate::platform;
use crate::transfer::Transfer;
use std::path::{Path, PathBuf};

/// Maximum number of history entries to keep.
const MAX_HISTORY_ENTRIES: usize = 500;

/// Get the path to the transfer history file.
pub fn history_file_path() -> PathBuf {
    platform::data_dir().join("transfer_history.json")
}

/// Storage container for transfer history.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct TransferHistory {
    /// List of completed transfers (newest first)
    transfers: Vec<Transfer>,
}

impl TransferHistory {
    /// Load transfer history from the default location.
    pub fn load() -> Result<Self> {
        let path = history_file_path();

        if path.exists() {
            Self::load_from_file(&path)
        } else {
            Ok(Self::default())
        }
    }

    /// Load transfer history from a specific file.
    fn load_from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let history: TransferHistory = serde_json::from_str(&contents)?;
        Ok(history)
    }

    /// Save transfer history to the default location.
    pub fn save(&self) -> Result<()> {
        let path = history_file_path();
        self.save_to_file(&path)
    }

    /// Save transfer history to a specific file.
    fn save_to_file(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(self)?;
        std::fs::write(path, contents)?;

        Ok(())
    }

    /// Add a completed transfer to history.
    ///
    /// The transfer is inserted at the beginning (newest first).
    /// If the history exceeds MAX_HISTORY_ENTRIES, oldest entries are removed.
    pub fn add(&mut self, transfer: Transfer) {
        // Insert at beginning (newest first)
        self.transfers.insert(0, transfer);

        // Trim to max size
        if self.transfers.len() > MAX_HISTORY_ENTRIES {
            self.transfers.truncate(MAX_HISTORY_ENTRIES);
        }
    }

    /// Add a transfer and save immediately.
    pub fn add_and_save(&mut self, transfer: Transfer) -> Result<()> {
        self.add(transfer);
        self.save()
    }

    /// Get all transfers in history (newest first).
    pub fn list(&self) -> &[Transfer] {
        &self.transfers
    }

    /// Get the number of history entries.
    pub fn len(&self) -> usize {
        self.transfers.len()
    }

    /// Check if history is empty.
    pub fn is_empty(&self) -> bool {
        self.transfers.is_empty()
    }

    /// Clear all history.
    pub fn clear(&mut self) {
        self.transfers.clear();
    }

    /// Clear history and save.
    pub fn clear_and_save(&mut self) -> Result<()> {
        self.clear();
        self.save()
    }

    /// Get recent transfers (up to limit).
    pub fn recent(&self, limit: usize) -> &[Transfer] {
        let end = std::cmp::min(limit, self.transfers.len());
        &self.transfers[..end]
    }

    /// Remove a specific transfer by ID.
    pub fn remove(&mut self, transfer_id: &str) -> bool {
        let original_len = self.transfers.len();
        self.transfers.retain(|t| t.id.0 != transfer_id);
        self.transfers.len() < original_len
    }

    /// Remove a transfer by ID and save.
    pub fn remove_and_save(&mut self, transfer_id: &str) -> Result<bool> {
        let removed = self.remove(transfer_id);
        if removed {
            self.save()?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::{TransferStatus, TransferType};
    use tempfile::TempDir;

    fn make_test_transfer(name: &str) -> Transfer {
        Transfer {
            id: crate::transfer::TransferId::new(),
            transfer_type: TransferType::Send,
            status: TransferStatus::Completed,
            code: None,
            files: vec![name.to_string()],
            progress: 100.0,
            speed: "1 MB/s".to_string(),
            error: None,
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            total_size: 1024,
            transferred: 1024,
            peer_endpoint_id: None,
            peer_name: None,
            file_hashes: Vec::new(),
        }
    }

    #[test]
    fn test_add_transfer() {
        let mut history = TransferHistory::default();

        let transfer = make_test_transfer("test.txt");
        history.add(transfer);

        assert_eq!(history.len(), 1);
    }

    #[test]
    fn test_newest_first() {
        let mut history = TransferHistory::default();

        let t1 = make_test_transfer("first.txt");
        let t2 = make_test_transfer("second.txt");

        let t1_id = t1.id.0.clone();
        let t2_id = t2.id.0.clone();

        history.add(t1);
        history.add(t2);

        // Second should be first in list
        assert_eq!(history.list()[0].id.0, t2_id);
        assert_eq!(history.list()[1].id.0, t1_id);
    }

    #[test]
    fn test_max_entries() {
        let mut history = TransferHistory::default();

        // Add more than MAX_HISTORY_ENTRIES
        for i in 0..(MAX_HISTORY_ENTRIES + 50) {
            let transfer = make_test_transfer(&format!("file_{}.txt", i));
            history.add(transfer);
        }

        assert_eq!(history.len(), MAX_HISTORY_ENTRIES);
    }

    #[test]
    fn test_persistence() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("history.json");

        let mut history = TransferHistory::default();
        let transfer = make_test_transfer("test.txt");
        let transfer_id = transfer.id.0.clone();
        history.add(transfer);
        history.save_to_file(&path).unwrap();

        // Reload and verify
        let loaded = TransferHistory::load_from_file(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.list()[0].id.0, transfer_id);
    }

    #[test]
    fn test_clear() {
        let mut history = TransferHistory::default();

        history.add(make_test_transfer("a.txt"));
        history.add(make_test_transfer("b.txt"));
        assert_eq!(history.len(), 2);

        history.clear();
        assert!(history.is_empty());
    }

    #[test]
    fn test_remove() {
        let mut history = TransferHistory::default();

        let t1 = make_test_transfer("a.txt");
        let t2 = make_test_transfer("b.txt");
        let t1_id = t1.id.0.clone();

        history.add(t1);
        history.add(t2);
        assert_eq!(history.len(), 2);

        let removed = history.remove(&t1_id);
        assert!(removed);
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn test_recent() {
        let mut history = TransferHistory::default();

        for i in 0..10 {
            history.add(make_test_transfer(&format!("file_{}.txt", i)));
        }

        let recent = history.recent(3);
        assert_eq!(recent.len(), 3);
    }
}
