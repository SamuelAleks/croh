//! Croc GUI Core Library
//!
//! This crate provides the core functionality for Croc GUI, including:
//! - Croc subprocess management
//! - Transfer state tracking
//! - File handling utilities
//! - Configuration management
//!
//! It is used by both the GUI and daemon crates.

pub mod config;
pub mod croc;
pub mod error;
pub mod files;
pub mod platform;
pub mod transfer;

// Re-export commonly used types
pub use config::Config;
pub use croc::{CrocOptions, CrocProcess, CrocProcessHandle, find_croc_executable, refresh_croc_cache};
pub use error::{Error, Result};
pub use transfer::{Transfer, TransferId, TransferManager, TransferStatus, TransferType};

