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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            download_dir: platform::default_download_dir(),
            default_relay: None,
            theme: Theme::default(),
            croc_path: None,
            default_hash: None,
            default_curve: None,
            throttle: None,
            no_local: false,
            window_size: WindowSize::default(),
            browse_settings: BrowseSettings::default(),
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
}

