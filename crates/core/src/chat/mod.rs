//! Chat functionality for peer-to-peer text messaging.
//!
//! This module provides text chat capabilities between trusted peers using
//! the existing Iroh control protocol. Features include:
//!
//! - Per-peer 1:1 conversations
//! - Persistent message storage via Sled
//! - Typing indicators
//! - Delivery and read receipts
//! - Message history synchronization
//! - Offline message queuing

pub mod handler;
pub mod store;
pub mod types;

pub use handler::{ChatEvent, ChatHandler};
pub use store::ChatStore;
pub use types::*;
