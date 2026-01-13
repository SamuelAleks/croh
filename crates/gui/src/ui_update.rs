//! UI update helpers with logging and error handling.
//!
//! This module provides utilities for safely updating the Slint UI from background threads,
//! with proper error handling and debug logging to diagnose partial update issues.

use slint::{ComponentHandle, Weak};
use tracing::{debug, warn};

/// Invoke a UI update on the Slint event loop with proper error handling and logging.
///
/// This helper wraps `slint::invoke_from_event_loop` with:
/// - Debug logging when updates are queued (with label)
/// - Warning when the window weak reference fails to upgrade
/// - Warning when the invoke itself fails
///
/// # Arguments
/// * `window_weak` - Weak reference to the main window
/// * `label` - A descriptive label for logging (e.g., "peer_status_update")
/// * `update_fn` - The function to run on the UI thread, receives the window reference
///
/// # Returns
/// `true` if the update was successfully queued, `false` if it failed
pub fn invoke_ui_update<W, F>(window_weak: &Weak<W>, label: &'static str, update_fn: F) -> bool
where
    W: ComponentHandle + 'static,
    F: FnOnce(&W) + Send + 'static,
{
    let window_weak = window_weak.clone();

    match slint::invoke_from_event_loop(move || {
        match window_weak.upgrade() {
            Some(window) => {
                debug!(label, "UI update executing");
                update_fn(&window);
            }
            None => {
                warn!(label, "UI update failed: window reference expired");
            }
        }
    }) {
        Ok(()) => {
            debug!(label, "UI update queued successfully");
            true
        }
        Err(e) => {
            warn!(label, error = %e, "Failed to queue UI update");
            false
        }
    }
}

/// Same as `invoke_ui_update` but with a dynamic label (for when the label needs runtime info).
#[allow(dead_code)]
pub fn invoke_ui_update_dyn<W, F>(window_weak: &Weak<W>, label: String, update_fn: F) -> bool
where
    W: ComponentHandle + 'static,
    F: FnOnce(&W) + Send + 'static,
{
    let window_weak = window_weak.clone();
    let label_clone = label.clone();

    match slint::invoke_from_event_loop(move || {
        match window_weak.upgrade() {
            Some(window) => {
                debug!(label = %label_clone, "UI update executing");
                update_fn(&window);
            }
            None => {
                warn!(label = %label_clone, "UI update failed: window reference expired");
            }
        }
    }) {
        Ok(()) => {
            debug!(label = %label, "UI update queued successfully");
            true
        }
        Err(e) => {
            warn!(label = %label, error = %e, "Failed to queue UI update");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    // Note: These would require a Slint test harness to properly test
    // For now, the module is tested via integration with the app
}
