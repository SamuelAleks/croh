# Screen Streaming Architecture: Phase 4 - Wayland Unattended Access

Detailed architecture for robust unattended screen sharing on Wayland.

## Current State Analysis

### What's Implemented

From `crates/core/src/screen/portal.rs`:

```rust
// Current implementation uses:
PersistMode::ExplicitlyRevoked  // Tokens persist until explicitly revoked
CursorMode::Embedded            // Cursor rendered into frame
SourceType::Monitor             // Monitor capture only

// Token handling:
- Token passed to select_sources() if available
- New token saved from response.restore_token()
- Token stored in self.restore_token
```

### Current Issues

1. **Token persistence is in-memory only** - Token is stored in `PortalCapture.restore_token` but not automatically persisted to disk
2. **No token refresh on each capture** - Restore tokens are single-use; need to save new token after each session start
3. **No compositor detection** - Same behavior for all compositors despite different capabilities
4. **No fallback messaging** - User not informed when unattended access isn't possible
5. **Session invalidation not handled** - If monitor disconnected or permissions revoked, no graceful recovery

---

## Architecture Goals

1. **Truly unattended access** when compositor supports it (GNOME 47+, KDE with tokens)
2. **Clear user messaging** when dialog is required
3. **Robust token lifecycle** - save immediately, handle invalidation gracefully
4. **Compositor-specific optimizations** where beneficial
5. **Automatic backend selection** considering unattended requirements

---

## Detailed Design

### 4.1 Compositor Detection

New file `crates/core/src/screen/wayland.rs`:

```rust
use std::env;

/// Known Wayland compositors and their capabilities
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaylandCompositor {
    /// GNOME Shell (Mutter)
    Gnome,
    /// KDE Plasma (KWin)
    KdePlasma,
    /// Sway (wlroots)
    Sway,
    /// Hyprland (wlroots-based)
    Hyprland,
    /// Other wlroots-based compositor
    Wlroots,
    /// Unknown compositor
    Unknown,
}

/// Compositor capabilities for screen sharing
#[derive(Debug, Clone)]
pub struct CompositorCapabilities {
    /// Compositor type
    pub compositor: WaylandCompositor,
    /// Whether restore tokens are supported
    pub supports_restore_tokens: bool,
    /// Whether fully unattended access is possible (no dialogs ever after setup)
    pub supports_unattended: bool,
    /// Whether cursor can be captured separately
    pub supports_cursor_metadata: bool,
    /// Minimum portal version needed for full features
    pub min_portal_version: Option<(u32, u32)>,
    /// Human-readable notes for the user
    pub notes: &'static str,
}

impl WaylandCompositor {
    /// Detect the current Wayland compositor
    pub fn detect() -> Self {
        // Check XDG_CURRENT_DESKTOP first (most reliable)
        if let Ok(desktop) = env::var("XDG_CURRENT_DESKTOP") {
            let desktop_lower = desktop.to_lowercase();

            if desktop_lower.contains("gnome") {
                return Self::Gnome;
            }
            if desktop_lower.contains("kde") || desktop_lower.contains("plasma") {
                return Self::KdePlasma;
            }
            if desktop_lower.contains("sway") {
                return Self::Sway;
            }
            if desktop_lower.contains("hyprland") {
                return Self::Hyprland;
            }
        }

        // Fallback: check DESKTOP_SESSION
        if let Ok(session) = env::var("DESKTOP_SESSION") {
            let session_lower = session.to_lowercase();

            if session_lower.contains("gnome") {
                return Self::Gnome;
            }
            if session_lower.contains("plasma") {
                return Self::KdePlasma;
            }
            if session_lower.contains("sway") {
                return Self::Sway;
            }
        }

        // Check for compositor-specific env vars
        if env::var("SWAYSOCK").is_ok() {
            return Self::Sway;
        }
        if env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
            return Self::Hyprland;
        }

        // Check if wlroots-based by looking for common indicators
        if env::var("WAYLAND_DISPLAY").is_ok() {
            // Could be any wlroots compositor
            return Self::Wlroots;
        }

        Self::Unknown
    }

    /// Get capabilities for this compositor
    pub fn capabilities(&self) -> CompositorCapabilities {
        match self {
            Self::Gnome => CompositorCapabilities {
                compositor: *self,
                supports_restore_tokens: true,
                // GNOME 47+ supports unattended via gnome-remote-desktop
                // Regular portal still needs dialog, but token works for restore
                supports_unattended: false, // Would need gnome-remote-desktop integration
                supports_cursor_metadata: true, // GNOME 44+ via portal
                min_portal_version: Some((1, 14)),
                notes: "Restore tokens work. For truly unattended access, \
                        enable 'Remote Desktop' in GNOME Settings.",
            },
            Self::KdePlasma => CompositorCapabilities {
                compositor: *self,
                supports_restore_tokens: true,
                supports_unattended: false, // Token helps but not guaranteed
                supports_cursor_metadata: false,
                min_portal_version: Some((1, 14)),
                notes: "Restore tokens supported. After granting permission once, \
                        subsequent sessions should connect without dialog.",
            },
            Self::Sway => CompositorCapabilities {
                compositor: *self,
                supports_restore_tokens: true,
                supports_unattended: false,
                supports_cursor_metadata: false,
                min_portal_version: Some((1, 14)),
                notes: "Uses xdg-desktop-portal-wlr. Restore tokens may help \
                        avoid repeated dialogs.",
            },
            Self::Hyprland => CompositorCapabilities {
                compositor: *self,
                supports_restore_tokens: true,
                supports_unattended: false,
                supports_cursor_metadata: false,
                min_portal_version: Some((1, 14)),
                notes: "Uses xdg-desktop-portal-hyprland. Restore tokens \
                        should persist across sessions.",
            },
            Self::Wlroots | Self::Unknown => CompositorCapabilities {
                compositor: *self,
                supports_restore_tokens: true,
                supports_unattended: false,
                supports_cursor_metadata: false,
                min_portal_version: Some((1, 14)),
                notes: "Unknown compositor. Token-based restore may or may not work.",
            },
        }
    }

    /// Get user-friendly name
    pub fn name(&self) -> &'static str {
        match self {
            Self::Gnome => "GNOME",
            Self::KdePlasma => "KDE Plasma",
            Self::Sway => "Sway",
            Self::Hyprland => "Hyprland",
            Self::Wlroots => "wlroots-based",
            Self::Unknown => "Unknown",
        }
    }
}

/// Check if session is on Wayland
pub fn is_wayland_session() -> bool {
    env::var("XDG_SESSION_TYPE")
        .map(|s| s == "wayland")
        .unwrap_or(false)
}

/// Check if XWayland is available (for X11 fallback)
pub fn has_xwayland() -> bool {
    env::var("DISPLAY").is_ok()
}
```

### 4.2 Token Persistence Manager

New file `crates/core/src/screen/token_manager.rs`:

```rust
use crate::config::Config;
use crate::error::Result;
use std::sync::{Arc, RwLock};

/// Manages Portal restore tokens with persistent storage
pub struct TokenManager {
    /// Current token (may be more recent than config)
    current_token: RwLock<Option<String>>,
    /// Config for persistence
    config: Arc<RwLock<Config>>,
    /// Compositor for logging
    compositor: WaylandCompositor,
}

impl TokenManager {
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
        }
    }

    /// Get current token for use in Portal session
    pub fn get_token(&self) -> Option<String> {
        self.current_token.read().ok()?.clone()
    }

    /// Update token after successful session start
    ///
    /// This should be called immediately after receiving a new token from the portal,
    /// as tokens are single-use and the new one must be saved.
    pub fn update_token(&self, new_token: Option<String>) -> Result<()> {
        // Update in-memory
        if let Ok(mut current) = self.current_token.write() {
            *current = new_token.clone();
        }

        // Persist to config immediately
        if let Ok(mut config) = self.config.write() {
            config.screen_stream.portal_restore_token = new_token.clone();

            // Best-effort save - don't fail the whole operation if save fails
            if let Err(e) = config.save() {
                tracing::warn!("Failed to persist restore token to config: {}", e);
                // Continue anyway - in-memory token is updated
            } else {
                tracing::debug!("Restore token persisted to config");
            }
        }

        Ok(())
    }

    /// Invalidate current token (e.g., after permission revoked)
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

    /// Check if we have a token that might allow silent restore
    pub fn has_token(&self) -> bool {
        self.current_token.read().ok()
            .map(|t| t.is_some())
            .unwrap_or(false)
    }

    /// Get compositor capabilities
    pub fn compositor_capabilities(&self) -> CompositorCapabilities {
        self.compositor.capabilities()
    }

    /// Check if silent restore is likely to work
    pub fn can_likely_restore_silently(&self) -> bool {
        let caps = self.compositor.capabilities();
        caps.supports_restore_tokens && self.has_token()
    }
}
```

### 4.3 Enhanced Portal Capture

Modify `crates/core/src/screen/portal.rs`:

```rust
use crate::screen::token_manager::TokenManager;
use crate::screen::wayland::{WaylandCompositor, CompositorCapabilities};

pub struct PortalCapture {
    // Existing fields...

    /// Token manager for persistent restore tokens
    token_manager: Arc<TokenManager>,
    /// Detected compositor
    compositor: WaylandCompositor,
    /// Whether last session used restore (no dialog shown)
    last_session_was_silent: bool,
    /// Number of consecutive restore failures
    restore_failures: u32,
}

impl PortalCapture {
    /// Create with token manager for persistent tokens
    pub async fn new_with_token_manager(
        token_manager: Arc<TokenManager>,
    ) -> Result<Self> {
        if !Self::is_available().await {
            return Err(Error::Screen(
                "XDG Desktop Portal ScreenCast not available".into(),
            ));
        }

        let compositor = WaylandCompositor::detect();
        let caps = compositor.capabilities();

        tracing::info!(
            "Portal capture initializing on {} (restore_tokens: {}, unattended: {})",
            compositor.name(),
            caps.supports_restore_tokens,
            caps.supports_unattended
        );

        Ok(Self {
            state: SessionState::Created,
            restore_token: token_manager.get_token(),
            current_display: None,
            displays: Vec::new(),
            pipewire_node_id: None,
            frame_receiver: None,
            command_sender: None,
            pipewire_thread: None,
            frame_width: 0,
            frame_height: 0,
            token_manager,
            compositor,
            last_session_was_silent: false,
            restore_failures: 0,
        })
    }

    /// Attempt to start session, with smart restore handling
    async fn create_and_start_portal_session(&mut self) -> Result<(u32, OwnedFd)> {
        let restore_token = self.token_manager.get_token();
        let attempting_restore = restore_token.is_some();

        tracing::info!(
            "Creating Portal session (attempting_restore: {}, failures: {})",
            attempting_restore,
            self.restore_failures
        );

        // If we've failed to restore multiple times, clear token and force dialog
        let use_token = if self.restore_failures >= 2 {
            tracing::warn!("Multiple restore failures, clearing token and forcing dialog");
            self.token_manager.invalidate_token();
            None
        } else {
            restore_token
        };

        let screencast = Screencast::new().await?;
        let session = screencast.create_session().await?;

        // Select sources with or without restore token
        let select_result = screencast
            .select_sources(
                &session,
                CursorMode::Embedded,
                SourceType::Monitor.into(),
                false, // multiple = false
                use_token.as_deref(),
                PersistMode::ExplicitlyRevoked,
            )
            .await;

        // Handle select_sources failure (token may be invalid)
        if let Err(e) = select_result {
            if attempting_restore {
                tracing::warn!("Restore failed ({}), will retry without token", e);
                self.restore_failures += 1;
                self.token_manager.invalidate_token();

                // Retry without token
                screencast
                    .select_sources(
                        &session,
                        CursorMode::Embedded,
                        SourceType::Monitor.into(),
                        false,
                        None,
                        PersistMode::ExplicitlyRevoked,
                    )
                    .await?;
            } else {
                return Err(Error::Screen(format!("select_sources failed: {}", e)));
            }
        }

        // Start the session
        let response = screencast.start(&session, None).await?
            .response()?;

        // Success! Save new token immediately
        if let Some(token) = response.restore_token() {
            self.token_manager.update_token(Some(token.to_string()))?;
            self.restore_failures = 0; // Reset failure count on success

            // Track if this was a silent restore (no user interaction)
            self.last_session_was_silent = attempting_restore && self.restore_failures == 0;
        }

        // Get stream info
        let streams = response.streams();
        if streams.is_empty() {
            return Err(Error::Screen("No streams available".into()));
        }

        let stream = &streams[0];
        let node_id = stream.pipe_wire_node_id();

        // ... rest of existing logic ...

        let pipewire_fd = screencast.open_pipe_wire_remote(&session).await?;

        Ok((node_id, pipewire_fd))
    }

    /// Check if the last session started silently (no user dialog)
    pub fn was_silent_restore(&self) -> bool {
        self.last_session_was_silent
    }

    /// Get compositor capabilities for UI hints
    pub fn compositor_capabilities(&self) -> CompositorCapabilities {
        self.compositor.capabilities()
    }
}
```

### 4.4 Session Recovery

Add session recovery logic for handling disconnections:

```rust
impl PortalCapture {
    /// Attempt to recover a broken session
    pub async fn recover_session(&mut self) -> Result<()> {
        tracing::info!("Attempting session recovery");

        // Stop existing PipeWire thread
        self.stop_pipewire_thread();

        // Clear session state
        self.state = SessionState::Stopped;

        // Try to restart - this will attempt restore first
        let display_id = self.current_display.clone()
            .unwrap_or_else(|| "portal-default".to_string());

        self.start(&display_id).await
    }

    /// Handle session errors with smart recovery
    pub async fn handle_session_error(&mut self, error: &str) -> RecoveryAction {
        tracing::warn!("Session error: {}", error);

        // Check if this is a recoverable error
        if error.contains("stream ended")
            || error.contains("pipewire")
            || error.contains("timeout")
        {
            // Transient error - try to recover
            self.restore_failures = 0; // Don't penalize restore attempts
            return RecoveryAction::Retry;
        }

        if error.contains("permission")
            || error.contains("denied")
            || error.contains("revoked")
        {
            // Permission revoked - need new authorization
            self.token_manager.invalidate_token();
            return RecoveryAction::ReauthorizeRequired;
        }

        if error.contains("cancelled") {
            // User cancelled - don't auto-retry
            return RecoveryAction::UserCancelled;
        }

        // Unknown error
        RecoveryAction::Fatal
    }
}

/// Action to take after session error
pub enum RecoveryAction {
    /// Retry with existing token
    Retry,
    /// Need user to grant permission again
    ReauthorizeRequired,
    /// User explicitly cancelled
    UserCancelled,
    /// Unrecoverable error
    Fatal,
}
```

### 4.5 Backend Selection with Unattended Preference

Update the backend selection logic in `manager.rs`:

```rust
use crate::screen::wayland::{is_wayland_session, has_xwayland, WaylandCompositor};

/// Backend selection criteria
pub struct BackendPreference {
    /// Require unattended (no dialog) capability
    pub require_unattended: bool,
    /// Prefer low latency over features
    pub prefer_low_latency: bool,
    /// Specific backend to use (overrides auto)
    pub force_backend: Option<CaptureBackend>,
}

/// Available capture backends
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackend {
    Drm,
    Portal,
    X11,
    Dxgi,
    Auto,
}

/// Select the best available capture backend
pub async fn select_backend(
    pref: &BackendPreference,
    token_manager: Option<Arc<TokenManager>>,
) -> Result<Box<dyn ScreenCapture>> {
    // Handle forced backend
    if let Some(forced) = pref.force_backend {
        return create_specific_backend(forced, token_manager).await;
    }

    // Platform detection
    #[cfg(target_os = "windows")]
    {
        return Ok(Box::new(DxgiCapture::new().await?));
    }

    #[cfg(target_os = "linux")]
    {
        // Check if we want unattended and can achieve it
        if pref.require_unattended {
            // DRM is best for unattended (if we have CAP_SYS_ADMIN)
            if has_cap_sys_admin() {
                if let Ok(drm) = DrmCapture::new().await {
                    tracing::info!("Using DRM backend for unattended access");
                    return Ok(Box::new(drm));
                }
            }

            // Check if Portal can do unattended
            if is_wayland_session() {
                if let Some(ref tm) = token_manager {
                    let caps = tm.compositor_capabilities();
                    if caps.supports_restore_tokens && tm.has_token() {
                        tracing::info!(
                            "Portal may support unattended via restore token on {}",
                            caps.compositor.name()
                        );
                        // Try Portal
                    } else {
                        tracing::warn!(
                            "Unattended requested but {} doesn't support it without dialog",
                            caps.compositor.name()
                        );
                        // Fall through to try anyway, but warn user
                    }
                }
            }
        }

        // Standard selection order
        // 1. DRM if available (best performance)
        if has_cap_sys_admin() {
            if let Ok(drm) = DrmCapture::new().await {
                tracing::info!("Using DRM backend");
                return Ok(Box::new(drm));
            }
        }

        // 2. Portal if on Wayland
        if is_wayland_session() {
            let portal = if let Some(tm) = token_manager {
                PortalCapture::new_with_token_manager(tm).await
            } else {
                PortalCapture::new(None).await
            };

            if let Ok(p) = portal {
                tracing::info!("Using Portal backend");
                return Ok(Box::new(p));
            }
        }

        // 3. X11 if available (Xwayland or X11 session)
        if has_xwayland() || !is_wayland_session() {
            if let Ok(x11) = X11Capture::new().await {
                tracing::info!("Using X11 backend");
                return Ok(Box::new(x11));
            }
        }

        Err(Error::Screen("No capture backend available".into()))
    }
}

/// Check if process has CAP_SYS_ADMIN capability
#[cfg(target_os = "linux")]
fn has_cap_sys_admin() -> bool {
    use caps::{CapSet, Capability};
    caps::has_cap(None, CapSet::Effective, Capability::CAP_SYS_ADMIN)
        .unwrap_or(false)
}
```

### 4.6 User-Facing Status and Hints

Add types for communicating status to UI:

```rust
/// Status of unattended access capability
#[derive(Debug, Clone)]
pub struct UnattendedStatus {
    /// Whether unattended is currently possible
    pub available: bool,
    /// Human-readable explanation
    pub message: String,
    /// Action user can take to enable (if any)
    pub action_hint: Option<String>,
    /// Whether a dialog will be shown on next capture
    pub dialog_expected: bool,
}

impl PortalCapture {
    /// Get status of unattended access capability
    pub fn unattended_status(&self) -> UnattendedStatus {
        let caps = self.compositor.capabilities();
        let has_token = self.token_manager.has_token();

        if !caps.supports_restore_tokens {
            return UnattendedStatus {
                available: false,
                message: format!(
                    "{} doesn't support session restore tokens",
                    caps.compositor.name()
                ),
                action_hint: Some("Consider using DRM backend with CAP_SYS_ADMIN".into()),
                dialog_expected: true,
            };
        }

        if !has_token {
            return UnattendedStatus {
                available: false,
                message: "No restore token saved. Permission dialog will be shown.".into(),
                action_hint: Some(
                    "Grant permission once - subsequent sessions will be automatic.".into()
                ),
                dialog_expected: true,
            };
        }

        if self.restore_failures >= 2 {
            return UnattendedStatus {
                available: false,
                message: "Restore token appears invalid. Re-authorization required.".into(),
                action_hint: None,
                dialog_expected: true,
            };
        }

        UnattendedStatus {
            available: true,
            message: format!(
                "Restore token available for {}. Should connect without dialog.",
                caps.compositor.name()
            ),
            action_hint: None,
            dialog_expected: false,
        }
    }
}
```

### 4.7 Config Schema Updates

Update `crates/core/src/config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScreenStreamSettings {
    // Existing fields...

    /// Portal restore token for Wayland unattended access
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal_restore_token: Option<String>,

    /// Preferred capture backend (auto, drm, portal, x11)
    #[serde(default)]
    pub capture_backend: CaptureBackendPref,

    /// Whether to prefer unattended-capable backends
    #[serde(default)]
    pub prefer_unattended: bool,

    /// Last detected compositor (for debugging)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_compositor: Option<String>,

    /// Last successful restore timestamp (for debugging)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_restore_success: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CaptureBackendPref {
    #[default]
    Auto,
    Drm,
    Portal,
    X11,
}
```

---

## File Structure

```
crates/core/src/screen/
├── wayland.rs           # NEW: Compositor detection and capabilities
├── token_manager.rs     # NEW: Token persistence manager
├── portal.rs            # MODIFIED: Enhanced with token manager, recovery
├── mod.rs               # MODIFIED: Backend selection logic
├── manager.rs           # MODIFIED: Backend preference support
└── ... (existing files)
```

---

## Implementation Order

1. **4.1** - Compositor detection (`wayland.rs`)
2. **4.2** - Token manager (`token_manager.rs`)
3. **4.3** - Enhanced Portal capture (modify `portal.rs`)
4. **4.4** - Session recovery logic
5. **4.5** - Backend selection with preferences
6. **4.6** - User-facing status types
7. **4.7** - Config schema updates

---

## Testing Plan

### Manual Testing Matrix

| Compositor | Test Case | Expected Behavior |
|------------|-----------|-------------------|
| GNOME | Fresh start (no token) | Dialog shown, token saved |
| GNOME | With token | Silent restore, no dialog |
| GNOME | Invalid token | Dialog shown, new token saved |
| KDE Plasma | Fresh start | Dialog shown, token saved |
| KDE Plasma | With token | Silent restore (compositor permitting) |
| Sway | Fresh start | Dialog shown |
| Sway | With token | Restore attempted |

### Automated Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compositor_detection() {
        // Set env vars and verify detection
        std::env::set_var("XDG_CURRENT_DESKTOP", "GNOME");
        assert_eq!(WaylandCompositor::detect(), WaylandCompositor::Gnome);

        std::env::set_var("XDG_CURRENT_DESKTOP", "KDE");
        assert_eq!(WaylandCompositor::detect(), WaylandCompositor::KdePlasma);
    }

    #[test]
    fn test_token_manager_persistence() {
        let temp_config = tempfile::NamedTempFile::new().unwrap();
        let config = Arc::new(RwLock::new(Config::default()));

        let tm = TokenManager::new(config.clone());

        // No token initially
        assert!(!tm.has_token());

        // Save token
        tm.update_token(Some("test-token-123".into())).unwrap();
        assert!(tm.has_token());
        assert_eq!(tm.get_token(), Some("test-token-123".into()));

        // Verify persisted
        let config_read = config.read().unwrap();
        assert_eq!(
            config_read.screen_stream.portal_restore_token,
            Some("test-token-123".into())
        );
    }

    #[test]
    fn test_unattended_status() {
        // Test various scenarios
        let status = UnattendedStatus {
            available: false,
            message: "Test".into(),
            action_hint: Some("Do something".into()),
            dialog_expected: true,
        };
        assert!(!status.available);
        assert!(status.dialog_expected);
    }
}
```

---

## Future Enhancements (Out of Scope)

These are noted for future work but not part of Phase 4:

1. **GNOME Remote Desktop integration** - Use gnome-remote-desktop service for true unattended
2. **libei integration** - For input injection on Wayland without portal
3. **KRdp integration** - Native KDE RDP protocol support
4. **Headless mode** - Virtual display for servers without physical display

---

## Summary

Phase 4 provides:

1. **Compositor-aware behavior** - Detect GNOME, KDE, Sway, etc. and adapt
2. **Robust token lifecycle** - Save immediately, handle invalidation gracefully
3. **Smart session recovery** - Automatic retry with appropriate fallback
4. **Clear user messaging** - Tell users what to expect and how to enable unattended
5. **Backend selection** - Choose best backend based on unattended requirements

The key insight is that "unattended" on Wayland is a spectrum:
- **DRM**: Truly unattended (with CAP_SYS_ADMIN)
- **Portal + Token**: Semi-unattended (no dialog after initial grant, but token may expire)
- **Portal without Token**: Interactive (always shows dialog)

The architecture accommodates all these levels and helps users understand their current state.
