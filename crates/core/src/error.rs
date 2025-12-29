//! Error types for the core library.

use thiserror::Error;

/// Main error type for the core library.
#[derive(Error, Debug)]
pub enum Error {
    /// Croc executable not found
    #[error("croc executable not found. Please install croc: https://github.com/schollz/croc")]
    CrocNotFound,

    /// Croc process error
    #[error("croc process error: {0}")]
    CrocProcess(String),

    /// Invalid croc code format
    #[error("invalid croc code format: {0}")]
    InvalidCode(String),

    /// Transfer not found
    #[error("transfer not found: {0}")]
    TransferNotFound(String),

    /// Transfer already exists
    #[error("transfer already exists: {0}")]
    TransferExists(String),

    /// File not found
    #[error("file not found: {0}")]
    FileNotFound(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Configuration error
    #[error("configuration error: {0}")]
    Config(String),
}

/// Result type alias using our Error type.
pub type Result<T> = std::result::Result<T, Error>;

