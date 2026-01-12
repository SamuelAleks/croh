//! Iroh Integration Tests
//!
//! This test suite validates the Iroh peer-to-peer functionality.
//!
//! # Testing Architecture
//!
//! The Iroh integration tests are organized into multiple levels:
//!
//! ## Unit Tests (in-module)
//! Located in each module's `#[cfg(test)]` section, these test individual
//! functions without network connectivity.
//!
//! ## Integration Tests (this file)
//! These tests verify the integration between modules but may require
//! specific network conditions to pass.
//!
//! ## Relay-Enabled Tests (feature-gated)
//! For comprehensive testing including relay functionality, enable the
//! `test-relay` feature which provides a local relay server.
//!
//! # Running Tests
//!
//! ```bash
//! # Run unit tests only (fast, no network required)
//! cargo test -p croh-core --lib
//!
//! # Run integration tests (may require network)
//! cargo test -p croh-core --test iroh_integration
//!
//! # Run with relay testing (comprehensive)
//! cargo test -p croh-core --features test-relay
//!
//! # Run ignored tests (network-dependent)
//! cargo test -p croh-core -- --ignored
//! ```
//!
//! # Test Infrastructure
//!
//! The test support module (`croh_core::iroh::test_support`) provides:
//! - `TestEndpoint`: Relay-disabled endpoint wrapper
//! - `EndpointPair`: Pre-configured pair of endpoints
//! - `TestFixtures`: Temporary file/directory utilities

mod common;

use croh_core::iroh::identity::Identity;
use croh_core::iroh::protocol::{ControlMessage, ALPN_CONTROL};
use croh_core::iroh::{hash_file, verify_file_hash};
use croh_core::peers::{PeerStore, Permissions, TrustedPeer};
use croh_core::trust::TrustBundle;
use tempfile::TempDir;

/// Test identity generation
#[tokio::test]
async fn test_identity_generation() {
    // Generate a new identity
    let identity = Identity::generate("TestDevice".to_string()).unwrap();

    // Validate fields
    assert!(!identity.endpoint_id.is_empty());
    assert_eq!(identity.name, "TestDevice");

    // Secret key should be available
    let secret_key = identity.secret_key();
    assert!(!secret_key.to_bytes().is_empty());
}

/// Test trust bundle creation and validation
#[tokio::test]
async fn test_trust_bundle_lifecycle() {
    let identity = Identity::generate("Sender".to_string()).unwrap();

    // Create a trust bundle
    let bundle = TrustBundle::new(&identity);

    // Should be valid immediately
    assert!(bundle.is_valid());
    assert!(!bundle.nonce.is_empty());
    assert_eq!(bundle.sender.endpoint_id, identity.endpoint_id);

    // Serialize and deserialize
    let json = serde_json::to_string(&bundle).unwrap();
    let parsed: TrustBundle = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.nonce, bundle.nonce);
    assert!(TrustBundle::is_trust_bundle(&json));
}

/// Test peer store operations
#[tokio::test]
async fn test_peer_store_lifecycle() {
    // Create a peer
    let peer = TrustedPeer::new(
        "test-endpoint-id".to_string(),
        "Test Peer".to_string(),
        Permissions::all(),
        Permissions::all(),
    );
    let peer_id = peer.id.clone();

    // Create store and add peer
    let mut store = PeerStore::default();
    store.add(peer).unwrap();

    // Should find by endpoint ID
    assert!(store.find_by_endpoint_id("test-endpoint-id").is_some());

    // Should find by ID
    assert!(store.find_by_id(&peer_id).is_some());
}

/// Test control message serialization
#[tokio::test]
async fn test_control_message_encoding() {
    let messages = vec![
        ControlMessage::Ping {
            timestamp: 12345678,
        },
        ControlMessage::Pong {
            timestamp: 12345678,
        },
        ControlMessage::StatusRequest,
        ControlMessage::TrustComplete,
    ];

    for msg in messages {
        let encoded = msg.encode().unwrap();
        // Encoded format: 4-byte length prefix + JSON bytes
        assert!(encoded.len() > 4);

        // Skip the 4-byte length prefix for decoding
        let json_bytes = &encoded[4..];
        let decoded = ControlMessage::decode(json_bytes).unwrap();

        // Re-encode should match
        let re_encoded = decoded.encode().unwrap();
        assert_eq!(encoded, re_encoded);
    }
}

/// Test file hashing and verification
#[tokio::test]
async fn test_file_hashing() {
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.txt");

    // Create test file
    std::fs::write(&test_file, b"Hello, World!").unwrap();

    // Hash it
    let hash = hash_file(&test_file).await.unwrap();
    assert!(!hash.is_empty());

    // Verification should pass
    assert!(verify_file_hash(&test_file, &hash).await.unwrap());

    // Wrong hash should fail
    assert!(!verify_file_hash(&test_file, "wrong_hash").await.unwrap());
}

/// Test permissions from capabilities conversion
#[test]
fn test_permissions_conversion() {
    use croh_core::trust::Capability;

    let caps = vec![Capability::Push, Capability::Browse];
    let perms = Permissions::from_capabilities(&caps);

    assert!(perms.push);
    assert!(!perms.pull);
    assert!(perms.browse);
    assert!(!perms.status);

    // Convert back
    let back = perms.to_capabilities();
    assert!(back.contains(&Capability::Push));
    assert!(back.contains(&Capability::Browse));
    assert!(!back.contains(&Capability::Pull));
}

/// Test ALPN constant is valid
#[test]
fn test_alpn_constant() {
    // ALPN should be a valid UTF-8 string
    let alpn_str = std::str::from_utf8(ALPN_CONTROL).unwrap();
    assert!(alpn_str.starts_with("croh/"));
}

// ============================================================================
// Network-dependent tests below (marked as ignored by default)
// ============================================================================

/// Full trust handshake between two endpoints.
///
/// This test requires network connectivity and working UDP/QUIC.
/// Run with: `cargo test -- --ignored`
#[tokio::test]
#[ignore = "requires specific network conditions"]
async fn test_full_trust_handshake() {
    // This would use EndpointPair from test_support
    // Currently placeholder until relay testing is enabled
    todo!("Implement with test-relay feature");
}

/// File push between two endpoints.
///
/// This test requires network connectivity and working UDP/QUIC.
/// Run with: `cargo test -- --ignored`
#[tokio::test]
#[ignore = "requires specific network conditions"]
async fn test_file_push() {
    // This would use EndpointPair and push_files from transfer module
    todo!("Implement with test-relay feature");
}

/// File pull between two endpoints.
///
/// This test requires network connectivity and working UDP/QUIC.
/// Run with: `cargo test -- --ignored`
#[tokio::test]
#[ignore = "requires specific network conditions"]
async fn test_file_pull() {
    // This would use EndpointPair and pull_files from transfer module
    todo!("Implement with test-relay feature");
}

/// Remote directory browsing.
///
/// This test requires network connectivity and working UDP/QUIC.
/// Run with: `cargo test -- --ignored`
#[tokio::test]
#[ignore = "requires specific network conditions"]
async fn test_browse_remote() {
    // This would use EndpointPair and browse_remote from transfer module
    todo!("Implement with test-relay feature");
}
