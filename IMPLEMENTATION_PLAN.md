# Implementation Plan: Remaining Features

## Overview

Five features remain to be implemented, listed in priority order:

| # | Feature | Complexity | Key Changes |
|---|---------|------------|-------------|
| 1 | Leave/Force Disconnect | Low | Use existing TrustRevoke, add handler + UI |
| 2 | Connection Status Details | Medium | Extend status struct, add UI panel |
| 3 | Transfer History Persistence | Medium | New module, JSON storage |
| 4 | Do Not Disturb Mode | Medium | New protocol message, config, UI |
| 5 | Speed Test | High | New module, protocol messages, UI dialog |

---

## Feature 1: Leave/Force Disconnect

### Summary
Allow users to disconnect from a peer, sending a `TrustRevoke` message and removing them from the peer store.

### Existing Infrastructure
- `TrustRevoke` message already in protocol.rs (line 68-71)
- `ControlConnection::close()` method exists
- `PeerStore::remove()` handles persistence
- `remove-peer` callback exists in UI

### Implementation Steps

**Step 1.1: Add TrustRevoke handler in background listener**
```rust
// In app.rs background listener match block
ControlMessage::TrustRevoke { reason } => {
    warn!("Peer {} revoked trust: {}", peer_name, reason);
    // Remove peer from store
    peer_store.write().await.remove(&peer_id);
    // Update UI - remove from peers list
    // Show notification to user
}
```

**Step 1.2: Add disconnect function**
```rust
async fn disconnect_peer(
    endpoint: &IrohEndpoint,
    peer: &TrustedPeer,
    reason: &str,
) -> Result<()> {
    // Connect to peer
    let conn = endpoint.connect(&peer.into()).await?;
    // Send TrustRevoke
    conn.send(&ControlMessage::TrustRevoke { reason: reason.to_string() }).await?;
    // Close connection
    conn.close().await;
    Ok(())
}
```

**Step 1.3: Add UI callback and button**
```slint
// In AppLogic global
callback disconnect-peer(string);  // peer id

// In PeerCard expanded details
CompactButton {
    label: "Disconnect";
    clicked => { AppLogic.disconnect-peer(peer.id); }
}
```

### Files to Modify
- `crates/gui/src/app.rs` - Add handler and callback
- `crates/gui/ui/main.slint` - Add button in PeerCard

---

## Feature 2: Connection Status Details Panel

### Summary
Show detailed connection info: latency, connection type, last contact time, peer system info.

### Existing Infrastructure
- `PeerConnectionStatus` struct with `online`, `last_ping`, `latency_ms`
- `Ping`/`Pong` and `StatusRequest`/`StatusResponse` in protocol
- `ping_peer()` function already measures latency

### Implementation Steps

**Step 2.1: Extend PeerConnectionStatus**
```rust
pub struct PeerConnectionStatus {
    pub online: bool,
    pub last_ping: Option<std::time::Instant>,
    pub latency_ms: Option<u32>,
    // New fields:
    pub connection_type: String,  // "direct", "relay", "unknown"
    pub last_contact: Option<chrono::DateTime<chrono::Utc>>,
    pub peer_hostname: Option<String>,
    pub peer_os: Option<String>,
}
```

**Step 2.2: Add PeerItem UI fields**
```slint
export struct PeerItem {
    // ... existing fields ...
    // New fields for status details:
    latency-ms: int,
    connection-type: string,
    last-contact: string,  // formatted "5 min ago"
    peer-hostname: string,
    peer-os: string,
}
```

**Step 2.3: Extend PeerCard details panel**
```slint
// In PeerCard expanded section, add:
HorizontalLayout {
    spacing: Theme.spacing-md;

    // Connection info
    VerticalLayout {
        Text { text: "Connection"; font-size: 11px; color: Theme.text-muted; }
        Text { text: peer.connection-type; font-size: 11px; color: Theme.text-dim; }
    }

    // Latency
    VerticalLayout {
        Text { text: "Latency"; font-size: 11px; color: Theme.text-muted; }
        Text { text: peer.latency-ms > 0 ? peer.latency-ms + " ms" : "N/A"; font-size: 11px; }
    }

    // Last contact
    VerticalLayout {
        Text { text: "Last Contact"; font-size: 11px; color: Theme.text-muted; }
        Text { text: peer.last-contact; font-size: 11px; color: Theme.text-dim; }
    }
}
```

**Step 2.4: Periodic status polling**
- Already have ping loop - extend to also query StatusRequest occasionally
- Parse StatusResponse for hostname, OS info
- Format last_contact as relative time ("5 min ago", "Just now")

### Files to Modify
- `crates/gui/src/app.rs` - Extend status struct, update ping loop
- `crates/gui/ui/main.slint` - Extend PeerItem and PeerCard

---

## Feature 3: Transfer History Persistence

### Summary
Persist completed transfers to JSON file for viewing history.

### Existing Infrastructure
- `Transfer` struct already serializable with serde
- `TransferManager` handles active transfers
- `platform::data_dir()` for storage path

### Implementation Steps

**Step 3.1: Create transfer_history.rs module**
```rust
// crates/core/src/transfer_history.rs
use crate::{Transfer, Result};
use std::path::PathBuf;

const MAX_HISTORY_ENTRIES: usize = 500;

pub struct TransferHistory {
    path: PathBuf,
    transfers: Vec<Transfer>,
}

impl TransferHistory {
    pub fn load() -> Result<Self> {
        let path = crate::platform::data_dir()?.join("transfer_history.json");
        let transfers = if path.exists() {
            let data = std::fs::read_to_string(&path)?;
            serde_json::from_str(&data)?
        } else {
            Vec::new()
        };
        Ok(Self { path, transfers })
    }

    pub fn save(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(&self.transfers)?;
        std::fs::write(&self.path, data)?;
        Ok(())
    }

    pub fn add(&mut self, transfer: Transfer) {
        self.transfers.push(transfer);
        // Trim to max size
        if self.transfers.len() > MAX_HISTORY_ENTRIES {
            self.transfers.remove(0);
        }
    }

    pub fn list(&self) -> &[Transfer] {
        &self.transfers
    }

    pub fn clear(&mut self) {
        self.transfers.clear();
    }
}
```

**Step 3.2: Integrate with App**
```rust
// In App struct
transfer_history: Arc<RwLock<TransferHistory>>,

// When transfer completes (in process monitoring):
if status == TransferStatus::Completed {
    let mut history = transfer_history.write().await;
    history.add(transfer.clone());
    history.save().ok();
}
```

**Step 3.3: Add History UI (optional - can show in transfers panel)**
```slint
// Option A: Add "History" section below active transfers
// Option B: Add "History" tab to main tabs
// Option C: Show completed transfers in existing transfers bar with different styling
```

### Files to Create/Modify
- Create `crates/core/src/transfer_history.rs`
- Modify `crates/core/src/lib.rs` - add module
- Modify `crates/gui/src/app.rs` - integrate history

---

## Feature 4: Do Not Disturb Mode

### Summary
Allow users to set DND status, broadcast to peers, and auto-reject/delay incoming requests.

### Implementation Steps

**Step 4.1: Add protocol message**
```rust
// In protocol.rs ControlMessage enum
DndStatus {
    enabled: bool,
    mode: String,  // "silent", "available", "busy"
    until: Option<i64>,  // Unix timestamp, None = indefinite
    message: Option<String>,  // Optional custom message
},
```

**Step 4.2: Add config fields**
```rust
// In config.rs Config struct
pub dnd_enabled: bool,
pub dnd_mode: String,  // "off", "silent", "busy"
pub dnd_until: Option<i64>,  // Unix timestamp
pub dnd_message: Option<String>,
```

**Step 4.3: Add App state and handlers**
```rust
// Track DND state
dnd_state: Arc<RwLock<DndState>>,

struct DndState {
    enabled: bool,
    mode: String,
    until: Option<chrono::DateTime<chrono::Utc>>,
    message: Option<String>,
}

// When DND changes, broadcast to all online peers
async fn broadcast_dnd_status(endpoint: &IrohEndpoint, peers: &[TrustedPeer], dnd: &DndState) {
    for peer in peers {
        if let Ok(conn) = endpoint.connect(&peer.into()).await {
            conn.send(&ControlMessage::DndStatus {
                enabled: dnd.enabled,
                mode: dnd.mode.clone(),
                until: dnd.until.map(|d| d.timestamp()),
                message: dnd.message.clone(),
            }).await.ok();
        }
    }
}

// When receiving push request, check DND
if dnd_state.read().await.enabled && dnd_state.read().await.mode == "silent" {
    // Auto-reject or queue
}
```

**Step 4.4: Add UI components**
```slint
// In AppLogic global
in property <bool> dnd-enabled: false;
in property <string> dnd-mode: "off";
callback set-dnd(bool, string);  // enabled, mode

// In Settings panel, add DND section
Section {
    title: "Do Not Disturb";

    SettingRow {
        label: "Enable DND";
        // Toggle switch
    }

    SettingRow {
        label: "Mode";
        ThemedDropdown {
            model: ["Off", "Silent", "Busy"];
        }
    }

    SettingRow {
        label: "Duration";
        ThemedDropdown {
            model: ["Indefinite", "1 hour", "2 hours", "4 hours", "8 hours"];
        }
    }
}

// In PeerCard, show DND indicator if peer is in DND
if peer.dnd-enabled: Rectangle {
    // Moon icon or "DND" badge
}
```

### Files to Modify
- `crates/core/src/iroh/protocol.rs` - Add DndStatus message
- `crates/core/src/config.rs` - Add DND config fields
- `crates/gui/src/app.rs` - DND state and handlers
- `crates/gui/ui/main.slint` - Settings UI and peer indicator

---

## Feature 5: Speed Test Between Peers

### Summary
Allow bandwidth measurement by transferring dummy data and timing it.

### Implementation Steps

**Step 5.1: Add protocol messages**
```rust
// In protocol.rs
SpeedTestRequest {
    direction: String,  // "upload" or "download"
    size_bytes: u64,
},
SpeedTestAccept,
SpeedTestReject {
    reason: String,
},
SpeedTestComplete {
    bytes_transferred: u64,
    duration_ms: u64,
},
```

**Step 5.2: Create speedtest.rs module**
```rust
// crates/core/src/iroh/speedtest.rs
use crate::{ControlConnection, ControlMessage, Result};
use std::time::Instant;

const DEFAULT_TEST_SIZE: u64 = 10 * 1024 * 1024;  // 10 MB
const CHUNK_SIZE: usize = 64 * 1024;  // 64 KB

pub struct SpeedTestResult {
    pub direction: String,
    pub bytes_transferred: u64,
    pub duration_ms: u64,
    pub bandwidth_mbps: f64,
}

pub async fn run_speed_test(
    conn: &mut ControlConnection,
    direction: &str,
    size: u64,
) -> Result<SpeedTestResult> {
    // Send request
    conn.send(&ControlMessage::SpeedTestRequest {
        direction: direction.to_string(),
        size_bytes: size,
    }).await?;

    // Wait for accept/reject
    match conn.recv().await? {
        ControlMessage::SpeedTestAccept => {},
        ControlMessage::SpeedTestReject { reason } => {
            return Err(crate::Error::SpeedTest(reason));
        },
        _ => return Err(crate::Error::Protocol("unexpected response".into())),
    }

    let start = Instant::now();

    if direction == "upload" {
        // Send dummy data
        let chunk = vec![0u8; CHUNK_SIZE];
        let mut sent = 0u64;
        while sent < size {
            let to_send = std::cmp::min(CHUNK_SIZE as u64, size - sent);
            conn.send_raw(&chunk[..to_send as usize]).await?;
            sent += to_send;
        }
    } else {
        // Receive dummy data
        let mut received = 0u64;
        while received < size {
            let data = conn.recv_raw().await?;
            received += data.len() as u64;
        }
    }

    let duration = start.elapsed();
    let duration_ms = duration.as_millis() as u64;
    let bandwidth_mbps = (size as f64 * 8.0) / (duration.as_secs_f64() * 1_000_000.0);

    Ok(SpeedTestResult {
        direction: direction.to_string(),
        bytes_transferred: size,
        duration_ms,
        bandwidth_mbps,
    })
}

pub async fn handle_speed_test_request(
    conn: &mut ControlConnection,
    direction: &str,
    size: u64,
) -> Result<()> {
    // Accept the test
    conn.send(&ControlMessage::SpeedTestAccept).await?;

    if direction == "upload" {
        // Peer is uploading, we receive
        let mut received = 0u64;
        while received < size {
            let data = conn.recv_raw().await?;
            received += data.len() as u64;
        }
    } else {
        // Peer is downloading, we send
        let chunk = vec![0u8; CHUNK_SIZE];
        let mut sent = 0u64;
        while sent < size {
            let to_send = std::cmp::min(CHUNK_SIZE as u64, size - sent);
            conn.send_raw(&chunk[..to_send as usize]).await?;
            sent += to_send;
        }
    }

    Ok(())
}
```

**Step 5.3: Add UI**
```slint
// In AppLogic
callback start-speed-test(string);  // peer id
in property <bool> speed-test-running: false;
in property <string> speed-test-result: "";

// In PeerCard details
CompactButton {
    label: "Speed Test";
    enabled: peer.status == "online" && !AppLogic.speed-test-running;
    clicked => { AppLogic.start-speed-test(peer.id); }
}

// Show result
if peer.speed-test-result != "": Text {
    text: peer.speed-test-result;  // "↑ 45 Mbps ↓ 52 Mbps"
    font-size: 10px;
    color: Theme.text-dim;
}
```

**Step 5.4: Optional - Store results history**
```rust
// In speedtest.rs or separate file
pub struct SpeedTestHistory {
    results: Vec<SpeedTestEntry>,
}

struct SpeedTestEntry {
    peer_id: String,
    timestamp: chrono::DateTime<chrono::Utc>,
    upload_mbps: f64,
    download_mbps: f64,
}
```

### Files to Create/Modify
- Create `crates/core/src/iroh/speedtest.rs`
- Modify `crates/core/src/iroh/mod.rs` - add module
- Modify `crates/core/src/iroh/protocol.rs` - add messages
- Modify `crates/gui/src/app.rs` - callbacks and handlers
- Modify `crates/gui/ui/main.slint` - button and result display

---

## Implementation Order Recommendation

1. **Leave/Force Disconnect** (Low effort, high impact)
   - Uses existing protocol message
   - Simple handler addition
   - 1-2 hours

2. **Connection Status Details** (Medium effort, high UX value)
   - Extends existing ping infrastructure
   - UI-focused changes
   - 2-3 hours

3. **Transfer History** (Medium effort, good feature)
   - New module following existing patterns
   - Relatively isolated change
   - 2-3 hours

4. **Do Not Disturb** (Medium effort, nice-to-have)
   - New protocol message
   - Config and UI changes
   - 3-4 hours

5. **Speed Test** (High effort, impressive feature)
   - New module with data transfer
   - Complex UI interaction
   - 4-6 hours

---

## Notes

- All features follow existing architectural patterns
- Use `Arc<RwLock<>>` for shared state
- Use `std::thread` + embedded tokio for async operations
- Use `slint::invoke_from_event_loop` for UI updates
- Follow existing error handling with `tracing` logging
