//! Croc GUI Core Library
//!
//! This crate provides the core functionality for Croc GUI, including:
//! - Croc subprocess management
//! - Transfer state tracking
//! - File handling utilities
//! - Configuration management
//! - Iroh networking for trusted peers
//! - Peer-to-peer chat
//!
//! It is used by both the GUI and daemon crates.

pub mod chat;
pub mod config;
pub mod croc;
pub mod error;
pub mod files;
pub mod iroh;
pub mod networks;
pub mod peers;
pub mod platform;
pub mod screen;
pub mod transfer;
pub mod transfer_history;
pub mod trust;

// Re-export commonly used types
pub use config::{BrowseSettings, CaptureBackend, Config, DndMode, GuestPolicy, RelayPreference, ScreenStreamSettings, SecurityPosture, WindowSize};
pub use croc::{CrocOptions, CrocProcess, CrocProcessHandle, find_croc_executable, refresh_croc_cache};
pub use error::{Error, Result};
pub use files::{format_size, format_duration, format_uptime, format_eta, get_disk_space};
pub use iroh::{
    complete_trust_as_receiver, accept_trust_connections, HandshakeResult,
    ControlConnection, ControlMessage, DirectoryEntry, FileRequest, Identity, IrohEndpoint, ALPN_CONTROL,
    push_files, pull_files, handle_incoming_push, handle_incoming_pull, handle_browse_request, browse_remote, TransferEvent,
    browse_directory, default_browsable_paths, get_browsable_roots,
    run_speed_test, handle_speed_test_request, SpeedTestResult, DEFAULT_TEST_SIZE,
    NodeAddr, NodeId, PeerAddress, RelayUrl,
    stream_screen_from_peer, handle_screen_stream_request, ScreenStreamEvent,
};
pub use networks::{NetworkSettings, NetworkStore, PeerNetwork};
pub use peers::{PeerStore, Permissions, TrustedPeer};
pub use transfer::{Transfer, TransferId, TransferManager, TransferStatus, TransferType};
pub use transfer_history::TransferHistory;
pub use trust::{Capability, PeerInfo, TrustBundle};
pub use chat::{ChatEvent, ChatHandler, ChatMessage, ChatStore, ChatConversation, MessageId, MessageStatus};

// Re-export serde_json for GUI use
pub use serde_json;

/// Generate a unique ID (UUID v4).
pub fn generate_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

