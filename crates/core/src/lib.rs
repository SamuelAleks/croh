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
pub use chat::{
    ChatConversation, ChatEvent, ChatHandler, ChatMessage, ChatStore, MessageId, MessageStatus,
};
pub use config::{
    BrowseSettings, CaptureBackend, Config, DndMode, GuestPolicy, RelayPreference,
    ScreenStreamSettings, SecurityPosture, ViewerQuality, ViewerScaleMode, ViewerSettings,
    WindowSize,
};
pub use croc::{
    find_croc_executable, refresh_croc_cache, CrocOptions, CrocProcess, CrocProcessHandle,
};
pub use error::{Error, Result};
pub use files::{format_duration, format_eta, format_size, format_uptime, get_disk_space};
pub use iroh::{
    accept_trust_connections, browse_directory, browse_remote, complete_trust_as_receiver,
    default_browsable_paths, get_browsable_roots, handle_browse_request, handle_incoming_pull,
    handle_incoming_push, handle_screen_stream_request, handle_speed_test_request, pull_files,
    push_files, run_speed_test, stream_screen_from_peer, ControlConnection, ControlMessage,
    DirectoryEntry, FileRequest, HandshakeResult, Identity, IrohEndpoint, NodeAddr, NodeId,
    PeerAddress, RelayUrl, ScreenStreamCommand, ScreenStreamEvent, SpeedTestResult, TransferEvent,
    ALPN_CONTROL, DEFAULT_TEST_SIZE,
};
pub use networks::{NetworkSettings, NetworkStore, PeerNetwork};
pub use peers::{PeerStore, Permissions, TrustedPeer};
pub use transfer::{Transfer, TransferId, TransferManager, TransferStatus, TransferType};
pub use transfer_history::TransferHistory;
pub use trust::{Capability, PeerInfo, TrustBundle};

// Re-export serde_json for GUI use
pub use serde_json;

/// Generate a unique ID (UUID v4).
pub fn generate_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
