//! Portal restore token persistence manager.
//!
//! This module manages the lifecycle of XDG Portal restore tokens for
//! unattended screen capture. Tokens are single-use and must be saved
//! immediately after each session to ensure persistence across restarts.

use crate::config::Config;
use crate::error::Result;
use crate::screen::wayland::{CompositorCapabilities, WaylandCompositor};
use std::sync::{Arc, RwLock};

/// Manages Portal restore tokens with persistent storage.
///
/// The token manager handles:
/// - Loading tokens from config at startup
/// - Saving new tokens immediately after session start
/// - Invalidating tokens when permission is revoked
/// - Tracking compositor capabilities for UI hints
pub struct TokenManager {
    /// Current token (may be more recent than config)
    current_token: RwLock<Option<String>>,
    /// Config for persistence
    config: Arc<RwLock<Config>>,
    /// Detected compositor
    compositor: WaylandCompositor,
    /// Last successful restore timestamp
    last_restore_success: RwLock<Option<i64>>,
}

impl TokenManager {
    /// Create a new token manager with the given config.
    ///
    /// Loads any existing token from the config's `screen_stream.portal_restore_token`.
    pub fn new(config: Arc<RwLock<Config>>) -> Self {
        let compositor = WaylandCompositor::detect();

        // Load token from config
        let current_token = config
            .read()
            .ok()
            .and_then(|c| c.screen_stream.portal_restore_token.clone());

        tracing::info!(
            "TokenManager initialized for {} compositor, token present: {}",
            compositor.name(),
            current_token.is_some()
        );

        Self {
            current_token: RwLock::new(current_token),
            config,
            compositor,
            last_restore_success: RwLock::new(None),
        }
    }

    /// Create a token manager without config persistence (for testing).
    #[cfg(test)]
    pub fn new_without_persistence() -> Self {
        Self {
            current_token: RwLock::new(None),
            config: Arc::new(RwLock::new(Config::default())),
            compositor: WaylandCompositor::detect(),
            last_restore_success: RwLock::new(None),
        }
    }

    /// Get current token for use in Portal session.
    pub fn get_token(&self) -> Option<String> {
        self.current_token.read().ok()?.clone()
    }

    /// Update token after successful session start.
    ///
    /// This should be called immediately after receiving a new token from the portal,
    /// as tokens are single-use and the new one must be saved.
    pub fn update_token(&self, new_token: Option<String>) -> Result<()> {
        let had_token = self.has_token();
        let has_new_token = new_token.is_some();

        // Update in-memory
        if let Ok(mut current) = self.current_token.write() {
            *current = new_token.clone();
        }

        // Update restore success timestamp
        if has_new_token {
            if let Ok(mut ts) = self.last_restore_success.write() {
                *ts = Some(chrono::Utc::now().timestamp());
            }
        }

        // Persist to config immediately
        if let Ok(mut config) = self.config.write() {
            config.screen_stream.portal_restore_token = new_token.clone();

            // Best-effort save - don't fail the whole operation if save fails
            if let Err(e) = config.save() {
                tracing::warn!("Failed to persist restore token to config: {}", e);
                // Continue anyway - in-memory token is updated
            } else {
                tracing::debug!(
                    "Restore token persisted to config (had: {}, has: {})",
                    had_token,
                    has_new_token
                );
            }
        }

        Ok(())
    }

    /// Invalidate current token (e.g., after permission revoked).
    pub fn invalidate_token(&self) {
        tracing::info!("Invalidating restore token");

        if let Ok(mut current) = self.current_token.write() {
            *current = None;
        }

        if let Ok(mut config) = self.config.write() {
            config.screen_stream.portal_restore_token = None;
            let _ = config.save();
        }
    }

    /// Check if we have a token that might allow silent restore.
    pub fn has_token(&self) -> bool {
        self.current_token
            .read()
            .ok()
            .map(|t| t.is_some())
            .unwrap_or(false)
    }

    /// Get the detected compositor.
    pub fn compositor(&self) -> WaylandCompositor {
        self.compositor
    }

    /// Get compositor capabilities.
    pub fn compositor_capabilities(&self) -> CompositorCapabilities {
        self.compositor.capabilities()
    }

    /// Check if silent restore is likely to work.
    ///
    /// Returns true if the compositor supports restore tokens and we have a token.
    pub fn can_likely_restore_silently(&self) -> bool {
        let caps = self.compositor.capabilities();
        caps.supports_restore_tokens && self.has_token()
    }

    /// Get the last successful restore timestamp.
    pub fn last_restore_success(&self) -> Option<i64> {
        self.last_restore_success.read().ok()?.clone()
    }

    /// Record a successful restore (silent session start).
    pub fn record_restore_success(&self) {
        if let Ok(mut ts) = self.last_restore_success.write() {
            *ts = Some(chrono::Utc::now().timestamp());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_manager_creation() {
        let config = Arc::new(RwLock::new(Config::default()));
        let tm = TokenManager::new(config);

        assert!(!tm.has_token());
        assert!(tm.get_token().is_none());
    }

    #[test]
    fn test_token_manager_update() {
        let config = Arc::new(RwLock::new(Config::default()));
        let tm = TokenManager::new(config.clone());

        // No token initially
        assert!(!tm.has_token());

        // Save token
        tm.update_token(Some("test-token-123".into())).unwrap();
        assert!(tm.has_token());
        assert_eq!(tm.get_token(), Some("test-token-123".into()));

        // Verify persisted in config (in-memory, not file)
        let config_read = config.read().unwrap();
        assert_eq!(
            config_read.screen_stream.portal_restore_token,
            Some("test-token-123".into())
        );
    }

    #[test]
    fn test_token_manager_invalidate() {
        let config = Arc::new(RwLock::new(Config::default()));
        let tm = TokenManager::new(config.clone());

        // Set token
        tm.update_token(Some("test-token".into())).unwrap();
        assert!(tm.has_token());

        // Invalidate
        tm.invalidate_token();
        assert!(!tm.has_token());
        assert!(tm.get_token().is_none());

        // Verify removed from config
        let config_read = config.read().unwrap();
        assert!(config_read.screen_stream.portal_restore_token.is_none());
    }

    #[test]
    fn test_can_likely_restore_silently() {
        let config = Arc::new(RwLock::new(Config::default()));
        let tm = TokenManager::new(config);

        // No token - can't restore silently
        assert!(!tm.can_likely_restore_silently());

        // With token - can restore silently (assuming compositor supports it)
        tm.update_token(Some("token".into())).unwrap();
        // Note: actual result depends on compositor detection
        // Most compositors support restore tokens
        assert!(
            tm.can_likely_restore_silently()
                || !tm.compositor_capabilities().supports_restore_tokens
        );
    }

    #[test]
    fn test_compositor_detection() {
        let config = Arc::new(RwLock::new(Config::default()));
        let tm = TokenManager::new(config);

        // Should detect something (even if Unknown)
        let caps = tm.compositor_capabilities();
        assert!(!caps.notes.is_empty());
    }

    #[test]
    fn test_restore_success_tracking() {
        let tm = TokenManager::new_without_persistence();

        // No success initially
        assert!(tm.last_restore_success().is_none());

        // Record success
        tm.record_restore_success();
        assert!(tm.last_restore_success().is_some());

        // Timestamp should be recent
        let ts = tm.last_restore_success().unwrap();
        let now = chrono::Utc::now().timestamp();
        assert!((now - ts).abs() < 5); // Within 5 seconds
    }

    #[test]
    fn test_token_update_records_timestamp() {
        let config = Arc::new(RwLock::new(Config::default()));
        let tm = TokenManager::new(config);

        // No timestamp initially
        assert!(tm.last_restore_success().is_none());

        // Update with token
        tm.update_token(Some("token".into())).unwrap();

        // Should have timestamp now
        assert!(tm.last_restore_success().is_some());
    }
}
