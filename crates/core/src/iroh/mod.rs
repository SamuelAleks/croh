//! Iroh networking module for trusted peer connections.
//!
//! This module provides:
//! - Identity management (keypair persistence)
//! - Endpoint wrapper for Iroh connections
//! - Control protocol for trust handshakes
//! - Handshake protocol implementation
//! - Blob-based file transfer
//! - File browsing for remote peers

pub mod blobs;
pub mod browse;
pub mod endpoint;
pub mod handshake;
pub mod identity;
pub mod protocol;
pub mod transfer;

pub use blobs::{hash_file, verify_file_hash, BlobStore};
pub use browse::{browse_directory, default_browsable_paths, get_browsable_roots, resolve_browse_path, validate_path};
pub use endpoint::{ControlConnection, IrohEndpoint};
pub use handshake::{accept_trust_connections, complete_trust_as_receiver, HandshakeResult};
pub use identity::Identity;
pub use protocol::{ControlMessage, DirectoryEntry, FileInfo, FileRequest, ALPN_BLOBS, ALPN_CONTROL};
pub use transfer::{browse_remote, handle_browse_request, handle_incoming_pull, handle_incoming_push, pull_files, push_files, TransferEvent};
