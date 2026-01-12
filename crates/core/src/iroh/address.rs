//! Peer address abstraction for multi-transport support.
//!
//! This module provides a transport-agnostic way to specify how to reach a peer.
//! Currently supports Iroh relay connections, with future support planned for
//! Tor onion services and other anonymous transports.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// How to reach a peer - extensible for future transports.
///
/// This abstraction allows the same peer to be reachable via different
/// transport mechanisms (Iroh relay, Tor, I2P, etc.) without changing
/// the core peer management logic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PeerAddress {
    /// Standard Iroh relay connection (current default).
    Iroh {
        /// Relay URL for NAT traversal (e.g., "https://relay.iroh.link")
        #[serde(skip_serializing_if = "Option::is_none")]
        relay_url: Option<String>,

        /// Direct socket addresses if known (for LAN optimization).
        /// These are tried first before falling back to relay.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        direct_addrs: Vec<SocketAddr>,
    },
    // Future variants (commented out for now, ready for implementation):
    //
    // /// Tor onion service connection.
    // Tor {
    //     /// v3 onion address (56 chars + .onion)
    //     onion_address: String,
    //     /// Port on the onion service
    //     port: u16,
    //     /// Whether client authorization is required
    //     #[serde(default)]
    //     requires_auth: bool,
    // },
    //
    // /// I2P destination.
    // I2P {
    //     /// Base64-encoded I2P destination
    //     destination: String,
    // },
    //
    // /// Multiple addresses with fallback order.
    // Multi {
    //     /// Addresses in preference order (first = most preferred)
    //     addresses: Vec<PeerAddress>,
    // },
}

impl Default for PeerAddress {
    fn default() -> Self {
        Self::Iroh {
            relay_url: None,
            direct_addrs: vec![],
        }
    }
}

impl PeerAddress {
    /// Create an Iroh address with a relay URL.
    pub fn iroh(relay_url: Option<String>) -> Self {
        Self::Iroh {
            relay_url,
            direct_addrs: vec![],
        }
    }

    /// Create an Iroh address with relay URL and direct addresses.
    pub fn iroh_with_direct(relay_url: Option<String>, direct_addrs: Vec<SocketAddr>) -> Self {
        Self::Iroh {
            relay_url,
            direct_addrs,
        }
    }

    /// Get the relay URL if this is an Iroh address.
    pub fn relay_url(&self) -> Option<&str> {
        match self {
            Self::Iroh { relay_url, .. } => relay_url.as_deref(),
        }
    }

    /// Get direct addresses if this is an Iroh address.
    pub fn direct_addrs(&self) -> &[SocketAddr] {
        match self {
            Self::Iroh { direct_addrs, .. } => direct_addrs,
        }
    }

    /// Check if this address type protects IP/location metadata.
    ///
    /// This is important for future hardened mode - Tor and I2P addresses
    /// protect the user's IP address, while Iroh relay connections expose
    /// the IP to the relay server.
    pub fn is_metadata_protected(&self) -> bool {
        match self {
            Self::Iroh { .. } => false, // Relay server sees IP
                                        // Self::Tor { .. } => true,   // IP hidden by Tor network
                                        // Self::I2P { .. } => true,   // IP hidden by I2P network
        }
    }

    /// Check if this address has any connection information.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Iroh {
                relay_url,
                direct_addrs,
            } => relay_url.is_none() && direct_addrs.is_empty(),
        }
    }

    /// Merge another address into this one, combining connection options.
    ///
    /// For Iroh addresses, this takes the relay URL from `other` if we don't
    /// have one, and combines direct addresses.
    pub fn merge(&mut self, other: &PeerAddress) {
        match (self, other) {
            (
                Self::Iroh {
                    relay_url,
                    direct_addrs,
                },
                Self::Iroh {
                    relay_url: other_relay,
                    direct_addrs: other_direct,
                },
            ) => {
                // Take other's relay if we don't have one
                if relay_url.is_none() && other_relay.is_some() {
                    *relay_url = other_relay.clone();
                }
                // Add any direct addresses we don't already have
                for addr in other_direct {
                    if !direct_addrs.contains(addr) {
                        direct_addrs.push(*addr);
                    }
                }
            }
        }
    }
}

/// Convenience conversion from Option<String> for relay_url.
impl From<Option<String>> for PeerAddress {
    fn from(relay_url: Option<String>) -> Self {
        Self::iroh(relay_url)
    }
}

/// Convenience conversion from &str for relay_url.
impl From<&str> for PeerAddress {
    fn from(relay_url: &str) -> Self {
        Self::iroh(Some(relay_url.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_address() {
        let addr = PeerAddress::default();
        assert!(matches!(addr, PeerAddress::Iroh { .. }));
        assert!(addr.relay_url().is_none());
        assert!(addr.direct_addrs().is_empty());
        assert!(addr.is_empty());
    }

    #[test]
    fn test_iroh_address_with_relay() {
        let addr = PeerAddress::iroh(Some("https://relay.example.com".to_string()));
        assert_eq!(addr.relay_url(), Some("https://relay.example.com"));
        assert!(!addr.is_empty());
        assert!(!addr.is_metadata_protected());
    }

    #[test]
    fn test_address_merge() {
        let mut addr1 = PeerAddress::default();
        let addr2 = PeerAddress::iroh(Some("https://relay.example.com".to_string()));

        addr1.merge(&addr2);
        assert_eq!(addr1.relay_url(), Some("https://relay.example.com"));
    }

    #[test]
    fn test_serialization() {
        let addr = PeerAddress::iroh(Some("https://relay.example.com".to_string()));
        let json = serde_json::to_string(&addr).unwrap();
        assert!(json.contains("\"type\":\"iroh\""));
        assert!(json.contains("relay.example.com"));

        let parsed: PeerAddress = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, addr);
    }

    #[test]
    fn test_from_conversions() {
        let addr1: PeerAddress = Some("https://relay.example.com".to_string()).into();
        assert_eq!(addr1.relay_url(), Some("https://relay.example.com"));

        let addr2: PeerAddress = "https://relay.example.com".into();
        assert_eq!(addr2.relay_url(), Some("https://relay.example.com"));

        let addr3: PeerAddress = None.into();
        assert!(addr3.relay_url().is_none());
    }
}
