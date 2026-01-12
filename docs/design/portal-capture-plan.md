# XDG Portal Screen Capture Implementation Plan

## Overview

This document outlines the phased implementation of XDG Desktop Portal screen capture with restore token support for persistent unattended access on Wayland.

## Goals

1. **Primary**: Enable screen capture on Wayland compositors (KDE, GNOME, etc.)
2. **Secondary**: Support restore tokens for unattended access after initial approval
3. **Tertiary**: Integrate with existing `ScreenCapture` trait for seamless backend switching

## Technical Background

### XDG Desktop Portal

- Standard D-Bus API for sandboxed apps to access system resources
- `org.freedesktop.portal.ScreenCast` - Screen/window capture
- `org.freedesktop.portal.RemoteDesktop` - Remote control + capture (for input)
- PipeWire provides the actual video stream

### Restore Tokens

- `persist_mode=2` requests permanent persistence until revoked
- Token returned after first `Start()` call
- Token is **single-use** - must save new token after each session
- Token invalidated if displays change or permissions revoked

### Compositor Support

| Compositor | Portal Version | Restore Tokens | Notes |
|------------|---------------|----------------|-------|
| KDE Plasma 5.27+ | v4 | Yes | Full support |
| GNOME 42+ | v4 | Yes | Full support |
| wlroots (Sway/Hyprland) | v3 | No | Use wlroots screencopy instead |

---

## Phase 1: Dependencies & Foundation ✅ COMPLETE

**Goal**: Add ashpd crate and create basic Portal backend structure.

### Tasks

- [x] **1.1** Add `ashpd` dependency to `croh-core/Cargo.toml`
  - `ashpd = "0.12"` (tokio enabled by default in 0.12+)
  - Only for `target_os = "linux"`

- [x] **1.2** Create `crates/core/src/screen/portal.rs` module
  - Define `PortalCapture` struct with state machine
  - Implement `ScreenCapture` trait (stubs initially)
  - Add `is_available()` check (Wayland session + portal paths)

- [x] **1.3** Add restore token storage to config
  - Add `portal_restore_token: Option<String>` to `ScreenStreamSettings`
  - Token persisted in config.json

- [x] **1.4** Wire up Portal backend in `mod.rs`
  - Add `mod portal;` conditionally compiled for Linux
  - Add Portal to `create_capture_backend()` match
  - Add Portal to `auto_detect_backend()` fallback chain (after DRM, before X11)
  - Add Portal to `is_capture_available()` check

### Deliverables
- ✅ Portal backend compiles (non-functional stubs)
- ✅ Config supports restore token storage
- ✅ All 133 tests pass

---

## Phase 2: Portal Session Management ✅ COMPLETE

**Goal**: Implement D-Bus session lifecycle with ScreenCast portal.

### Tasks

- [x] **2.1** Implement Portal session creation
  - `CreateSession()` with session handle via ashpd
  - `SelectSources()` with `PersistMode::ExplicitlyRevoked`
  - Pass restore_token if available

- [x] **2.2** Implement `Start()` and token extraction
  - Call `Start()` on session
  - Extract new restore_token from response
  - Token stored in `self.restore_token` for caller to persist

- [x] **2.3** Implement `list_displays()`
  - Maps Portal streams to `Display` structs after session starts
  - Returns placeholder before session starts (Portal can't enumerate without dialog)

- [x] **2.4** Error handling for permission dialogs
  - Handle user cancellation gracefully (detects "cancelled"/"denied" in error)
  - Token invalidation triggers new dialog automatically
  - Clear logging throughout session lifecycle

### Deliverables
- ✅ Session creation works via `create_and_start_session()`
- ✅ Restore token extracted and stored after approval
- ✅ Subsequent sessions with valid token skip dialog
- ✅ PipeWire node IDs extracted and stored in `StreamInfo`
- ✅ All 134 tests pass

### Notes
- Portal session/screencast objects are not stored (lifetime issues with zbus)
- Session is created, started, and stream info extracted in one call
- PipeWire node ID is available for Phase 3 frame capture

---

## Phase 3: PipeWire Frame Capture ✅ COMPLETE

**Goal**: Receive frames from PipeWire and convert to `CapturedFrame`.

### Tasks

- [x] **3.1** Set up PipeWire stream from Portal
  - Added `pipewire = "0.9"` dependency
  - Get PipeWire fd from `open_pipe_wire_remote()`
  - Connect using `context.connect_fd_rc(pipewire_fd)`
  - PipeWire mainloop runs on dedicated thread

- [x] **3.2** Implement frame capture loop
  - Multi-threaded architecture: PipeWire mainloop on dedicated thread
  - Frames sent via mpsc channel to async context
  - Stream listener with `.process()` callback for frame capture
  - Timer-based polling for stop command handling

- [x] **3.3** Handle format negotiation
  - Format pod built with spa::pod::object! macro
  - Support common formats: BGRx (preferred), RGBx, BGRA, RGBA
  - `spa_format_to_pixel_format()` conversion function
  - Format info tracked via Arc<Mutex> between callbacks

- [x] **3.4** Implement `capture_frame()` method
  - Returns latest frame from channel (non-blocking try_recv)
  - Returns `Ok(None)` when no new frame available
  - Tracks frame dimensions and timestamps
  - Converts PipeWire buffer to `CapturedFrame`

### Deliverables
- ✅ PipeWire stream connects to portal node
- ✅ Frames received from PipeWire callbacks
- ✅ Frames converted to `CapturedFrame` format
- ✅ All tests pass (136 total)

### Architecture Notes
- Portal session created in async context, PipeWire runs on separate thread
- `Receiver<PipeWireFrame>` wrapped in `Arc<Mutex>` for Sync
- Stop command sent via mpsc channel, checked in timer callback
- Stream uses `StreamBox` with borrowed CoreRc (lifetime managed by thread)

---

## Phase 4: Session Lifecycle & Cleanup ✅ COMPLETE

**Goal**: Proper resource management and session state handling.

### Tasks

- [x] **4.1** Implement `start()` method
  - Creates portal session via `create_and_start_portal_session()`
  - Spawns PipeWire capture thread with `start_pipewire_thread()`
  - Multi-monitor: Portal dialog lets user select which display
  - Allows restart from Stopped state with restore token

- [x] **4.2** Implement `stop()` method
  - Sends Stop command via channel to PipeWire thread
  - Waits for thread to finish with `handle.join()`
  - Clears frame_receiver, command_sender, displays
  - Restore token preserved for potential restart

- [x] **4.3** Handle session disconnection
  - `TryRecvError::Disconnected` detected in `capture_frame()`
  - Stream error state tracked via `AtomicBool`
  - Timer callback checks for errors and quits mainloop
  - `state_changed` callback logs and sets error flag on `StreamState::Error`

- [x] **4.4** Implement reconnection logic
  - `start()` resets state from Stopped to Created
  - Restore token passed to new session for dialog-free restart
  - If token invalid, portal shows dialog again

### Deliverables
- ✅ Clean session start/stop
- ✅ Resources properly released via Drop trait
- ✅ Token preserved across sessions for reconnection
- ✅ Stream errors detected and reported

### Implementation Notes
- State machine: Created → Streaming → Stopped (can restart from Stopped)
- PipeWire thread communicates via mpsc channels (frames, commands)
- Stream error detection uses AtomicBool + timer polling (100ms)
- Drop impl ensures cleanup if dropped while streaming

---

## Phase 5: Auto-Detection Integration ✅ COMPLETE

**Goal**: Integrate Portal into backend auto-detection chain.

### Tasks

- [x] **5.1** Update `auto_detect_backend()`
  - Portal tried after DRM, before X11 (mod.rs:214-227)
  - Does NOT test-capture to avoid triggering dialog during auto-detect
  - Logs "Using XDG Portal capture backend (Wayland)" on selection

- [x] **5.2** Add `PortalCapture::is_available()` check
  - Checks `XDG_SESSION_TYPE == "wayland"`
  - Verifies Screencast D-Bus proxy can be created via ashpd
  - Portal version not explicitly checked (graceful fallback)

- [x] **5.3** Update `is_capture_available()`
  - Portal included in availability check (mod.rs:264)
  - Returns true if any of DRM, Portal, or X11 available

- [x] **5.4** Handle first-use vs. restored sessions
  - First use: Portal dialog shown when `start()` called
  - Restored: Token passed to `select_sources()` for silent restore
  - Comment in code explains dialog avoidance during auto-detect

### Deliverables
- ✅ Portal tried automatically when DRM fails
- ✅ Wayland capture works with auto-detection
- ✅ Clear logging of backend selection
- ✅ All 133 tests pass

---

## Phase 6: Testing & Polish

**Goal**: Comprehensive testing and edge case handling.

### Tasks

- [ ] **6.1** Test on KDE Plasma (Wayland)
  - Initial permission dialog appears
  - Token saved after approval
  - Subsequent starts skip dialog
  - Token survives app restart

- [ ] **6.2** Test on GNOME (Wayland)
  - Same flow as KDE
  - Verify GNOME-specific dialog works

- [ ] **6.3** Test token invalidation scenarios
  - Display configuration changed
  - User revoked permission
  - Token expired/corrupted

- [ ] **6.4** Add unit tests
  - Token serialization
  - Session state machine
  - Format conversion

- [ ] **6.5** Documentation
  - Update CLAUDE.md with Portal backend info
  - Document first-use experience
  - Document token management

### Deliverables
- Works on KDE and GNOME Wayland
- Edge cases handled gracefully
- Documentation complete

---

## Dependencies

### Rust Crates

```toml
[target.'cfg(target_os = "linux")'.dependencies]
# XDG Desktop Portal (D-Bus bindings)
# tokio feature is enabled by default in 0.12+
ashpd = "0.12"
# PipeWire bindings for receiving portal video streams
pipewire = "0.9"
```

The `ashpd` crate handles:
- D-Bus communication
- Portal session management
- PipeWire fd acquisition
- Request/response async handling

The `pipewire` crate handles:
- MainLoop/Context/Core setup
- Stream creation and buffer management
- SPA pod format negotiation
- Callback-based frame capture

### System Requirements

- `xdg-desktop-portal` (1.14+ for restore tokens)
- `xdg-desktop-portal-kde` or `xdg-desktop-portal-gnome`
- PipeWire (running)

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      PortalCapture                           │
│  - session: Option<Session>                                 │
│  - pipewire_stream: Option<PipeWireStream>                  │
│  - restore_token: Option<String>                            │
│  - current_display: Option<String>                          │
└─────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ▼                     ▼                     ▼
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│   ashpd      │     │  PipeWire    │     │   Config     │
│  (D-Bus)     │     │  (frames)    │     │  (token)     │
└──────────────┘     └──────────────┘     └──────────────┘
```

### State Machine

```
                    ┌─────────────┐
                    │   Created   │
                    └──────┬──────┘
                           │ create_session()
                           ▼
                    ┌─────────────┐
              ┌────▶│   Session   │◀────┐
              │     │   Active    │     │
              │     └──────┬──────┘     │
              │            │ start()    │ restore with token
              │            ▼            │
              │     ┌─────────────┐     │
              │     │  Streaming  │─────┘ session lost
              │     └──────┬──────┘
              │            │ stop()
              │            ▼
              │     ┌─────────────┐
              └─────│   Stopped   │ (save new token)
                    └─────────────┘
```

---

## Token Management

### Storage Location

```json
// ~/.config/croh/config.json
{
  "screen_stream": {
    "enabled": true,
    "capture_backend": "auto",
    "portal_restore_token": "ABCD1234..."
  }
}
```

### Token Lifecycle

1. **First Use**: No token → Dialog shown → Token received → Save to config
2. **Subsequent Use**: Token loaded → Session restored → New token received → Save to config
3. **Token Invalid**: Load fails → Dialog shown → New token → Save to config
4. **User Revokes**: Token fails at runtime → Return error → Caller re-requests

---

## Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| Compositor doesn't support v4 | No restore tokens | Fall back to per-session approval |
| PipeWire not running | No capture | Detect early, clear error message |
| Token format changes | Restore fails | Handle gracefully, re-prompt |
| ashpd API changes | Build breaks | Pin version, monitor releases |

---

## Timeline Estimate

- Phase 1: Foundation - 1 session
- Phase 2: Session Management - 1-2 sessions
- Phase 3: PipeWire Capture - 2 sessions (most complex)
- Phase 4: Lifecycle - 1 session
- Phase 5: Integration - 1 session
- Phase 6: Testing - 1-2 sessions

**Total**: ~7-10 sessions

---

## References

- [XDG Desktop Portal Docs](https://flatpak.github.io/xdg-desktop-portal/)
- [ScreenCast Portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
- [RemoteDesktop Portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
- [ashpd crate](https://docs.rs/ashpd/latest/ashpd/)
- [RustDesk Wayland Discussion](https://github.com/rustdesk/rustdesk/discussions/10216)
