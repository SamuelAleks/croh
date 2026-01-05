//! Iroh networking module for trusted peer connections.
//!
//! This module provides:
//! - Identity management (keypair persistence)
//! - Endpoint wrapper for Iroh connections
//! - Control protocol for trust handshakes
//! - Handshake protocol implementation
//! - Blob-based file transfer

pub mod blobs;
pub mod endpoint;
pub mod handshake;
pub mod identity;
pub mod protocol;
pub mod transfer;

pub use blobs::{hash_file, verify_file_hash, BlobStore};
pub use endpoint::{ControlConnection, IrohEndpoint};
pub use handshake::{accept_trust_connections, complete_trust_as_receiver, HandshakeResult};
pub use identity::Identity;
pub use protocol::{ControlMessage, DirectoryEntry, FileInfo, FileRequest, ALPN_BLOBS, ALPN_CONTROL};
pub use transfer::{handle_incoming_push, pull_files, push_files, TransferEvent};
