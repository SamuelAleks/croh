//! Speed test functionality for measuring peer connection quality.
//!
//! Provides bidirectional throughput and latency measurement between trusted peers.

use crate::error::{Error, Result};
use crate::iroh::endpoint::ControlConnection;
use crate::iroh::protocol::ControlMessage;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::time::Instant;
use tracing::{debug, info, warn};

/// Default size for speed test data (10 MB).
pub const DEFAULT_TEST_SIZE: u64 = 10 * 1024 * 1024;

/// Chunk size for sending data (64 KB).
const CHUNK_SIZE: usize = 64 * 1024;

/// Result of a speed test.
#[derive(Debug, Clone)]
pub struct SpeedTestResult {
    /// Upload speed in bytes per second (initiator -> responder).
    pub upload_speed: u64,
    /// Download speed in bytes per second (responder -> initiator).
    pub download_speed: u64,
    /// Round-trip latency in milliseconds.
    pub latency_ms: u64,
    /// Total bytes transferred (both directions).
    pub bytes_transferred: u64,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
}

impl SpeedTestResult {
    /// Format upload speed as human-readable string.
    pub fn upload_speed_formatted(&self) -> String {
        format_speed(self.upload_speed)
    }

    /// Format download speed as human-readable string.
    pub fn download_speed_formatted(&self) -> String {
        format_speed(self.download_speed)
    }
}

/// Format bytes per second as human-readable speed.
fn format_speed(bytes_per_sec: u64) -> String {
    if bytes_per_sec >= 1_000_000_000 {
        format!("{:.2} GB/s", bytes_per_sec as f64 / 1_000_000_000.0)
    } else if bytes_per_sec >= 1_000_000 {
        format!("{:.2} MB/s", bytes_per_sec as f64 / 1_000_000.0)
    } else if bytes_per_sec >= 1_000 {
        format!("{:.2} KB/s", bytes_per_sec as f64 / 1_000.0)
    } else {
        format!("{} B/s", bytes_per_sec)
    }
}

/// Generate a unique test ID.
pub fn generate_test_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Initiator side: run a speed test with a peer.
///
/// This function:
/// 1. Sends SpeedTestRequest
/// 2. Waits for SpeedTestAccept/Reject
/// 3. Sends test data (upload test)
/// 4. Receives test data back (download test)
/// 5. Returns the results
pub async fn run_speed_test(conn: &mut ControlConnection, size: u64) -> Result<SpeedTestResult> {
    let test_id = generate_test_id();
    let overall_start = Instant::now();

    info!("Starting speed test {} with {} bytes", test_id, size);

    // Step 1: Send request
    conn.send(&ControlMessage::SpeedTestRequest {
        test_id: test_id.clone(),
        size,
    })
    .await?;

    // Step 2: Wait for accept/reject
    let msg = conn.recv().await?;
    match msg {
        ControlMessage::SpeedTestAccept { test_id: tid } => {
            if tid != test_id {
                return Err(Error::Iroh("test_id mismatch".to_string()));
            }
        }
        ControlMessage::SpeedTestReject { test_id: _, reason } => {
            return Err(Error::Iroh(format!("Speed test rejected: {}", reason)));
        }
        _ => {
            return Err(Error::Iroh(
                "Unexpected message during speed test".to_string(),
            ));
        }
    }

    // Step 3: Measure latency with a ping
    let ping_start = Instant::now();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    conn.send(&ControlMessage::Ping { timestamp }).await?;

    let pong = conn.recv().await?;
    match pong {
        ControlMessage::Pong { timestamp: ts } if ts == timestamp => {}
        _ => {
            return Err(Error::Iroh("Invalid pong response".to_string()));
        }
    }
    let latency_ms = ping_start.elapsed().as_millis() as u64;
    debug!("Latency: {} ms", latency_ms);

    // Step 4: Upload test - send data chunks
    let upload_start = Instant::now();
    let mut bytes_sent: u64 = 0;
    let mut sequence: u32 = 0;

    // Generate random data for each chunk
    let chunk_data = vec![0xABu8; CHUNK_SIZE];
    let chunk_base64 = BASE64.encode(&chunk_data);

    while bytes_sent < size {
        let remaining = size - bytes_sent;
        let chunk_size = remaining.min(CHUNK_SIZE as u64);

        // For the last chunk, truncate if needed
        let data = if chunk_size < CHUNK_SIZE as u64 {
            let partial_data = vec![0xABu8; chunk_size as usize];
            BASE64.encode(&partial_data)
        } else {
            chunk_base64.clone()
        };

        conn.send(&ControlMessage::SpeedTestData {
            test_id: test_id.clone(),
            sequence,
            data,
        })
        .await?;

        bytes_sent += chunk_size;
        sequence += 1;
    }

    let upload_duration = upload_start.elapsed();
    let upload_speed = if upload_duration.as_millis() > 0 {
        (bytes_sent as f64 / upload_duration.as_secs_f64()) as u64
    } else {
        bytes_sent
    };
    debug!(
        "Upload: {} bytes in {:?} = {}",
        bytes_sent,
        upload_duration,
        format_speed(upload_speed)
    );

    // Step 5: Download test - receive data back from responder
    let download_start = Instant::now();
    let mut bytes_received: u64 = 0;

    while bytes_received < size {
        let msg = conn.recv().await?;
        match msg {
            ControlMessage::SpeedTestData {
                test_id: tid, data, ..
            } => {
                if tid != test_id {
                    warn!("Received data with wrong test_id");
                    continue;
                }
                let decoded = BASE64
                    .decode(&data)
                    .map_err(|e| Error::Iroh(format!("Invalid base64: {}", e)))?;
                bytes_received += decoded.len() as u64;
            }
            _ => {
                return Err(Error::Iroh(
                    "Unexpected message during download".to_string(),
                ));
            }
        }
    }

    let download_duration = download_start.elapsed();
    let download_speed = if download_duration.as_millis() > 0 {
        (bytes_received as f64 / download_duration.as_secs_f64()) as u64
    } else {
        bytes_received
    };
    debug!(
        "Download: {} bytes in {:?} = {}",
        bytes_received,
        download_duration,
        format_speed(download_speed)
    );

    // Step 6: Send/receive completion
    let total_duration = overall_start.elapsed();
    let result = SpeedTestResult {
        upload_speed,
        download_speed,
        latency_ms,
        bytes_transferred: bytes_sent + bytes_received,
        duration_ms: total_duration.as_millis() as u64,
    };

    conn.send(&ControlMessage::SpeedTestComplete {
        test_id: test_id.clone(),
        upload_speed: result.upload_speed,
        download_speed: result.download_speed,
        latency_ms: result.latency_ms,
        bytes_transferred: result.bytes_transferred,
        duration_ms: result.duration_ms,
    })
    .await?;

    info!(
        "Speed test {} complete: upload={}, download={}, latency={}ms",
        test_id,
        result.upload_speed_formatted(),
        result.download_speed_formatted(),
        result.latency_ms
    );

    Ok(result)
}

/// Responder side: handle an incoming speed test request.
///
/// This function should be called when a SpeedTestRequest is received.
/// It:
/// 1. Sends SpeedTestAccept
/// 2. Receives test data (upload from initiator's perspective)
/// 3. Sends test data back (download from initiator's perspective)
/// 4. Waits for SpeedTestComplete
pub async fn handle_speed_test_request(
    conn: &mut ControlConnection,
    test_id: String,
    size: u64,
) -> Result<SpeedTestResult> {
    info!(
        "Handling speed test request {} with {} bytes",
        test_id, size
    );

    // Step 1: Accept the test
    conn.send(&ControlMessage::SpeedTestAccept {
        test_id: test_id.clone(),
    })
    .await?;

    // Step 2: Handle ping for latency measurement
    let msg = conn.recv().await?;
    match msg {
        ControlMessage::Ping { timestamp } => {
            conn.send(&ControlMessage::Pong { timestamp }).await?;
        }
        _ => {
            return Err(Error::Iroh("Expected Ping message".to_string()));
        }
    }

    // Step 3: Receive upload data from initiator
    let mut bytes_received: u64 = 0;

    while bytes_received < size {
        let msg = conn.recv().await?;
        match msg {
            ControlMessage::SpeedTestData {
                test_id: tid, data, ..
            } => {
                if tid != test_id {
                    warn!("Received data with wrong test_id");
                    continue;
                }
                let decoded = BASE64
                    .decode(&data)
                    .map_err(|e| Error::Iroh(format!("Invalid base64: {}", e)))?;
                bytes_received += decoded.len() as u64;
            }
            _ => {
                return Err(Error::Iroh(
                    "Unexpected message during upload receive".to_string(),
                ));
            }
        }
    }
    debug!("Received {} bytes from initiator", bytes_received);

    // Step 4: Send data back to initiator (for download test)
    let chunk_data = vec![0xCDu8; CHUNK_SIZE];
    let chunk_base64 = BASE64.encode(&chunk_data);
    let mut bytes_sent: u64 = 0;
    let mut sequence: u32 = 0;

    while bytes_sent < size {
        let remaining = size - bytes_sent;
        let chunk_size = remaining.min(CHUNK_SIZE as u64);

        let data = if chunk_size < CHUNK_SIZE as u64 {
            let partial_data = vec![0xCDu8; chunk_size as usize];
            BASE64.encode(&partial_data)
        } else {
            chunk_base64.clone()
        };

        conn.send(&ControlMessage::SpeedTestData {
            test_id: test_id.clone(),
            sequence,
            data,
        })
        .await?;

        bytes_sent += chunk_size;
        sequence += 1;
    }
    debug!("Sent {} bytes back to initiator", bytes_sent);

    // Step 5: Wait for completion message with results
    let msg = conn.recv().await?;
    match msg {
        ControlMessage::SpeedTestComplete {
            test_id: tid,
            upload_speed,
            download_speed,
            latency_ms,
            bytes_transferred,
            duration_ms,
        } => {
            if tid != test_id {
                return Err(Error::Iroh("test_id mismatch in completion".to_string()));
            }

            let result = SpeedTestResult {
                upload_speed,
                download_speed,
                latency_ms,
                bytes_transferred,
                duration_ms,
            };

            info!(
                "Speed test {} handled: upload={}, download={}, latency={}ms",
                test_id,
                result.upload_speed_formatted(),
                result.download_speed_formatted(),
                result.latency_ms
            );

            Ok(result)
        }
        _ => Err(Error::Iroh(
            "Expected SpeedTestComplete message".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_speed() {
        assert_eq!(format_speed(500), "500 B/s");
        assert_eq!(format_speed(1500), "1.50 KB/s");
        assert_eq!(format_speed(1_500_000), "1.50 MB/s");
        assert_eq!(format_speed(1_500_000_000), "1.50 GB/s");
    }

    #[test]
    fn test_generate_test_id() {
        let id1 = generate_test_id();
        let id2 = generate_test_id();
        assert_ne!(id1, id2);
        assert!(!id1.is_empty());
    }
}
