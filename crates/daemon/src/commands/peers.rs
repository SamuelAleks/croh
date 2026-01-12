//! Peers command - lists trusted peers.

use anyhow::Result;
use croh_core::{platform, PeerStore};

pub async fn execute() -> Result<()> {
    println!("Trusted Peers");
    println!("=============");
    println!();

    // Load peer store
    let store = PeerStore::load()?;
    let peers = store.list();

    if peers.is_empty() {
        println!("No trusted peers configured.");
        println!();
        println!("To add a trusted peer:");
        println!("  1. Use the GUI to initiate trust (Add Peer button)");
        println!("  2. Share the croc code with your peer");
        println!("  3. They run: croh-daemon receive <code>");
        println!();
        let peers_path = platform::peers_file_path();
        println!("Peers file: {:?}", peers_path);
    } else {
        for peer in peers {
            let last_seen = peer
                .last_seen
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "Never".to_string());

            println!("  {} - {}", peer.name, peer.id);
            println!(
                "    Endpoint: {}...{}",
                &peer.endpoint_id[..8.min(peer.endpoint_id.len())],
                if peer.endpoint_id.len() > 8 {
                    &peer.endpoint_id[peer.endpoint_id.len() - 8..]
                } else {
                    ""
                }
            );
            println!("    Added: {}", peer.added_at.format("%Y-%m-%d %H:%M:%S"));
            println!("    Last seen: {}", last_seen);
            println!(
                "    We granted: push={}, pull={}, browse={}",
                peer.permissions_granted.push,
                peer.permissions_granted.pull,
                peer.permissions_granted.browse
            );
            println!(
                "    They granted: push={}, pull={}, browse={}",
                peer.their_permissions.push,
                peer.their_permissions.pull,
                peer.their_permissions.browse
            );
            println!();
        }

        println!("Total: {} peer(s)", peers.len());
    }

    // Check for pending trust
    let pending_path = platform::data_dir().join("pending_trust.json");
    if pending_path.exists() {
        println!();
        println!("⚠️  Pending trust request found.");
        println!("Run 'croh-daemon run' to complete the handshake.");
    }

    Ok(())
}
