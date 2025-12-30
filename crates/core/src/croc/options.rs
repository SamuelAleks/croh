//! Croc command options.

use serde::{Deserialize, Serialize};

/// Elliptic curve for PAKE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Curve {
    #[default]
    Siec,
    P256,
    P384,
    P521,
}

impl Curve {
    /// Get the croc command-line argument value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Curve::Siec => "siec",
            Curve::P256 => "p256",
            Curve::P384 => "p384",
            Curve::P521 => "p521",
        }
    }
}

/// Hash algorithm for file verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    #[default]
    Xxhash,
    Imohash,
    Md5,
}

impl HashAlgorithm {
    /// Get the croc command-line argument value.
    pub fn as_str(&self) -> &'static str {
        match self {
            HashAlgorithm::Xxhash => "xxhash",
            HashAlgorithm::Imohash => "imohash",
            HashAlgorithm::Md5 => "md5",
        }
    }
}

/// Options for croc send/receive commands.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrocOptions {
    /// Custom code phrase (send only).
    pub code: Option<String>,

    /// Elliptic curve for PAKE.
    pub curve: Option<Curve>,

    /// Hash algorithm.
    pub hash: Option<HashAlgorithm>,

    /// Throttle transfer speed (e.g., "1M", "500K").
    pub throttle: Option<String>,

    /// Custom relay address.
    pub relay: Option<String>,

    /// Disable local network transfer.
    pub no_local: bool,

    /// Overwrite existing files without prompting (receive only).
    pub overwrite: bool,

    /// Enable debug/verbose output from croc.
    pub debug: bool,
}

impl CrocOptions {
    /// Create new default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set custom code.
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    /// Set curve.
    pub fn with_curve(mut self, curve: Curve) -> Self {
        self.curve = Some(curve);
        self
    }

    /// Set hash algorithm.
    pub fn with_hash(mut self, hash: HashAlgorithm) -> Self {
        self.hash = Some(hash);
        self
    }

    /// Set throttle.
    pub fn with_throttle(mut self, throttle: impl Into<String>) -> Self {
        self.throttle = Some(throttle.into());
        self
    }

    /// Set relay.
    pub fn with_relay(mut self, relay: impl Into<String>) -> Self {
        self.relay = Some(relay.into());
        self
    }

    /// Disable local network.
    pub fn with_no_local(mut self, no_local: bool) -> Self {
        self.no_local = no_local;
        self
    }

    /// Set overwrite mode.
    pub fn with_overwrite(mut self, overwrite: bool) -> Self {
        self.overwrite = overwrite;
        self
    }

    /// Enable debug output.
    pub fn with_debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    /// Convert options to command-line arguments for send command.
    pub fn to_send_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if let Some(ref code) = self.code {
            args.push("--code".to_string());
            args.push(code.clone());
        }

        // --no-local is send-only
        if self.no_local {
            args.push("--no-local".to_string());
        }

        self.add_common_args(&mut args);
        args
    }

    /// Convert options to command-line arguments for receive command.
    pub fn to_receive_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        // Always auto-accept for receive
        args.push("--yes".to_string());

        if self.overwrite {
            args.push("--overwrite".to_string());
        }

        self.add_common_args(&mut args);
        args
    }

    /// Add common arguments shared between send and receive.
    fn add_common_args(&self, args: &mut Vec<String>) {
        if let Some(ref curve) = self.curve {
            args.push("--curve".to_string());
            args.push(curve.as_str().to_string());
        }

        if let Some(ref hash) = self.hash {
            args.push("--hash".to_string());
            args.push(hash.as_str().to_string());
        }

        if let Some(ref throttle) = self.throttle {
            args.push("--throttle".to_string());
            args.push(throttle.clone());
        }

        if let Some(ref relay) = self.relay {
            args.push("--relay".to_string());
            args.push(relay.clone());
        }
    }

    /// Get global arguments that must come before the subcommand.
    pub fn to_global_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if self.debug {
            args.push("--debug".to_string());
        }

        args
    }
}

/// Validate a croc code format.
///
/// Valid codes have the format: `number-word-word-word` (at least 2 hyphens).
pub fn validate_croc_code(code: &str) -> bool {
    let parts: Vec<&str> = code.split('-').collect();
    
    // Must have at least 3 parts (e.g., "7-alpha-beta")
    if parts.len() < 3 {
        return false;
    }

    // First part should be a number
    if parts[0].parse::<u32>().is_err() {
        return false;
    }

    // Other parts should be non-empty words
    parts[1..].iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_croc_code() {
        assert!(validate_croc_code("7-alpha-beta-gamma"));
        assert!(validate_croc_code("3-word-word"));
        assert!(validate_croc_code("123-a-b-c-d"));
        
        assert!(!validate_croc_code("alpha-beta-gamma")); // no number
        assert!(!validate_croc_code("7-alpha")); // too few parts
        assert!(!validate_croc_code("7")); // no words
        assert!(!validate_croc_code("")); // empty
        assert!(!validate_croc_code("7--beta")); // empty part
    }

    #[test]
    fn test_send_args() {
        let opts = CrocOptions::new()
            .with_code("my-custom-code")
            .with_curve(Curve::P256)
            .with_no_local(true);

        let args = opts.to_send_args();
        assert!(args.contains(&"--code".to_string()));
        assert!(args.contains(&"my-custom-code".to_string()));
        assert!(args.contains(&"--curve".to_string()));
        assert!(args.contains(&"p256".to_string()));
        assert!(args.contains(&"--no-local".to_string()));
    }

    #[test]
    fn test_receive_args() {
        let opts = CrocOptions::new()
            .with_overwrite(true)
            .with_relay("relay.example.com");

        let args = opts.to_receive_args();
        assert!(args.contains(&"--yes".to_string()));
        assert!(args.contains(&"--overwrite".to_string()));
        assert!(args.contains(&"--relay".to_string()));
        assert!(args.contains(&"relay.example.com".to_string()));
    }
}

