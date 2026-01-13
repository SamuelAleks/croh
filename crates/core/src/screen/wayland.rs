//! Wayland compositor detection and capabilities.
//!
//! This module provides detection of Wayland compositors and their capabilities
//! for screen sharing. Different compositors have different levels of support
//! for features like restore tokens and unattended access.

use std::env;

/// Known Wayland compositors and their capabilities.
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

/// Compositor capabilities for screen sharing.
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
    /// Detect the current Wayland compositor.
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

    /// Get capabilities for this compositor.
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

    /// Get user-friendly name.
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

/// Check if session is on Wayland.
pub fn is_wayland_session() -> bool {
    env::var("XDG_SESSION_TYPE")
        .map(|s| s == "wayland")
        .unwrap_or(false)
}

/// Check if XWayland is available (for X11 fallback).
pub fn has_xwayland() -> bool {
    env::var("DISPLAY").is_ok()
}

/// Check if session is on X11.
pub fn is_x11_session() -> bool {
    env::var("XDG_SESSION_TYPE")
        .map(|s| s == "x11")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Global mutex to serialize tests that modify environment variables
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper to save and restore environment variables around a test
    fn with_env_vars<F, R>(vars: &[&str], f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _guard = ENV_MUTEX.lock().unwrap();

        // Save all original values
        let originals: Vec<_> = vars.iter().map(|v| (*v, env::var(*v).ok())).collect();

        let result = f();

        // Restore all original values
        for (var, original) in originals {
            match original {
                Some(v) => env::set_var(var, v),
                None => env::remove_var(var),
            }
        }

        result
    }

    #[test]
    fn test_compositor_detection_gnome() {
        with_env_vars(
            &[
                "XDG_CURRENT_DESKTOP",
                "DESKTOP_SESSION",
                "SWAYSOCK",
                "HYPRLAND_INSTANCE_SIGNATURE",
            ],
            || {
                env::remove_var("SWAYSOCK");
                env::remove_var("HYPRLAND_INSTANCE_SIGNATURE");
                env::remove_var("DESKTOP_SESSION");

                env::set_var("XDG_CURRENT_DESKTOP", "GNOME");
                assert_eq!(WaylandCompositor::detect(), WaylandCompositor::Gnome);

                env::set_var("XDG_CURRENT_DESKTOP", "ubuntu:GNOME");
                assert_eq!(WaylandCompositor::detect(), WaylandCompositor::Gnome);
            },
        );
    }

    #[test]
    fn test_compositor_detection_kde() {
        with_env_vars(
            &[
                "XDG_CURRENT_DESKTOP",
                "DESKTOP_SESSION",
                "SWAYSOCK",
                "HYPRLAND_INSTANCE_SIGNATURE",
            ],
            || {
                env::remove_var("SWAYSOCK");
                env::remove_var("HYPRLAND_INSTANCE_SIGNATURE");
                env::remove_var("DESKTOP_SESSION");

                env::set_var("XDG_CURRENT_DESKTOP", "KDE");
                assert_eq!(WaylandCompositor::detect(), WaylandCompositor::KdePlasma);

                env::set_var("XDG_CURRENT_DESKTOP", "plasma");
                assert_eq!(WaylandCompositor::detect(), WaylandCompositor::KdePlasma);
            },
        );
    }

    #[test]
    fn test_compositor_detection_sway() {
        with_env_vars(
            &[
                "XDG_CURRENT_DESKTOP",
                "DESKTOP_SESSION",
                "SWAYSOCK",
                "HYPRLAND_INSTANCE_SIGNATURE",
            ],
            || {
                env::remove_var("HYPRLAND_INSTANCE_SIGNATURE");
                env::remove_var("DESKTOP_SESSION");

                // Via XDG_CURRENT_DESKTOP
                env::remove_var("SWAYSOCK");
                env::set_var("XDG_CURRENT_DESKTOP", "sway");
                assert_eq!(WaylandCompositor::detect(), WaylandCompositor::Sway);

                // Via SWAYSOCK
                env::remove_var("XDG_CURRENT_DESKTOP");
                env::set_var("SWAYSOCK", "/run/user/1000/sway-ipc.sock");
                assert_eq!(WaylandCompositor::detect(), WaylandCompositor::Sway);
            },
        );
    }

    #[test]
    fn test_compositor_detection_hyprland() {
        with_env_vars(
            &[
                "XDG_CURRENT_DESKTOP",
                "DESKTOP_SESSION",
                "SWAYSOCK",
                "HYPRLAND_INSTANCE_SIGNATURE",
            ],
            || {
                env::remove_var("XDG_CURRENT_DESKTOP");
                env::remove_var("DESKTOP_SESSION");
                env::remove_var("SWAYSOCK");

                env::set_var("HYPRLAND_INSTANCE_SIGNATURE", "abc123");
                assert_eq!(WaylandCompositor::detect(), WaylandCompositor::Hyprland);
            },
        );
    }

    #[test]
    fn test_compositor_capabilities() {
        let caps = WaylandCompositor::Gnome.capabilities();
        assert!(caps.supports_restore_tokens);
        assert!(!caps.supports_unattended);
        assert!(caps.supports_cursor_metadata);
        assert_eq!(caps.min_portal_version, Some((1, 14)));

        let caps = WaylandCompositor::KdePlasma.capabilities();
        assert!(caps.supports_restore_tokens);
        assert!(!caps.supports_cursor_metadata);

        let caps = WaylandCompositor::Unknown.capabilities();
        assert!(caps.supports_restore_tokens);
    }

    #[test]
    fn test_compositor_names() {
        assert_eq!(WaylandCompositor::Gnome.name(), "GNOME");
        assert_eq!(WaylandCompositor::KdePlasma.name(), "KDE Plasma");
        assert_eq!(WaylandCompositor::Sway.name(), "Sway");
        assert_eq!(WaylandCompositor::Hyprland.name(), "Hyprland");
        assert_eq!(WaylandCompositor::Wlroots.name(), "wlroots-based");
        assert_eq!(WaylandCompositor::Unknown.name(), "Unknown");
    }

    #[test]
    fn test_wayland_session_detection() {
        with_env_vars(&["XDG_SESSION_TYPE"], || {
            env::set_var("XDG_SESSION_TYPE", "wayland");
            assert!(is_wayland_session());
            assert!(!is_x11_session());

            env::set_var("XDG_SESSION_TYPE", "x11");
            assert!(!is_wayland_session());
            assert!(is_x11_session());
        });
    }
}
