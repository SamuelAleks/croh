//! Cross-platform utilities.

use std::path::PathBuf;

/// Get the default download directory for received files.
pub fn default_download_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .map(|h| h.join("Downloads"))
            .unwrap_or_else(|| PathBuf::from("."))
    })
}

/// Get the application data directory.
///
/// - Linux: `~/.local/share/croh`
/// - Windows: `%LOCALAPPDATA%\croh`
/// - macOS: `~/Library/Application Support/croh`
pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("croh")
}

/// Get the configuration directory.
///
/// - Linux: `~/.config/croh`
/// - Windows: `%APPDATA%\croh`
/// - macOS: `~/Library/Application Support/croh`
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("croh")
}

/// Get the path to the main config file.
pub fn config_file_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Get the path to the identity file (for Iroh, future use).
pub fn identity_file_path() -> PathBuf {
    data_dir().join("identity.json")
}

/// Get the path to the peers file.
pub fn peers_file_path() -> PathBuf {
    data_dir().join("peers.json")
}

/// Get the temporary upload directory for files being sent.
pub fn upload_dir() -> PathBuf {
    data_dir().join("uploads")
}

/// Open a path in the system file explorer.
#[cfg(target_os = "windows")]
pub fn open_in_explorer(path: &std::path::Path) -> std::io::Result<()> {
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn open_in_explorer(path: &std::path::Path) -> std::io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn open_in_explorer(path: &std::path::Path) -> std::io::Result<()> {
    std::process::Command::new("open")
        .arg(path)
        .spawn()?;
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
pub fn open_in_explorer(_path: &std::path::Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "open_in_explorer not supported on this platform",
    ))
}

