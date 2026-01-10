//! Receive command - receives files using a croc code.

use anyhow::Result;
use croh_core::{
    complete_trust_as_receiver,
    croc::{CrocEvent, CrocProcess, Progress},
    Config, CrocOptions, Identity, IrohEndpoint, PeerStore,
    trust::TrustBundle,
};
use std::path::PathBuf;
use tracing::{debug, error, info, warn};

pub async fn execute(code: &str) -> Result<()> {
    info!("Starting receive for code: {}", code);

    // Load config for download directory
    let config = Config::load_with_env()?;
    let download_dir = &config.download_dir;
    std::fs::create_dir_all(download_dir)?;

    info!("Download directory: {:?}", download_dir);

    // Build croc options
    let options = CrocOptions {
        overwrite: false,
        ..Default::default()
    };

    // Start croc process
    let (process, handle) = CrocProcess::receive(code, &options, Some(download_dir)).await?;
    let mut events = handle.events;

    // Track received files
    let mut received_files: Vec<PathBuf> = Vec::new();
    let mut current_file: Option<String> = None;

    // Process events
    while let Some(event) = events.recv().await {
        match event {
            CrocEvent::Progress(Progress {
                percentage,
                speed,
                bytes_transferred,
                total_bytes,
                current_file: file,
            }) => {
                if let Some(ref f) = file {
                    if current_file.as_ref() != Some(f) {
                        current_file = Some(f.clone());
                        println!("Receiving: {}", f);
                    }
                }
                if let (Some(transferred), Some(total)) = (bytes_transferred, total_bytes) {
                    println!(
                        "Progress: {:.1}% ({}/{}) - {}",
                        percentage, transferred, total, speed
                    );
                } else {
                    println!("Progress: {:.1}% - {}", percentage, speed);
                }
            }
            CrocEvent::Completed => {
                info!("Transfer complete");
                println!("Transfer complete!");

                // Get the list of files in download directory
                // (croc doesn't give us the exact list, so we use what we tracked)
                if let Some(filename) = current_file.take() {
                    received_files.push(download_dir.join(&filename));
                }

                // Check each file for trust bundle
                for file in &received_files {
                    if let Some(bundle) = check_for_trust_bundle(file).await {
                        handle_trust_bundle(bundle, file).await;
                    }
                }
                break;
            }
            CrocEvent::Failed(err) => {
                error!("Transfer failed: {}", err);
                eprintln!("Error: {}", err);
                return Err(anyhow::anyhow!("Transfer failed: {}", err));
            }
            CrocEvent::Output(line) => {
                debug!("croc: {}", line);

                // Try to detect file completion from output
                if line.contains("100%") {
                    if let Some(filename) = current_file.take() {
                        received_files.push(download_dir.join(&filename));
                    }
                }
            }
            CrocEvent::CodeReady(_) => {
                // Not relevant for receive
            }
            CrocEvent::Started => {
                info!("Croc process started");
            }
            CrocEvent::Waiting => {
                println!("Connecting...");
            }
        }
    }

    // Wait for process to finish
    drop(process); // Process will be cleaned up when dropped

    Ok(())
}

/// Check if a received file is a trust bundle.
async fn check_for_trust_bundle(path: &PathBuf) -> Option<TrustBundle> {
    // Check filename first
    if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
        if !TrustBundle::is_trust_bundle_filename(filename) {
            return None;
        }
    }

    // Try to parse the file as a trust bundle
    match TrustBundle::load(path) {
        Ok(bundle) => {
            info!("Detected trust bundle from: {}", bundle.sender.name);
            Some(bundle)
        }
        Err(e) => {
            debug!("File matched trust bundle name but failed to parse: {}", e);
            None
        }
    }
}

/// Handle a received trust bundle - initiate Iroh handshake.
async fn handle_trust_bundle(bundle: TrustBundle, bundle_path: &PathBuf) {
    println!();
    println!("╔══════════════════════════════════════════╗");
    println!("║     Trust Bundle Received!               ║");
    println!("╚══════════════════════════════════════════╝");
    println!();
    println!("  From: {}", bundle.sender.name);
    println!(
        "  Endpoint: {}...{}",
        &bundle.sender.endpoint_id[..8.min(bundle.sender.endpoint_id.len())],
        if bundle.sender.endpoint_id.len() > 8 {
            &bundle.sender.endpoint_id[bundle.sender.endpoint_id.len() - 8..]
        } else {
            ""
        }
    );
    println!(
        "  Capabilities: {}",
        bundle
            .capabilities_offered
            .iter()
            .map(|c| format!("{:?}", c).to_lowercase())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!();

    // Check if bundle has expired
    if !bundle.is_valid() {
        warn!("Trust bundle has expired");
        println!("⚠️  This trust bundle has expired. Please request a new one.");
        // Clean up the bundle file
        let _ = std::fs::remove_file(bundle_path);
        return;
    }

    println!("📡 Initiating secure connection...");
    println!();

    // Load or create our identity
    let identity = match Identity::load_or_create() {
        Ok(id) => id,
        Err(e) => {
            error!("Failed to load identity: {}", e);
            println!("Error: Failed to load identity: {}", e);
            return;
        }
    };

    // Create an Iroh endpoint
    let endpoint = match IrohEndpoint::new(identity.clone()).await {
        Ok(ep) => ep,
        Err(e) => {
            error!("Failed to create Iroh endpoint: {}", e);
            println!("Error: Failed to create network endpoint: {}", e);
            return;
        }
    };

    println!("Connecting to {}...", bundle.sender.name);

    // Perform the handshake
    match complete_trust_as_receiver(&endpoint, &bundle, &identity).await {
        Ok(result) => {
            println!();
            println!("✅ Trust established with {}!", result.peer.name);
            println!();

            // Add to peer store
            let mut peer_store = match PeerStore::load() {
                Ok(store) => store,
                Err(e) => {
                    error!("Failed to load peer store: {}", e);
                    println!("Warning: Failed to save peer - {}", e);
                    // Still clean up and return success
                    let _ = std::fs::remove_file(bundle_path);
                    endpoint.close().await;
                    return;
                }
            };

            if let Err(e) = peer_store.add(result.peer.clone()) {
                error!("Failed to save peer: {}", e);
                println!("Warning: Failed to save peer - {}", e);
            } else {
                println!("Peer {} added to trusted peers.", result.peer.name);
            }
        }
        Err(e) => {
            error!("Handshake failed: {}", e);
            println!();
            println!("❌ Failed to establish trust: {}", e);
            println!();
            println!("The sender may need to re-initiate the trust request.");
        }
    }

    // Clean up
    endpoint.close().await;

    // Clean up the bundle file (it contains sensitive connection info)
    if let Err(e) = std::fs::remove_file(bundle_path) {
        warn!("Failed to remove trust bundle file: {}", e);
    }
}
