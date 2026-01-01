//! Configuration management for Croc GUI.

use crate::croc::{Curve, HashAlgorithm};
use crate::error::Result;
use crate::platform;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Application theme.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
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

        if let Ok(dir) = std::env::var("CROC_GUI_DOWNLOAD_DIR") {
            config.download_dir = PathBuf::from(dir);
        }

        Ok(config)
    }
}

