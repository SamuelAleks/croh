//! Iroh networking module for trusted peer connections.
//!
//! This module provides:
//! - Identity management (keypair persistence)
//! - Endpoint wrapper for Iroh connections
//! - Control protocol for trust handshakes
//! - Handshake protocol implementation

pub mod endpoint;
pub mod handshake;
pub mod identity;
pub mod protocol;

pub use endpoint::{ControlConnection, IrohEndpoint};
pub use handshake::{complete_trust_as_receiver, accept_trust_connections, HandshakeResult};
pub use identity::Identity;
pub use protocol::{ControlMessage, ALPN_CONTROL};
