# Screen Streaming Improvements Plan

Based on research of RustDesk, Sunshine/Moonlight, Parsec, NoMachine, and analysis of the current croh implementation.

> **Note:** Phase 1 (Time Sync & Adaptive Streaming) has been implemented. See [architecture/screen-streaming-phase1-3.md](architecture/screen-streaming-phase1-3.md) for implementation details.

## Executive Summary

The current implementation has a solid foundation but lacks several key features for production-quality streaming:

1. **Time sync/latency** - No clock sync, no formal frame pacing or catch-up strategies
2. **Cursor** - Not implemented despite framework existing
3. **Viewer preferences** - Config exists but no runtime UI
4. **Unattended Wayland** - Portal restore tokens exist but need verification/improvement

---

## 1. Time Synchronization and Latency Management

### Problem
Currently experiencing variable perceived latency (sub-second to multiple seconds). The implementation has timestamps but no formal sync protocol.

### Current State
- ✓ `captured_at` (Unix millis) in frame metadata
- ✓ Latency calculation viewer-side
- ✓ Frame sequence numbers
- ✗ No clock synchronization
- ✗ No adaptive frame dropping based on latency
- ✗ No bitrate reduction when falling behind

### Proposed Solution

#### 1.1 Clock Synchronization Protocol

Add a lightweight NTP-like sync at stream start:

```rust
// New protocol messages
pub struct TimeSyncRequest {
    pub client_time: i64,  // Unix millis
    pub sequence: u32,
}

pub struct TimeSyncResponse {
    pub client_time: i64,      // Echo back
    pub server_time: i64,      // Host's time when received
    pub server_send_time: i64, // Host's time when sending response
}

// Viewer calculates:
// RTT = (response_received - client_time)
// Offset = server_time + (RTT/2) - response_received
```

**Implementation:**
- Run 3-5 sync exchanges at stream start
- Use median offset to minimize outlier impact
- Re-sync periodically (every 30 seconds) during streaming
- Store offset in `ViewerStats`

#### 1.2 Frame Pacing and Sync Detection

**Detect when falling behind:**
```rust
pub struct FrameSyncState {
    pub target_latency_ms: u32,      // User-configurable (default: 100ms)
    pub max_latency_ms: u32,         // Before dropping (default: 500ms)
    pub latency_window: VecDeque<i64>, // Rolling 30 samples
    pub frames_behind: u32,          // Sequential late frames
}

impl FrameSyncState {
    pub fn is_falling_behind(&self) -> bool {
        self.average_latency() > self.target_latency_ms as i64 * 2
    }

    pub fn should_drop_frame(&self) -> bool {
        self.average_latency() > self.max_latency_ms as i64
    }
}
```

**Catch-up strategies (in order of aggressiveness):**

1. **Skip decode for old frames** - If frame's `captured_at + max_latency < now`, skip
2. **Request quality reduction** - Send `ScreenStreamAdjust` with lower quality hint
3. **Request keyframe + flush** - Drop all buffered, request fresh keyframe
4. **Emergency bitrate reduction** - Halve bitrate until caught up

#### 1.3 Adaptive Bitrate Algorithm

Implement Google Congestion Control (GCC) inspired algorithm:

```rust
pub struct AdaptiveBitrate {
    pub min_bitrate_kbps: u32,       // Floor (500 kbps)
    pub max_bitrate_kbps: u32,       // Ceiling (20,000 kbps)
    pub current_bitrate_kbps: u32,
    pub rtt_history: VecDeque<u32>,
    pub loss_history: VecDeque<f32>,
    pub state: BitrateState,         // Increase, Hold, Decrease
}

impl AdaptiveBitrate {
    pub fn update(&mut self, rtt_ms: u32, packet_loss: f32) -> u32 {
        // If packet loss > 2%, decrease bitrate by 15%
        // If RTT increasing trend, hold bitrate
        // If RTT stable and loss < 1%, increase by 5%
        // Returns new target bitrate
    }
}
```

**Integration points:**
- `ScreenFrameAck` already has `estimated_bandwidth` field
- Add packet loss tracking via sequence gaps
- Encoder already supports runtime quality adjustment

#### 1.4 Latency Display UI

Add overlay statistics (like Moonlight's Ctrl+Alt+Shift+S):

```
┌─ Stream Stats ────────────────┐
│ Network RTT:     45ms         │
│ End-to-end:      78ms         │
│ Decode time:     8ms          │
│ Frame queue:     2 frames     │
│ FPS: 58/60      Quality: High │
│ Bitrate: 15.2 Mbps            │
└───────────────────────────────┘
```

---

## 2. Cursor Handling

### Problem
No mouse cursor visible during screen share. Backend `supports_cursor` is false for all platforms.

### Current State
- Framework exists (`BackendInfo.supports_cursor`)
- Config has `show_cursor`, `highlight_clicks`
- No actual cursor capture in any backend
- Input events work (mouse moves are transmitted)

### Proposed Solution

#### 2.1 Cursor Protocol Messages

```rust
pub struct CursorUpdate {
    pub position: (i32, i32),      // Screen coordinates
    pub visible: bool,
    pub shape: Option<CursorShape>, // Only when shape changes
}

pub struct CursorShape {
    pub width: u32,
    pub height: u32,
    pub hotspot: (u32, u32),       // Click point within image
    pub format: CursorFormat,      // RGBA32, Monochrome, etc.
    pub data: Vec<u8>,             // Image data
}

pub enum CursorFormat {
    Rgba32,
    Monochrome,      // 1-bit (Windows legacy)
    MaskedColor,     // Color + mask (Windows)
}
```

#### 2.2 Platform-Specific Capture

**Linux/X11:**
```rust
// Use XFixesCursorImage
pub fn capture_cursor_x11() -> Option<CursorUpdate> {
    let cursor = XFixesGetCursorImage(display);
    // cursor contains: x, y, width, height, xhot, yhot, pixels
}
```

**Linux/Wayland (Portal):**
- Portal doesn't provide cursor directly
- Option 1: Render cursor into frame (compositor handles this if requested)
- Option 2: Use `cursor_mode` in ScreenCast portal (GNOME 44+)

**Linux/DRM:**
- Read cursor plane from DRM if available
- Many GPUs expose cursor as separate plane

**Windows/DXGI:**
```rust
// Use GetCursorInfo + CopyIcon
pub fn capture_cursor_win32() -> Option<CursorUpdate> {
    let mut info = CURSORINFO::default();
    GetCursorInfo(&mut info);
    if info.flags == CURSOR_SHOWING {
        // Extract cursor bitmap
    }
}
```

#### 2.3 Transmission Strategy

**Separate cursor channel (recommended):**
- Send cursor updates independently from video frames
- Higher update rate possible (120Hz+)
- Lower latency for mouse movement
- Protocol: Include in existing control stream, not video stream

**Cursor in frame (fallback):**
- Composite cursor into video frame before encoding
- Simpler but adds latency
- Required when client-side rendering not possible

#### 2.4 Client-Side Rendering

```rust
pub struct CursorRenderer {
    current_shape: Option<CursorShape>,
    current_position: (i32, i32),
    visible: bool,
}

impl CursorRenderer {
    pub fn render_on_frame(&self, frame: &mut [u8], frame_width: u32) {
        if !self.visible || self.current_shape.is_none() {
            return;
        }
        // Alpha-blend cursor onto frame at position
    }
}
```

---

## 3. Viewer Preferences UI

### Problem
Settings exist in config but no runtime UI to adjust them.

### Proposed Solution

#### 3.1 Settings Panel Structure

```
┌─ Stream Settings ─────────────────────────────┐
│                                               │
│ Quality Preset: [Fast] [Balanced] [Quality]   │
│                                               │
│ ─── Advanced ──────────────────────────────── │
│                                               │
│ Max FPS:        [────●──────] 60              │
│ Target Latency: [──●────────] 100ms           │
│ Max Bitrate:    [────────●──] 15 Mbps         │
│                                               │
│ [x] Show remote cursor                        │
│ [x] Hardware acceleration                     │
│ [ ] Prioritize quality over latency           │
│                                               │
│ Compression: [Auto ▼]                         │
│   - Auto (recommended)                        │
│   - H.264 (best quality)                      │
│   - Zstd (fast, lossless)                     │
│   - PNG (static content)                      │
│                                               │
│ [Apply] [Reset to Defaults]                   │
└───────────────────────────────────────────────┘
```

#### 3.2 Runtime Preference Changes

```rust
pub struct ViewerPreferences {
    pub quality_preset: ScreenQuality,    // Fast/Balanced/Quality
    pub max_fps: u32,                     // 15-120
    pub target_latency_ms: u32,           // 50-500
    pub max_bitrate_kbps: u32,            // 500-50000
    pub show_cursor: bool,
    pub hardware_decode: bool,
    pub prefer_quality: bool,             // vs prefer latency
    pub compression: ScreenCompression,
}

impl ViewerPreferences {
    pub fn apply(&self, stream: &mut StreamSession) {
        // Send ScreenStreamAdjust message to host
        // Update local decoder settings
    }
}
```

#### 3.3 Persistence

Add to existing config structure:

```rust
pub struct ScreenStreamSettings {
    // Existing fields...

    // Viewer preferences (per-peer optional)
    pub viewer_presets: HashMap<String, ViewerPreferences>,
    pub default_viewer_prefs: ViewerPreferences,
}
```

---

## 4. Unattended Wayland Access

### Problem
Wayland requires permission dialog for each screen share session. Current Portal implementation has restore tokens but behavior is inconsistent.

### Current State
- ✓ Portal capture implemented (`portal.rs`)
- ✓ Restore token storage in config
- ✗ Token may not survive session restart
- ✗ No fallback strategies
- ✗ No compositor-specific handling

### Research Findings

**Compositor Support:**
| Compositor | Unattended Support | Method |
|------------|-------------------|--------|
| GNOME 47+  | ✓ Yes | gnome-remote-desktop + libei |
| KDE Plasma | Partial | KRdp, portal restore tokens |
| wlroots    | Partial | wayvnc, xdg-desktop-portal-wlr |

**Key insight:** True unattended access on Wayland requires compositor cooperation. The XDG portal `persist_mode=2` is necessary but not always sufficient.

### Proposed Solution

#### 4.1 Robust Token Handling

```rust
pub struct PortalSession {
    restore_token: Option<String>,
    persist_mode: PersistMode,
    last_successful_capture: Option<Instant>,
    consecutive_failures: u32,
}

impl PortalSession {
    pub async fn start_with_token(&mut self) -> Result<PipeWireStream> {
        if let Some(token) = &self.restore_token {
            match self.try_restore(token).await {
                Ok(stream) => {
                    // Update token (they're single-use)
                    self.restore_token = stream.new_token();
                    return Ok(stream);
                }
                Err(e) => {
                    log::warn!("Restore failed, will prompt: {}", e);
                    self.restore_token = None;
                }
            }
        }
        // Fall through to interactive selection
        self.start_interactive().await
    }
}
```

#### 4.2 Token Persistence Improvements

```rust
// Save token immediately after each successful capture
pub fn persist_token(&self, config: &mut Config) {
    if let Some(token) = &self.restore_token {
        config.screen_stream.portal_restore_token = Some(token.clone());
        config.save().ok(); // Best effort
    }
}

// Clear invalid tokens
pub fn invalidate_token(&mut self, config: &mut Config) {
    self.restore_token = None;
    config.screen_stream.portal_restore_token = None;
    config.save().ok();
}
```

#### 4.3 Compositor Detection and Hints

```rust
pub enum WaylandCompositor {
    Gnome,
    KdePlasma,
    Sway,
    Hyprland,
    Unknown,
}

impl WaylandCompositor {
    pub fn detect() -> Self {
        // Check XDG_CURRENT_DESKTOP, DESKTOP_SESSION, etc.
    }

    pub fn supports_unattended(&self) -> bool {
        matches!(self, Self::Gnome) // GNOME 47+ only currently
    }

    pub fn setup_instructions(&self) -> &'static str {
        match self {
            Self::Gnome => "Enable 'Remote Desktop' in GNOME Settings",
            Self::KdePlasma => "Grant permission once; token will be remembered",
            _ => "Permission dialog required for each session",
        }
    }
}
```

#### 4.4 Fallback Chain

```rust
pub async fn create_capture_backend() -> Result<Box<dyn ScreenCapture>> {
    // 1. Try DRM if CAP_SYS_ADMIN available (best performance)
    if has_cap_sys_admin() {
        if let Ok(drm) = DrmCapture::new() {
            return Ok(Box::new(drm));
        }
    }

    // 2. Try Portal with restore token
    if is_wayland() {
        if let Ok(portal) = PortalCapture::new_with_token().await {
            return Ok(Box::new(portal));
        }
        // 3. Try Portal interactive (will show dialog)
        if let Ok(portal) = PortalCapture::new_interactive().await {
            return Ok(Box::new(portal));
        }
    }

    // 4. Try X11 if available (Xwayland or X11 session)
    if let Ok(x11) = X11Capture::new() {
        return Ok(Box::new(x11));
    }

    Err(CaptureError::NoBackendAvailable)
}
```

---

## 5. Implementation Priority

### Phase 1: Critical Latency Fixes (Highest Impact)
1. Frame dropping based on latency threshold
2. Clock sync protocol
3. Basic adaptive bitrate (decrease on packet loss)

### Phase 2: Cursor Support
4. Cursor capture (X11 first, then Windows)
5. Cursor protocol messages
6. Client-side cursor rendering

### Phase 3: User Experience
7. Statistics overlay (latency, FPS, bitrate)
8. Viewer preferences UI
9. Quality presets runtime switching

### Phase 4: Wayland Polish
10. Robust token persistence
11. Compositor detection and messaging
12. GNOME Remote Desktop integration research

---

## 6. Testing Strategy

### Latency Testing
- Simulate high-latency network (tc netem)
- Measure with on-screen timestamp comparison
- Verify frame dropping triggers correctly

### Cursor Testing
- Different cursor types (arrow, hand, text, custom)
- Rapid movement scenarios
- Cursor at screen edges

### Wayland Testing
- Test token persistence across session restart
- Test on GNOME, KDE, Sway
- Verify fallback chain works

---

## References

- [Moonlight FAQ - Statistics](https://github.com/moonlight-stream/moonlight-docs/wiki)
- [RustDesk ABR Discussion](https://github.com/rustdesk/rustdesk/discussions/792)
- [XDG Portal ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/)
- [GNOME Remote Desktop](https://wiki.gnome.org/Projects/Mutter/RemoteDesktop)
- [Google Congestion Control (GCC)](https://datatracker.ietf.org/doc/html/draft-ietf-rmcat-gcc)
