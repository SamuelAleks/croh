//! File browsing functionality for trusted peers.
//!
//! This module provides directory listing and path validation for remote file browsing.

use crate::error::{Error, Result};
use crate::iroh::protocol::DirectoryEntry;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Default allowed paths for browsing if none are configured.
/// Returns the user's home directory and common locations.
pub fn default_browsable_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(home) = dirs::home_dir() {
        // Add common subdirectories
        let downloads = home.join("Downloads");
        if downloads.exists() {
            paths.push(downloads);
        }

        let documents = home.join("Documents");
        if documents.exists() {
            paths.push(documents);
        }

        // On Linux, also check XDG directories
        #[cfg(target_os = "linux")]
        {
            if let Some(download_dir) = dirs::download_dir() {
                if !paths.contains(&download_dir) {
                    paths.push(download_dir);
                }
            }
            if let Some(document_dir) = dirs::document_dir() {
                if !paths.contains(&document_dir) {
                    paths.push(document_dir);
                }
            }
        }
    }

    paths
}

/// Validates that a path is within one of the allowed paths.
/// Returns the canonicalized path if valid.
pub fn validate_path(path: &Path, allowed_paths: &[PathBuf]) -> Result<PathBuf> {
    // Canonicalize the requested path
    let canonical = path.canonicalize().map_err(|e| {
        Error::Browse(format!("Cannot access path '{}': {}", path.display(), e))
    })?;

    // Check for path traversal attempts
    let path_str = path.to_string_lossy();
    if path_str.contains("..") {
        return Err(Error::Browse("Path traversal not allowed".to_string()));
    }

    // Check if the canonical path is within any allowed path
    for allowed in allowed_paths {
        let allowed_canonical = match allowed.canonicalize() {
            Ok(p) => p,
            Err(_) => continue, // Skip non-existent allowed paths
        };

        if canonical.starts_with(&allowed_canonical) {
            debug!("Path {} validated under {}", canonical.display(), allowed_canonical.display());
            return Ok(canonical);
        }
    }

    Err(Error::Browse(format!(
        "Path '{}' is not within allowed directories",
        path.display()
    )))
}

/// Lists the contents of a directory.
///
/// If `path` is None, returns the list of allowed root paths.
/// If `path` is Some, returns the directory contents if it's within allowed paths.
pub fn browse_directory(
    path: Option<&Path>,
    allowed_paths: &[PathBuf],
    show_hidden: bool,
) -> Result<(String, Vec<DirectoryEntry>)> {
    match path {
        None => {
            // Return the list of browsable roots
            let mut entries = Vec::new();

            for root in allowed_paths {
                if root.exists() {
                    let name = root.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_else(|| root.to_str().unwrap_or("unknown"));

                    entries.push(DirectoryEntry {
                        name: name.to_string(),
                        is_dir: true,
                        size: 0,
                        modified: get_modified_time(root),
                    });
                }
            }

            Ok(("/".to_string(), entries))
        }
        Some(dir_path) => {
            // Validate and list the directory
            let validated = validate_path(dir_path, allowed_paths)?;

            if !validated.is_dir() {
                return Err(Error::Browse(format!(
                    "'{}' is not a directory",
                    dir_path.display()
                )));
            }

            let mut entries = Vec::new();

            let read_dir = std::fs::read_dir(&validated).map_err(|e| {
                Error::Browse(format!("Cannot read directory '{}': {}", validated.display(), e))
            })?;

            for entry in read_dir {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("Error reading directory entry: {}", e);
                        continue;
                    }
                };

                let name = entry.file_name().to_string_lossy().to_string();

                // Skip hidden files unless requested
                if !show_hidden && name.starts_with('.') {
                    continue;
                }

                let metadata = match entry.metadata() {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("Error reading metadata for {}: {}", name, e);
                        continue;
                    }
                };

                let modified = metadata.modified().ok().and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs() as i64)
                });

                entries.push(DirectoryEntry {
                    name,
                    is_dir: metadata.is_dir(),
                    size: if metadata.is_file() { metadata.len() } else { 0 },
                    modified,
                });
            }

            // Sort: directories first, then alphabetically
            entries.sort_by(|a, b| {
                match (a.is_dir, b.is_dir) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                }
            });

            Ok((validated.to_string_lossy().to_string(), entries))
        }
    }
}

/// Gets the browsable root paths, checking that they exist.
pub fn get_browsable_roots(configured_paths: Option<&[PathBuf]>) -> Vec<PathBuf> {
    match configured_paths {
        Some(paths) if !paths.is_empty() => {
            paths.iter()
                .filter(|p| p.exists())
                .cloned()
                .collect()
        }
        _ => default_browsable_paths(),
    }
}

/// Helper to get modified time as Unix timestamp.
fn get_modified_time(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs() as i64)
        })
}

/// Resolves a path for browsing based on context.
///
/// If `path` is "/" or empty, returns None to indicate roots should be listed.
/// If `path` starts with a root path name, resolves it to the full path.
/// Otherwise, treats it as an absolute path.
pub fn resolve_browse_path(path: &str, allowed_paths: &[PathBuf]) -> Option<PathBuf> {
    if path.is_empty() || path == "/" {
        return None;
    }

    // Check if it's an absolute path
    let path_buf = PathBuf::from(path);
    if path_buf.is_absolute() {
        return Some(path_buf);
    }

    // Try to match against root names
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if let Some(first) = parts.first() {
        for root in allowed_paths {
            let root_name = root.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");

            if root_name == *first {
                // Build the full path
                let mut full_path = root.clone();
                for part in parts.iter().skip(1) {
                    if !part.is_empty() {
                        full_path.push(part);
                    }
                }
                return Some(full_path);
            }
        }
    }

    // Fallback: treat as absolute path
    Some(path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_browse_roots() {
        let temp = TempDir::new().unwrap();
        let allowed = vec![temp.path().to_path_buf()];

        let (path, entries) = browse_directory(None, &allowed, false).unwrap();
        assert_eq!(path, "/");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_dir);
    }

    #[test]
    fn test_browse_directory() {
        let temp = TempDir::new().unwrap();
        let allowed = vec![temp.path().to_path_buf()];

        // Create some files
        fs::write(temp.path().join("file1.txt"), "hello").unwrap();
        fs::write(temp.path().join("file2.txt"), "world").unwrap();
        fs::create_dir(temp.path().join("subdir")).unwrap();
        fs::write(temp.path().join(".hidden"), "secret").unwrap();

        // Browse without hidden
        let (_, entries) = browse_directory(Some(temp.path()), &allowed, false).unwrap();
        assert_eq!(entries.len(), 3); // subdir, file1.txt, file2.txt

        // Browse with hidden
        let (_, entries) = browse_directory(Some(temp.path()), &allowed, true).unwrap();
        assert_eq!(entries.len(), 4); // includes .hidden

        // Verify sorting: directory first
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].name, "subdir");
    }

    #[test]
    fn test_path_validation() {
        let temp = TempDir::new().unwrap();
        let subdir = temp.path().join("allowed");
        fs::create_dir(&subdir).unwrap();
        let allowed = vec![subdir.clone()];

        // Valid path
        assert!(validate_path(&subdir, &allowed).is_ok());

        // Invalid path (outside allowed)
        assert!(validate_path(temp.path(), &allowed).is_err());

        // Path traversal attempt
        let traversal = subdir.join("..").join("other");
        assert!(validate_path(&traversal, &allowed).is_err());
    }

    #[test]
    fn test_resolve_browse_path() {
        let temp = TempDir::new().unwrap();
        let downloads = temp.path().join("Downloads");
        fs::create_dir(&downloads).unwrap();
        let allowed = vec![downloads.clone()];

        // Root
        assert!(resolve_browse_path("/", &allowed).is_none());
        assert!(resolve_browse_path("", &allowed).is_none());

        // Absolute path
        let abs = resolve_browse_path("/some/absolute/path", &allowed);
        assert_eq!(abs, Some(PathBuf::from("/some/absolute/path")));

        // Relative to root name
        let rel = resolve_browse_path("Downloads/subdir", &allowed);
        assert_eq!(rel, Some(downloads.join("subdir")));
    }
}
