# Screen Streaming Architecture: Phases 1-3

Detailed implementation architecture for time sync, adaptive streaming, cursor capture, and viewer UI.

## Overview

| Phase | Focus | Key Deliverables | Status |
|-------|-------|------------------|--------|
| 1 | Latency & Sync | Clock sync, frame dropping, adaptive bitrate | **IMPLEMENTED** |
| 2 | Cursor | Cursor capture, transmission, client rendering | **IMPLEMENTED** |
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

**Status: IMPLEMENTED**

### 2.1 Protocol Messages

**File:** `crates/core/src/iroh/protocol.rs`

```rust
/// Cursor pixel format
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CursorFormat {
    #[default]
    Rgba32,      // 32-bit RGBA (most common)
    Monochrome,  // 1-bit with AND/XOR masks
    MaskedColor, // Color with transparency mask
}

/// Cursor shape data for transmission
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorShape {
    pub shape_id: u64,    // Unique ID for caching
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub format: CursorFormat,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,    // RGBA pixel data
}

/// Cursor update sent to viewer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorUpdate {
    pub x: i32,           // Screen coordinates
    pub y: i32,
    pub visible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<CursorShape>,  // Only sent when shape changes
}

// In ControlMessage enum:
ScreenCursorUpdate {
    stream_id: String,
    cursor: CursorUpdate,
},
```

---

### 2.2 Backend Types

**File:** `crates/core/src/screen/types.rs`

```rust
/// Captured cursor information from backend
#[derive(Debug, Clone)]
pub struct CapturedCursor {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub shape: Option<CursorShape>,
}

/// Cursor shape data (backend format)
#[derive(Debug, Clone)]
pub struct CursorShape {
    pub shape_id: u64,    // Hash of cursor data for deduplication
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub data: Vec<u8>,    // RGBA pixel data
}

// Added to ScreenCapture trait:
#[async_trait]
pub trait ScreenCapture: Send + Sync {
    // ... existing methods ...

    /// Capture cursor state (position and optionally shape).
    /// Default implementation returns None (cursor capture not supported).
    async fn capture_cursor(&mut self) -> Result<Option<CapturedCursor>> {
        Ok(None)
    }
}
```

---

### 2.3 X11 Cursor Capture

**File:** `crates/core/src/screen/x11.rs`

Uses XFixes extension for hardware cursor capture.

```rust
impl X11Capture {
    /// Capture cursor using XFixes extension
    fn capture_cursor_xfixes(&mut self) -> Result<Option<CapturedCursor>> {
        // 1. Get cursor image via xfixes_get_cursor_image()
        // 2. Convert ARGB (X11 format) to RGBA
        // 3. Calculate shape_id from cursor serial
        // 4. Return CapturedCursor with position and shape
    }
}
```

**Key Implementation Details:**
- Uses `x11rb::protocol::xfixes` extension
- Converts X11 ARGB format to standard RGBA
- Shape ID derived from cursor serial for efficient caching
- Handles cursor outside display bounds gracefully

**Dependencies Added:**
```toml
# In Cargo.toml
x11rb = { version = "0.13", features = ["shm", "randr", "xfixes"] }
```

---

### 2.4 Windows Cursor Capture

**File:** `crates/core/src/screen/dxgi.rs`

Uses Win32 API for cursor capture.

```rust
impl DxgiCapture {
    /// Capture cursor using Win32 API
    fn capture_cursor_win32(&mut self) -> Result<Option<CapturedCursor>> {
        // 1. GetCursorInfo() for position and visibility
        // 2. CopyIcon() + GetIconInfo() for cursor image
        // 3. GetDIBits() to extract pixel data
        // 4. Handle both color and monochrome cursors
    }

    /// Extract RGBA data from color cursor
    fn extract_color_cursor(&self, ...) -> Option<Vec<u8>>;

    /// Extract RGBA data from monochrome cursor (AND/XOR masks)
    fn extract_mono_cursor(&self, ...) -> Option<Vec<u8>>;
}
```

**Key Implementation Details:**
- Handles both 32-bit color cursors and monochrome (1-bit) cursors
- Monochrome cursors use AND/XOR mask logic for proper rendering
- Coordinates adjusted relative to captured display
- Shape ID based on HCURSOR handle for caching

---

### 2.5 Client-Side Cursor Renderer

**File:** `crates/core/src/screen/cursor.rs`

```rust
/// Maximum cached cursor shapes
const MAX_CACHED_SHAPES: usize = 32;

/// Client-side cursor renderer with alpha blending
pub struct CursorRenderer {
    position: (i32, i32),
    visible: bool,
    current_shape_id: u64,
    shape_cache: HashMap<u64, CachedShape>,
    shape_order: Vec<u64>,  // LRU order
}

struct CachedShape {
    width: u32,
    height: u32,
    hotspot_x: u32,
    hotspot_y: u32,
    data: Vec<u8>,  // RGBA
}

impl CursorRenderer {
    pub fn new() -> Self;

    /// Update cursor from network message
    pub fn update(&mut self, update: &CursorUpdate);

    /// Render cursor onto RGBA frame buffer
    pub fn render_on_frame(&self, frame: &mut [u8], width: u32, height: u32);

    /// Query methods
    pub fn position(&self) -> (i32, i32);
    pub fn is_visible(&self) -> bool;
    pub fn set_visible(&mut self, visible: bool);
    pub fn clear_cache(&mut self);
    pub fn cached_shape_count(&self) -> usize;
}
```

**Rendering Algorithm:**
1. Look up current shape in cache
2. Calculate render position (subtract hotspot)
3. For each cursor pixel:
   - Skip if fully transparent (alpha = 0)
   - If fully opaque (alpha = 255): direct copy
   - Otherwise: alpha blend with destination
4. Handle clipping at frame boundaries

**Alpha Blending Formula:**
```rust
// For partial transparency
let alpha = src_a as u16;
let inv_alpha = 255 - alpha;
dst_r = ((src_r * alpha + dst_r * inv_alpha) / 255) as u8;
// Same for G, B channels
```

---

### 2.6 Usage Flow

1. **Host-side Capture:**
   - Call `capture.capture_cursor()` alongside frame capture
   - Compare `shape_id` with previous to detect shape changes
   - Only include `CursorShape` in update when changed

2. **Transmission:**
   - Send `ScreenCursorUpdate` message with `CursorUpdate`
   - Shape data only sent when cursor changes (efficient bandwidth)
   - Position updates sent at capture rate

3. **Client-side Rendering:**
   - `CursorRenderer::update()` processes incoming updates
   - Cache stores shapes by `shape_id` (LRU eviction at 32 entries)
   - `render_on_frame()` composites cursor onto decoded frame

4. **Bandwidth Optimization:**
   - Shape ID caching prevents redundant data transmission
   - Only position/visibility sent when shape unchanged
   - Typical cursor shape: ~1-4KB (32x32 RGBA)

---

### 2.7 Testing

```bash
# Run cursor tests
cargo test -p croh-core --lib -- cursor

# Tests included:
# - test_cursor_renderer_creation
# - test_cursor_update
# - test_cursor_render
# - test_cache_eviction
```

All 4 cursor tests pass.

---

## Phase 3: Viewer Preferences and UI

**Status: Planned**

See original architecture document for detailed design.

---

## File Structure

```
crates/core/src/screen/
├── mod.rs                  # Module exports (updated)
├── adaptive.rs             # Phase 1: AdaptiveBitrate, PacketLossTracker, FrameSyncState
├── time_sync.rs            # Phase 1: ClockSync
├── cursor.rs               # Phase 2: CursorRenderer (client-side rendering)
├── viewer.rs               # MODIFIED: Integrated new components
├── types.rs                # MODIFIED: Added CapturedCursor, CursorShape, capture_cursor()
├── session.rs              # Existing
├── events.rs               # Existing
├── manager.rs              # Existing
├── encoder.rs              # Existing
├── decoder.rs              # Existing
├── input.rs                # Existing
├── portal.rs               # Existing (Linux)
├── drm.rs                  # Existing (Linux)
├── x11.rs                  # MODIFIED: Added XFixes cursor capture
└── dxgi.rs                 # MODIFIED: Added Win32 cursor capture (Windows)

crates/core/src/iroh/
├── protocol.rs             # MODIFIED: Added TimeSyncRequest/Response, ScreenCursorUpdate, cursor types
└── transfer.rs             # MODIFIED: Added clock sync to stream_screen_from_peer(), ClockSynced event

crates/gui/src/
└── app.rs                  # MODIFIED: Added TimeSyncRequest handler in background listener
```

---

## Testing

```bash
# Run Phase 1 tests (time sync, adaptive streaming)
cargo test -p croh-core --lib -- time_sync adaptive

# Run Phase 2 tests (cursor rendering)
cargo test -p croh-core --lib -- cursor

# Run viewer tests
cargo test -p croh-core --lib -- screen::viewer

# Run all screen module tests
cargo test -p croh-core --lib -- screen::
```

All 154 tests pass as of Phase 2 implementation (150 original + 4 cursor tests).
