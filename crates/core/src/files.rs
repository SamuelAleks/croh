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

/// Format a duration in seconds as a compact time string.
/// Examples: "0:45", "2:34", "1:23:45"
pub fn format_duration(secs: u64) -> String {
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let secs = secs % 60;

    if hours > 0 {
        format!("{}:{:02}:{:02}", hours, mins, secs)
    } else {
        format!("{}:{:02}", mins, secs)
    }
}

/// Format uptime in seconds as a human-readable string.
/// Examples: "45s", "5m", "2h 30m", "3d 5h"
pub fn format_uptime(secs: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = MINUTE * 60;
    const DAY: u64 = HOUR * 24;

    if secs >= DAY {
        let days = secs / DAY;
        let hours = (secs % DAY) / HOUR;
        if hours > 0 {
            format!("{}d {}h", days, hours)
        } else {
            format!("{}d", days)
        }
    } else if secs >= HOUR {
        let hours = secs / HOUR;
        let mins = (secs % HOUR) / MINUTE;
        if mins > 0 {
            format!("{}h {}m", hours, mins)
        } else {
            format!("{}h", hours)
        }
    } else if secs >= MINUTE {
        format!("{}m", secs / MINUTE)
    } else {
        format!("{}s", secs)
    }
}

/// Format an ETA in seconds as a human-readable string.
/// Examples: "~0:45", "~2:34", "~1h 23m"
pub fn format_eta(secs: u64) -> String {
    const HOUR: u64 = 3600;

    if secs >= HOUR {
        let hours = secs / HOUR;
        let mins = (secs % HOUR) / 60;
        format!("~{}h {:02}m", hours, mins)
    } else {
        let mins = secs / 60;
        let secs = secs % 60;
        format!("~{}:{:02}", mins, secs)
    }
}

/// Get the available disk space for a given path.
/// Returns (available_bytes, total_bytes) or (0, 0) on error.
pub fn get_disk_space(path: &Path) -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::mem::MaybeUninit;

        // Get the path, using parent if it doesn't exist yet
        let check_path = if path.exists() {
            path.to_path_buf()
        } else {
            path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| path.to_path_buf())
        };

        let c_path = match CString::new(check_path.to_string_lossy().as_bytes()) {
            Ok(p) => p,
            Err(_) => return (0, 0),
        };

        let mut stat: MaybeUninit<libc::statvfs> = MaybeUninit::uninit();
        let result = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };

        if result == 0 {
            let stat = unsafe { stat.assume_init() };
            let available = stat.f_bavail as u64 * stat.f_frsize as u64;
            let total = stat.f_blocks as u64 * stat.f_frsize as u64;
            (available, total)
        } else {
            (0, 0)
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let check_path = if path.exists() {
            path.to_path_buf()
        } else {
            path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| path.to_path_buf())
        };

        let wide: Vec<u16> = check_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

        let mut free_bytes_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut _total_free_bytes: u64 = 0;

        let result = unsafe {
            winapi::um::fileapi::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_bytes_available as *mut u64 as *mut _,
                &mut total_bytes as *mut u64 as *mut _,
                &mut _total_free_bytes as *mut u64 as *mut _,
            )
        };

        if result != 0 {
            (free_bytes_available, total_bytes)
        } else {
            (0, 0)
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        (0, 0)
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

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(45), "0:45");
        assert_eq!(format_duration(90), "1:30");
        assert_eq!(format_duration(3661), "1:01:01");
        assert_eq!(format_duration(7200), "2:00:00");
    }

    #[test]
    fn test_format_uptime() {
        assert_eq!(format_uptime(30), "30s");
        assert_eq!(format_uptime(90), "1m");
        assert_eq!(format_uptime(3600), "1h");
        assert_eq!(format_uptime(5400), "1h 30m");
        assert_eq!(format_uptime(86400), "1d");
        assert_eq!(format_uptime(90000), "1d 1h");
    }

    #[test]
    fn test_format_eta() {
        assert_eq!(format_eta(45), "~0:45");
        assert_eq!(format_eta(154), "~2:34");
        assert_eq!(format_eta(3600), "~1h 00m");
        assert_eq!(format_eta(4980), "~1h 23m");
    }
}

