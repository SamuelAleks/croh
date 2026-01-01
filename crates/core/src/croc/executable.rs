//! Croc executable detection.

use crate::error::{Error, Result};
use std::path::PathBuf;
use std::sync::RwLock;
use tracing::{debug, info, warn};

/// Cached croc executable path (uses RwLock to allow refresh).
static CROC_PATH: RwLock<Option<Option<PathBuf>>> = RwLock::new(None);

/// Find the croc executable.
///
/// Search order:
/// 1. CROC_PATH environment variable
/// 2. PATH environment variable
/// 3. Common installation locations (platform-specific)
///
/// The result is cached after the first call. Use `refresh_croc_cache()` to re-detect.
pub fn find_croc_executable() -> Result<PathBuf> {
    // Check cache first
    {
        let cache = CROC_PATH.read().unwrap();
        if let Some(ref cached) = *cache {
            return cached.clone().ok_or(Error::CrocNotFound);
        }
    }

    // Not cached, do the search
    let result = detect_croc();

    // Store in cache
    {
        let mut cache = CROC_PATH.write().unwrap();
        *cache = Some(result.clone());
    }

    result.ok_or(Error::CrocNotFound)
}

/// Clear the croc path cache and re-detect.
/// Call this after the user installs croc.
pub fn refresh_croc_cache() -> Result<PathBuf> {
    {
        let mut cache = CROC_PATH.write().unwrap();
        *cache = None;
    }
    find_croc_executable()
}

/// Actually detect croc without caching.
fn detect_croc() -> Option<PathBuf> {
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
    let mut locations: Vec<PathBuf> = vec![
        // Chocolatey
        PathBuf::from(r"C:\ProgramData\chocolatey\bin\croc.exe"),
        // Program Files
        PathBuf::from(r"C:\Program Files\croc\croc.exe"),
        PathBuf::from(r"C:\Program Files (x86)\croc\croc.exe"),
    ];

    // Add user-specific locations
    if let Some(home) = dirs::home_dir() {
        // Scoop
        locations.push(home.join("scoop").join("shims").join("croc.exe"));
        locations.push(home.join("scoop").join("apps").join("croc").join("current").join("croc.exe"));

        // Go bin (if installed via go install)
        locations.push(home.join("go").join("bin").join("croc.exe"));

        // Local bin
        locations.push(home.join(".local").join("bin").join("croc.exe"));
    }

    if let Some(local_data) = dirs::data_local_dir() {
        // Local app data direct install
        locations.push(local_data.join("croc").join("croc.exe"));

        // WinGet packages - search for croc in the packages directory
        let winget_packages = local_data.join("Microsoft").join("WinGet").join("Packages");
        if winget_packages.exists() {
            if let Ok(entries) = std::fs::read_dir(&winget_packages) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let dir_name = path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("");
                        // Look for croc package (schollz.croc or similar)
                        if dir_name.to_lowercase().contains("croc") {
                            // Search for croc.exe in this package
                            if let Some(exe) = find_exe_in_dir(&path, "croc.exe") {
                                locations.push(exe);
                            }
                        }
                    }
                }
            }
        }

        // WinGet Links directory (where winget creates shims)
        let winget_links = local_data.join("Microsoft").join("WinGet").join("Links");
        locations.push(winget_links.join("croc.exe"));
    }

    for location in locations {
        debug!("Checking for croc at: {:?}", location);
        if location.exists() {
            return Some(location);
        }
    }

    None
}

/// Recursively find an executable in a directory (limited depth).
#[cfg(target_os = "windows")]
fn find_exe_in_dir(dir: &PathBuf, exe_name: &str) -> Option<PathBuf> {
    find_exe_recursive(dir, exe_name, 3)
}

#[cfg(target_os = "windows")]
fn find_exe_recursive(dir: &PathBuf, exe_name: &str, depth: u32) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }

    let direct = dir.join(exe_name);
    if direct.exists() {
        return Some(direct);
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find_exe_recursive(&path, exe_name, depth - 1) {
                    return Some(found);
                }
            }
        }
    }

    None
}

#[cfg(target_os = "linux")]
fn find_in_common_locations() -> Option<PathBuf> {
    let locations: [Option<PathBuf>; 6] = [
        Some(PathBuf::from("/usr/bin/croc")),
        Some(PathBuf::from("/usr/local/bin/croc")),
        Some(PathBuf::from("/snap/bin/croc")),
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
    let mut cache = CROC_PATH.write().unwrap();
    *cache = None;
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

