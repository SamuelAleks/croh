//! Croc subprocess management.
//!
//! This module provides functionality for:
//! - Finding the croc executable
//! - Spawning croc send/receive processes
//! - Parsing croc output for progress and codes
//! - Managing croc process lifecycle

mod executable;
mod options;
mod output;
mod process;

pub use executable::{find_croc_executable, refresh_croc_cache};
pub use options::{CrocOptions, Curve, HashAlgorithm, validate_croc_code};
pub use output::{parse_code, parse_progress, detect_completion, detect_error, Progress};
pub use process::{CrocProcess, CrocProcessHandle, CrocEvent};

