//! Iroh networking module for trusted peer connections.
//!
//! This module provides:
//! - Identity management (keypair persistence)
//! - Endpoint wrapper for Iroh connections
//! - Control protocol for trust handshakes

pub mod endpoint;
pub mod identity;
pub mod protocol;

pub use endpoint::{ControlConnection, IrohEndpoint};
pub use identity::Identity;
pub use protocol::{ControlMessage, ALPN_CONTROL};
