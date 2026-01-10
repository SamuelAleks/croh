//! Croc GUI Core Library
//!
//! This crate provides the core functionality for Croc GUI, including:
//! - Croc subprocess management
//! - Transfer state tracking
//! - File handling utilities
//! - Configuration management
//! - Iroh networking for trusted peers
//!
//! It is used by both the GUI and daemon crates.

pub mod config;
pub mod croc;
pub mod error;
pub mod files;
pub mod iroh;
pub mod peers;
pub mod platform;
pub mod transfer;
pub mod trust;

// Re-export commonly used types
pub use config::{BrowseSettings, Config, WindowSize};
pub use croc::{CrocOptions, CrocProcess, CrocProcessHandle, find_croc_executable, refresh_croc_cache};
pub use error::{Error, Result};
pub use iroh::{
    complete_trust_as_receiver, accept_trust_connections, HandshakeResult,
    ControlConnection, ControlMessage, DirectoryEntry, FileRequest, Identity, IrohEndpoint, ALPN_CONTROL,
    push_files, pull_files, handle_incoming_push, handle_incoming_pull, handle_browse_request, browse_remote, TransferEvent,
    browse_directory, default_browsable_paths, get_browsable_roots,
    NodeAddr, NodeId,
};
pub use peers::{PeerStore, Permissions, TrustedPeer};
pub use transfer::{Transfer, TransferId, TransferManager, TransferStatus, TransferType};
pub use trust::{Capability, PeerInfo, TrustBundle};

