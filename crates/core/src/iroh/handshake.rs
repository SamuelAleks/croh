//! Trust handshake protocol implementation.
//!
//! After receiving a trust bundle, the receiver connects to the sender's
//! Iroh endpoint and performs a mutual trust handshake:
//!
//! 1. Receiver connects to sender's endpoint
//! 2. Receiver sends TrustConfirm with the nonce from the bundle
//! 3. Sender verifies the nonce and responds with TrustComplete
//! 4. Both sides add each other as trusted peers

use crate::error::{Error, Result};
use crate::iroh::{ControlConnection, ControlMessage, Identity, IrohEndpoint, PeerAddress};
use crate::peers::{PeerStore, Permissions, TrustedPeer};
use crate::trust::{PeerInfo, TrustBundle};
use chrono::Duration as ChronoDuration;
use iroh::NodeId;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Timeout for handshake operations.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of a trust handshake.
#[derive(Debug)]
pub struct HandshakeResult {
    /// The newly trusted peer
    pub peer: TrustedPeer,
    /// Whether we initiated the handshake (sent the trust bundle)
    pub is_initiator: bool,
}

/// Complete the trust handshake as the receiver of a trust bundle.
///
/// This connects to the sender's Iroh endpoint and confirms the trust.
pub async fn complete_trust_as_receiver(
    endpoint: &IrohEndpoint,
    bundle: &TrustBundle,
    our_identity: &Identity,
) -> Result<HandshakeResult> {
    info!(
        "Completing trust handshake with {} ({})",
        bundle.sender.name, bundle.sender.endpoint_id
    );

    // Parse the sender's endpoint ID
    let sender_node_id: NodeId = bundle
        .sender
        .endpoint_id
        .parse()
        .map_err(|e| Error::Iroh(format!("invalid sender endpoint_id: {}", e)))?;

    // Add the sender's address information to our endpoint so we can connect
    // This includes the relay URL if provided in the trust bundle
    let mut node_addr = iroh::NodeAddr::new(sender_node_id);
    if let Some(ref relay_url) = bundle.sender.relay_url {
        if let Ok(url) = relay_url.parse() {
            node_addr = node_addr.with_relay_url(url);
            info!("Using relay URL: {}", relay_url);
        }
    }
    endpoint.add_node_addr(node_addr)?;

    // Connect to the sender
    let mut conn =
        tokio::time::timeout(HANDSHAKE_TIMEOUT, endpoint.connect_to_node(sender_node_id))
            .await
            .map_err(|_| Error::Iroh("connection timeout".to_string()))??;

    info!("Connected to sender, sending TrustConfirm");

    // Get our relay URL to include in the confirm message
    // This allows the initiator to connect back to us later
    let our_relay_url = endpoint.relay_url().map(|u| u.to_string());
    if our_relay_url.is_some() {
        info!(
            "Including our relay URL in TrustConfirm: {:?}",
            our_relay_url
        );
    }

    // Send TrustConfirm with the nonce and our info (including relay URL)
    let confirm_msg = ControlMessage::TrustConfirm {
        peer: our_identity.to_peer_info_with_relay(our_relay_url),
        nonce: bundle.nonce.clone(),
        permissions: Permissions::all(), // Offer all permissions
    };
    conn.send(&confirm_msg).await?;

    // Wait for TrustComplete response
    let response = tokio::time::timeout(HANDSHAKE_TIMEOUT, conn.recv())
        .await
        .map_err(|_| Error::Iroh("timeout waiting for TrustComplete".to_string()))??;

    match response {
        ControlMessage::TrustComplete => {
            info!(
                "Trust handshake completed successfully with {} (guest: {})",
                bundle.sender.name, bundle.guest_mode
            );

            // Create the peer from the bundle's sender info
            // Use guest or trusted depending on bundle settings
            let peer = if bundle.guest_mode {
                // Create a guest peer with time-limited access
                let duration_hours = bundle.guest_duration_hours.unwrap_or(24);
                let duration = ChronoDuration::hours(duration_hours as i64);

                // Build address from relay URL
                let address = PeerAddress::Iroh {
                    relay_url: bundle.sender.relay_url.clone(),
                    direct_addrs: vec![],
                };

                info!("Creating guest peer with {} hour duration", duration_hours);

                TrustedPeer::new_guest(
                    bundle.sender.endpoint_id.clone(),
                    bundle.sender.name.clone(),
                    // We grant permissions based on what they offered (limited for guests)
                    Permissions::from_capabilities(&bundle.capabilities_offered),
                    // They get guest-level permissions
                    Permissions::guest_default(),
                    address,
                    duration,
                )
            } else {
                // Create a fully trusted peer
                TrustedPeer::new_with_relay(
                    bundle.sender.endpoint_id.clone(),
                    bundle.sender.name.clone(),
                    // We grant permissions based on what they offered
                    Permissions::from_capabilities(&bundle.capabilities_offered),
                    // They get all permissions since we sent all in TrustConfirm
                    Permissions::all(),
                    // Store their relay URL for future connections
                    bundle.sender.relay_url.clone(),
                )
            };

            // Close the connection gracefully
            if let Err(e) = conn.close().await {
                warn!("Failed to close connection gracefully: {}", e);
            }

            Ok(HandshakeResult {
                peer,
                is_initiator: false,
            })
        }
        ControlMessage::TrustRevoke { reason } => {
            Err(Error::Trust(format!("trust revoked: {}", reason)))
        }
        other => {
            error!("Unexpected message during handshake: {:?}", other);
            Err(Error::Iroh(format!(
                "unexpected message: expected TrustComplete, got {:?}",
                other
            )))
        }
    }
}

/// Handle an incoming trust confirmation (as the initiator/sender).
///
/// This is called when a receiver connects to confirm the trust bundle we sent.
/// The `pending_bundle` parameter should be the bundle we sent, so we know if
/// this is a guest trust relationship.
pub async fn handle_trust_confirm(
    conn: &mut ControlConnection,
    their_peer_info: &PeerInfo,
    their_permissions: &Permissions,
    pending_bundle: Option<&TrustBundle>,
) -> Result<HandshakeResult> {
    info!(
        "Handling TrustConfirm from {} ({})",
        their_peer_info.name,
        conn.remote_id_string()
    );

    // Send TrustComplete response
    let response = ControlMessage::TrustComplete;
    conn.send(&response).await?;

    // Give the message time to be delivered before the connection closes
    // QUIC may not deliver data if the endpoint closes too quickly
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Determine if this is a guest trust relationship
    let is_guest = pending_bundle.map(|b| b.guest_mode).unwrap_or(false);

    // Create the peer, storing their relay URL for future connections
    let peer = if is_guest {
        // Create a guest peer with time-limited access
        let duration_hours = pending_bundle
            .and_then(|b| b.guest_duration_hours)
            .unwrap_or(24);
        let duration = ChronoDuration::hours(duration_hours as i64);

        // Build address from relay URL
        let address = PeerAddress::Iroh {
            relay_url: their_peer_info.relay_url.clone(),
            direct_addrs: vec![],
        };

        info!(
            "Creating guest peer {} with {} hour duration",
            their_peer_info.name, duration_hours
        );

        TrustedPeer::new_guest(
            their_peer_info.endpoint_id.clone(),
            their_peer_info.name.clone(),
            // Grant guest-level permissions
            Permissions::guest_default(),
            // Their permissions to us (what they offered in TrustConfirm)
            their_permissions.clone(),
            address,
            duration,
        )
    } else {
        // Create a fully trusted peer
        TrustedPeer::new_with_relay(
            their_peer_info.endpoint_id.clone(),
            their_peer_info.name.clone(),
            // We grant all permissions (they get what we offered in the bundle)
            Permissions::all(),
            // Their permissions to us (what they offered in TrustConfirm)
            their_permissions.clone(),
            // Store their relay URL for future connections
            their_peer_info.relay_url.clone(),
        )
    };

    Ok(HandshakeResult {
        peer,
        is_initiator: true,
    })
}

/// Accept incoming connections and handle trust handshakes.
///
/// This runs in a loop accepting connections and processing trust confirmations.
/// The `pending_bundle` parameter should be the bundle we sent, used to verify
/// the nonce and determine if this is a guest trust relationship.
pub async fn accept_trust_connections(
    endpoint: &IrohEndpoint,
    pending_bundle: Option<&TrustBundle>,
    peer_store: &mut PeerStore,
) -> Result<()> {
    info!("Waiting for incoming trust connections...");

    loop {
        // Accept a connection
        let mut conn = match tokio::time::timeout(HANDSHAKE_TIMEOUT, endpoint.accept()).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(e)) => {
                debug!("Accept error (may be normal shutdown): {}", e);
                break;
            }
            Err(_) => {
                debug!("No incoming connections within timeout");
                continue;
            }
        };

        let remote_id = conn.remote_id_string();
        info!("Accepted connection from: {}", remote_id);

        // Receive the first message
        let msg = match tokio::time::timeout(HANDSHAKE_TIMEOUT, conn.recv()).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                warn!("Failed to receive message from {}: {}", remote_id, e);
                continue;
            }
            Err(_) => {
                warn!("Timeout waiting for message from {}", remote_id);
                continue;
            }
        };

        match msg {
            ControlMessage::TrustConfirm {
                peer: their_peer_info,
                nonce,
                permissions: their_permissions,
            } => {
                // Verify the nonce if we have a pending trust
                let nonce_valid = pending_bundle
                    .as_ref()
                    .map(|b| b.nonce == nonce)
                    .unwrap_or(false);

                if !nonce_valid {
                    warn!("Invalid nonce from {}: {}", remote_id, nonce);
                    // Send TrustRevoke for invalid nonce
                    let response = ControlMessage::TrustRevoke {
                        reason: "invalid nonce".to_string(),
                    };
                    let _ = conn.send(&response).await;
                    continue;
                }

                info!(
                    "Valid TrustConfirm from {} ({})",
                    their_peer_info.name, remote_id
                );

                // Handle the trust confirmation, passing the bundle so we know if it's a guest
                match handle_trust_confirm(
                    &mut conn,
                    &their_peer_info,
                    &their_permissions,
                    pending_bundle,
                )
                .await
                {
                    Ok(result) => {
                        info!("Trust established with {}", result.peer.name);

                        // Add to peer store (or update if exists)
                        match peer_store.add_or_update(result.peer.clone()) {
                            Ok(updated) => {
                                if updated {
                                    info!("Peer {} updated in store", result.peer.name);
                                } else {
                                    info!("Peer {} added to store", result.peer.name);
                                }
                            }
                            Err(e) => {
                                error!("Failed to save peer: {}", e);
                            }
                        }

                        // Close connection
                        let _ = conn.close().await;

                        // Exit after successful handshake
                        return Ok(());
                    }
                    Err(e) => {
                        error!("Handshake failed: {}", e);
                        continue;
                    }
                }
            }
            ControlMessage::Ping { timestamp } => {
                // Handle ping from already-trusted peer
                debug!("Received ping from {}", remote_id);
                let response = ControlMessage::Pong { timestamp };
                let _ = conn.send(&response).await;
            }
            ControlMessage::StatusRequest => {
                // Handle status request from trusted peer
                debug!("Received status request from {}", remote_id);
                let response = ControlMessage::StatusResponse {
                    hostname: hostname::get()
                        .map(|h| h.to_string_lossy().to_string())
                        .unwrap_or_else(|_| "unknown".to_string()),
                    os: std::env::consts::OS.to_string(),
                    free_space: 0,
                    total_space: 0,
                    download_dir: "".to_string(),
                    uptime: 0,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    active_transfers: 0,
                };
                let _ = conn.send(&response).await;
            }
            other => {
                debug!(
                    "Ignoring unexpected message from {}: {:?}",
                    remote_id, other
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    // Integration tests require network connectivity
    // Unit tests for the protocol logic are in protocol.rs
}
