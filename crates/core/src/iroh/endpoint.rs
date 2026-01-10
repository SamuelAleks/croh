//! Iroh endpoint wrapper for managing peer connections.
//!
//! Provides a managed Iroh endpoint with protocol handler registration
//! and connection lifecycle management.

use crate::error::{Error, Result};
use crate::iroh::identity::Identity;
use crate::iroh::protocol::{ControlMessage, ALPN_CONTROL};
use iroh::{Endpoint, NodeId, RelayMode, RelayUrl};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

/// Default timeout for connection attempts.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum message size (1MB).
const MAX_MESSAGE_SIZE: u32 = 1024 * 1024;

/// Wrapper around an Iroh endpoint with control protocol support.
#[derive(Clone)]
pub struct IrohEndpoint {
    endpoint: Endpoint,
    identity: Arc<Identity>,
}

impl IrohEndpoint {
    /// Create a new Iroh endpoint from an identity.
    pub async fn new(identity: Identity) -> Result<Self> {
        let secret_key = identity.secret_key().clone();

        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![ALPN_CONTROL.to_vec()])
            .relay_mode(RelayMode::Default)
            .discovery_n0() // Enable n0 DNS discovery for peer address publishing/resolution
            .bind()
            .await
            .map_err(|e| Error::Iroh(format!("failed to create endpoint: {}", e)))?;

        // Log full node address for debugging connectivity
        if let Ok(addr) = endpoint.node_addr().await {
            info!(
                "Iroh endpoint created: node_id={}, relay={:?}, direct_addrs={:?}",
                endpoint.node_id(),
                addr.relay_url(),
                addr.direct_addresses().collect::<Vec<_>>()
            );
        } else {
            info!(
                "Iroh endpoint created with node_id: {}",
                endpoint.node_id()
            );
        }

        Ok(Self {
            endpoint,
            identity: Arc::new(identity),
        })
    }

    /// Get the node ID (endpoint ID) as a string.
    pub fn endpoint_id(&self) -> String {
        self.endpoint.node_id().to_string()
    }

    /// Get the node ID.
    pub fn node_id(&self) -> NodeId {
        self.endpoint.node_id()
    }

    /// Get the identity associated with this endpoint.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Get the relay URL if connected to a relay.
    pub fn relay_url(&self) -> Option<RelayUrl> {
        self.endpoint.home_relay().get().ok().flatten()
    }

    /// Wait for a relay connection to be established.
    /// Returns the relay URL once connected, or None if timeout.
    pub async fn wait_for_relay(&self, timeout: Duration) -> Option<RelayUrl> {
        let start = std::time::Instant::now();
        loop {
            if let Some(url) = self.relay_url() {
                return Some(url);
            }
            if start.elapsed() > timeout {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Connect to a remote peer by node ID string.
    pub async fn connect(&self, remote_id: &str) -> Result<ControlConnection> {
        let node_id: NodeId = remote_id
            .parse()
            .map_err(|e| Error::Iroh(format!("invalid node_id: {}", e)))?;

        self.connect_to_node(node_id).await
    }

    /// Connect to a remote peer by NodeId.
    pub async fn connect_to_node(&self, node_id: NodeId) -> Result<ControlConnection> {
        debug!("Connecting to node: {}", node_id);

        let conn = tokio::time::timeout(CONNECT_TIMEOUT, self.endpoint.connect(node_id, ALPN_CONTROL))
            .await
            .map_err(|_| Error::Iroh("connection timeout".to_string()))?
            .map_err(|e| Error::Iroh(format!("connection failed: {}", e)))?;

        info!("Connected to node: {}", node_id);

        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| Error::Iroh(format!("failed to open stream: {}", e)))?;

        Ok(ControlConnection {
            remote_id: node_id,
            send,
            recv,
        })
    }

    /// Accept an incoming connection.
    pub async fn accept(&self) -> Result<ControlConnection> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| Error::Iroh("endpoint closed".to_string()))?;

        let conn = incoming
            .await
            .map_err(|e| Error::Iroh(format!("failed to accept connection: {}", e)))?;

        let remote_id = conn
            .remote_node_id()
            .map_err(|e| Error::Iroh(format!("failed to get remote node id: {}", e)))?;
        debug!("Accepted connection from: {}", remote_id);

        let alpn = conn.alpn();
        if alpn.as_deref() != Some(ALPN_CONTROL) {
            warn!("Unknown ALPN: {:?}", alpn);
            return Err(Error::Iroh(format!("unknown ALPN: {:?}", alpn)));
        }

        let (send, recv) = conn
            .accept_bi()
            .await
            .map_err(|e| Error::Iroh(format!("failed to accept stream: {}", e)))?;

        Ok(ControlConnection {
            remote_id,
            send,
            recv,
        })
    }

    /// Close the endpoint gracefully.
    pub async fn close(&self) {
        self.endpoint.close().await;
        info!("Iroh endpoint closed");
    }

    /// Check if the endpoint is still running.
    pub fn is_closed(&self) -> bool {
        self.endpoint.is_closed()
    }

    /// Add a node address for connecting to a remote peer.
    /// This is useful when you have the peer's address from a trust bundle.
    pub fn add_node_addr(&self, addr: iroh::NodeAddr) -> Result<()> {
        self.endpoint
            .add_node_addr(addr)
            .map_err(|e| Error::Iroh(format!("failed to add node addr: {}", e)))
    }
}

/// A bidirectional connection for sending/receiving control messages.
pub struct ControlConnection {
    remote_id: NodeId,
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
}

impl ControlConnection {
    /// Get the remote node ID.
    pub fn remote_id(&self) -> NodeId {
        self.remote_id
    }

    /// Get the remote node ID as a string.
    pub fn remote_id_string(&self) -> String {
        self.remote_id.to_string()
    }

    /// Send a control message.
    pub async fn send(&mut self, msg: &ControlMessage) -> Result<()> {
        let encoded = msg.encode()?;
        self.send
            .write_all(&encoded)
            .await
            .map_err(|e| Error::Iroh(format!("send failed: {}", e)))?;
        self.send
            .flush()
            .await
            .map_err(|e| Error::Iroh(format!("flush failed: {}", e)))?;
        Ok(())
    }

    /// Receive a control message.
    pub async fn recv(&mut self) -> Result<ControlMessage> {
        // Read 4-byte length prefix
        let mut len_buf = [0u8; 4];
        self.recv
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| Error::Iroh(format!("recv length failed: {}", e)))?;

        let len = u32::from_be_bytes(len_buf);
        if len > MAX_MESSAGE_SIZE {
            return Err(Error::Iroh(format!("message too large: {} bytes", len)));
        }

        // Read message body
        let mut buf = vec![0u8; len as usize];
        self.recv
            .read_exact(&mut buf)
            .await
            .map_err(|e| Error::Iroh(format!("recv body failed: {}", e)))?;

        ControlMessage::decode(&buf)
    }

    /// Send raw bytes without framing (for file data transfer).
    pub async fn send_raw(&mut self, data: &[u8]) -> Result<()> {
        self.send
            .write_all(data)
            .await
            .map_err(|e| Error::Iroh(format!("send_raw failed: {}", e)))?;
        self.send
            .flush()
            .await
            .map_err(|e| Error::Iroh(format!("flush failed: {}", e)))?;
        Ok(())
    }

    /// Receive raw bytes without framing (for file data transfer).
    pub async fn recv_raw(&mut self, buf: &mut [u8]) -> Result<()> {
        self.recv
            .read_exact(buf)
            .await
            .map_err(|e| Error::Iroh(format!("recv_raw failed: {}", e)))?;
        Ok(())
    }

    /// Close the connection gracefully.
    ///
    /// This finishes the send stream and waits for the peer to acknowledge
    /// receipt of all data before returning.
    pub async fn close(mut self) -> Result<()> {
        self.send
            .finish()
            .map_err(|e| Error::Iroh(format!("finish failed: {}", e)))?;
        // Wait for peer to acknowledge receipt of all data
        let _ = self.send.stopped().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_endpoint_creation() {
        let identity = Identity::generate("Test Device".to_string()).unwrap();
        let endpoint = IrohEndpoint::new(identity).await.unwrap();

        assert!(!endpoint.endpoint_id().is_empty());
        assert!(!endpoint.is_closed());

        endpoint.close().await;
        assert!(endpoint.is_closed());
    }

    // Note: This test requires network/relay connectivity which may not be available
    // in all CI environments.
    //
    // For testing connections without relay, see the test_support module which provides:
    // - TestEndpoint: Relay-disabled endpoint wrapper
    // - EndpointPair: Pre-configured pair of endpoints
    //
    // For comprehensive testing with relay, enable the `test-relay` feature:
    //   cargo test --features test-relay
    #[tokio::test]
    #[ignore = "requires network connectivity via relay"]
    async fn test_endpoint_connection() {
        use iroh::NodeAddr;

        // Create two endpoints
        let identity1 = Identity::generate("Device 1".to_string()).unwrap();
        let identity2 = Identity::generate("Device 2".to_string()).unwrap();

        let ep1 = IrohEndpoint::new(identity1).await.unwrap();
        let ep2 = IrohEndpoint::new(identity2).await.unwrap();

        // Get ep1's node address for direct connection
        let ep1_node_id = ep1.node_id();
        let ep1_addr = NodeAddr::new(ep1_node_id);

        // Add ep1's address to ep2 so it can connect directly
        ep2.add_node_addr(ep1_addr).ok();

        // Spawn acceptor
        let accept_handle = tokio::spawn(async move {
            let mut conn = ep1.accept().await.unwrap();
            let msg = conn.recv().await.unwrap();
            match msg {
                ControlMessage::Ping { timestamp } => {
                    conn.send(&ControlMessage::Pong { timestamp }).await.unwrap();
                }
                _ => panic!("unexpected message"),
            }
            conn.close().await.unwrap();
            ep1
        });

        // Give acceptor time to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect and send ping
        let mut conn = ep2.connect_to_node(ep1_node_id).await.unwrap();
        conn.send(&ControlMessage::Ping { timestamp: 12345 })
            .await
            .unwrap();

        // Receive pong
        let msg = conn.recv().await.unwrap();
        match msg {
            ControlMessage::Pong { timestamp } => assert_eq!(timestamp, 12345),
            _ => panic!("unexpected message"),
        }

        conn.close().await.unwrap();

        // Wait for acceptor to finish
        let ep1 = accept_handle.await.unwrap();

        // Cleanup
        ep1.close().await;
        ep2.close().await;
    }
}
