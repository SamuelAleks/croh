//! Blob-based file transfer using iroh-blobs.
//!
//! This module provides:
//! - BlobStore wrapper for managing local blobs
//! - File hashing and verification with BLAKE3
//! - Integration with iroh-blobs protocol

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
use tracing::{debug, info};

/// Hash a file and return its BLAKE3 hash as hex string.
pub async fn hash_file(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| Error::Io(e.to_string()))?;

    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 64 * 1024]; // 64KB chunks

    loop {
        let bytes_read = file
            .read(&mut buffer)
            .await
            .map_err(|e| Error::Io(e.to_string()))?;

        if bytes_read == 0 {
            break;
        }

        hasher.update(&buffer[..bytes_read]);
    }

    Ok(hasher.finalize().to_hex().to_string())
}

/// Verify a file's BLAKE3 hash matches the expected value.
pub async fn verify_file_hash(path: &Path, expected_hash: &str) -> Result<bool> {
    let actual_hash = hash_file(path).await?;
    Ok(actual_hash == expected_hash)
}

/// Get file metadata (size) for a path.
pub async fn get_file_size(path: &Path) -> Result<u64> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|e| Error::Io(e.to_string()))?;
    Ok(metadata.len())
}

/// Blob store for managing file transfers.
///
/// Currently uses a simple approach of reading/writing files directly.
/// Can be extended to use iroh-blobs' persistent storage in the future.
pub struct BlobStore {
    /// Directory for storing blob data
    data_dir: PathBuf,
}

impl BlobStore {
    /// Create a new blob store.
    pub fn new(data_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&data_dir).map_err(|e| Error::Io(e.to_string()))?;
        info!("BlobStore initialized at {:?}", data_dir);
        Ok(Self { data_dir })
    }

    /// Get the data directory.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Read a file's contents.
    pub async fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        tokio::fs::read(path)
            .await
            .map_err(|e| Error::Io(format!("failed to read file {:?}: {}", path, e)))
    }

    /// Write data to a file in the data directory.
    pub async fn write_file(&self, name: &str, data: &[u8]) -> Result<PathBuf> {
        let dest = self.data_dir.join(name);

        // Ensure parent directory exists
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::Io(e.to_string()))?;
        }

        tokio::fs::write(&dest, data)
            .await
            .map_err(|e| Error::Io(format!("failed to write file {:?}: {}", dest, e)))?;

        debug!("Wrote {} bytes to {:?}", data.len(), dest);
        Ok(dest)
    }

    /// Copy a file to a destination, returning the new path.
    pub async fn copy_file(&self, src: &Path, dest_name: &str) -> Result<PathBuf> {
        let dest = self.data_dir.join(dest_name);

        // Ensure parent directory exists
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::Io(e.to_string()))?;
        }

        tokio::fs::copy(src, &dest)
            .await
            .map_err(|e| Error::Io(format!("failed to copy {:?} to {:?}: {}", src, dest, e)))?;

        debug!("Copied {:?} to {:?}", src, dest);
        Ok(dest)
    }

    /// Get a unique filename in the data directory.
    pub fn get_unique_path(&self, name: &str) -> PathBuf {
        let base = self.data_dir.join(name);
        if !base.exists() {
            return base;
        }

        // Add suffix to make unique
        let stem = base
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let ext = base.extension().and_then(|s| s.to_str()).unwrap_or("");

        for i in 1..1000 {
            let new_name = if ext.is_empty() {
                format!("{}_{}", stem, i)
            } else {
                format!("{}_{}.{}", stem, i, ext)
            };
            let new_path = self.data_dir.join(new_name);
            if !new_path.exists() {
                return new_path;
            }
        }

        // Fallback: use UUID
        let uuid = uuid::Uuid::new_v4();
        if ext.is_empty() {
            self.data_dir.join(format!("{}_{}", stem, uuid))
        } else {
            self.data_dir.join(format!("{}_{}.{}", stem, uuid, ext))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_hash_file() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        tokio::fs::write(&test_file, b"hello world")
            .await
            .unwrap();

        let hash = hash_file(&test_file).await.unwrap();
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // BLAKE3 produces 32 bytes = 64 hex chars

        // Verify it's deterministic
        let hash2 = hash_file(&test_file).await.unwrap();
        assert_eq!(hash, hash2);
    }

    #[tokio::test]
    async fn test_verify_file_hash() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        tokio::fs::write(&test_file, b"hello world")
            .await
            .unwrap();

        let hash = hash_file(&test_file).await.unwrap();

        assert!(verify_file_hash(&test_file, &hash).await.unwrap());
        assert!(!verify_file_hash(&test_file, "wrong_hash").await.unwrap());
    }

    #[tokio::test]
    async fn test_blob_store() {
        let temp_dir = TempDir::new().unwrap();
        let store = BlobStore::new(temp_dir.path().to_path_buf()).unwrap();

        // Write a file
        let path = store.write_file("test.txt", b"hello").await.unwrap();
        assert!(path.exists());

        // Read it back
        let data = store.read_file(&path).await.unwrap();
        assert_eq!(data, b"hello");
    }

    #[tokio::test]
    async fn test_unique_path() {
        let temp_dir = TempDir::new().unwrap();
        let store = BlobStore::new(temp_dir.path().to_path_buf()).unwrap();

        // First path should be as-is
        let path1 = store.get_unique_path("test.txt");
        assert_eq!(path1.file_name().unwrap().to_str().unwrap(), "test.txt");

        // Create the file
        tokio::fs::write(&path1, b"content").await.unwrap();

        // Second path should be suffixed
        let path2 = store.get_unique_path("test.txt");
        assert_eq!(path2.file_name().unwrap().to_str().unwrap(), "test_1.txt");
    }
}
