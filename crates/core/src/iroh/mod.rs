//! Iroh networking module for trusted peer connections.
//!
//! This module provides:
//! - Identity management (keypair persistence)
//! - Endpoint wrapper for Iroh connections
//! - Control protocol for trust handshakes
//! - Handshake protocol implementation
//! - Blob-based file transfer
//! - File browsing for remote peers
//! - Address abstraction for multi-transport support
//!
//! # Testing
//!
//! For integration testing, use the [`test_support`] module which provides
//! utilities for testing peer-to-peer functionality without external relay servers.
//! See the module documentation for usage examples.

pub mod address;
pub mod blobs;
pub mod browse;
pub mod endpoint;
pub mod handshake;
pub mod identity;
pub mod protocol;
pub mod speedtest;
pub mod transfer;

#[cfg(test)]
pub mod test_support;

pub use address::PeerAddress;
pub use blobs::{hash_file, verify_file_hash, BlobStore};
pub use browse::{
    browse_directory, default_browsable_paths, get_browsable_roots, resolve_browse_path,
    validate_path,
};
pub use endpoint::{ControlConnection, IrohEndpoint};
pub use handshake::{accept_trust_connections, complete_trust_as_receiver, HandshakeResult};
pub use identity::Identity;
pub use protocol::{
    ControlMessage, DirectoryEntry, FileInfo, FileRequest, ALPN_BLOBS, ALPN_CONTROL,
};
pub use speedtest::{
    handle_speed_test_request, run_speed_test, SpeedTestResult, DEFAULT_TEST_SIZE,
};
pub use transfer::{
    browse_remote, handle_browse_request, handle_incoming_pull, handle_incoming_push,
    handle_screen_stream_request, pull_files, push_files, stream_screen_from_peer,
    ScreenStreamEvent, TransferEvent,
};

// Re-export iroh types used by the GUI
pub use iroh::{NodeAddr, NodeId, RelayUrl};
