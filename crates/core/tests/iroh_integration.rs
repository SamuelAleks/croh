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

// ============================================================================
// Screen Streaming Integration Tests
// ============================================================================

use croh_core::iroh::protocol::{
    DisplayInfo, FrameMetadata, ScreenCompression, ScreenQuality,
};
use croh_core::screen::{
    viewer_event_channel, ScreenViewer, ViewerConfig, ViewerEvent, ViewerState,
};

/// Test screen streaming protocol message serialization
#[tokio::test]
async fn test_screen_streaming_messages() {
    // Test ScreenStreamRequest
    let request = ControlMessage::ScreenStreamRequest {
        stream_id: "test-stream-123".to_string(),
        display_id: Some("display-0".to_string()),
        compression: ScreenCompression::H264,
        quality: ScreenQuality::Quality,
        target_fps: 60,
    };

    let encoded = request.encode().unwrap();
    let json_bytes = &encoded[4..];
    let decoded = ControlMessage::decode(json_bytes).unwrap();

    match decoded {
        ControlMessage::ScreenStreamRequest {
            stream_id,
            display_id,
            compression,
            quality,
            target_fps,
        } => {
            assert_eq!(stream_id, "test-stream-123");
            assert_eq!(display_id, Some("display-0".to_string()));
            assert_eq!(compression, ScreenCompression::H264);
            assert_eq!(quality, ScreenQuality::Quality);
            assert_eq!(target_fps, 60);
        }
        _ => panic!("Wrong message type decoded"),
    }

    // Test ScreenStreamResponse
    let displays = vec![
        DisplayInfo {
            id: "disp-0".to_string(),
            name: "Primary Monitor".to_string(),
            width: 1920,
            height: 1080,
            refresh_rate: Some(60),
            is_primary: true,
        },
        DisplayInfo {
            id: "disp-1".to_string(),
            name: "Secondary Monitor".to_string(),
            width: 2560,
            height: 1440,
            refresh_rate: Some(144),
            is_primary: false,
        },
    ];

    let response = ControlMessage::ScreenStreamResponse {
        stream_id: "test-stream-123".to_string(),
        accepted: true,
        reason: None,
        displays: displays.clone(),
        compression: Some(ScreenCompression::H264),
    };

    let encoded = response.encode().unwrap();
    let json_bytes = &encoded[4..];
    let decoded = ControlMessage::decode(json_bytes).unwrap();

    match decoded {
        ControlMessage::ScreenStreamResponse {
            stream_id,
            accepted,
            displays: decoded_displays,
            compression,
            ..
        } => {
            assert_eq!(stream_id, "test-stream-123");
            assert!(accepted);
            assert_eq!(decoded_displays.len(), 2);
            assert_eq!(decoded_displays[0].name, "Primary Monitor");
            assert!(decoded_displays[0].is_primary);
            assert_eq!(compression, Some(ScreenCompression::H264));
        }
        _ => panic!("Wrong message type decoded"),
    }

    // Test ScreenStreamAdjust
    let adjust = ControlMessage::ScreenStreamAdjust {
        stream_id: "test-stream-123".to_string(),
        quality: Some(ScreenQuality::Fast),
        target_fps: Some(15),
        request_keyframe: true,
    };

    let encoded = adjust.encode().unwrap();
    let json_bytes = &encoded[4..];
    let decoded = ControlMessage::decode(json_bytes).unwrap();

    match decoded {
        ControlMessage::ScreenStreamAdjust {
            quality,
            target_fps,
            request_keyframe,
            ..
        } => {
            assert_eq!(quality, Some(ScreenQuality::Fast));
            assert_eq!(target_fps, Some(15));
            assert!(request_keyframe);
        }
        _ => panic!("Wrong message type decoded"),
    }
}

/// Test frame metadata serialization
#[tokio::test]
async fn test_frame_metadata_serialization() {
    let metadata = FrameMetadata {
        sequence: 42,
        width: 1920,
        height: 1080,
        captured_at: 1700000000000,
        compression: ScreenCompression::WebP,
        is_keyframe: true,
        size: 1024000,
        cursor: None,
    };

    let json = serde_json::to_string(&metadata).unwrap();
    let decoded: FrameMetadata = serde_json::from_str(&json).unwrap();

    assert_eq!(decoded.sequence, 42);
    assert_eq!(decoded.width, 1920);
    assert_eq!(decoded.height, 1080);
    assert_eq!(decoded.size, 1024000);
    assert_eq!(decoded.compression, ScreenCompression::WebP);
    assert_eq!(decoded.captured_at, 1700000000000);
    assert!(decoded.is_keyframe);
    assert!(decoded.cursor.is_none());
}

/// Test viewer lifecycle with mock events
#[tokio::test]
async fn test_viewer_lifecycle_integration() {
    let (event_tx, mut event_rx) = viewer_event_channel(32);
    let config = ViewerConfig::default();
    let mut viewer = ScreenViewer::new(config, event_tx);

    // Initial state
    assert_eq!(viewer.state(), ViewerState::Disconnected);

    // Start connecting
    viewer.start_connect("peer-abc".to_string(), Some("display-0".to_string()));
    assert_eq!(viewer.state(), ViewerState::Connecting);
    assert_eq!(viewer.peer_id(), Some("peer-abc"));
    assert!(viewer.stream_id().is_some());

    // Check state change event was emitted
    let event = event_rx.try_recv().unwrap();
    match event {
        ViewerEvent::StateChanged { old_state, new_state } => {
            assert_eq!(old_state, ViewerState::Disconnected);
            assert_eq!(new_state, ViewerState::Connecting);
        }
        _ => panic!("Expected StateChanged event"),
    }

    // Simulate connection with display list
    let displays = vec![DisplayInfo {
        id: "display-0".to_string(),
        name: "Test Display".to_string(),
        width: 1920,
        height: 1080,
        refresh_rate: Some(60),
        is_primary: true,
    }];
    viewer.on_connected(displays.clone());
    assert_eq!(viewer.state(), ViewerState::WaitingForFrame);
    assert_eq!(viewer.displays().len(), 1);

    // Check events
    let event = event_rx.try_recv().unwrap();
    assert!(matches!(event, ViewerEvent::StateChanged { .. }));
    let event = event_rx.try_recv().unwrap();
    assert!(matches!(event, ViewerEvent::DisplaysReceived(_)));

    // Simulate stream accepted
    viewer.on_stream_accepted(ScreenCompression::H264);
    let event = event_rx.try_recv().unwrap();
    match event {
        ViewerEvent::StreamAccepted { compression, .. } => {
            assert_eq!(compression, ScreenCompression::H264);
        }
        _ => panic!("Expected StreamAccepted event"),
    }

    // Simulate receiving a frame (10x10 RGBA = 400 bytes)
    let frame_data = vec![0u8; 400];
    viewer
        .on_frame_received(&frame_data, 10, 10, 0)
        .unwrap();
    assert_eq!(viewer.state(), ViewerState::Streaming);

    // Check frame ready event
    let event = event_rx.try_recv().unwrap();
    assert!(matches!(event, ViewerEvent::StateChanged { .. }));
    let event = event_rx.try_recv().unwrap();
    match event {
        ViewerEvent::FrameReady { width, height, sequence } => {
            assert_eq!(width, 10);
            assert_eq!(height, 10);
            assert_eq!(sequence, 0);
        }
        _ => panic!("Expected FrameReady event"),
    }

    // Verify stats
    let stats = viewer.stats();
    assert!(stats.frames_received > 0);

    // Disconnect
    viewer.disconnect("test complete".to_string());
    assert_eq!(viewer.state(), ViewerState::Disconnected);
    assert!(viewer.peer_id().is_none());
}

/// Test viewer reconnection integration
#[tokio::test]
async fn test_viewer_reconnection_integration() {
    let (event_tx, mut event_rx) = viewer_event_channel(32);
    let mut config = ViewerConfig::default();
    config.enable_auto_reconnect = true;
    config.max_reconnect_attempts = 2;

    let mut viewer = ScreenViewer::new(config, event_tx);

    // Connect
    viewer.start_connect("peer-xyz".to_string(), Some("display-1".to_string()));
    viewer.on_connected(vec![]);

    // Drain initial events
    while event_rx.try_recv().is_ok() {}

    // Simulate connection loss
    let can_retry = viewer.on_connection_lost("network timeout".to_string());
    assert!(can_retry);

    // Check ConnectionLost event
    let event = event_rx.try_recv().unwrap();
    match event {
        ViewerEvent::ConnectionLost { reason, can_retry, .. } => {
            assert_eq!(reason, "network timeout");
            assert!(can_retry);
        }
        _ => panic!("Expected ConnectionLost event"),
    }

    // Start reconnect
    let result = viewer.start_reconnect();
    assert!(result.is_some());
    let (peer_id, display_id) = result.unwrap();
    assert_eq!(peer_id, "peer-xyz");
    assert_eq!(display_id, Some("display-1".to_string()));
    assert_eq!(viewer.state(), ViewerState::Reconnecting);

    // Check ReconnectAttempt event (emitted before StateChanged in start_reconnect)
    let event = event_rx.try_recv().unwrap();
    match event {
        ViewerEvent::ReconnectAttempt { attempt, max_attempts } => {
            assert_eq!(attempt, 1);
            assert_eq!(max_attempts, 2);
        }
        _ => panic!("Expected ReconnectAttempt event, got {:?}", event),
    }
    // StateChanged event comes after
    let event = event_rx.try_recv().unwrap();
    assert!(matches!(event, ViewerEvent::StateChanged { .. }));

    // Simulate successful reconnection
    viewer.on_reconnect_success();
    assert_eq!(viewer.reconnect_attempt_count(), 0);
    assert!(viewer.last_error().is_none());
}

/// Test screen view permission checks
#[test]
fn test_screen_view_permissions() {
    use croh_core::trust::Capability;

    // Permissions with screen_view
    let caps = vec![Capability::ScreenView, Capability::ScreenControl];
    let perms = Permissions::from_capabilities(&caps);

    assert!(perms.screen_view);
    assert!(perms.screen_control);
    assert!(!perms.push);
    assert!(!perms.pull);

    // Convert back
    let back = perms.to_capabilities();
    assert!(back.contains(&Capability::ScreenView));
    assert!(back.contains(&Capability::ScreenControl));
    assert!(!back.contains(&Capability::Push));

    // All permissions should include screen capabilities
    let all = Permissions::all();
    assert!(all.screen_view);
    assert!(all.screen_control);
}

/// Test compression enum serialization
#[test]
fn test_compression_enum() {
    // Test all compression variants
    let variants = vec![
        ScreenCompression::Raw,
        ScreenCompression::WebP,
        ScreenCompression::H264,
        ScreenCompression::Vp9,
        ScreenCompression::Av1,
    ];

    for variant in variants {
        let json = serde_json::to_string(&variant).unwrap();
        let decoded: ScreenCompression = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, variant);
    }
}

/// Test quality enum serialization
#[test]
fn test_quality_enum() {
    // Test all quality variants
    let variants = vec![
        ScreenQuality::Fast,
        ScreenQuality::Balanced,
        ScreenQuality::Quality,
        ScreenQuality::Auto,
    ];

    for variant in variants {
        let json = serde_json::to_string(&variant).unwrap();
        let decoded: ScreenQuality = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, variant);
    }
}

/// Full screen stream between two endpoints.
///
/// This test requires network connectivity and working UDP/QUIC.
/// Run with: `cargo test -- --ignored`
#[tokio::test]
#[ignore = "requires specific network conditions"]
async fn test_full_screen_stream() {
    // This would use EndpointPair and stream_screen_from_peer
    todo!("Implement with test-relay feature");
}
