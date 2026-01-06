//! File transfer implementation using Iroh.
//!
//! This module provides push and pull file transfer functionality
//! between trusted peers using the Iroh control protocol.

use crate::error::{Error, Result};
use crate::iroh::blobs::{hash_file, verify_file_hash};
use crate::iroh::browse::{browse_directory, get_browsable_roots, resolve_browse_path, validate_path};
use crate::iroh::endpoint::IrohEndpoint;
use crate::iroh::protocol::{ControlMessage, DirectoryEntry, FileInfo, FileRequest};
use crate::peers::TrustedPeer;
use crate::transfer::TransferId;
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
    Started {
        transfer_id: TransferId,
    },
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
    Complete {
        transfer_id: TransferId,
    },
    /// Transfer failed.
    Failed {
        transfer_id: TransferId,
        error: String,
    },
    /// Transfer was cancelled.
    Cancelled {
        transfer_id: TransferId,
    },
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
    info!("Files hashed: {} files, {} bytes total", files.len(), total_size);

    // Parse peer node ID
    let node_id: NodeId = peer
        .endpoint_id
        .parse()
        .map_err(|e| Error::Iroh(format!("invalid node id: {}", e)))?;

    // Add peer's address information so we can connect
    // This includes the relay URL for NAT traversal
    let mut node_addr = iroh::NodeAddr::new(node_id);
    if let Some(ref relay_url) = peer.relay_url {
        if let Ok(url) = relay_url.parse() {
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
    info!("Sent push offer with {} files, {} bytes", files.len(), total_size);

    // Wait for response
    let response = conn.recv().await?;
    match response {
        ControlMessage::PushResponse {
            accepted: true, ..
        } => {
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
    if let Some(ref relay_url) = peer.relay_url {
        if let Ok(url) = relay_url.parse() {
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
/// 3. Lists directory contents
/// 4. Returns the directory listing
pub async fn handle_browse_request(
    conn: &mut crate::iroh::endpoint::ControlConnection,
    request_path: Option<String>,
    allowed_paths: Option<&[PathBuf]>,
) -> Result<()> {
    let roots = get_browsable_roots(allowed_paths);

    // Resolve the browse path
    let resolved_path = match &request_path {
        Some(p) => resolve_browse_path(p, &roots),
        None => None,
    };

    // Browse the directory
    let (path, entries) = match browse_directory(resolved_path.as_deref(), &roots, false) {
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
        ControlMessage::PullRequest {
            transfer_id,
            files,
        } => (transfer_id, files),
        _ => return Err(Error::Iroh("expected PullRequest message".to_string())),
    };

    let transfer_id = TransferId(transfer_id_str.clone());
    info!(
        "Received pull request for {} files",
        file_requests.len()
    );

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
                            let metadata = tokio::fs::metadata(&canonical).await
                                .map_err(|e| Error::Io(format!("failed to read file metadata: {}", e)))?;

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
        reason: if errors.is_empty() { None } else { Some(format!("some files skipped: {}", errors.join("; "))) },
    };
    conn.send(&response).await?;
    info!("Pull granted for {} files, {} bytes", file_infos.len(), total_size);

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
    if let Some(ref relay_url) = peer.relay_url {
        if let Ok(url) = relay_url.parse() {
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
        ControlMessage::BrowseResponse { path, entries, error: None } => {
            info!("Browse succeeded: {} entries", entries.len());
            let _ = conn.close().await;
            Ok((path, entries))
        }
        ControlMessage::BrowseResponse { error: Some(err), .. } => {
            let _ = conn.close().await;
            Err(Error::Browse(err))
        }
        _ => {
            let _ = conn.close().await;
            Err(Error::Iroh("unexpected response to browse request".to_string()))
        }
    }
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
