//! Croc output parsing.

use regex::Regex;
use std::sync::OnceLock;

/// Progress information parsed from croc output.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// Progress percentage (0.0 - 100.0).
    pub percentage: f64,
    /// Transfer speed (human-readable).
    pub speed: String,
    /// Current file being transferred (if available).
    pub current_file: Option<String>,
    /// Bytes transferred (if available).
    pub bytes_transferred: Option<u64>,
    /// Total bytes (if available).
    pub total_bytes: Option<u64>,
}

/// Compiled regex patterns.
static PROGRESS_REGEX: OnceLock<Regex> = OnceLock::new();
static CODE_REGEX: OnceLock<Regex> = OnceLock::new();
static SIZE_REGEX: OnceLock<Regex> = OnceLock::new();

fn progress_regex() -> &'static Regex {
    PROGRESS_REGEX.get_or_init(|| {
        // Match patterns like:
        // "45.2%" or "100%"
        // "45.2% |████████░░░░░░| 1.2 MB/s"
        Regex::new(r"(\d+(?:\.\d+)?)\s*%").expect("Invalid progress regex")
    })
}

fn code_regex() -> &'static Regex {
    CODE_REGEX.get_or_init(|| {
        // Match "Code is: 7-alpha-beta-gamma" or similar
        Regex::new(r"Code is:\s*(\S+)").expect("Invalid code regex")
    })
}

fn speed_regex() -> &'static Regex {
    SIZE_REGEX.get_or_init(|| {
        // Match "1.2 MB/s", "500 KB/s", "2.5 GB/s"
        Regex::new(r"(\d+(?:\.\d+)?\s*[KMGT]?B/s)").expect("Invalid speed regex")
    })
}

/// Parse progress information from a croc output line.
pub fn parse_progress(line: &str) -> Option<Progress> {
    let percentage = progress_regex()
        .captures(line)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<f64>().ok())?;

    let speed = speed_regex()
        .captures(line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();

    Some(Progress {
        percentage,
        speed,
        current_file: None,
        bytes_transferred: None,
        total_bytes: None,
    })
}

/// Parse croc code from output line.
///
/// Looks for patterns like "Code is: 7-alpha-beta-gamma"
pub fn parse_code(line: &str) -> Option<String> {
    code_regex()
        .captures(line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Detect if the transfer has completed successfully.
pub fn detect_completion(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("file(s) sent")
        || lower.contains("file(s) received")
        || lower.contains("sent ")  // "Sent filename.txt"
        || (lower.contains("received") && lower.contains("written"))
        || lower.contains("transfer complete")
        || (lower.contains("100%") && lower.contains("done"))
        || lower.contains("successfully")
        // Also detect completion when we see 100% with matching byte counts like "(462/462 B"
        || (lower.contains("100%") && is_complete_byte_progress(line))
}

/// Check if a progress line shows completed byte transfer (e.g., "(462/462 B")
fn is_complete_byte_progress(line: &str) -> bool {
    // Look for pattern like (N/N B) where both numbers are the same
    let bytes_regex = Regex::new(r"\((\d+)/(\d+)\s*B").unwrap();

    if let Some(caps) = bytes_regex.captures(line) {
        if let (Some(transferred), Some(total)) = (caps.get(1), caps.get(2)) {
            return transferred.as_str() == total.as_str();
        }
    }
    false
}

/// Detect if an error occurred.
pub fn detect_error(line: &str) -> Option<String> {
    let lower = line.to_lowercase();

    // Ignore debug lines - they often contain "error" in normal debug output
    // Debug lines look like: [debug] timestamp file.go:line: message
    if lower.starts_with("[debug]") || lower.contains("].go:") {
        return None;
    }

    // Common error patterns - be more specific than just "error"
    if lower.contains("error:") || lower.contains("error connecting") || lower.contains("transfer error") {
        return Some(line.to_string());
    }
    
    if lower.contains("could not connect") {
        return Some("Could not connect to relay".to_string());
    }
    
    if lower.contains("incorrect code phrase") || lower.contains("bad code") {
        return Some("Incorrect code phrase".to_string());
    }
    
    if lower.contains("timeout") {
        return Some("Connection timed out".to_string());
    }
    
    if lower.contains("cancelled") || lower.contains("canceled") {
        return Some("Transfer cancelled".to_string());
    }
    
    if lower.contains("file exists") {
        return Some("File already exists".to_string());
    }
    
    if lower.contains("no such file") || lower.contains("not found") {
        return Some("File not found".to_string());
    }
    
    if lower.contains("permission denied") {
        return Some("Permission denied".to_string());
    }

    if lower.contains("room") && lower.contains("not ready") {
        return Some("Connection failed - peer may have disconnected".to_string());
    }

    if lower.contains("peer disconnected") {
        return Some("Peer disconnected".to_string());
    }

    if lower.contains("no peers found") {
        return Some("No peers found".to_string());
    }

    None
}

/// Detect if croc is waiting for a receiver.
#[allow(dead_code)] // Will be used in future phases
pub fn detect_waiting(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("sending") && lower.contains("code")
        || lower.contains("on local network")
        || lower.contains("waiting for")
}

/// Parse file information from croc output.
#[allow(dead_code)] // Will be used in future phases
pub fn parse_file_info(line: &str) -> Option<(String, u64)> {
    // Match patterns like "Sending 'filename.txt' (1.2 MB)"
    let re = Regex::new(r"Sending\s+'([^']+)'\s+\(([^)]+)\)").ok()?;
    let caps = re.captures(line)?;
    
    let filename = caps.get(1)?.as_str().to_string();
    let size_str = caps.get(2)?.as_str();
    
    // Parse size string like "1.2 MB" to bytes
    let size = parse_size_string(size_str)?;
    
    Some((filename, size))
}

/// Parse a human-readable size string to bytes.
#[allow(dead_code)] // Used by parse_file_info
fn parse_size_string(s: &str) -> Option<u64> {
    let s = s.trim().to_uppercase();
    let re = Regex::new(r"([\d.]+)\s*([KMGT]?B?)").ok()?;
    let caps = re.captures(&s)?;
    
    let value: f64 = caps.get(1)?.as_str().parse().ok()?;
    let unit = caps.get(2).map(|m| m.as_str()).unwrap_or("B");
    
    let multiplier: u64 = match unit {
        "KB" | "K" => 1024,
        "MB" | "M" => 1024 * 1024,
        "GB" | "G" => 1024 * 1024 * 1024,
        "TB" | "T" => 1024 * 1024 * 1024 * 1024,
        _ => 1,
    };
    
    Some((value * multiplier as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_progress() {
        let p = parse_progress("45.2% |████████░░░░░░| 1.2 MB/s").unwrap();
        assert!((p.percentage - 45.2).abs() < 0.01);
        assert_eq!(p.speed, "1.2 MB/s");

        let p = parse_progress("100%").unwrap();
        assert!((p.percentage - 100.0).abs() < 0.01);

        assert!(parse_progress("no progress here").is_none());
    }

    #[test]
    fn test_parse_code() {
        assert_eq!(
            parse_code("Code is: 7-alpha-beta-gamma"),
            Some("7-alpha-beta-gamma".to_string())
        );
        assert_eq!(
            parse_code("Code is: custom-code"),
            Some("custom-code".to_string())
        );
        assert!(parse_code("no code here").is_none());
    }

    #[test]
    fn test_detect_completion() {
        assert!(detect_completion("file(s) sent successfully"));
        assert!(detect_completion("1 file(s) received"));
        assert!(!detect_completion("sending file..."));

        // Test 100% progress with matching byte counts
        assert!(detect_completion("croc-gui-trust-c13292c2.json 100% || (462/462 B, 910 kB/s)"));
        assert!(detect_completion("file.txt 100% |████████████| (1024/1024 B, 1.2 MB/s)"));

        // Should not trigger on partial progress
        assert!(!detect_completion("file.txt 50% |████░░░░| (256/512 B, 1.2 MB/s)"));
        assert!(!detect_completion("file.txt 100% |████████████| (512/1024 B, 1.2 MB/s)"));
    }

    #[test]
    fn test_is_complete_byte_progress() {
        assert!(is_complete_byte_progress("(462/462 B, 910 kB/s)"));
        assert!(is_complete_byte_progress("(1024/1024 B)"));
        assert!(!is_complete_byte_progress("(256/512 B)"));
        assert!(!is_complete_byte_progress("no bytes here"));
    }

    #[test]
    fn test_detect_error() {
        assert!(detect_error("Error: connection failed").is_some());
        assert!(detect_error("incorrect code phrase").is_some());
        assert!(detect_error("sending file...").is_none());

        // Debug lines should be ignored even if they contain "error"
        assert!(detect_error("[debug] 15:26:50 compress.go:50: error copying data: unexpected EOF").is_none());
        assert!(detect_error("[debug] croc.go:1506: problem with decoding").is_none());
    }

    #[test]
    fn test_parse_size_string() {
        assert_eq!(parse_size_string("1024 B"), Some(1024));
        assert_eq!(parse_size_string("1 KB"), Some(1024));
        assert_eq!(parse_size_string("1.5 MB"), Some(1572864));
        assert_eq!(parse_size_string("2 GB"), Some(2147483648));
    }
}

