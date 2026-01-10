//! Common test utilities for integration tests.
//!
//! This module provides shared test helpers and configuration for
//! integration testing the Iroh peer-to-peer functionality.

use std::time::Duration;

/// Default timeout for test operations.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Shorter timeout for operations expected to fail quickly.
pub const FAIL_TIMEOUT: Duration = Duration::from_secs(2);

/// Initialize test logging with appropriate filters.
///
/// Call this at the start of tests that need debug output.
/// Safe to call multiple times (subsequent calls are no-ops).
#[allow(dead_code)]
pub fn init_test_logging() {
    use tracing_subscriber::EnvFilter;

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("croh_core=debug,iroh=warn")),
        )
        .with_test_writer()
        .try_init();
}

/// Run an async operation with a timeout.
///
/// Returns the result if the operation completes within the timeout,
/// or panics with a timeout message if it doesn't.
#[allow(dead_code)]
pub async fn with_timeout<T, F>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(TEST_TIMEOUT, fut)
        .await
        .expect("Test operation timed out")
}
