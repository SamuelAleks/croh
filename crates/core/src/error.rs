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
    Io(String),

    /// Serialization error
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Configuration error
    #[error("configuration error: {0}")]
    Config(String),

    /// Iroh networking error
    #[error("iroh error: {0}")]
    Iroh(String),

    /// Identity error
    #[error("identity error: {0}")]
    Identity(String),

    /// Trust error
    #[error("trust error: {0}")]
    Trust(String),

    /// Peer error
    #[error("peer error: {0}")]
    Peer(String),

    /// Transfer error
    #[error("transfer error: {0}")]
    Transfer(String),

    /// Browse error
    #[error("browse error: {0}")]
    Browse(String),

    /// Network error
    #[error("network error: {0}")]
    Network(String),

    /// Chat error
    #[error("chat error: {0}")]
    Chat(String),

    /// Screen capture error
    #[error("screen capture error: {0}")]
    Screen(String),

    /// Video encoder error
    #[error("encoder error: {0}")]
    Encoder(String),

    /// Input injection error
    #[error("input error: {0}")]
    Input(String),
}

/// Result type alias using our Error type.
pub type Result<T> = std::result::Result<T, Error>;

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err.to_string())
    }
}

