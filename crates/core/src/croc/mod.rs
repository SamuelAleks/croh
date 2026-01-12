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
pub use options::{validate_croc_code, CrocOptions, Curve, HashAlgorithm};
pub use output::{detect_completion, detect_error, parse_code, parse_progress, Progress};
pub use process::{CrocEvent, CrocProcess, CrocProcessHandle};
