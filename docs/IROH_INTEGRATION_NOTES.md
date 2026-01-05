# Iroh Integration Notes

This document captures findings from debugging and implementing Iroh-based peer connectivity for the trust establishment feature.

## Overview

The trust establishment feature uses Iroh (a Rust implementation of peer-to-peer QUIC networking) to create secure connections between trusted peers. The flow is:

1. **Initiator** creates an Iroh endpoint and generates a trust bundle containing their node ID and relay URL
2. **Trust bundle** is sent via croc (the existing file transfer tool)
3. **Receiver** receives the trust bundle, creates their own Iroh endpoint, and connects back to the initiator
4. **Handshake** completes with TrustConfirm/TrustComplete message exchange
5. Both peers are added to each other's trusted peer lists

## Key Discovery: `.discovery_n0()` is Required

### The Problem

Initial implementation only used basic endpoint configuration:

```rust
let endpoint = Endpoint::builder()
    .secret_key(secret_key)
    .alpns(vec![ALPN_CONTROL.to_vec()])
    .relay_mode(RelayMode::Default)
    .bind()
    .await?;
```

This caused connectivity issues:
- Windows could not accept incoming connections via the relay
- Connections would timeout with "Relay connection failed: Ipv4: deadline has elapsed, Ipv6: deadline has elapsed"
- Only one direction of trust establishment worked (when Linux initiated)

### The Solution

Adding `.discovery_n0()` enables the n0 DNS discovery service:

```rust
let endpoint = Endpoint::builder()
    .secret_key(secret_key)
    .alpns(vec![ALPN_CONTROL.to_vec()])
    .relay_mode(RelayMode::Default)
    .discovery_n0()  // <-- Critical for cross-platform connectivity
    .bind()
    .await?;
```

### What `.discovery_n0()` Does

1. **PkarrPublisher** - Publishes node address information (node ID, relay URL, direct addresses) to the n0 DNS system (`https://dns.iroh.link/pkarr`)
2. **DnsDiscovery** - Resolves peer addresses from the n0 DNS system

From the Iroh documentation:
> "If no discovery service is set, connecting to a node without providing its direct addresses or relay URLs will fail."

### Evidence of Discovery Working

After adding `.discovery_n0()`, logs show:
```
pkarr_publish{me=66576fc93d}: Publish node info to pkarr relay_url=Some("https://use1-1.relay.iroh.network./") pkarr_relay=https://dns.iroh.link/pkarr
```

And connections successfully establish direct paths:
```
selecting new direct path for node addr=129.222.44.15:4414 latency=103.1347ms
new connection type typ=direct(129.222.44.15:4414)
```

## Connection Upgrade Path

Iroh attempts connections in this order:
1. **Relay** - Initial connection through the Iroh relay server
2. **Mixed** - Using both relay and discovered UDP addresses
3. **Direct** - Direct UDP connection after hole punching succeeds

Example from logs:
```
typ=relay(https://use1-1.relay.iroh.network./)
typ=mixed(udp: 172.18.0.1:36720, relay: https://use1-1.relay.iroh.network./)
typ=direct(97.149.180.19:63667)
```

## Message Delivery Timing Issue

### The Problem

After sending `TrustComplete`, the connection was closed immediately. With QUIC, data might still be in flight when the endpoint closes, causing the peer to never receive the message.

Symptoms:
- Initiator logs "Trust established"
- Receiver times out waiting for TrustComplete
- Duplicate log messages appeared

### The Solution

Add a small delay after sending TrustComplete:

```rust
// Send TrustComplete
let response = ControlMessage::TrustComplete;
conn.send(&response).await?;

// Give the message time to be delivered before closing
// QUIC may not deliver data if the endpoint closes too quickly
tokio::time::sleep(Duration::from_millis(500)).await;
```

## Platform-Specific Observations

### Windows

- Initially had issues connecting to the Iroh relay server
- "check_captive_portal timed out" warnings are common
- Direct addresses include both IPv4 and IPv6
- Multiple network interfaces detected (WSL, VPN, physical)

Example direct addresses on Windows:
```
[97.149.180.19:61703, 172.27.128.1:61703, 192.168.1.181:61703,
 192.168.10.1:61703, [2600:1015:a030:b262:...]:61704]
```

### Linux

- Connects to relay faster and more reliably
- Also detects multiple network interfaces (Docker, libvirt, physical)

Example direct addresses on Linux:
```
[100.92.236.141:32959, 129.222.44.15:30548, 172.17.0.1:49592,
 172.18.0.1:49592, 192.168.0.109:49592, 192.168.122.1:49592]
```

## Iroh Version Compatibility

- **Current version**: iroh 0.32.1
- **Latest version**: iroh 0.95.1 (as of writing)

Important note from Iroh 0.91.0 release notes:
> "Version 0.91.0 of iroh can't connect to older relays or older clients on newer relays."

When upgrading Iroh versions, ensure both peers use compatible versions.

## Configuration Reference

### Endpoint Builder Options (iroh 0.32)

| Method | Description |
|--------|-------------|
| `.secret_key(key)` | Set the node's secret key (identity) |
| `.alpns(vec)` | Set ALPN protocols for connection identification |
| `.relay_mode(mode)` | Configure relay behavior (Default, Custom, Disabled) |
| `.discovery_n0()` | Enable n0 DNS discovery (recommended) |
| `.add_discovery(fn)` | Add custom discovery mechanisms |
| `.bind_addr_v4(addr)` | Bind to specific IPv4 address |
| `.bind_addr_v6(addr)` | Bind to specific IPv6 address |
| `.dns_resolver(resolver)` | Use custom DNS resolver |
| `.proxy_url(url)` | Use HTTP proxy |

### Relay Servers

Default relay servers (iroh 0.32):
- `https://use1-1.relay.iroh.network./` (US East)
- `https://euw1-1.relay.iroh.network./` (EU West)
- `https://aps1-1.relay.iroh.network./` (Asia Pacific)

## Debugging Tips

### Enable Verbose Logging

```powershell
$env:RUST_LOG="iroh=debug,iroh_relay=debug"
cargo run -p croc-gui
```

### Check Node Address

Log the full node address to see what's being published:

```rust
if let Ok(addr) = endpoint.node_addr().await {
    info!(
        "node_id={}, relay={:?}, direct_addrs={:?}",
        endpoint.node_id(),
        addr.relay_url(),
        addr.direct_addresses().collect::<Vec<_>>()
    );
}
```

### Use iroh-doctor

The `iroh-doctor` CLI tool can diagnose connectivity issues:
```bash
iroh-doctor report
```

Shows:
- UDP availability (IPv4/IPv6)
- NAT mapping behavior
- Port mapping capabilities (UPnP, PCP, NAT-PMP)
- Preferred relay server and latency

## Future Implementation: File Push/Pull

The trust establishment creates a foundation for peer-to-peer file transfer. The next phase would implement:

1. **File Push**: Initiator sends file to trusted peer
   - Use existing croc for actual file transfer
   - Or implement native Iroh-based transfer using QUIC streams

2. **File Pull**: Request file from trusted peer
   - Receiver browses/requests files from trusted peer
   - Peer sends the requested files

3. **Status/Presence**: Check if trusted peer is online
   - Periodic pings to trusted peers
   - Update peer status in UI

### Protocol Messages (already defined in protocol.rs)

```rust
pub enum ControlMessage {
    // Trust establishment
    TrustConfirm { peer, nonce, permissions },
    TrustComplete,
    TrustRevoke { reason },

    // Status
    Ping { timestamp },
    Pong { timestamp },
    StatusRequest,
    StatusResponse { ... },

    // File operations (to be implemented)
    FileRequest { path },
    FileResponse { ... },
    // etc.
}
```

## Files Modified for Trust Fix

1. **crates/core/src/iroh/endpoint.rs**
   - Added `.discovery_n0()` to endpoint builder
   - Enhanced logging to show full node address

2. **crates/core/src/iroh/handshake.rs**
   - Added 500ms delay after sending TrustComplete

3. **crates/gui/src/app.rs**
   - Added 500ms delay in `wait_for_trust_handshake`
   - Reduced duplicate logging

## References

- [Iroh Documentation](https://www.iroh.computer/docs)
- [Iroh Discovery Concepts](https://www.iroh.computer/docs/concepts/discovery)
- [Iroh Relay Concepts](https://www.iroh.computer/docs/concepts/relay)
- [iroh-ssh Project](https://github.com/rustonbsd/iroh-ssh) - Reference implementation
- [Iroh GitHub Issues](https://github.com/n0-computer/iroh/issues)
