//! Croc executable detection.

use crate::error::{Error, Result};
use std::path::PathBuf;
use std::sync::OnceLock;
use tracing::{debug, info, warn};

/// Cached croc executable path.
static CROC_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Find the croc executable.
///
/// Search order:
/// 1. CROC_PATH environment variable
/// 2. PATH environment variable
/// 3. Common installation locations (platform-specific)
///
/// The result is cached after the first call.
pub fn find_croc_executable() -> Result<PathBuf> {
    let path = CROC_PATH.get_or_init(|| {
        // 1. Check CROC_PATH environment variable
        if let Ok(path) = std::env::var("CROC_PATH") {
            let path = PathBuf::from(&path);
            if path.exists() {
                info!("Found croc via CROC_PATH: {:?}", path);
                return Some(path);
            }
            warn!("CROC_PATH set but file not found: {:?}", path);
        }

        // 2. Check PATH
        if let Some(path) = find_in_path() {
            info!("Found croc in PATH: {:?}", path);
            return Some(path);
        }

        // 3. Check common installation locations
        if let Some(path) = find_in_common_locations() {
            info!("Found croc in common location: {:?}", path);
            return Some(path);
        }

        warn!("croc executable not found");
        None
    });

    path.clone().ok_or(Error::CrocNotFound)
}

/// Find croc in the system PATH.
fn find_in_path() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "croc.exe" } else { "croc" };

    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join(exe_name))
            .find(|p| p.exists())
    })
}

/// Find croc in common installation locations.
#[cfg(target_os = "windows")]
fn find_in_common_locations() -> Option<PathBuf> {
    let locations = [
        // Scoop
        dirs::home_dir().map(|h| h.join("scoop").join("shims").join("croc.exe")),
        // Chocolatey
        Some(PathBuf::from(r"C:\ProgramData\chocolatey\bin\croc.exe")),
        // Local app data
        dirs::data_local_dir().map(|d| d.join("croc").join("croc.exe")),
        // Program Files
        Some(PathBuf::from(r"C:\Program Files\croc\croc.exe")),
        Some(PathBuf::from(r"C:\Program Files (x86)\croc\croc.exe")),
        // WinGet typical location
        dirs::home_dir().map(|h| {
            h.join("AppData")
                .join("Local")
                .join("Microsoft")
                .join("WinGet")
                .join("Packages")
        }),
    ];

    for location in locations.into_iter().flatten() {
        debug!("Checking for croc at: {:?}", location);
        if location.exists() {
            return Some(location);
        }
    }

    None
}

#[cfg(target_os = "linux")]
fn find_in_common_locations() -> Option<PathBuf> {
    let locations = [
        PathBuf::from("/usr/bin/croc"),
        PathBuf::from("/usr/local/bin/croc"),
        PathBuf::from("/snap/bin/croc"),
        dirs::home_dir().map(|h| h.join(".local").join("bin").join("croc")),
        dirs::home_dir().map(|h| h.join("bin").join("croc")),
        dirs::home_dir().map(|h| h.join("go").join("bin").join("croc")),
    ];

    for location in locations.into_iter().flatten() {
        debug!("Checking for croc at: {:?}", location);
        if location.exists() {
            return Some(location);
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn find_in_common_locations() -> Option<PathBuf> {
    let locations = [
        PathBuf::from("/usr/local/bin/croc"),
        PathBuf::from("/opt/homebrew/bin/croc"),
        dirs::home_dir().map(|h| h.join(".local").join("bin").join("croc")),
        dirs::home_dir().map(|h| h.join("go").join("bin").join("croc")),
    ];

    for location in locations.into_iter().flatten() {
        debug!("Checking for croc at: {:?}", location);
        if location.exists() {
            return Some(location);
        }
    }

    None
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn find_in_common_locations() -> Option<PathBuf> {
    None
}

/// Clear the cached croc path (useful for testing).
#[cfg(test)]
pub fn clear_cache() {
    // OnceLock doesn't support clearing, so tests should use fresh process instances
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_croc_returns_result() {
        // This test just ensures the function doesn't panic
        let result = find_croc_executable();
        // May or may not find croc depending on the system
        println!("croc found: {:?}", result);
    }
}

