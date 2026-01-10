//! File handling utilities.

use std::path::{Path, PathBuf};

/// Sanitize a filename to be safe for the filesystem.
///
/// Removes path components and dangerous characters.
pub fn secure_filename(name: &str) -> String {
    // Get just the filename part (strip any path components)
    let name = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed");

    // Remove or replace dangerous characters
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            // Replace path separators and null bytes
            '/' | '\\' | '\0' => '_',
            // Replace other potentially problematic characters
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            // Keep everything else
            _ => c,
        })
        .collect();

    // Handle empty or dot-only names
    let sanitized = sanitized.trim_start_matches('.');
    if sanitized.is_empty() {
        return "unnamed".to_string();
    }

    sanitized.to_string()
}

/// Get a unique filepath by appending _1, _2, etc. if the file already exists.
pub fn get_unique_filepath(dir: &Path, name: &str) -> PathBuf {
    let sanitized = secure_filename(name);
    let target = dir.join(&sanitized);

    if !target.exists() {
        return target;
    }

    // Split name into stem and extension
    let path = Path::new(&sanitized);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed");
    let extension = path.extension().and_then(|e| e.to_str());

    // Try incrementing numbers until we find a unique name
    for i in 1..10000 {
        let new_name = match extension {
            Some(ext) => format!("{}_{}.{}", stem, i, ext),
            None => format!("{}_{}", stem, i),
        };

        let new_path = dir.join(&new_name);
        if !new_path.exists() {
            return new_path;
        }
    }

    // Fallback: use timestamp
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let new_name = match extension {
        Some(ext) => format!("{}_{}.{}", stem, timestamp, ext),
        None => format!("{}_{}", stem, timestamp),
    };

    dir.join(&new_name)
}

/// Format a byte size as a human-readable string.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secure_filename() {
        assert_eq!(secure_filename("normal.txt"), "normal.txt");
        assert_eq!(secure_filename("../../../etc/passwd"), "passwd");
        assert_eq!(secure_filename("file<with>bad:chars"), "file_with_bad_chars");
        // "..." becomes empty after stripping leading dots, so falls back to "unnamed"
        assert_eq!(secure_filename("..."), "unnamed");
        assert_eq!(secure_filename(""), "unnamed");
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1048576), "1.00 MB");
        assert_eq!(format_size(1073741824), "1.00 GB");
    }
}

