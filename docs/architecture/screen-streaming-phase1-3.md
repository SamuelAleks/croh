# Screen Streaming Architecture: Phases 1-3

Detailed implementation architecture for time sync, adaptive streaming, cursor capture, and viewer UI.

## Overview

| Phase | Focus | Key Deliverables | Status |
|-------|-------|------------------|--------|
| 1 | Latency & Sync | Clock sync, frame dropping, adaptive bitrate | **IMPLEMENTED** |
| 2 | Cursor | Cursor capture, transmission, client rendering | Planned |
| 3 | Viewer UX | Settings UI, statistics overlay, preferences | Planned |

---

## Phase 1: Time Synchronization & Adaptive Streaming

**Status: IMPLEMENTED**

### 1.1 Clock Synchronization Protocol

**Files:**
- `crates/core/src/screen/time_sync.rs` - Clock sync implementation
- `crates/core/src/iroh/protocol.rs` - Protocol messages

#### Protocol Messages (Implemented)

```rust
// In protocol.rs

/// Time synchronization request (viewer → host)
TimeSyncRequest {
    /// Viewer's local time when sending (Unix millis)
    client_time: i64,
    /// Request sequence (0-4 for initial sync)
    sequence: u8,
},

/// Time synchronization response (host → viewer)
TimeSyncResponse {
    /// Echo back client_time
    client_time: i64,
    /// Host's time when request was received (Unix millis)
    server_receive_time: i64,
    /// Host's time when sending response (Unix millis)
    server_send_time: i64,
},
```

#### ClockSync Implementation

```rust
// In time_sync.rs

/// Clock synchronization state.
pub struct ClockSync {
    offset_ms: i64,              // host_time = local_time + offset
    rtt_samples: Vec<u32>,       // RTT samples from exchanges
    offset_samples: Vec<i64>,    // Offset samples for median
    synced: bool,
    next_sequence: u8,
}

impl ClockSync {
    pub fn new() -> Self;
    pub fn process_response(...) -> bool;  // Returns true when sync complete
    pub fn to_local_time(&self, host_ms: i64) -> i64;
    pub fn to_host_time(&self, local_ms: i64) -> i64;
    pub fn is_synced(&self) -> bool;
    pub fn needs_more_samples(&self) -> bool;
    pub fn avg_rtt_ms(&self) -> u32;
    pub fn offset_ms(&self) -> i64;
}
```

**Algorithm:**
- Performs 5 sync exchanges
- Uses median RTT and offset for stability
- Offset calculation: `server_receive_time - client_time - (RTT/2)`

---

### 1.2 Frame Sync State

**File:** `crates/core/src/screen/adaptive.rs`

#### FrameSyncState Implementation

```rust
/// Action to take for a received frame
pub enum FrameAction {
    Display,                    // Display normally
    Drop,                       // Too old, skip
    RequestQualityReduction,    // Falling behind, suggest lower quality
    RequestKeyframeFlush,       // Severely behind, flush and request keyframe
}

/// Frame timing and synchronization state
pub struct FrameSyncState {
    target_latency_ms: u32,      // Target (default: 100ms)
    max_latency_ms: u32,         // Max before dropping (default: 500ms)
    latency_window: VecDeque<i64>,
    frames_behind: u32,          // Consecutive late frames
    catching_up: bool,
}

impl FrameSyncState {
    pub fn new(target_latency_ms: u32, max_latency_ms: u32) -> Self;
    pub fn evaluate_frame(&mut self, captured_at_ms: i64, now_ms: i64) -> FrameAction;
    pub fn record_latency(&mut self, latency_ms: i64) -> FrameAction;
    pub fn avg_latency_ms(&self) -> u32;
    pub fn reset_after_flush(&mut self);
    pub fn suggest_quality(&self, current: ScreenQuality) -> ScreenQuality;
}
```

**Decision Logic:**
- `latency > max_latency` → `Drop`
- `latency > 2 * target` for 10+ frames → `RequestQualityReduction`
- `catching_up && frames_behind > 20` → `RequestKeyframeFlush`
- Otherwise → `Display`

---

### 1.3 Adaptive Bitrate Controller

**File:** `crates/core/src/screen/adaptive.rs`

#### AdaptiveBitrate Implementation

```rust
pub enum BitrateState {
    Increase,   // Probing for more bandwidth
    Hold,       // Stable
    Decrease,   // Reducing due to congestion
}

/// GCC-inspired adaptive bitrate controller
pub struct AdaptiveBitrate {
    min_bitrate_kbps: u32,
    max_bitrate_kbps: u32,
    current_bitrate_kbps: u32,
    rtt_history: VecDeque<u32>,     // Last 10 RTT samples
    loss_history: VecDeque<f32>,    // Last 10 loss samples
    state: BitrateState,
    frames_since_adjust: u32,
    adjust_interval: u32,           // ~30 frames (0.5s at 60fps)
}

impl AdaptiveBitrate {
    pub fn new(min_kbps: u32, max_kbps: u32, initial_kbps: u32) -> Self;
    pub fn update(&mut self, rtt_ms: u32, packet_loss: f32) -> Option<u32>;
    pub fn force_decrease(&mut self) -> u32;
    pub fn current_bitrate(&self) -> u32;
    pub fn state(&self) -> BitrateState;
}
```

**Algorithm (GCC-inspired):**
- Packet loss > 2% → Decrease by 15%
- RTT increasing → Hold steady
- Loss < 1% && RTT stable → Increase by 5%
- Minimum 30 frames between adjustments

---

### 1.4 Packet Loss Tracker

**File:** `crates/core/src/screen/adaptive.rs`

```rust
pub struct PacketLossTracker {
    expected_seq: u64,
    received: u32,
    lost: u32,
    window_size: u32,    // Default: 60 (1 second at 60fps)
    initialized: bool,
}

impl PacketLossTracker {
    pub fn new(window_size: u32) -> Self;
    pub fn record(&mut self, sequence: u64) -> f32;  // Returns current loss rate
    pub fn current_rate(&self) -> f32;
    pub fn reset(&mut self);
}
```

**Behavior:**
- Detects gaps in sequence numbers
- Calculates loss rate over sliding window
- Resets counters after each window

---

### 1.5 Enhanced ScreenFrameAck

**File:** `crates/core/src/iroh/protocol.rs`

```rust
/// Acknowledge receipt of frames (for flow control and adaptive streaming)
ScreenFrameAck {
    stream_id: String,
    up_to_sequence: u64,
    estimated_bandwidth: Option<u64>,
    quality_hint: Option<ScreenQuality>,
    // NEW FIELDS:
    measured_latency_ms: Option<u32>,    // End-to-end latency
    packet_loss: Option<f32>,            // Loss rate (0.0-1.0)
    request_keyframe: bool,              // Request keyframe for recovery
},
```

---

### 1.6 Viewer Integration

**File:** `crates/core/src/screen/viewer.rs`

#### New Fields in ScreenViewer

```rust
pub struct ScreenViewer {
    // Existing fields...

    // NEW: Sync and adaptive components
    clock_sync: ClockSync,
    frame_sync: FrameSyncState,
    loss_tracker: PacketLossTracker,
    adaptive_bitrate: AdaptiveBitrate,
    current_quality: ScreenQuality,
    frames_dropped_latency: u64,
}
```

#### New ViewerConfig Fields

```rust
pub struct ViewerConfig {
    // Existing fields...

    // NEW: Latency settings
    pub target_latency_ms: u32,        // Default: 100
    pub max_latency_ms: u32,           // Default: 500
    pub enable_adaptive_bitrate: bool, // Default: true
    pub min_bitrate_kbps: u32,         // Default: 500
    pub max_bitrate_kbps: u32,         // Default: 20000
}
```

#### New ViewerStats Fields

```rust
pub struct ViewerStats {
    // Existing fields...

    // NEW: Enhanced stats
    pub network_rtt_ms: u32,
    pub packet_loss: f32,
    pub clock_synced: bool,
    pub clock_offset_ms: i64,
}
```

#### New ViewerEvent Variants

```rust
pub enum ViewerEvent {
    // Existing variants...

    // NEW:
    FrameDropped { sequence: u64, reason: String },
    ClockSynced { offset_ms: i64, rtt_ms: u32 },
    QualityAdjustmentNeeded { suggested_quality: ScreenQuality, reason: String },
    KeyframeNeeded { reason: String },
    BitrateAdjustmentNeeded { suggested_kbps: u32 },
}
```

#### Key Methods

```rust
impl ScreenViewer {
    /// Main frame reception with full adaptive logic
    pub fn on_frame_received_with_metadata(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        sequence: u64,
        captured_at_ms: i64,
    ) -> Result<FrameAction>;

    /// Process time sync response
    pub fn on_time_sync_response(
        &mut self,
        client_time: i64,
        server_receive_time: i64,
        server_send_time: i64,
    ) -> bool;  // Returns true when sync complete

    /// Query methods
    pub fn is_clock_synced(&self) -> bool;
    pub fn needs_time_sync(&self) -> bool;
    pub fn next_time_sync_sequence(&mut self) -> u8;
    pub fn packet_loss_rate(&self) -> f32;
    pub fn current_bitrate(&self) -> u32;
}
```

---

### 1.7 Usage Flow

1. **Connection Established:**
   - Viewer connects to host via Iroh
   - Viewer sends 5 `TimeSyncRequest` messages sequentially
   - Host immediately responds with `TimeSyncResponse` (handler in `app.rs`)
   - Viewer calculates clock offset via `ClockSync::process_response()`
   - `ScreenStreamEvent::ClockSynced` emitted with offset and RTT

2. **Stream Request:**
   - Viewer sends `ScreenStreamRequest` after clock sync completes
   - Host validates permissions and starts capture
   - Host responds with `ScreenStreamResponse`

3. **Frame Reception:**
   - Call `on_frame_received_with_metadata()` with captured_at
   - Method adjusts timestamp for clock offset
   - FrameSyncState evaluates latency and returns action
   - PacketLossTracker records sequence for gap detection
   - AdaptiveBitrate updates based on RTT and loss

4. **Action Handling:**
   - `FrameAction::Display` → Decode and buffer frame
   - `FrameAction::Drop` → Skip frame, emit FrameDropped event
   - `FrameAction::RequestQualityReduction` → Emit QualityAdjustmentNeeded
   - `FrameAction::RequestKeyframeFlush` → Clear buffer, emit KeyframeNeeded

5. **ACK Generation:**
   - Periodically send `ScreenFrameAck` with:
     - `measured_latency_ms`: From `frame_sync.avg_latency_ms()`
     - `packet_loss`: From `loss_tracker.current_rate()`
     - `request_keyframe`: true if flush was needed

### 1.8 Integration Points

**Viewer-side (`crates/core/src/iroh/transfer.rs`):**
- `stream_screen_from_peer()` performs clock sync before sending `ScreenStreamRequest`
- Emits `ScreenStreamEvent::ClockSynced` with offset and RTT

**Host-side (`crates/gui/src/app.rs`):**
- Background listener handles `TimeSyncRequest` messages
- Responds immediately with `TimeSyncResponse` including server timestamps

**New Event Types:**
- `ScreenStreamEvent::ClockSynced { offset_ms, rtt_ms }` - Notifies when clock sync completes

---

## Phase 2: Cursor Capture and Transmission

**Status: Planned**

See original architecture document for detailed design.

---

## Phase 3: Viewer Preferences and UI

**Status: Planned**

See original architecture document for detailed design.

---

## File Structure

```
crates/core/src/screen/
├── mod.rs                  # Module exports (updated)
├── adaptive.rs             # NEW: AdaptiveBitrate, PacketLossTracker, FrameSyncState
├── time_sync.rs            # NEW: ClockSync
├── viewer.rs               # MODIFIED: Integrated new components
├── types.rs                # Existing
├── session.rs              # Existing
├── events.rs               # Existing
├── manager.rs              # Existing
├── encoder.rs              # Existing
├── decoder.rs              # Existing
├── input.rs                # Existing
├── portal.rs               # Existing (Linux)
├── drm.rs                  # Existing (Linux)
├── x11.rs                  # Existing (Linux)
└── dxgi.rs                 # Existing (Windows)

crates/core/src/iroh/
├── protocol.rs             # MODIFIED: Added TimeSyncRequest/Response, enhanced ScreenFrameAck
└── transfer.rs             # MODIFIED: Added clock sync to stream_screen_from_peer(), ClockSynced event

crates/gui/src/
└── app.rs                  # MODIFIED: Added TimeSyncRequest handler in background listener
```

---

## Testing

```bash
# Run Phase 1 tests
cargo test -p croh-core --lib -- time_sync adaptive

# Run viewer tests
cargo test -p croh-core --lib -- screen::viewer
```

All tests pass as of implementation.
