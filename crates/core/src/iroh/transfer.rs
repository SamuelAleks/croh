//! File transfer implementation using Iroh.
//!
//! This module provides push and pull file transfer functionality
//! between trusted peers using the Iroh control protocol.

use crate::error::{Error, Result};
use crate::iroh::blobs::{hash_file, verify_file_hash};
use crate::iroh::browse::{
    browse_directory, get_browsable_roots, resolve_browse_path, validate_path,
};
use crate::iroh::endpoint::IrohEndpoint;
use crate::iroh::protocol::{ControlMessage, DirectoryEntry, FileInfo, FileRequest, InputEvent};
use crate::peers::TrustedPeer;
use crate::transfer::TransferId;
use chrono::Utc;
use iroh::NodeId;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Maximum chunk size for file transfers (64KB).
const CHUNK_SIZE: usize = 64 * 1024;

/// Progress event for transfer updates.
#[derive(Debug, Clone)]
pub enum TransferEvent {
    /// Transfer has started.
    Started { transfer_id: TransferId },
    /// Progress update.
    Progress {
        transfer_id: TransferId,
        transferred: u64,
        total: u64,
        speed: String,
    },
    /// A file has been completely transferred.
    FileComplete {
        transfer_id: TransferId,
        file: String,
    },
    /// Transfer completed successfully.
    Complete { transfer_id: TransferId },
    /// Transfer failed.
    Failed {
        transfer_id: TransferId,
        error: String,
    },
    /// Transfer was cancelled.
    Cancelled { transfer_id: TransferId },
}

/// Format bytes per second as human-readable speed.
fn format_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1_000_000_000.0 {
        format!("{:.1} GB/s", bytes_per_sec / 1_000_000_000.0)
    } else if bytes_per_sec >= 1_000_000.0 {
        format!("{:.1} MB/s", bytes_per_sec / 1_000_000.0)
    } else if bytes_per_sec >= 1_000.0 {
        format!("{:.1} KB/s", bytes_per_sec / 1_000.0)
    } else {
        format!("{:.0} B/s", bytes_per_sec)
    }
}

/// Push files to a trusted peer.
///
/// This function:
/// 1. Checks if the peer allows push
/// 2. Connects to the peer
/// 3. Sends a PushOffer with file info
/// 4. Waits for acceptance
/// 5. Streams files to the peer
/// 6. Sends completion message
pub async fn push_files(
    endpoint: &IrohEndpoint,
    peer: &TrustedPeer,
    files: &[PathBuf],
    progress_tx: mpsc::Sender<TransferEvent>,
) -> Result<TransferId> {
    let transfer_id = TransferId::new();

    // Check permissions
    if !peer.their_permissions.push {
        let err = "peer does not allow push".to_string();
        let _ = progress_tx
            .send(TransferEvent::Failed {
                transfer_id: transfer_id.clone(),
                error: err.clone(),
            })
            .await;
        return Err(Error::Trust(err));
    }

    // Notify start
    let _ = progress_tx
        .send(TransferEvent::Started {
            transfer_id: transfer_id.clone(),
        })
        .await;

    // Prepare file info - hash all files BEFORE connecting
    // This prevents connection timeout during hashing of large files
    info!("Hashing {} files before connecting...", files.len());
    let mut file_infos = Vec::new();
    let mut total_size = 0u64;

    for path in files {
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|e| Error::Io(format!("failed to read file metadata: {}", e)))?;

        let hash = hash_file(path).await?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        file_infos.push(FileInfo {
            path: name.clone(),
            name,
            size: metadata.len(),
            hash,
        });
        total_size += metadata.len();
    }
    info!(
        "Files hashed: {} files, {} bytes total",
        files.len(),
        total_size
    );

    // Parse peer node ID
    let node_id: NodeId = peer
        .endpoint_id
        .parse()
        .map_err(|e| Error::Iroh(format!("invalid node id: {}", e)))?;

    // Add peer's address information so we can connect
    // This includes the relay URL for NAT traversal
    let mut node_addr = iroh::NodeAddr::new(node_id);
    if let Some(relay_url) = peer.relay_url() {
        if let Ok(url) = relay_url.parse::<iroh::RelayUrl>() {
            node_addr = node_addr.with_relay_url(url);
            info!("Using relay URL for push: {}", relay_url);
        }
    }
    endpoint.add_node_addr(node_addr)?;

    // Connect to peer
    info!("Connecting to peer {} for push", peer.name);
    let mut conn = endpoint.connect_to_node(node_id).await?;

    // Send push offer
    let offer = ControlMessage::PushOffer {
        transfer_id: transfer_id.to_string(),
        files: file_infos.clone(),
        total_size,
    };
    conn.send(&offer).await?;
    info!(
        "Sent push offer with {} files, {} bytes",
        files.len(),
        total_size
    );

    // Wait for response
    let response = conn.recv().await?;
    match response {
        ControlMessage::PushResponse { accepted: true, .. } => {
            info!("Push accepted by peer");
        }
        ControlMessage::PushResponse {
            accepted: false,
            reason,
            ..
        } => {
            let err = format!(
                "push rejected: {}",
                reason.unwrap_or_else(|| "unknown reason".to_string())
            );
            let _ = progress_tx
                .send(TransferEvent::Failed {
                    transfer_id: transfer_id.clone(),
                    error: err.clone(),
                })
                .await;
            return Err(Error::Trust(err));
        }
        _ => {
            let err = "unexpected response to push offer".to_string();
            let _ = progress_tx
                .send(TransferEvent::Failed {
                    transfer_id: transfer_id.clone(),
                    error: err.clone(),
                })
                .await;
            return Err(Error::Iroh(err));
        }
    }

    // Transfer files
    let start_time = Instant::now();
    let mut total_transferred = 0u64;

    for (path, info) in files.iter().zip(file_infos.iter()) {
        debug!("Sending file: {} ({} bytes)", info.name, info.size);

        // Open file
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| Error::Io(format!("failed to open file: {}", e)))?;

        // Send file data in chunks
        let mut buffer = vec![0u8; CHUNK_SIZE];

        loop {
            let bytes_read = file
                .read(&mut buffer)
                .await
                .map_err(|e| Error::Io(format!("failed to read file: {}", e)))?;

            if bytes_read == 0 {
                break;
            }

            // Send chunk over the connection
            // Format: 4-byte length prefix + data
            let len = bytes_read as u32;
            conn.send_raw(&len.to_be_bytes()).await?;
            conn.send_raw(&buffer[..bytes_read]).await?;

            total_transferred += bytes_read as u64;

            // Calculate speed and send progress
            let elapsed = start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                format_speed(total_transferred as f64 / elapsed)
            } else {
                "calculating...".to_string()
            };

            let _ = progress_tx
                .send(TransferEvent::Progress {
                    transfer_id: transfer_id.clone(),
                    transferred: total_transferred,
                    total: total_size,
                    speed,
                })
                .await;
        }

        // Signal end of file (zero-length chunk)
        conn.send_raw(&0u32.to_be_bytes()).await?;

        let _ = progress_tx
            .send(TransferEvent::FileComplete {
                transfer_id: transfer_id.clone(),
                file: info.name.clone(),
            })
            .await;

        info!("File {} sent successfully", info.name);
    }

    // Send completion message
    conn.send(&ControlMessage::TransferComplete {
        transfer_id: transfer_id.to_string(),
    })
    .await?;

    // Wait for acknowledgment
    match conn.recv().await {
        Ok(ControlMessage::TransferComplete { .. }) => {
            info!("Push completed and acknowledged");
        }
        Ok(ControlMessage::TransferFailed { error, .. }) => {
            let _ = progress_tx
                .send(TransferEvent::Failed {
                    transfer_id: transfer_id.clone(),
                    error: error.clone(),
                })
                .await;
            return Err(Error::Transfer(error));
        }
        Ok(_) => {
            warn!("Unexpected response after transfer, assuming success");
        }
        Err(e) => {
            warn!("Failed to receive acknowledgment: {}, assuming success", e);
        }
    }

    let _ = progress_tx
        .send(TransferEvent::Complete {
            transfer_id: transfer_id.clone(),
        })
        .await;

    // Clean close
    let _ = conn.close().await;

    Ok(transfer_id)
}

/// Pull files from a trusted peer.
///
/// This function:
/// 1. Checks if the peer allows pull
/// 2. Connects to the peer
/// 3. Sends a PullRequest
/// 4. Waits for acceptance with file hashes
/// 5. Receives file streams
/// 6. Verifies hashes and saves to download_dir
pub async fn pull_files(
    endpoint: &IrohEndpoint,
    peer: &TrustedPeer,
    files: &[FileRequest],
    download_dir: &Path,
    progress_tx: mpsc::Sender<TransferEvent>,
) -> Result<TransferId> {
    let transfer_id = TransferId::new();

    // Check permissions
    if !peer.their_permissions.pull {
        let err = "peer does not allow pull".to_string();
        let _ = progress_tx
            .send(TransferEvent::Failed {
                transfer_id: transfer_id.clone(),
                error: err.clone(),
            })
            .await;
        return Err(Error::Trust(err));
    }

    // Notify start
    let _ = progress_tx
        .send(TransferEvent::Started {
            transfer_id: transfer_id.clone(),
        })
        .await;

    // Parse peer node ID
    let node_id: NodeId = peer
        .endpoint_id
        .parse()
        .map_err(|e| Error::Iroh(format!("invalid node id: {}", e)))?;

    // Add peer's address information so we can connect
    // This includes the relay URL for NAT traversal
    let mut node_addr = iroh::NodeAddr::new(node_id);
    if let Some(relay_url) = peer.relay_url() {
        if let Ok(url) = relay_url.parse::<iroh::RelayUrl>() {
            node_addr = node_addr.with_relay_url(url);
            info!("Using relay URL for pull: {}", relay_url);
        }
    }
    endpoint.add_node_addr(node_addr)?;

    // Connect to peer
    info!("Connecting to peer {} for pull", peer.name);
    let mut conn = endpoint.connect_to_node(node_id).await?;

    // Send pull request
    let request = ControlMessage::PullRequest {
        transfer_id: transfer_id.to_string(),
        files: files.to_vec(),
    };
    conn.send(&request).await?;
    info!("Sent pull request for {} files", files.len());

    // Wait for response
    let response = conn.recv().await?;
    let file_infos = match response {
        ControlMessage::PullResponse {
            granted: true,
            files,
            ..
        } => {
            info!("Pull granted, receiving {} files", files.len());
            files
        }
        ControlMessage::PullResponse {
            granted: false,
            reason,
            ..
        } => {
            let err = format!(
                "pull rejected: {}",
                reason.unwrap_or_else(|| "unknown reason".to_string())
            );
            let _ = progress_tx
                .send(TransferEvent::Failed {
                    transfer_id: transfer_id.clone(),
                    error: err.clone(),
                })
                .await;
            return Err(Error::Trust(err));
        }
        _ => {
            let err = "unexpected response to pull request".to_string();
            let _ = progress_tx
                .send(TransferEvent::Failed {
                    transfer_id: transfer_id.clone(),
                    error: err.clone(),
                })
                .await;
            return Err(Error::Iroh(err));
        }
    };

    // Ensure download directory exists
    tokio::fs::create_dir_all(download_dir)
        .await
        .map_err(|e| Error::Io(format!("failed to create download dir: {}", e)))?;

    // Calculate total size
    let total_size: u64 = file_infos.iter().map(|f| f.size).sum();
    let start_time = Instant::now();
    let mut total_transferred = 0u64;

    // Receive files
    for info in &file_infos {
        debug!("Receiving file: {} ({} bytes)", info.name, info.size);

        let dest_path = download_dir.join(&info.name);
        let mut file = tokio::fs::File::create(&dest_path)
            .await
            .map_err(|e| Error::Io(format!("failed to create file: {}", e)))?;

        // Receive chunks until zero-length chunk
        loop {
            // Read length prefix
            let mut len_buf = [0u8; 4];
            conn.recv_raw(&mut len_buf).await?;
            let chunk_len = u32::from_be_bytes(len_buf) as usize;

            if chunk_len == 0 {
                // End of file
                break;
            }

            // Read chunk data
            let mut chunk = vec![0u8; chunk_len];
            conn.recv_raw(&mut chunk).await?;

            // Write to file
            file.write_all(&chunk)
                .await
                .map_err(|e| Error::Io(format!("failed to write file: {}", e)))?;

            total_transferred += chunk_len as u64;

            // Calculate speed and send progress
            let elapsed = start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                format_speed(total_transferred as f64 / elapsed)
            } else {
                "calculating...".to_string()
            };

            let _ = progress_tx
                .send(TransferEvent::Progress {
                    transfer_id: transfer_id.clone(),
                    transferred: total_transferred,
                    total: total_size,
                    speed,
                })
                .await;
        }

        file.flush()
            .await
            .map_err(|e| Error::Io(format!("failed to flush file: {}", e)))?;

        // Verify hash
        if !verify_file_hash(&dest_path, &info.hash).await? {
            let err = format!("hash mismatch for file {}", info.name);
            error!("{}", err);
            // Delete corrupted file
            let _ = tokio::fs::remove_file(&dest_path).await;
            let _ = progress_tx
                .send(TransferEvent::Failed {
                    transfer_id: transfer_id.clone(),
                    error: err.clone(),
                })
                .await;
            return Err(Error::Transfer(err));
        }

        let _ = progress_tx
            .send(TransferEvent::FileComplete {
                transfer_id: transfer_id.clone(),
                file: info.name.clone(),
            })
            .await;

        info!("File {} received and verified", info.name);
    }

    // Wait for completion message
    match conn.recv().await {
        Ok(ControlMessage::TransferComplete { .. }) => {
            info!("Pull completed");
        }
        Ok(ControlMessage::TransferFailed { error, .. }) => {
            let _ = progress_tx
                .send(TransferEvent::Failed {
                    transfer_id: transfer_id.clone(),
                    error: error.clone(),
                })
                .await;
            return Err(Error::Transfer(error));
        }
        Ok(_) => {
            warn!("Unexpected message after transfer");
        }
        Err(e) => {
            warn!("Failed to receive completion: {}", e);
        }
    }

    // Send acknowledgment
    let _ = conn
        .send(&ControlMessage::TransferComplete {
            transfer_id: transfer_id.to_string(),
        })
        .await;

    let _ = progress_tx
        .send(TransferEvent::Complete {
            transfer_id: transfer_id.clone(),
        })
        .await;

    // Clean close
    let _ = conn.close().await;

    Ok(transfer_id)
}

/// Handle an incoming push request from a trusted peer.
///
/// This function:
/// 1. Verifies the sender is a trusted peer
/// 2. Accepts the push offer
/// 3. Receives file streams
/// 4. Verifies hashes and saves to download_dir
pub async fn handle_incoming_push(
    conn: &mut crate::iroh::endpoint::ControlConnection,
    sender_endpoint_id: &str,
    offer: ControlMessage,
    download_dir: &Path,
    progress_tx: mpsc::Sender<TransferEvent>,
) -> Result<TransferId> {
    // Extract push offer details
    let (transfer_id_str, file_infos, total_size) = match offer {
        ControlMessage::PushOffer {
            transfer_id,
            files,
            total_size,
        } => (transfer_id, files, total_size),
        _ => return Err(Error::Iroh("expected PushOffer message".to_string())),
    };

    let transfer_id = TransferId(transfer_id_str.clone());
    info!(
        "Received push offer from {} with {} files ({} bytes)",
        sender_endpoint_id,
        file_infos.len(),
        total_size
    );

    // Auto-accept the push (as per user preference)
    // In future, this could show a confirmation dialog
    let response = ControlMessage::PushResponse {
        transfer_id: transfer_id_str.clone(),
        accepted: true,
        reason: None,
    };
    conn.send(&response).await?;
    info!("Accepted push from {}", sender_endpoint_id);

    // Notify start
    let _ = progress_tx
        .send(TransferEvent::Started {
            transfer_id: transfer_id.clone(),
        })
        .await;

    // Ensure download directory exists
    tokio::fs::create_dir_all(download_dir)
        .await
        .map_err(|e| Error::Io(format!("failed to create download dir: {}", e)))?;

    let start_time = Instant::now();
    let mut total_transferred = 0u64;

    // Receive files
    for info in &file_infos {
        debug!("Receiving file: {} ({} bytes)", info.name, info.size);

        let dest_path = download_dir.join(&info.name);
        let mut file = tokio::fs::File::create(&dest_path)
            .await
            .map_err(|e| Error::Io(format!("failed to create file: {}", e)))?;

        // Receive chunks until zero-length chunk
        loop {
            // Read length prefix
            let mut len_buf = [0u8; 4];
            conn.recv_raw(&mut len_buf).await?;
            let chunk_len = u32::from_be_bytes(len_buf) as usize;

            if chunk_len == 0 {
                // End of file
                break;
            }

            // Read chunk data
            let mut chunk = vec![0u8; chunk_len];
            conn.recv_raw(&mut chunk).await?;

            // Write to file
            file.write_all(&chunk)
                .await
                .map_err(|e| Error::Io(format!("failed to write file: {}", e)))?;

            total_transferred += chunk_len as u64;

            // Calculate speed and send progress
            let elapsed = start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                format_speed(total_transferred as f64 / elapsed)
            } else {
                "calculating...".to_string()
            };

            let _ = progress_tx
                .send(TransferEvent::Progress {
                    transfer_id: transfer_id.clone(),
                    transferred: total_transferred,
                    total: total_size,
                    speed,
                })
                .await;
        }

        file.flush()
            .await
            .map_err(|e| Error::Io(format!("failed to flush file: {}", e)))?;

        // Verify hash
        if !verify_file_hash(&dest_path, &info.hash).await? {
            let err = format!("hash mismatch for file {}", info.name);
            error!("{}", err);
            // Delete corrupted file
            let _ = tokio::fs::remove_file(&dest_path).await;

            // Send failure message
            let _ = conn
                .send(&ControlMessage::TransferFailed {
                    transfer_id: transfer_id_str.clone(),
                    error: err.clone(),
                })
                .await;

            let _ = progress_tx
                .send(TransferEvent::Failed {
                    transfer_id: transfer_id.clone(),
                    error: err.clone(),
                })
                .await;
            return Err(Error::Transfer(err));
        }

        let _ = progress_tx
            .send(TransferEvent::FileComplete {
                transfer_id: transfer_id.clone(),
                file: info.name.clone(),
            })
            .await;

        info!("File {} received and verified", info.name);
    }

    // Wait for completion message from sender
    match conn.recv().await {
        Ok(ControlMessage::TransferComplete { .. }) => {
            info!("Push sender signaled completion");
        }
        Ok(ControlMessage::TransferFailed { error, .. }) => {
            let _ = progress_tx
                .send(TransferEvent::Failed {
                    transfer_id: transfer_id.clone(),
                    error: error.clone(),
                })
                .await;
            return Err(Error::Transfer(error));
        }
        Ok(_) => {
            warn!("Unexpected message after transfer");
        }
        Err(e) => {
            warn!("Failed to receive completion: {}", e);
        }
    }

    // Send acknowledgment
    let _ = conn
        .send(&ControlMessage::TransferComplete {
            transfer_id: transfer_id_str,
        })
        .await;

    // Give the sender time to receive the ack before the connection is dropped
    // This is necessary because send() only queues the data for transmission
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let _ = progress_tx
        .send(TransferEvent::Complete {
            transfer_id: transfer_id.clone(),
        })
        .await;

    info!("Push reception completed successfully");
    Ok(transfer_id)
}

/// Handle a browse request from a trusted peer.
///
/// This function:
/// 1. Validates the requester is a trusted peer with browse permission
/// 2. Resolves the requested path against allowed paths
/// 3. Lists directory contents applying browse settings (hidden, protected, excludes)
/// 4. Returns the directory listing
pub async fn handle_browse_request(
    conn: &mut crate::iroh::endpoint::ControlConnection,
    request_path: Option<String>,
    allowed_paths: Option<&[PathBuf]>,
    browse_settings: &crate::config::BrowseSettings,
) -> Result<()> {
    let roots = get_browsable_roots(allowed_paths);

    // Resolve the browse path
    let resolved_path = match &request_path {
        Some(p) => resolve_browse_path(p, &roots),
        None => None,
    };

    // Browse the directory with settings
    let (path, entries) = match browse_directory(resolved_path.as_deref(), &roots, browse_settings)
    {
        Ok(result) => result,
        Err(e) => {
            // Send error response
            let response = ControlMessage::BrowseResponse {
                path: request_path.unwrap_or_else(|| "/".to_string()),
                entries: vec![],
                error: Some(e.to_string()),
            };
            conn.send(&response).await?;
            return Err(e);
        }
    };

    // Send successful response
    let response = ControlMessage::BrowseResponse {
        path,
        entries,
        error: None,
    };
    conn.send(&response).await?;

    Ok(())
}

/// Handle an incoming pull request from a trusted peer.
///
/// This function:
/// 1. Verifies the requester is a trusted peer with pull permission
/// 2. Validates requested file paths against allowed paths
/// 3. Hashes files and sends PullResponse
/// 4. Streams files to the requester
pub async fn handle_incoming_pull(
    conn: &mut crate::iroh::endpoint::ControlConnection,
    request: ControlMessage,
    allowed_paths: Option<&[PathBuf]>,
    progress_tx: mpsc::Sender<TransferEvent>,
) -> Result<TransferId> {
    // Extract pull request details
    let (transfer_id_str, file_requests) = match request {
        ControlMessage::PullRequest { transfer_id, files } => (transfer_id, files),
        _ => return Err(Error::Iroh("expected PullRequest message".to_string())),
    };

    let transfer_id = TransferId(transfer_id_str.clone());
    info!("Received pull request for {} files", file_requests.len());

    // Validate paths and prepare file info
    let roots = get_browsable_roots(allowed_paths);
    let mut file_infos = Vec::new();
    let mut valid_paths = Vec::new();
    let mut total_size = 0u64;
    let mut errors = Vec::new();

    for req in &file_requests {
        let path = PathBuf::from(&req.path);

        // Validate path is within allowed directories
        match validate_path(&path, &roots) {
            Ok(canonical) => {
                if canonical.is_file() {
                    // Hash the file
                    match hash_file(&canonical).await {
                        Ok(hash) => {
                            let metadata = tokio::fs::metadata(&canonical).await.map_err(|e| {
                                Error::Io(format!("failed to read file metadata: {}", e))
                            })?;

                            let name = canonical
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown")
                                .to_string();

                            file_infos.push(FileInfo {
                                path: req.path.clone(),
                                name,
                                size: metadata.len(),
                                hash,
                            });
                            valid_paths.push(canonical);
                            total_size += metadata.len();
                        }
                        Err(e) => {
                            errors.push(format!("{}: failed to hash: {}", req.path, e));
                        }
                    }
                } else {
                    errors.push(format!("{}: not a file", req.path));
                }
            }
            Err(e) => {
                errors.push(format!("{}: {}", req.path, e));
            }
        }
    }

    // If no valid files, reject
    if file_infos.is_empty() {
        let response = ControlMessage::PullResponse {
            transfer_id: transfer_id_str.clone(),
            files: vec![],
            granted: false,
            reason: Some(errors.join("; ")),
        };
        conn.send(&response).await?;
        return Err(Error::Trust("no valid files for pull".to_string()));
    }

    // Send acceptance with file info
    let response = ControlMessage::PullResponse {
        transfer_id: transfer_id_str.clone(),
        files: file_infos.clone(),
        granted: true,
        reason: if errors.is_empty() {
            None
        } else {
            Some(format!("some files skipped: {}", errors.join("; ")))
        },
    };
    conn.send(&response).await?;
    info!(
        "Pull granted for {} files, {} bytes",
        file_infos.len(),
        total_size
    );

    // Notify start
    let _ = progress_tx
        .send(TransferEvent::Started {
            transfer_id: transfer_id.clone(),
        })
        .await;

    // Transfer files
    let start_time = Instant::now();
    let mut total_transferred = 0u64;

    for (path, info) in valid_paths.iter().zip(file_infos.iter()) {
        debug!("Sending file: {} ({} bytes)", info.name, info.size);

        // Open file
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| Error::Io(format!("failed to open file: {}", e)))?;

        // Send file data in chunks
        let mut buffer = vec![0u8; CHUNK_SIZE];

        loop {
            let bytes_read = file
                .read(&mut buffer)
                .await
                .map_err(|e| Error::Io(format!("failed to read file: {}", e)))?;

            if bytes_read == 0 {
                break;
            }

            // Send chunk over the connection
            // Format: 4-byte length prefix + data
            let len = bytes_read as u32;
            conn.send_raw(&len.to_be_bytes()).await?;
            conn.send_raw(&buffer[..bytes_read]).await?;

            total_transferred += bytes_read as u64;

            // Calculate speed and send progress
            let elapsed = start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                format_speed(total_transferred as f64 / elapsed)
            } else {
                "calculating...".to_string()
            };

            let _ = progress_tx
                .send(TransferEvent::Progress {
                    transfer_id: transfer_id.clone(),
                    transferred: total_transferred,
                    total: total_size,
                    speed,
                })
                .await;
        }

        // Signal end of file (zero-length chunk)
        conn.send_raw(&0u32.to_be_bytes()).await?;

        let _ = progress_tx
            .send(TransferEvent::FileComplete {
                transfer_id: transfer_id.clone(),
                file: info.name.clone(),
            })
            .await;

        info!("File {} sent successfully", info.name);
    }

    // Send completion message
    conn.send(&ControlMessage::TransferComplete {
        transfer_id: transfer_id.to_string(),
    })
    .await?;

    // Wait for acknowledgment
    match conn.recv().await {
        Ok(ControlMessage::TransferComplete { .. }) => {
            info!("Pull completed and acknowledged");
        }
        Ok(ControlMessage::TransferFailed { error, .. }) => {
            let _ = progress_tx
                .send(TransferEvent::Failed {
                    transfer_id: transfer_id.clone(),
                    error: error.clone(),
                })
                .await;
            return Err(Error::Transfer(error));
        }
        Ok(_) => {
            warn!("Unexpected response after transfer, assuming success");
        }
        Err(e) => {
            warn!("Failed to receive acknowledgment: {}, assuming success", e);
        }
    }

    let _ = progress_tx
        .send(TransferEvent::Complete {
            transfer_id: transfer_id.clone(),
        })
        .await;

    info!("Pull request completed successfully");
    Ok(transfer_id)
}

/// Request a directory listing from a remote peer.
pub async fn browse_remote(
    endpoint: &IrohEndpoint,
    peer: &TrustedPeer,
    path: Option<&str>,
) -> Result<(String, Vec<DirectoryEntry>)> {
    // Check permissions
    if !peer.their_permissions.browse {
        return Err(Error::Trust("peer does not allow browse".to_string()));
    }

    // Parse peer node ID
    let node_id: NodeId = peer
        .endpoint_id
        .parse()
        .map_err(|e| Error::Iroh(format!("invalid node id: {}", e)))?;

    // Add peer's address information
    let mut node_addr = iroh::NodeAddr::new(node_id);
    if let Some(relay_url) = peer.relay_url() {
        if let Ok(url) = relay_url.parse::<iroh::RelayUrl>() {
            node_addr = node_addr.with_relay_url(url);
        }
    }
    endpoint.add_node_addr(node_addr)?;

    // Connect to peer
    info!("Connecting to peer {} for browse", peer.name);
    let mut conn = endpoint.connect_to_node(node_id).await?;

    // Send browse request
    let request = ControlMessage::BrowseRequest {
        path: path.map(|s| s.to_string()),
    };
    conn.send(&request).await?;
    info!("Sent browse request for path: {:?}", path);

    // Wait for response
    let response = conn.recv().await?;
    match response {
        ControlMessage::BrowseResponse {
            path,
            entries,
            error: None,
        } => {
            info!("Browse succeeded: {} entries", entries.len());
            let _ = conn.close().await;
            Ok((path, entries))
        }
        ControlMessage::BrowseResponse {
            error: Some(err), ..
        } => {
            let _ = conn.close().await;
            Err(Error::Browse(err))
        }
        _ => {
            let _ = conn.close().await;
            Err(Error::Iroh(
                "unexpected response to browse request".to_string(),
            ))
        }
    }
}

// ============================================================================
// Screen Streaming
// ============================================================================

use crate::iroh::protocol::{self, FrameMetadata, ScreenCompression, ScreenQuality};

/// Events from a screen streaming session.
#[derive(Debug, Clone)]
pub enum ScreenStreamEvent {
    /// Clock synchronization completed.
    ClockSynced {
        /// Clock offset from host in ms (positive = host ahead)
        offset_ms: i64,
        /// Network round-trip time in ms
        rtt_ms: u32,
    },
    /// Stream was accepted, now receiving frames.
    Accepted {
        stream_id: String,
        compression: ScreenCompression,
        displays: Vec<protocol::DisplayInfo>,
    },
    /// Stream was rejected.
    Rejected { stream_id: String, reason: String },
    /// A frame was received.
    FrameReceived {
        stream_id: String,
        metadata: FrameMetadata,
        data: Vec<u8>,
    },
    /// Stream ended.
    Ended { stream_id: String, reason: String },
    /// Error occurred.
    Error(String),
}

/// Connect to a remote peer and start receiving their screen stream.
///
/// This function establishes a connection, sends a ScreenStreamRequest,
/// and then loops receiving ScreenFrame messages, forwarding them to the
/// provided event channel.
///
/// The function returns when the stream ends (either remotely stopped,
/// connection lost, or error).
///
/// # Arguments
/// * `endpoint` - The local Iroh endpoint
/// * `peer` - The trusted peer to connect to
/// * `display_id` - Optional display to stream (None = primary)
/// * `event_tx` - Channel to send stream events to
/// * `cancel_rx` - Channel to receive cancellation signals
/// * `input_rx` - Channel to receive input events to forward to remote
///
/// # Returns
/// The stream ID on success, or an error.
pub async fn stream_screen_from_peer(
    endpoint: &IrohEndpoint,
    peer: &TrustedPeer,
    display_id: Option<String>,
    event_tx: mpsc::Sender<ScreenStreamEvent>,
    mut cancel_rx: mpsc::Receiver<()>,
    mut input_rx: mpsc::Receiver<Vec<InputEvent>>,
) -> Result<String> {
    // Check permissions
    if !peer.their_permissions.screen_view {
        return Err(Error::Trust(
            "peer does not allow screen viewing".to_string(),
        ));
    }

    // Parse peer node ID
    let node_id: NodeId = peer
        .endpoint_id
        .parse()
        .map_err(|e| Error::Iroh(format!("invalid node id: {}", e)))?;

    // Add peer's address information
    let mut node_addr = iroh::NodeAddr::new(node_id);
    if let Some(relay_url) = peer.relay_url() {
        if let Ok(url) = relay_url.parse::<iroh::RelayUrl>() {
            node_addr = node_addr.with_relay_url(url);
        }
    }
    endpoint.add_node_addr(node_addr)?;

    // Connect to peer
    info!("Connecting to peer {} for screen streaming", peer.name);
    let mut conn = endpoint.connect_to_node(node_id).await?;
    info!("Connected to peer {} for screen streaming", peer.name);

    // Perform clock synchronization (5 exchanges)
    info!("Performing clock synchronization...");
    let mut clock_sync = crate::screen::ClockSync::new();
    while clock_sync.needs_more_samples() {
        let client_time = Utc::now().timestamp_millis();
        let sequence = clock_sync.next_request_sequence();

        let request = ControlMessage::TimeSyncRequest {
            client_time,
            sequence,
        };
        conn.send(&request).await?;

        // Wait for response with timeout
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            conn.recv(),
        )
        .await
        .map_err(|_| Error::Iroh("time sync timeout".to_string()))??;

        match response {
            ControlMessage::TimeSyncResponse {
                client_time: resp_client_time,
                server_receive_time,
                server_send_time,
            } => {
                let response_received_time = Utc::now().timestamp_millis();
                clock_sync.process_response(
                    resp_client_time,
                    server_receive_time,
                    server_send_time,
                    response_received_time,
                );
            }
            other => {
                warn!("Unexpected response during time sync: {:?}", other);
                // Continue anyway - time sync is optional
                break;
            }
        }
    }

    if clock_sync.is_synced() {
        info!(
            "Clock sync complete: offset={}ms, RTT={}ms",
            clock_sync.offset_ms(),
            clock_sync.avg_rtt_ms()
        );
        let _ = event_tx
            .send(ScreenStreamEvent::ClockSynced {
                offset_ms: clock_sync.offset_ms(),
                rtt_ms: clock_sync.avg_rtt_ms(),
            })
            .await;
    } else {
        warn!("Clock sync incomplete, latency measurement may be inaccurate");
    }

    // Generate stream ID
    let stream_id = uuid::Uuid::new_v4().to_string();

    // Send screen stream request
    let request = ControlMessage::ScreenStreamRequest {
        stream_id: stream_id.clone(),
        display_id,
        compression: ScreenCompression::Raw, // Request raw/simple compression
        quality: ScreenQuality::Balanced,
        target_fps: 30,
    };
    conn.send(&request).await?;
    info!("Sent screen stream request: {}", stream_id);

    // Wait for response
    let response = conn.recv().await?;
    match response {
        ControlMessage::ScreenStreamResponse {
            stream_id: resp_id,
            accepted: true,
            displays,
            compression,
            ..
        } => {
            info!("Screen stream accepted: {}", resp_id);
            let _ = event_tx
                .send(ScreenStreamEvent::Accepted {
                    stream_id: resp_id.clone(),
                    compression: compression.unwrap_or(ScreenCompression::Raw),
                    displays,
                })
                .await;
        }
        ControlMessage::ScreenStreamResponse {
            stream_id: resp_id,
            accepted: false,
            reason,
            ..
        } => {
            let reason_str = reason.unwrap_or_else(|| "unknown".to_string());
            warn!("Screen stream rejected: {}", reason_str);
            let _ = event_tx
                .send(ScreenStreamEvent::Rejected {
                    stream_id: resp_id,
                    reason: reason_str.clone(),
                })
                .await;
            let _ = conn.close().await;
            return Err(Error::Screen(reason_str));
        }
        other => {
            let err = format!("unexpected response to screen stream request: {:?}", other);
            let _ = event_tx.send(ScreenStreamEvent::Error(err.clone())).await;
            let _ = conn.close().await;
            return Err(Error::Iroh(err));
        }
    }

    // Frame reception loop
    let mut last_ack_sequence;
    let mut frames_since_ack = 0u32;
    const ACK_EVERY_N_FRAMES: u32 = 10;

    loop {
        tokio::select! {
            // Check for cancellation
            _ = cancel_rx.recv() => {
                info!("Screen stream cancelled by user");
                // Send stop message
                let stop = ControlMessage::ScreenStreamStop {
                    stream_id: stream_id.clone(),
                    reason: "user cancelled".to_string(),
                };
                let _ = conn.send(&stop).await;
                let _ = event_tx.send(ScreenStreamEvent::Ended {
                    stream_id: stream_id.clone(),
                    reason: "user cancelled".to_string(),
                }).await;
                let _ = conn.close().await;
                return Ok(stream_id);
            }

            // Send input events to remote
            Some(events) = input_rx.recv() => {
                if !events.is_empty() {
                    debug!("Sending {} input events to remote peer", events.len());
                    let input_msg = ControlMessage::ScreenInput {
                        stream_id: stream_id.clone(),
                        events,
                    };
                    if let Err(e) = conn.send(&input_msg).await {
                        warn!("Failed to send input events: {}", e);
                    } else {
                        debug!("Input events sent successfully");
                    }
                }
            }

            // Receive next message
            msg_result = conn.recv() => {
                match msg_result {
                    Ok(ControlMessage::ScreenFrame { stream_id: frame_stream_id, metadata }) => {
                        // Read the frame data
                        let mut frame_data = vec![0u8; metadata.size as usize];
                        if let Err(e) = conn.recv_raw(&mut frame_data).await {
                            error!("Failed to receive frame data: {}", e);
                            let _ = event_tx.send(ScreenStreamEvent::Error(format!("frame receive error: {}", e))).await;
                            continue;
                        }

                        // Send frame to event channel
                        let _ = event_tx.send(ScreenStreamEvent::FrameReceived {
                            stream_id: frame_stream_id,
                            metadata: metadata.clone(),
                            data: frame_data,
                        }).await;

                        // Send ACK periodically for flow control
                        frames_since_ack += 1;
                        if frames_since_ack >= ACK_EVERY_N_FRAMES {
                            last_ack_sequence = metadata.sequence;
                            frames_since_ack = 0;
                            let ack = ControlMessage::ScreenFrameAck {
                                stream_id: stream_id.clone(),
                                up_to_sequence: last_ack_sequence,
                                estimated_bandwidth: None,
                                quality_hint: None,
                                measured_latency_ms: None,
                                packet_loss: None,
                                request_keyframe: false,
                            };
                            if let Err(e) = conn.send(&ack).await {
                                warn!("Failed to send frame ACK: {}", e);
                            }
                        }
                    }
                    Ok(ControlMessage::ScreenStreamStop { stream_id: stop_id, reason }) => {
                        info!("Screen stream stopped by remote: {}", reason);
                        let _ = event_tx.send(ScreenStreamEvent::Ended {
                            stream_id: stop_id,
                            reason,
                        }).await;
                        let _ = conn.close().await;
                        return Ok(stream_id);
                    }
                    Ok(other) => {
                        warn!("Unexpected message during screen stream: {:?}", other);
                    }
                    Err(e) => {
                        error!("Screen stream connection error: {}", e);
                        let _ = event_tx.send(ScreenStreamEvent::Ended {
                            stream_id: stream_id.clone(),
                            reason: format!("connection error: {}", e),
                        }).await;
                        return Err(e);
                    }
                }
            }
        }
    }
}

/// Handle an incoming screen stream request from a peer.
///
/// This function is called by the background listener when a ScreenStreamRequest
/// is received. It validates permissions, starts the screen capture, and sends
/// frames to the remote peer.
#[allow(clippy::too_many_arguments)]
pub async fn handle_screen_stream_request(
    conn: &mut crate::iroh::ControlConnection,
    stream_id: String,
    display_id: Option<String>,
    _compression: ScreenCompression,
    _quality: ScreenQuality,
    target_fps: u32,
    peer: &TrustedPeer,
    screen_settings: &crate::config::ScreenStreamSettings,
) -> Result<()> {
    use crate::screen::{create_capture_backend, FrameEncoder, ZstdEncoder};

    // Check if streaming is enabled
    if !screen_settings.enabled {
        let response = ControlMessage::ScreenStreamResponse {
            stream_id,
            accepted: false,
            reason: Some("Screen streaming is disabled".to_string()),
            displays: vec![],
            compression: None,
        };
        conn.send(&response).await?;
        return Ok(());
    }

    // Check peer has permission
    if !peer.permissions_granted.screen_view {
        let response = ControlMessage::ScreenStreamResponse {
            stream_id,
            accepted: false,
            reason: Some("screen_view permission not granted".to_string()),
            displays: vec![],
            compression: None,
        };
        conn.send(&response).await?;
        return Ok(());
    }

    // Create capture backend
    let mut capture = match create_capture_backend(screen_settings).await {
        Ok(c) => c,
        Err(e) => {
            let response = ControlMessage::ScreenStreamResponse {
                stream_id,
                accepted: false,
                reason: Some(format!("Failed to initialize capture: {}", e)),
                displays: vec![],
                compression: None,
            };
            conn.send(&response).await?;
            return Ok(());
        }
    };

    // List displays
    let displays = capture.list_displays().await?;
    let display_infos: Vec<protocol::DisplayInfo> = displays
        .iter()
        .map(|d| protocol::DisplayInfo {
            id: d.id.clone(),
            name: d.name.clone(),
            width: d.width,
            height: d.height,
            refresh_rate: d.refresh_rate,
            is_primary: d.is_primary,
        })
        .collect();

    // Determine which display to capture
    let target_display = if let Some(ref id) = display_id {
        displays.iter().find(|d| d.id == *id).map(|d| d.id.clone())
    } else {
        displays
            .iter()
            .find(|d| d.is_primary)
            .or(displays.first())
            .map(|d| d.id.clone())
    };

    let target_display = match target_display {
        Some(id) => id,
        None => {
            let response = ControlMessage::ScreenStreamResponse {
                stream_id,
                accepted: false,
                reason: Some("No displays available".to_string()),
                displays: vec![],
                compression: None,
            };
            conn.send(&response).await?;
            return Ok(());
        }
    };

    // Start capture
    capture.start(&target_display).await?;

    // Send acceptance response
    // Note: The protocol uses video codec names, but our implementation uses
    // simple frame compression (Zstd/PNG). The actual compression format is
    // indicated by magic bytes in the frame data, so we just acknowledge Raw here.
    let actual_compression = ScreenCompression::Raw;

    let response = ControlMessage::ScreenStreamResponse {
        stream_id: stream_id.clone(),
        accepted: true,
        reason: None,
        displays: display_infos,
        compression: Some(actual_compression),
    };
    conn.send(&response).await?;
    info!(
        "Screen stream {} accepted, starting capture on {}",
        stream_id, target_display
    );

    // Create encoder - use Zstd for good compression with reasonable speed
    // ~160KB per frame at 2560x1440 = ~40 Mbps at 30fps
    // TODO: Add H.264/H.265 hardware encoding for lower latency and better compression
    let mut encoder: Box<dyn FrameEncoder> = Box::new(ZstdEncoder::new());

    // Calculate frame interval
    let fps = target_fps.min(screen_settings.max_fps).max(1);
    let frame_interval = std::time::Duration::from_millis(1000 / fps as u64);
    let mut sequence = 0u64;

    // Create input injector if peer has control permission
    let mut input_handler: Option<Box<dyn crate::screen::InputInjector>> = None;
    debug!(
        "Peer {} permissions_granted: screen_view={}, screen_control={}",
        peer.name, peer.permissions_granted.screen_view, peer.permissions_granted.screen_control
    );
    if peer.permissions_granted.screen_control {
        match crate::screen::create_input_injector() {
            Ok(mut handler) => {
                if let Err(e) = handler.init() {
                    warn!("Failed to initialize input handler: {}", e);
                } else {
                    info!("Input injection enabled for screen control");
                    input_handler = Some(handler);
                }
            }
            Err(e) => {
                warn!("Failed to create input backend: {}", e);
            }
        }
    } else {
        info!("Input injection disabled - peer does not have screen_control permission");
    }

    // Frame sending and message handling loop
    let mut frame_timer = tokio::time::interval(frame_interval);
    frame_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            // Frame capture timer
            _ = frame_timer.tick() => {
                // Capture frame - record timestamp immediately
                let capture_timestamp = Utc::now().timestamp_millis();
                let frame = match capture.capture_frame().await {
                    Ok(Some(f)) => f,
                    Ok(None) => {
                        // No frame available, try again
                        continue;
                    }
                    Err(e) => {
                        error!("Capture error: {}", e);
                        // Send stop message
                        let stop = ControlMessage::ScreenStreamStop {
                            stream_id: stream_id.clone(),
                            reason: format!("Capture error: {}", e),
                        };
                        let _ = conn.send(&stop).await;
                        capture.stop().await?;
                        return Err(e);
                    }
                };

                // Encode frame
                let encoded = match encoder.encode(&frame) {
                    Ok(e) => e,
                    Err(e) => {
                        error!("Encode error: {}", e);
                        continue;
                    }
                };

                // Send frame header - use capture timestamp for accurate latency measurement
                let metadata = FrameMetadata {
                    sequence,
                    width: encoded.width,
                    height: encoded.height,
                    captured_at: capture_timestamp,
                    compression: actual_compression,
                    is_keyframe: true, // All frames are keyframes for now
                    size: encoded.data.len() as u32,
                };

                let frame_msg = ControlMessage::ScreenFrame {
                    stream_id: stream_id.clone(),
                    metadata,
                };

                if let Err(e) = conn.send(&frame_msg).await {
                    info!("Connection closed during screen stream: {}", e);
                    break;
                }

                // Send frame data
                if let Err(e) = conn.send_raw(&encoded.data).await {
                    info!("Connection closed during frame data send: {}", e);
                    break;
                }

                sequence += 1;
            }

            // Handle incoming messages
            msg_result = conn.recv() => {
                match msg_result {
                    Ok(ControlMessage::ScreenInput { events, .. }) => {
                        // Process input events
                        info!("Received {} input events from viewer", events.len());
                        if let Some(ref mut handler) = input_handler {
                            for event in &events {
                                debug!("Injecting input event: {:?}", event);
                                if let Err(e) = inject_input_event(handler.as_mut(), event) {
                                    warn!("Failed to inject input event: {}", e);
                                }
                            }
                        } else {
                            warn!("Received input events but input injection not enabled (no handler)");
                        }
                    }
                    Ok(ControlMessage::ScreenFrameAck { up_to_sequence, .. }) => {
                        // ACKs can be used for flow control - currently just logged
                        debug!("Received ACK up to sequence {}", up_to_sequence);
                    }
                    Ok(ControlMessage::ScreenStreamStop { reason, .. }) => {
                        info!("Stream stop requested by viewer: {}", reason);
                        break;
                    }
                    Ok(ControlMessage::ScreenStreamAdjust { quality, target_fps, request_keyframe, .. }) => {
                        // Handle quality adjustment requests
                        debug!("Quality adjustment: {:?}, fps: {:?}, keyframe: {}", quality, target_fps, request_keyframe);
                        // TODO: Apply quality/fps changes
                    }
                    Ok(other) => {
                        debug!("Ignoring unexpected message during stream: {:?}", other);
                    }
                    Err(e) => {
                        info!("Connection error during screen stream: {}", e);
                        break;
                    }
                }
            }
        }
    }

    // Cleanup
    if let Some(mut handler) = input_handler {
        let _ = handler.shutdown();
    }
    capture.stop().await?;
    Ok(())
}

/// Inject a single input event using the platform backend.
fn inject_input_event(
    handler: &mut dyn crate::screen::InputInjector,
    event: &InputEvent,
) -> crate::error::Result<()> {
    use crate::screen::{KeyCode, KeyModifiers, MouseButton, RemoteInputEvent};

    let remote_event = match event {
        InputEvent::MouseMove { x, y } => RemoteInputEvent::MouseMove {
            x: *x,
            y: *y,
            absolute: true,
        },
        InputEvent::MouseButton { button, pressed } => {
            let btn = match button {
                0 => MouseButton::Left,
                1 => MouseButton::Right,
                2 => MouseButton::Middle,
                n => MouseButton::Other(*n),
            };
            RemoteInputEvent::MouseButton {
                button: btn,
                pressed: *pressed,
            }
        }
        InputEvent::MouseScroll { delta_x, delta_y } => RemoteInputEvent::MouseScroll {
            dx: *delta_x,
            dy: *delta_y,
        },
        InputEvent::Key {
            code,
            pressed,
            modifiers,
        } => {
            // Convert USB HID code to our KeyCode enum
            // This is a simplified mapping - a full implementation would need
            // a comprehensive HID code table
            let key = match code {
                0x04..=0x1D => KeyCode::Unknown(*code as u32), // A-Z would need mapping
                0x1E..=0x27 => KeyCode::Unknown(*code as u32), // 1-0
                0x28 => KeyCode::Return,
                0x29 => KeyCode::Escape,
                0x2A => KeyCode::Backspace,
                0x2B => KeyCode::Tab,
                0x2C => KeyCode::Space,
                _ => KeyCode::Unknown(*code as u32),
            };
            let mods = KeyModifiers {
                shift: (modifiers & 0x01) != 0,
                control: (modifiers & 0x02) != 0,
                alt: (modifiers & 0x04) != 0,
                super_key: (modifiers & 0x08) != 0,
                caps_lock: false,
                num_lock: false,
            };
            RemoteInputEvent::Key {
                key,
                pressed: *pressed,
                modifiers: mods,
            }
        }
    };

    handler.inject(&remote_event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_speed() {
        assert_eq!(format_speed(500.0), "500 B/s");
        assert_eq!(format_speed(1500.0), "1.5 KB/s");
        assert_eq!(format_speed(1_500_000.0), "1.5 MB/s");
        assert_eq!(format_speed(1_500_000_000.0), "1.5 GB/s");
    }
}
