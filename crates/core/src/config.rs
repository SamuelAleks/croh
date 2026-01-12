//! Configuration management for Croc GUI.

use crate::croc::{Curve, HashAlgorithm};
use crate::error::Result;
use crate::platform;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Application theme.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
    Dracula,
    Nord,
    SolarizedDark,
    SolarizedLight,
    GruvboxDark,
    GruvboxLight,
    CatppuccinMocha,
    CatppuccinLatte,
}

impl Theme {
    /// Convert theme to the string format expected by the UI.
    pub fn to_ui_string(&self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
            Theme::Dracula => "dracula",
            Theme::Nord => "nord",
            Theme::SolarizedDark => "solarized-dark",
            Theme::SolarizedLight => "solarized-light",
            Theme::GruvboxDark => "gruvbox-dark",
            Theme::GruvboxLight => "gruvbox-light",
            Theme::CatppuccinMocha => "catppuccin-mocha",
            Theme::CatppuccinLatte => "catppuccin-latte",
        }
    }

    /// Parse theme from UI string.
    pub fn from_ui_string(s: &str) -> Self {
        match s {
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            "dracula" => Theme::Dracula,
            "nord" => Theme::Nord,
            "solarized-dark" => Theme::SolarizedDark,
            "solarized-light" => Theme::SolarizedLight,
            "gruvbox-dark" => Theme::GruvboxDark,
            "gruvbox-light" => Theme::GruvboxLight,
            "catppuccin-mocha" => Theme::CatppuccinMocha,
            "catppuccin-latte" => Theme::CatppuccinLatte,
            _ => Theme::System,
        }
    }
}

/// Window size configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

impl Default for WindowSize {
    fn default() -> Self {
        Self {
            width: 700,
            height: 600,
        }
    }
}

/// Browse settings for file sharing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseSettings {
    /// Show hidden files (files starting with .)
    #[serde(default)]
    pub show_hidden: bool,

    /// Show protected system files/folders
    #[serde(default)]
    pub show_protected: bool,

    /// Patterns to exclude from browsing (glob patterns like "*.tmp", "node_modules")
    #[serde(default)]
    pub exclude_patterns: Vec<String>,

    /// Custom allowed paths for browsing (if empty, uses default home directory)
    #[serde(default)]
    pub allowed_paths: Vec<PathBuf>,
}

impl Default for BrowseSettings {
    fn default() -> Self {
        Self {
            show_hidden: false,
            show_protected: false,
            exclude_patterns: vec![
                // Common excludes
                "node_modules".to_string(),
                ".git".to_string(),
                "__pycache__".to_string(),
                "*.tmp".to_string(),
                "*.swp".to_string(),
            ],
            allowed_paths: Vec::new(),
        }
    }
}

/// Do Not Disturb mode setting.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DndMode {
    /// DND is off, available for transfers
    #[default]
    Off,
    /// Silent mode - accept but queue notifications
    Silent,
    /// Busy mode - reject incoming requests
    Busy,
}

impl DndMode {
    /// Convert to UI string.
    pub fn to_ui_string(&self) -> &'static str {
        match self {
            DndMode::Off => "off",
            DndMode::Silent => "silent",
            DndMode::Busy => "busy",
        }
    }

    /// Parse from UI string.
    pub fn from_ui_string(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "silent" => DndMode::Silent,
            "busy" => DndMode::Busy,
            _ => DndMode::Off,
        }
    }
}

/// Relay connection preference for peer-to-peer connections.
///
/// This controls how the application uses relay servers for NAT traversal.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RelayPreference {
    /// Normal mode - use relays with automatic direct connection upgrades (default).
    /// Iroh will connect via relay and automatically upgrade to direct when possible.
    #[default]
    Normal,
    /// Prefer direct connections - still use relay as fallback if direct fails.
    /// May have slightly longer initial connection times.
    DirectPreferred,
    /// Relay only - always use relay, never attempt direct connections.
    /// More reliable but higher latency and bandwidth usage.
    RelayOnly,
    /// Disabled - no relay at all, only direct connections.
    /// May fail to connect through NAT/firewalls.
    Disabled,
}

impl RelayPreference {
    /// Convert to UI string.
    pub fn to_ui_string(&self) -> &'static str {
        match self {
            RelayPreference::Normal => "normal",
            RelayPreference::DirectPreferred => "direct-preferred",
            RelayPreference::RelayOnly => "relay-only",
            RelayPreference::Disabled => "disabled",
        }
    }

    /// Parse from UI string.
    pub fn from_ui_string(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "direct-preferred" => RelayPreference::DirectPreferred,
            "relay-only" => RelayPreference::RelayOnly,
            "disabled" => RelayPreference::Disabled,
            _ => RelayPreference::Normal,
        }
    }
}

/// Security posture for the application.
///
/// This controls default settings for guest peers, auto-accept behavior,
/// and other security-related options.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SecurityPosture {
    /// Maximum convenience, minimal friction.
    /// Best for trusted home networks.
    Relaxed,
    /// Good balance for most users (default).
    #[default]
    Balanced,
    /// Tighter security for sensitive situations.
    /// Shorter guest durations, requires approval for pushes.
    Cautious,
}

impl SecurityPosture {
    /// Convert to UI string.
    pub fn to_ui_string(&self) -> &'static str {
        match self {
            SecurityPosture::Relaxed => "relaxed",
            SecurityPosture::Balanced => "balanced",
            SecurityPosture::Cautious => "cautious",
        }
    }

    /// Parse from UI string.
    pub fn from_ui_string(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "relaxed" => SecurityPosture::Relaxed,
            "cautious" => SecurityPosture::Cautious,
            _ => SecurityPosture::Balanced,
        }
    }

    /// Get the default guest policy for this posture.
    pub fn default_guest_policy(&self) -> GuestPolicy {
        match self {
            SecurityPosture::Relaxed => GuestPolicy {
                default_duration_hours: 168, // 1 week
                max_duration_hours: 336,     // 2 weeks
                allow_extensions: true,
                max_extensions: u32::MAX, // Unlimited
                allow_promotion_requests: true,
                auto_accept_guest_pushes: true,
            },
            SecurityPosture::Balanced => GuestPolicy {
                default_duration_hours: 72, // 3 days
                max_duration_hours: 168,    // 1 week
                allow_extensions: true,
                max_extensions: 3,
                allow_promotion_requests: true,
                auto_accept_guest_pushes: true,
            },
            SecurityPosture::Cautious => GuestPolicy {
                default_duration_hours: 24, // 1 day
                max_duration_hours: 48,     // 2 days
                allow_extensions: true,
                max_extensions: 1,
                allow_promotion_requests: false,
                auto_accept_guest_pushes: false,
            },
        }
    }
}

// ==================== Screen Streaming Settings ====================

/// Capture backend preference for screen streaming.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureBackend {
    /// Auto-detect best backend for platform
    #[default]
    Auto,
    /// DRM/KMS direct framebuffer (Linux, requires CAP_SYS_ADMIN)
    Drm,
    /// wlroots screencopy protocol (Sway, Hyprland - no prompts)
    WlrScreencopy,
    /// XDG Desktop Portal (may prompt on first use)
    Portal,
    /// X11 SHM (X11 sessions only)
    X11,
    /// DXGI Desktop Duplication (Windows)
    Dxgi,
}

impl CaptureBackend {
    /// Convert to UI string.
    pub fn to_ui_string(&self) -> &'static str {
        match self {
            CaptureBackend::Auto => "auto",
            CaptureBackend::Drm => "drm",
            CaptureBackend::WlrScreencopy => "wlr-screencopy",
            CaptureBackend::Portal => "portal",
            CaptureBackend::X11 => "x11",
            CaptureBackend::Dxgi => "dxgi",
        }
    }

    /// Parse from UI string.
    pub fn from_ui_string(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "drm" => CaptureBackend::Drm,
            "wlr-screencopy" | "wlroots" => CaptureBackend::WlrScreencopy,
            "portal" | "xdg" => CaptureBackend::Portal,
            "x11" => CaptureBackend::X11,
            "dxgi" => CaptureBackend::Dxgi,
            _ => CaptureBackend::Auto,
        }
    }
}

/// Screen streaming settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenStreamSettings {
    /// Enable screen streaming functionality.
    /// When false, all screen streaming requests are rejected.
    #[serde(default)]
    pub enabled: bool,

    /// Maximum FPS to stream (0 = use system default, typically 60).
    #[serde(default = "default_max_fps")]
    pub max_fps: u32,

    /// Minimum bitrate in Kbps (quality floor).
    #[serde(default = "default_min_bitrate")]
    pub min_bitrate_kbps: u32,

    /// Maximum bitrate in Kbps (bandwidth cap).
    #[serde(default = "default_max_bitrate")]
    pub max_bitrate_kbps: u32,

    /// Allow input control (keyboard/mouse) from viewers.
    #[serde(default = "default_true")]
    pub allow_input: bool,

    /// Require confirmation dialog for each stream request.
    #[serde(default)]
    pub require_confirmation: bool,

    /// Lock screen when streaming ends (security feature).
    #[serde(default)]
    pub lock_on_disconnect: bool,

    /// Capture backend preference.
    #[serde(default)]
    pub capture_backend: CaptureBackend,

    /// Show cursor in captured frames.
    #[serde(default = "default_true")]
    pub show_cursor: bool,

    /// Highlight cursor clicks (visual feedback for viewers).
    #[serde(default)]
    pub highlight_clicks: bool,

    /// Maximum frames to buffer before applying backpressure.
    #[serde(default = "default_max_buffer_frames")]
    pub max_buffer_frames: u32,

    /// XDG Portal restore token for persistent unattended screen capture.
    ///
    /// This token allows bypassing the permission dialog on subsequent screen
    /// capture sessions. The token is single-use and updated after each session.
    ///
    /// Note: Only used on Linux with the Portal capture backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portal_restore_token: Option<String>,
}

fn default_max_buffer_frames() -> u32 {
    10 // Maximum frames in-flight before backpressure
}

fn default_max_fps() -> u32 {
    60
}

fn default_min_bitrate() -> u32 {
    500 // 500 Kbps
}

fn default_max_bitrate() -> u32 {
    20000 // 20 Mbps
}

fn default_true() -> bool {
    true
}

impl Default for ScreenStreamSettings {
    fn default() -> Self {
        Self {
            enabled: false, // Opt-in by default for security
            max_fps: 60,
            min_bitrate_kbps: 500,
            max_bitrate_kbps: 20000,
            allow_input: true,
            require_confirmation: false,
            lock_on_disconnect: false,
            capture_backend: CaptureBackend::Auto,
            show_cursor: true,
            highlight_clicks: false,
            max_buffer_frames: 10,
            portal_restore_token: None,
        }
    }
}

/// Guest peer policy settings.
///
/// These settings control how temporary guest peers are handled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuestPolicy {
    /// Default duration for guest access (hours).
    pub default_duration_hours: u32,

    /// Maximum duration for a single guest session (hours).
    pub max_duration_hours: u32,

    /// Whether guests can request extensions.
    pub allow_extensions: bool,

    /// Maximum number of extensions allowed (u32::MAX = unlimited).
    pub max_extensions: u32,

    /// Whether guests can request promotion to trusted.
    pub allow_promotion_requests: bool,

    /// Auto-accept pushes from guests (vs require approval).
    pub auto_accept_guest_pushes: bool,
}

impl Default for GuestPolicy {
    fn default() -> Self {
        SecurityPosture::Balanced.default_guest_policy()
    }
}

impl GuestPolicy {
    /// Get the default duration as a chrono Duration.
    pub fn default_duration(&self) -> chrono::Duration {
        chrono::Duration::hours(self.default_duration_hours as i64)
    }

    /// Check if an extension is allowed given the current extension count.
    pub fn can_extend(&self, current_count: u32) -> bool {
        self.allow_extensions && current_count < self.max_extensions
    }
}

/// Main configuration struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Directory where received files are saved.
    pub download_dir: PathBuf,

    /// Default croc relay address (None = use croc default).
    pub default_relay: Option<String>,

    /// UI theme.
    pub theme: Theme,

    /// Path to croc executable (None = auto-detect).
    pub croc_path: Option<PathBuf>,

    /// Device nickname for display in UI (None = use hostname).
    #[serde(default)]
    pub device_nickname: Option<String>,

    /// Default hash algorithm for file verification.
    #[serde(default)]
    pub default_hash: Option<HashAlgorithm>,

    /// Default elliptic curve for PAKE encryption.
    #[serde(default)]
    pub default_curve: Option<Curve>,

    /// Bandwidth throttle limit (e.g., "1M", "500K").
    #[serde(default)]
    pub throttle: Option<String>,

    /// Force relay-only transfers (disable local network).
    #[serde(default)]
    pub no_local: bool,

    /// Window size (persisted across sessions).
    #[serde(default)]
    pub window_size: WindowSize,

    /// Browse settings for file sharing.
    #[serde(default)]
    pub browse_settings: BrowseSettings,

    /// Do Not Disturb mode.
    #[serde(default)]
    pub dnd_mode: DndMode,

    /// Custom DND message to show to peers.
    #[serde(default)]
    pub dnd_message: Option<String>,

    /// Show session statistics in header (privacy toggle).
    #[serde(default)]
    pub show_session_stats: bool,

    /// Keep completed transfers in the list (false = auto-clear after completion).
    #[serde(default = "default_keep_completed_transfers")]
    pub keep_completed_transfers: bool,

    /// Security posture (Easy-Secure toggle).
    #[serde(default)]
    pub security_posture: SecurityPosture,

    /// Guest peer policy (derived from security_posture by default).
    #[serde(default)]
    pub guest_policy: GuestPolicy,

    /// Relay connection preference for peer-to-peer connections.
    #[serde(default)]
    pub relay_preference: RelayPreference,

    /// Screen streaming settings.
    #[serde(default)]
    pub screen_stream: ScreenStreamSettings,
}

fn default_keep_completed_transfers() -> bool {
    true // Default to keeping transfers visible
}

impl Default for Config {
    fn default() -> Self {
        Self {
            download_dir: platform::default_download_dir(),
            default_relay: None,
            theme: Theme::default(),
            croc_path: None,
            device_nickname: None,
            default_hash: None,
            default_curve: None,
            throttle: None,
            no_local: false,
            window_size: WindowSize::default(),
            browse_settings: BrowseSettings::default(),
            dnd_mode: DndMode::default(),
            dnd_message: None,
            show_session_stats: false,
            keep_completed_transfers: true,
            security_posture: SecurityPosture::default(),
            guest_policy: GuestPolicy::default(),
            relay_preference: RelayPreference::default(),
            screen_stream: ScreenStreamSettings::default(),
        }
    }
}

impl Config {
    /// Load configuration from the default config file.
    pub fn load() -> Result<Self> {
        let config_path = platform::config_file_path();

        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)?;
            let mut config: Config = serde_json::from_str(&contents)?;
            config.fix_invalid_values();
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    /// Fix any invalid or empty values with sensible defaults.
    fn fix_invalid_values(&mut self) {
        // If download_dir is empty, use the default
        if self.download_dir.as_os_str().is_empty() {
            self.download_dir = platform::default_download_dir();
        }
    }

    /// Save configuration to the default config file.
    pub fn save(&mut self) -> Result<()> {
        // Fix any invalid values before saving
        self.fix_invalid_values();

        let config_path = platform::config_file_path();

        // Ensure parent directory exists
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(self)?;
        std::fs::write(&config_path, contents)?;

        Ok(())
    }

    /// Load configuration from environment variables, falling back to file/defaults.
    pub fn load_with_env() -> Result<Self> {
        let mut config = Self::load()?;

        // Override with environment variables
        if let Ok(path) = std::env::var("CROC_PATH") {
            config.croc_path = Some(PathBuf::from(path));
        }

        // CROH_DOWNLOAD_DIR takes precedence, fall back to legacy CROC_GUI_DOWNLOAD_DIR
        if let Ok(dir) = std::env::var("CROH_DOWNLOAD_DIR") {
            config.download_dir = PathBuf::from(dir);
        } else if let Ok(dir) = std::env::var("CROC_GUI_DOWNLOAD_DIR") {
            config.download_dir = PathBuf::from(dir);
        }

        Ok(config)
    }

    /// Get the device display name (nickname if set, otherwise hostname).
    pub fn get_device_display_name(&self) -> String {
        if let Some(ref nickname) = self.device_nickname {
            if !nickname.trim().is_empty() {
                return nickname.clone();
            }
        }
        // Fall back to hostname
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "Unknown Device".to_string())
    }

    /// Get just the hostname (for technical displays).
    pub fn get_hostname() -> String {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "Unknown".to_string())
    }
}
