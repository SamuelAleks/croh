# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Status

**Pre-alpha development** - This project has no users yet. Breaking changes to APIs, package names, config paths, and protocols are acceptable and expected during this phase. Do not hesitate to make large refactors or renames when they improve the codebase.

## Build Commands

```bash
# Build all crates (debug)
cargo build

# Build release binaries
cargo build --release

# Run GUI
cargo run -p croh

# Run daemon
cargo run -p croh-daemon -- <subcommand>

# Run with debug logging
RUST_LOG=debug cargo run -p croh
```

## Testing

```bash
# Run all unit tests (fast, no network required)
cargo test -p croh-core --lib

# Run integration tests
cargo test -p croh-core --test iroh_integration

# Run with relay testing (comprehensive, slower)
cargo test -p croh-core --features test-relay

# Run network-dependent tests (may fail in CI)
cargo test -p croh-core -- --ignored

# Run with debug logging
RUST_LOG=croh_core=debug cargo test -p croh-core
```

### Iroh Test Infrastructure

The `crates/core/src/iroh/test_support.rs` module provides testing utilities:

- **`TestEndpoint`**: Relay-disabled endpoint wrapper for local testing
- **`EndpointPair`**: Pre-configured pair of endpoints for bidirectional tests
- **`TestFixtures`**: Temporary directories and test file utilities

For tests requiring actual peer-to-peer connectivity, enable the `test-relay` feature which provides a local relay server.

## Architecture

This is a Rust workspace wrapping the external [croc](https://github.com/schollz/croc) CLI tool with a native desktop GUI and headless daemon.

### Crate Structure

- **`crates/core`** (`croh-core`): Shared library used by both GUI and daemon
  - `croc/` - Subprocess wrapper: spawns croc CLI, parses stdout/stderr for events (codes, progress, completion)
  - `config.rs` - JSON config management (platform-specific paths via `dirs` crate)
  - `transfer.rs` - Transfer state machine and `TransferManager` for tracking active transfers
  - `iroh/` - Peer-to-peer networking via Iroh (see below)

- **`crates/gui`** (`croh`): Slint-based desktop app
  - `main.rs` - Window setup, Slint module inclusion via `slint::include_modules!()`
  - `app.rs` - All application state and UI callbacks; spawns threads with tokio runtimes for async operations
  - `ui/main.slint` - UI definition

- **`crates/daemon`** (`croh-daemon`): Headless CLI service using clap
  - Subcommands: `run`, `receive`, `status`, `peers`, `config`

### Key Patterns

**Croc Integration**: The `CrocProcess` struct (`core/src/croc/process.rs`) spawns croc as a child process, monitors stdout/stderr via tokio tasks, and emits `CrocEvent` variants through mpsc channels. The `output.rs` module contains regex parsers for croc's output format.

**GUI Threading**: Slint requires UI updates on the main thread. The app spawns `std::thread` with embedded tokio runtimes for async work, then uses `Weak<MainWindow>` to post updates back to UI.

**Config Locations**:
- Linux: `~/.config/croh/config.json`
- Windows: `%APPDATA%\croh\config.json`

### Iroh Integration

The `iroh/` module provides peer-to-peer networking for trusted peer connections:

**Module Structure:**
- `identity.rs` - Keypair generation and persistence (SecretKey management)
- `endpoint.rs` - `IrohEndpoint` wrapper with connection lifecycle management
- `protocol.rs` - `ControlMessage` definitions (JSON length-prefixed, 1MB max)
- `handshake.rs` - Trust establishment protocol
- `transfer.rs` - Push/pull file transfer with chunk streaming
- `blobs.rs` - BLAKE3 file hashing and verification
- `browse.rs` - Secure directory browsing with path validation
- `test_support.rs` - Testing utilities (cfg(test) only)

**Trust Flow:**
1. Sender creates `TrustBundle` with endpoint ID, relay URL, nonce, capabilities
2. Bundle sent to receiver via croc (JSON file)
3. Receiver connects to sender via Iroh, sends `TrustConfirm` with nonce
4. Sender verifies nonce and sends `TrustComplete`
5. Both sides store `TrustedPeer` with mutual permissions

**Testing:**
- Unit tests in each module (run with `cargo test --lib`)
- Integration tests in `tests/iroh_integration.rs`
- Test utilities in `test_support.rs` for relay-disabled endpoint testing
- Feature `test-relay` enables local relay server for comprehensive testing

### Screen Streaming

The `screen/` module provides remote desktop/screen sharing functionality:

**Module Structure:**
- `types.rs` - Core types: `Display`, `CapturedFrame`, `PixelFormat`, `ScreenCapture` trait
- `drm.rs` - Linux DRM/KMS capture backend (requires `CAP_SYS_ADMIN`)
- `portal.rs` - Linux XDG Portal capture backend for Wayland (see below)
- `x11.rs` - Linux X11 SHM capture backend (fallback for X11 sessions)
- `dxgi.rs` - Windows DXGI Desktop Duplication backend
- `encoder.rs` - Frame encoders: Raw, PNG, Zstd (with `FrameEncoder` trait)
- `decoder.rs` - Frame decoders: Raw, PNG, Zstd, Auto-detect (with `FrameDecoder` trait)
- `session.rs` - `StreamSession`, `StreamState`, `StreamStats`
- `events.rs` - `StreamEvent`, `StreamCommand`, `RemoteInputEvent`
- `manager.rs` - `ScreenStreamManager` for session lifecycle
- `input.rs` - `InputInjector` trait, `SecureInputHandler` with rate limiting
- `uinput.rs` - Linux uinput backend for input injection
- `windows_input.rs` - Windows SendInput backend for input injection
- `viewer.rs` - Client-side `ScreenViewer`, `FrameBuffer`, `InputQueue`

**Permissions:**
- `screen_view` - Permission to view a peer's screen
- `screen_control` - Permission to control a peer's screen (mouse/keyboard)

**Protocol Messages:**
- `DisplayListRequest/Response` - Enumerate available displays
- `ScreenStreamRequest/Response` - Start streaming session
- `ScreenFrame` - Frame data with metadata
- `ScreenFrameAck` - Flow control acknowledgment
- `ScreenStreamAdjust` - Quality/FPS adjustment
- `ScreenStreamStop` - Stop streaming
- `ScreenInput` - Mouse/keyboard input events

**Setup (Linux):**
```bash
# For DRM capture (best performance, requires capabilities)
sudo setcap cap_sys_admin+ep ./target/release/croh-daemon

# For uinput (input injection)
sudo usermod -aG input $USER
# Create udev rule: /etc/udev/rules.d/99-uinput.rules
# KERNEL=="uinput", MODE="0660", GROUP="input"
```

**Portal Capture (Wayland):**

The `portal.rs` module implements screen capture via XDG Desktop Portal for Wayland compositors (KDE Plasma, GNOME). Key features:

- **No root required** - Works without CAP_SYS_ADMIN
- **Restore tokens** - After first approval, subsequent sessions skip the dialog
- **PipeWire integration** - Receives video frames via PipeWire stream

Architecture:
- Portal session created via `ashpd` crate (D-Bus bindings)
- PipeWire mainloop runs on dedicated thread
- Frames sent via mpsc channel to async context
- Format negotiation for BGRx, RGBx, BGRA, RGBA

Backend selection order (auto-detect):
1. DRM/KMS (if CAP_SYS_ADMIN available)
2. Portal (if Wayland + xdg-desktop-portal available)
3. X11 SHM (if X11 session)

Config storage for restore token:
```json
{
  "screen_stream": {
    "portal_restore_token": "..."
  }
}
```

## Reference Files

The `/misc` directory (gitignored) contains reference files that may be useful for development context.

**`misc/croh-v2-plan.md`** - Master planning document for the entire project. Contains:
- Migration plan (Part One): Phases 0.1-0.10 covering Python→Rust migration
- Feature implementation plan (Part Two): Phases 1-8 for Iroh integration, trust, file push/pull
- Protocol specifications, data models, security model, UI/UX designs
- Current progress: approximately Phase 0.10 level (testing & verification)
