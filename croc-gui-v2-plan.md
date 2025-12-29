# Croc GUI v2: Complete Project Plan

## Executive Summary

This document outlines the complete architecture and implementation plan for Croc GUI v2, a native desktop file transfer application built with **Slint** that combines:

- **Croc** (Go): Ad-hoc transfers with human-readable codes, backwards compatible with any croc client
- **Iroh** (Rust): Persistent trusted peer connections with NAT traversal, bidirectional file operations

The system enables both one-off file transfers (via croc) and persistent "trusted peer" relationships where files can be pushed, pulled, and browsed without requiring codes for each transfer.

**Native-first approach:** Uses Slint for cross-platform native UI (Windows/Linux). No web server, no browser dependency.

---

## Table of Contents

1. [Part One: Migration Plan (Python → Rust)](#part-one-migration-plan)
2. [Part Two: Feature Implementation Plan](#part-two-feature-implementation-plan)
3. [Architecture](#architecture)
4. [Protocol Specifications](#protocol-specifications)
5. [Data Models](#data-models)
6. [Security Model](#security-model)
7. [UI/UX Design](#uiux-design)
8. [Project Structure](#project-structure)
9. [Future Considerations](#future-considerations)

---

# Part One: Migration Plan

## Overview

The migration converts the existing Python/FastAPI prototype to a Rust foundation while maintaining feature parity. This creates the base for Iroh integration in Part Two.

## Current Python Implementation Analysis

### Existing Features

**Croc Send:**
- File selection via native picker
- Multiple file support
- Options: custom code, curve, hash, throttle, relay, no-local
- Progress display
- Code display for sharing

**Croc Receive:**
- Code entry
- Auto-accept (--yes flag)
- Progress display
- Open received files location

**Transfer Management:**
- Active transfer tracking
- Cancel transfer
- Transfer status updates
- Automatic cleanup (expired transfers)

**Infrastructure:**
- Cross-platform croc executable detection
- Graceful shutdown

### Reference Implementation

The existing Python/FastAPI prototype provides reference behavior. Key logic to port:

| Python Component | Description | Rust Equivalent |
|------------------|-------------|-----------------|
| `app.py` croc handling | Process spawn, output parsing | `core/src/croc/` |
| `app.py` transfer state | Status tracking, cleanup | `core/src/transfer.rs` |
| `*.service` | Systemd service | Keep, update ExecStart |
| `install.sh/ps1` | Installers | Update for Rust binary |

---

## Migration Phases

### Phase 0.1: Project Setup

**Goal:** Establish Rust workspace structure

**Tasks:**

```
□ Create Cargo workspace
    croc-gui/
    ├── Cargo.toml (workspace)
    ├── crates/
    │   ├── core/
    │   ├── gui/
    │   └── daemon/

□ Set up dependencies in core/Cargo.toml:
    tokio (async runtime)
    serde / serde_json (serialization)
    thiserror (error handling)
    tracing (logging)
    
□ Set up dependencies in gui/Cargo.toml:
    slint (UI framework)
    tokio (async runtime)
    
□ Create basic main.rs that opens Slint window
□ Verify builds on Windows, Linux
```

**Exit Criteria:** `cargo build` succeeds, empty Slint window opens

---

### Phase 0.2: Croc Subprocess Wrapper

**Goal:** Replicate croc process management from Python

**Tasks:**

```
□ core/src/croc.rs - Croc executable management
    □ find_croc_executable() - Cross-platform detection
        - Check CROC_PATH env
        - Check PATH
        - Windows: Check common install locations
            - %LOCALAPPDATA%\croc\croc.exe
            - Scoop, Chocolatey, Program Files
        - Cache result at startup
    
    □ CrocOptions struct
        - code: Option<String>
        - curve: Option<Curve>  // enum: P256, P384, P521, Siec
        - hash: Option<Hash>    // enum: Xxhash, Imohash, Md5
        - throttle: Option<String>
        - relay: Option<String>
        - no_local: bool
        - to_args() -> Vec<String>
    
    □ validate_croc_code(code: &str) -> bool
        - Pattern: alphanumeric with 2+ hyphens
    
    □ CrocProcess struct
        - Wraps tokio::process::Child
        - stdout/stderr capture
        - Progress parsing
        - Graceful termination

□ core/src/croc_output.rs - Output parsing
    □ parse_progress(line: &str) -> Option<Progress>
        - Extract percentage (regex: \d+(?:\.\d+)?\s*%)
        - Extract speed (regex: \d+(?:\.\d+)?\s*[KMG]?B/s)
    
    □ parse_code(line: &str) -> Option<String>
        - Pattern: "Code is: <code>"
    
    □ detect_completion(line: &str) -> bool
        - "file(s) sent" or "file(s) received"
    
    □ detect_error(line: &str) -> Option<String>
        - Error patterns from Python implementation
```

**Exit Criteria:** Can spawn croc, capture output, parse progress

---

### Phase 0.3: Transfer State Management

**Goal:** Replicate transfer tracking from Python

**Tasks:**

```
□ core/src/transfer.rs - Transfer model
    □ TransferId (newtype: String)
    
    □ TransferType enum
        - Send
        - Receive
    
    □ TransferStatus enum
        - Pending
        - Running
        - Completed
        - Failed
        - Cancelled
    
    □ Transfer struct
        - id: TransferId
        - transfer_type: TransferType
        - status: TransferStatus
        - code: Option<String>
        - files: Vec<String>
        - progress: f64
        - speed: String
        - error: Option<String>
        - started_at: Instant
        - total_size: u64
        - transferred: u64
        - options: CrocOptions

□ core/src/transfer_manager.rs - Transfer orchestration
    □ TransferManager struct
        - transfers: HashMap<TransferId, Transfer>
        - Active process handles
    
    □ Methods:
        - create_send(files, options) -> TransferId
        - create_receive(code, options) -> TransferId
        - get_status(id) -> Option<Transfer>
        - cancel(id) -> Result<()>
        - cleanup_expired()
    
    □ Background task for periodic cleanup
```

**Exit Criteria:** Can create, track, and clean up transfers

---

### Phase 0.4: File Handling

**Goal:** Replicate file upload/download handling

**Tasks:**

```
□ core/src/files.rs - File utilities
    □ secure_filename(name: &str) -> String
        - Strip path components
        - Remove dangerous characters
        - Handle empty/dot-only names
    
    □ get_unique_filepath(dir: &Path, name: &str) -> PathBuf
        - Add _1, _2, etc. for duplicates
    
    □ get_upload_dir() -> PathBuf
        - Linux: /var/lib/croc-gui (fallback: temp)
        - Windows: %LOCALAPPDATA%\croc-gui
        - macOS: ~/Library/Application Support/croc-gui
    
    □ cleanup_directory(path: &Path) -> Result<()>

□ gui/src/api/upload.rs - Multipart handling
    □ Handle multipart file upload
    □ Save to transfer directory
    □ Return file paths for croc
```

**Exit Criteria:** Can receive file uploads, save securely, clean up

---

### Phase 0.5: HTTP API

**Goal:** Create Slint UI and wire up to core

**Tasks:**

```
□ gui/ui/main.slint - Main window layout
    □ Tab bar: Send | Receive | Peers
    □ Active transfers list (bottom panel)
    □ Basic styling

□ gui/ui/send.slint - Send panel
    □ File picker button / drop zone placeholder
    □ Selected files list with remove button
    □ Options expander (code, relay, etc.)
    □ Send button

□ gui/ui/receive.slint - Receive panel
    □ Code input field
    □ Receive button
    □ Options expander

□ gui/src/bridge.rs - Slint ↔ Core bindings
    □ Define Slint structs matching core models
    □ Conversion traits (From/Into)

□ gui/src/callbacks.rs - UI action handlers
    □ on_send_clicked → core::create_send()
    □ on_receive_clicked → core::create_receive()
    □ on_cancel_clicked → core::cancel_transfer()
    □ on_open_folder_clicked → platform::open_in_explorer()
```

**Exit Criteria:** Can send/receive files via native UI, progress updates shown

---

### Phase 0.6: Transfer Progress & Updates

**Goal:** Real-time transfer updates in UI

**Tasks:**

```
□ gui/src/app.rs - App state management
    □ Hold TransferManager
    □ Tokio channel for progress updates
    □ Slint ModelRc for transfers list
    
□ Progress flow:
    □ Core emits progress events via channel
    □ App receives, updates Slint model
    □ UI automatically re-renders
    
□ gui/ui/transfers.slint - Transfer list component
    □ Progress bar per transfer
    □ Speed display
    □ Cancel button
    □ Status indicator (pending/running/complete/failed)
```

**Exit Criteria:** Live progress bars, cancel works, completion shown

---

### Phase 0.7: File Selection & Native Dialogs

**Goal:** Native file picker integration

**Tasks:**

```
□ Add rfd (Rusty File Dialogs) dependency

□ gui/src/callbacks.rs - File picker
    □ on_browse_files() → rfd::FileDialog
    □ Return selected paths to Slint
    □ Update selected files model
    
□ gui/ui/send.slint - Enhance
    □ Show file names, sizes
    □ Remove individual files
    □ Clear all button
    
□ Open received files location
    □ platform::open_in_explorer() on complete
```

**Exit Criteria:** Native file picker works, can select multiple files

---

### Phase 0.8: Configuration & Environment

**Goal:** App configuration and settings persistence

**Tasks:**

```
□ core/src/config.rs - Configuration
    □ Config struct:
        - download_dir: PathBuf
        - default_relay: Option<String>
        - theme: Theme (system/light/dark)
    
    □ Load from environment:
        - CROC_PATH
        - CROC_GUI_DOWNLOAD_DIR
    
    □ Load from config file:
        - Linux: ~/.config/croc-gui/config.toml
        - Windows: %APPDATA%\croc-gui\config.toml

□ gui/ui/settings.slint - Settings panel
    □ Download directory picker
    □ Default relay input
    □ Theme selector
```

**Exit Criteria:** Settings persist across app restarts

---

### Phase 0.9: Service Integration

**Goal:** Run daemon as system service

**Tasks:**

```
□ Graceful shutdown handling
    □ SIGTERM/SIGINT handling
    □ Cancel active transfers
    □ Clean up temp files

□ Update service files
    □ croc-gui.service (Linux)
        - Update ExecStart path
    
    □ Windows service wrapper
        - Consider windows-service crate
        - Or keep existing NSSM approach

□ Update install scripts
    □ install.sh - Build or download binary
    □ install.ps1 - Windows equivalent
```

**Exit Criteria:** Installs and runs as service on all platforms

---

### Phase 0.10: Testing & Verification

**Goal:** Verify core functionality works

**Tasks:**

```
□ Manual testing checklist:
    □ Send single file
    □ Send multiple files
    □ Receive with code
    □ Cancel mid-transfer
    □ Custom options (relay, curve, hash)
    □ Progress updates
    □ Error handling
    □ Open received files folder
    □ Concurrent transfers
    □ Large file handling
    □ Unicode filenames

□ Cross-platform testing:
    □ Windows 10/11
    □ Ubuntu 22.04/24.04

□ Integration with standard croc client:
    □ GUI send → croc CLI receive
    □ croc CLI send → GUI receive
```

**Exit Criteria:** Core croc functionality works reliably

---

## Phase Summary

| Phase | Description | Duration |
|-------|-------------|----------|
| 0.1 | Project Setup | 1-2 days |
| 0.2 | Croc Subprocess Wrapper | 2-3 days |
| 0.3 | Transfer State Management | 1-2 days |
| 0.4 | File Handling | 1 day |
| 0.5 | Slint UI & Core Bindings | 3-4 days |
| 0.6 | Transfer Progress & Updates | 2 days |
| 0.7 | File Selection & Native Dialogs | 1-2 days |
| 0.8 | Configuration & Settings | 1 day |
| 0.9 | Service Integration (daemon) | 1-2 days |
| 0.10 | Testing & Verification | 2-3 days |

**Total Estimated:** 2-3 weeks

**Deliverable:** Native Slint app with croc send/receive  
**Ready for:** Part Two (Iroh Integration)

---

# Part Two: Feature Implementation Plan

## Phase 1: Iroh Integration

**Goal:** Add Iroh endpoint to core, establish basic connectivity

**Duration:** 1-2 weeks

### Tasks

```
□ Add Iroh dependencies to core/Cargo.toml:
    iroh = "0.x"
    iroh-blobs = "0.x"
    iroh-base = "0.x"

□ core/src/iroh/mod.rs - Module structure
    □ endpoint.rs - Endpoint management
    □ protocol.rs - Message types
    □ connection.rs - Connection handling

□ core/src/iroh/endpoint.rs
    □ IrohEndpoint struct
        - endpoint: iroh::Endpoint
        - router: iroh::Router (for protocol handlers)
    
    □ IrohEndpoint::new() -> Self
        - Bind to random port
        - Enable mDNS discovery
        - Configure relays (n0 defaults)
    
    □ IrohEndpoint::endpoint_id() -> EndpointId
    
    □ IrohEndpoint::connect(remote: EndpointId) -> Connection
    
    □ IrohEndpoint::accept() -> Stream<Connection>
    
    □ Lifecycle management (start, stop, restart)

□ core/src/iroh/identity.rs
    □ Load or generate keypair
    □ Persist to:
        - Linux: ~/.local/share/croc-gui/identity.json
        - Windows: %LOCALAPPDATA%\croc-gui\identity.json
        - macOS: ~/Library/Application Support/croc-gui/identity.json
    
    □ Identity file format:
        {
          "endpoint_id": "...",
          "private_key": "...",  // Encrypted or plaintext
          "created_at": "..."
        }

□ gui/src/state.rs
    □ Add IrohEndpoint to AppState
    □ Start Iroh on server startup
    □ Shutdown Iroh on server shutdown

□ gui/src/api/iroh.rs
    □ GET /api/iroh/status
        - endpoint_id
        - is_running
        - connection_count
        - relay_status

□ Slint UI updates
    □ Display EndpointId somewhere (settings? header?)
    □ Connection status indicator
```

### Exit Criteria

- Iroh endpoint starts with server
- EndpointId persists across restarts  
- Can see EndpointId in UI
- Two instances can connect by EndpointId (verified via logs)

---

## Phase 2: Trust Establishment

**Goal:** Croc bootstrap → Iroh confirmation handshake

**Duration:** 1-2 weeks

### Trust Flow Diagram

```
┌──────────────────┐                              ┌──────────────────┐
│       GUI        │                              │      Daemon      │
│   (initiator)    │                              │    (acceptor)    │
└────────┬─────────┘                              └────────┬─────────┘
         │                                                  │
         │  1. Generate trust bundle                        │
         │     {                                            │
         │       "croc_gui_trust": 1,                      │
         │       "sender": { endpoint_id, name },          │
         │       "nonce": "..."                            │
         │     }                                            │
         │                                                  │
         │  2. croc send trust-bundle.json                  │
         │     "7-alpha-beta-gamma"                         │
         │─────────────────────────────────────────────────►│
         │                                                  │
         │                    3. User runs:                 │
         │                       daemon receive 7-alpha...  │
         │                       File saved to inbox/       │
         │                       Daemon detects bundle      │
         │                                                  │
         │  4. Iroh connect (using EndpointId from bundle)  │
         │◄─────────────────────────────────────────────────│
         │                                                  │
         │  5. trust_confirm (Iroh stream)                  │
         │     {                                            │
         │       "type": "trust_confirm",                   │
         │       "peer": { endpoint_id, name, os, ... },   │
         │       "nonce": "...",                           │
         │       "permissions": { push, pull, browse }     │
         │     }                                            │
         │◄─────────────────────────────────────────────────│
         │                                                  │
         │  6. trust_confirm (response)                     │
         │     {                                            │
         │       "type": "trust_confirm",                   │
         │       "peer": { ... },                          │
         │       "permissions": { ... }                    │
         │     }                                            │
         │─────────────────────────────────────────────────►│
         │                                                  │
         │  7. trust_complete                               │
         │     { "type": "trust_complete" }                 │
         │◄─────────────────────────────────────────────────│
         │                                                  │
         │  ═══════════ TRUST ESTABLISHED ═══════════       │
         │                                                  │
```

### Tasks

```
□ core/src/trust.rs - Trust bundle
    □ TrustBundle struct
        - version: u32
        - sender: PeerInfo
        - capabilities_offered: Vec<Capability>
        - relays: Vec<String>
        - created_at: DateTime
        - expires_at: DateTime
        - nonce: String
    
    □ TrustBundle::new(endpoint: &IrohEndpoint) -> Self
    □ TrustBundle::is_valid() -> bool (not expired)
    □ TrustBundle::save(path: &Path) -> Result<()>
    □ TrustBundle::load(path: &Path) -> Result<Self>
    □ Detect if file is trust bundle (by content)

□ core/src/iroh/protocol.rs - Control messages
    □ ALPN: b"croc-gui/control/1"
    
    □ ControlMessage enum (serde tagged):
        - TrustConfirm { peer, nonce, permissions }
        - TrustComplete
        - TrustRevoke { reason }
        - Ping
        - Pong
        ... (more added in later phases)
    
    □ Send/receive helpers:
        - send_message(stream, msg) -> Result<()>
        - recv_message(stream) -> Result<ControlMessage>
    
    □ Message framing:
        - Length-prefixed JSON
        - Or newline-delimited JSON

□ core/src/peers.rs - Trusted peer model
    □ TrustedPeer struct
        - id: String (local UUID)
        - endpoint_id: EndpointId
        - name: String
        - added_at: DateTime
        - last_seen: DateTime
        - permissions_granted: Permissions
        - their_permissions: Permissions
        - allowed_paths: Vec<PathBuf>
        - connection_quality: Option<ConnectionQuality>
        - os: Option<String>
        - free_space: Option<u64>
    
    □ Permissions struct
        - push: bool
        - pull: bool
        - browse: bool
        - status: bool
    
    □ PeerStore
        - Load/save to peers.json
        - CRUD operations
        - Query by endpoint_id

□ core/src/trust_handler.rs - Trust protocol handler
    □ Handle incoming trust_confirm
    □ Send trust_confirm response
    □ Create TrustedPeer on success
    □ Emit events for UI

□ daemon/src/inbox.rs - Inbox processing
    □ Get inbox directory path
    □ Process trust bundle file
    □ Trigger Iroh connect-back
    □ Delete bundle after processing

□ daemon/src/commands/receive.rs
    □ `daemon receive <code>` command
    □ Spawn croc with --yes
    □ Save to inbox directory
    □ Invoke inbox processing

□ gui/src/api/trust.rs
    □ POST /api/trust/initiate
        - Generate trust bundle
        - Start croc send
        - Return { transfer_id, code }
    
    □ GET /api/trust/pending
        - List pending trust handshakes
    
    □ Slint UI updates for trust completion

□ Slint UI: Add Trusted Peer flow
    □ "Add Trusted Peer" button
    □ Display croc code
    □ Show waiting state
    □ Confirmation on success
```

### Exit Criteria

- Can initiate trust from GUI
- Daemon receives bundle via croc
- Iroh handshake completes
- Both sides persist TrustedPeer
- UI shows new peer

---

## Phase 3: Status & Presence

**Goal:** Real-time peer status

**Duration:** 1 week

### Tasks

```
□ core/src/iroh/protocol.rs - Add messages
    □ StatusRequest
    □ StatusResponse
        - hostname: String
        - os: String
        - free_space: u64
        - download_dir: String
        - uptime: u64
        - daemon_version: String
        - active_transfers: u32

□ core/src/status.rs - System info gathering
    □ get_hostname() -> String
    □ get_os_info() -> String
    □ get_free_space(path: &Path) -> u64
    □ get_uptime() -> Duration
    □ Aggregate into StatusInfo struct

□ core/src/peer_connection.rs - Connection management
    □ PeerConnection struct
        - peer_id: String
        - endpoint_id: EndpointId
        - connection: Option<Connection>
        - last_seen: Instant
        - quality: ConnectionQuality
    
    □ ConnectionQuality enum
        - Direct
        - Relay
        - Disconnected
    
    □ PeerConnectionManager
        - Track all peer connections
        - Auto-reconnect logic
        - Periodic ping/pong
        - Status polling

□ gui/src/api/peers.rs
    □ GET /api/peers
        - List all trusted peers with status
    
    □ GET /api/peers/:id
        - Single peer details
    
    □ DELETE /api/peers/:id
        - Remove trust (send trust_revoke first)
    
    □ PATCH /api/peers/:id
        - Update name, permissions, allowed_paths

□ Slint UI updates
    □ peer_online { peer_id, status }
    □ peer_offline { peer_id }
    □ peer_status_update { peer_id, free_space, ... }

□ Slint UI: Peers list
    □ Show online/offline indicator
    □ Show free space, OS
    □ Show connection quality (direct/relay)
    □ Last seen timestamp
    □ Auto-refresh
```

### Exit Criteria

- Peers list shows live status
- Online/offline detection works
- Free space and system info displayed
- Reconnects automatically after disconnect

---

## Phase 4: File Push

**Goal:** Send files to trusted peers via Iroh

**Duration:** 1-2 weeks

### Tasks

```
□ Add iroh-blobs integration
    □ core/src/blobs.rs
        - Wrapper around iroh_blobs::Blobs
        - add_file(path) -> Hash
        - download(hash, endpoint_id) -> Result<PathBuf>
        - Progress events

□ core/src/iroh/protocol.rs - Add messages
    □ PushIntent
        - transfer_id: String
        - files: Vec<FileInfo>
        - total_size: u64
    
    □ FileInfo
        - name: String
        - size: u64
        - hash: Hash (blake3)
    
    □ PushAccept
        - transfer_id: String
        - resume_from: HashMap<String, u64>  // file -> bytes already have
    
    □ PushReject
        - transfer_id: String
        - reason: String
    
    □ PushProgress
        - transfer_id: String
        - file: String
        - bytes_transferred: u64
    
    □ PushComplete
        - transfer_id: String
        - status: String
        - files_received: Vec<String>

□ core/src/transfer.rs - Extend for Iroh transfers
    □ TransferType::IrohPush
    □ TransferType::IrohPull
    □ Add peer_id field

□ core/src/push.rs - Push logic
    □ initiate_push(peer_id, files) -> TransferId
        - Add files to blobs store
        - Get hashes
        - Send PushIntent
        - Wait for PushAccept
        - iroh-blobs handles transfer
        - Track progress
        - Handle completion

    □ handle_push_intent(intent) -> Result<()>
        - Check permissions
        - Send PushAccept or PushReject
        - Receive via iroh-blobs
        - Save to download_dir
        - Send PushComplete

□ gui/src/api/push.rs
    □ POST /api/push/:peer_id
        - Multipart file upload
        - Initiate push to peer
        - Return transfer_id
    
    □ Progress via existing WebSocket

□ daemon/src/push_handler.rs
    □ Accept incoming pushes
    □ Save to configured download_dir
    □ Desktop notification (optional)

□ Slint UI: Send to Peer
    □ File selection (existing)
    □ Peer selection dropdown/list
    □ "Send to Peer" button
    □ Progress display
    □ Completion notification
```

### Exit Criteria

- Can select files and send to online peer
- No croc code needed
- Progress shown in real-time
- Files appear in peer's download directory
- Works through NAT (via relay if needed)

---

## Phase 5: Permissions System

**Goal:** Configurable per-peer permissions

**Duration:** 1 week

### Tasks

```
□ core/src/permissions.rs - Permission logic
    □ Permission enum
        - Push
        - Pull
        - Browse
        - Status
    
    □ check_permission(peer_id, permission) -> bool
    
    □ Default permissions (all true)

□ core/src/iroh/protocol.rs - Add messages
    □ PermissionUpdate
        - permissions: Permissions
    
    □ PermissionDenied
        - action: String
        - reason: String
        - can_request: bool
    
    □ PermissionRequest
        - action: String
        - message: Option<String>
    
    □ PermissionGrant
        - action: String
        - granted: bool

□ Enforcement points
    □ Push handler - check push permission
    □ Browse handler - check browse permission
    □ Pull handler - check pull permission
    □ Status handler - check status permission

□ gui/src/api/peers.rs - Permission endpoints
    □ PATCH /api/peers/:id/permissions
        - Update permissions for peer
        - Send PermissionUpdate to peer
    
    □ POST /api/peers/:id/request-permission
        - Send PermissionRequest
    
    □ POST /api/permission-requests/:id/respond
        - Grant or deny

□ Slint UI updates
    □ permission_denied { peer_id, action, can_request }
    □ permission_request { peer_id, action, message }
    □ permission_update { peer_id, permissions }

□ Slint UI: Permission management
    □ Per-peer permission toggles
    □ Permission denied notification with "Request" option
    □ Incoming permission request notification
    □ Accept/deny buttons
```

### Exit Criteria

- Can disable specific permissions per peer
- Denied actions show error with request option
- Permission requests delivered to other peer
- Granting updates permissions

---

## Phase 6: File Browsing

**Goal:** Browse remote filesystem

**Duration:** 1-2 weeks

### Tasks

```
□ core/src/iroh/protocol.rs - Add messages
    □ BrowseRequest
        - path: Option<String>  // None = list roots
        - show_hidden: bool
    
    □ BrowseResponse
        - path: String
        - entries: Vec<FileEntry>
        - can_write: bool
    
    □ FileEntry
        - name: String
        - entry_type: FileType  // File, Directory, Symlink
        - size: Option<u64>
        - modified: Option<DateTime>
    
    □ BrowseError
        - path: String
        - error: BrowseErrorKind  // NotFound, AccessDenied, NotDirectory

□ core/src/browse.rs - Browse logic
    □ browse_directory(path) -> Result<Vec<FileEntry>>
        - List directory contents
        - Get metadata (size, modified)
        - Handle permissions
    
    □ validate_path(path, allowed_paths) -> Result<PathBuf>
        - Path traversal prevention
        - Check against allowed_paths
    
    □ get_browsable_roots() -> Vec<PathBuf>
        - Return configured allowed_paths

□ daemon/src/browse_handler.rs
    □ Handle BrowseRequest
    □ Validate against allowed_paths
    □ Return BrowseResponse or BrowseError

□ gui/src/api/browse.rs
    □ GET /api/peers/:peer_id/browse?path=...
        - Send BrowseRequest
        - Return BrowseResponse
    
    □ GET /api/peers/:peer_id/roots
        - Get browsable root paths

□ Slint UI: File browser component
    □ Design for reuse (future file manager)
    □ Directory tree or breadcrumb navigation
    □ File list with icons, sizes, dates
    □ Click to navigate directories
    □ Selection support (single for now, multi later)
    □ Loading states
    □ Error handling (access denied, not found)
```

### Exit Criteria

- Can browse allowed directories on remote peer
- Navigation works (up, into subdirectory)
- File metadata displayed
- Errors handled gracefully
- Respects browse permission

---

## Phase 7: File Pull

**Goal:** Retrieve files from trusted peers

**Duration:** 1-2 weeks

### Tasks

```
□ core/src/iroh/protocol.rs - Add messages
    □ PullRequest
        - transfer_id: String
        - paths: Vec<String>
    
    □ PullAccept
        - transfer_id: String
        - files: Vec<FileInfo>  // With hashes
    
    □ PullReject
        - transfer_id: String
        - reason: String
        - failed_paths: Vec<String>
    
    □ PullComplete
        - transfer_id: String

□ core/src/pull.rs - Pull logic
    □ initiate_pull(peer_id, paths) -> TransferId
        - Send PullRequest
        - Wait for PullAccept
        - Download via iroh-blobs using hashes
        - Track progress
        - Send PullComplete

    □ handle_pull_request(request) -> Result<()>
        - Check permissions
        - Validate paths against allowed_paths
        - Add files to blobs store
        - Send PullAccept with hashes
        - iroh-blobs serves the data

□ Conflict resolution
    □ ConflictStrategy enum
        - Overwrite
        - Rename
        - Skip
    □ Apply during save

□ gui/src/api/pull.rs
    □ POST /api/pull/:peer_id
        - JSON: { paths: [...], conflict_strategy: "..." }
        - Return transfer_id

□ Slint UI: Pull integration
    □ In file browser: Select files → "Pull" button
    □ Confirm dialog (optional)
    □ Conflict resolution choice
    □ Progress display
    □ Completion notification
    □ Option to open containing folder
```

### Exit Criteria

- Can select files in browser and pull them
- Progress shown in real-time
- Conflict handling works
- Files saved to local download directory
- Respects pull permission and allowed_paths

---

## Phase 8: Polish & Reliability

**Goal:** Production readiness

**Duration:** 2-3 weeks

### Tasks

```
□ Transfer reliability
    □ Resume interrupted transfers (iroh-blobs)
    □ Verify file integrity after transfer
    □ Retry logic for transient failures
    □ Timeout handling

□ Connection reliability
    □ Robust reconnection with backoff
    □ Handle endpoint restarts gracefully
    □ Connection state recovery
    □ Detect and report stale connections

□ Error handling
    □ User-friendly error messages
    □ Error categorization (network, permission, file, etc.)
    □ Actionable error suggestions
    □ Error reporting to UI

□ Logging & diagnostics
    □ Structured logging (tracing)
    □ Log levels configuration
    □ Diagnostic commands:
        - daemon status
        - daemon peers
        - daemon logs

□ Cross-platform testing
    □ Windows 10/11
        - Service installation
        - File paths
        - Permissions
    □ Ubuntu 22.04/24.04
        - Systemd service
        - File paths
        - Permissions
    □ macOS (Intel + ARM)
        - LaunchAgent/LaunchDaemon
        - File paths
        - Permissions

□ Installers & packages
    □ Linux
        - .deb package
        - .rpm package
        - Install script (existing, updated)
    □ Windows
        - MSI installer
        - Or: Install script (existing, updated)
    □ macOS
        - .pkg installer
        - Or: Install script

□ Documentation
    □ README.md - Overview, quick start
    □ INSTALL.md - Detailed installation
    □ CONFIGURATION.md - All options
    □ PROTOCOL.md - Protocol specification
    □ TROUBLESHOOTING.md - Common issues

□ Trust management
    □ Trust revocation flow
    □ "Forget peer" cleanup
    □ Trust expiry (optional)
    □ Re-establish trust flow
```

### Exit Criteria

- No known critical bugs
- Works reliably on all platforms
- Easy to install and configure
- Comprehensive documentation
- Trust can be revoked and re-established

---

## Future Phases (Post-MVP)

### Phase 9: Full File Manager

```
□ Dual-pane interface (local | remote)
□ Drag-drop between panes
□ Multi-select
□ Context menus
    - Rename
    - Delete
    - New folder
    - Properties
□ Keyboard shortcuts
□ Search
□ Favorites/bookmarks
□ Preview pane (images, text, PDF)
```

### Phase 10: GUI ↔ GUI Trust

```
□ Symmetric trust establishment
□ Both sides can initiate
□ Mutual browsing/push/pull
□ UI adapts to peer type (GUI vs Daemon)
```

### Phase 11: Advanced Features

```
□ Watch folders (auto-sync on changes)
□ Remote terminal (PTY over Iroh)
□ Screen sharing (VNC proxy or WebRTC signaling)
□ Port forwarding
□ Clipboard sync
□ Chat/messaging between peers
```

### Phase 12: Mobile

```
□ PWA for status viewing
□ Native sender app (iOS/Android)
□ Iroh integration via FFI
```

---

# Architecture

## Component Overview

```
SHARED CORE (Rust)
├── Croc Wrapper
├── Iroh Endpoint
├── Protocol Handler
├── Blobs (Transfers)
├── Trust
├── Peers
├── Permissions
├── Browse
├── Transfer Manager
├── Config
└── Persistence
        │
        ├───────────────────────┬───────────────────────┐
        ▼                       ▼                       
   GUI (Slint)             DAEMON (Rust)           
   ├── Main Window         ├── CLI                 
   ├── Send/Receive        ├── Service Mode        
   ├── Peers View          └── Inbox Processor     
   └── File Browser        

EXTERNAL DEPENDENCIES
├── croc (Go CLI)
├── Iroh Relays (n0-hosted)
└── DNS Discovery (dns.iroh.link)
```

## Data Flow: Ad-hoc Transfer (Croc)

```
Sender (GUI)                                 Receiver (Any croc client)

Slint UI
  │
  │ 1. Select files via native dialog
  ▼
Core Library
  │
  │ 2. Copy to temp dir
  │ 3. Spawn croc process
  ▼
croc send ──────────────────────────────────────────► croc receive
  │              (via relay or direct)                     │
  │                                                        │
  │ 4. Parse output for code                               │
  │ 5. Stream progress to Slint via callback               │
  ▼                                                        ▼
UI shows code                                         Files saved
UI shows progress
```

## Data Flow: Trusted Push (Iroh)

```
Sender (GUI)                                 Receiver (Daemon)

Slint UI
  │
  │ 1. Select files + select peer
  ▼
Core Library
  │
  │ 2. Add files to iroh-blobs
  │ 3. Get BLAKE3 hashes
  ▼
Iroh Endpoint
  │
  │ 4. Connect to peer (via relay or direct)
  │ 5. Send PushIntent { files, hashes }
  ▼
═══════════════════ Iroh QUIC Stream ═══════════════════
                                                        │
                                                        ▼
                                                 Iroh Endpoint
                                                        │
                                           6. Check permissions
                                           7. Send PushAccept
                                                        │
═══════════════════ iroh-blobs transfer ════════════════
  │                                                     │
  │ 8. Transfer file data                               │
  │    (with progress events)                           │
  ▼                                                     ▼
UI shows progress                                Files saved to
                                                 download_dir
  │                                                     │
  │ 9. Receive PushComplete                             │
  ▼                                                     ▼
UI shows success                               Desktop notification
```

---

# Protocol Specifications

## ALPN Identifiers

| ALPN | Description |
|------|-------------|
| `croc-gui/control/1` | Control protocol (JSON messages) |
| `iroh-blobs` | File transfer (built into iroh-blobs) |

## Control Protocol

All messages are JSON, length-prefixed (4-byte big-endian length, then JSON bytes).

### Message Envelope

```json
{
  "type": "message_type",
  "payload": { ... }
}
```

### Trust Messages

**trust_confirm**
```json
{
  "type": "trust_confirm",
  "peer": {
    "endpoint_id": "un3p7i8ynct5kqhgp...",
    "name": "Living Room PC",
    "os": "Windows 11",
    "version": "1.0.0"
  },
  "nonce": "a1b2c3d4e5f6",
  "permissions": {
    "push": true,
    "pull": true,
    "browse": true,
    "status": true
  }
}
```

**trust_complete**
```json
{
  "type": "trust_complete"
}
```

**trust_revoke**
```json
{
  "type": "trust_revoke",
  "reason": "user_initiated"
}
```

### Status Messages

**status_request**
```json
{
  "type": "status_request"
}
```

**status_response**
```json
{
  "type": "status_response",
  "hostname": "living-room-pc",
  "os": "Windows 11 24H2",
  "free_space": 524288000000,
  "download_dir": "C:\\Users\\Alex\\Downloads",
  "uptime": 86400,
  "active_transfers": 0,
  "version": "1.0.0"
}
```

**ping / pong**
```json
{ "type": "ping", "timestamp": 1705312200 }
{ "type": "pong", "timestamp": 1705312200 }
```

### Permission Messages

**permission_update**
```json
{
  "type": "permission_update",
  "permissions": {
    "push": true,
    "pull": false,
    "browse": true,
    "status": true
  }
}
```

**permission_denied**
```json
{
  "type": "permission_denied",
  "action": "browse",
  "reason": "browse_disabled",
  "message": "Peer has disabled file browsing",
  "can_request": true
}
```

**permission_request**
```json
{
  "type": "permission_request",
  "action": "browse",
  "message": "Would like to browse your files"
}
```

**permission_grant**
```json
{
  "type": "permission_grant",
  "action": "browse",
  "granted": true
}
```

### File Browsing Messages

**browse_request**
```json
{
  "type": "browse_request",
  "path": "/home/alex/Documents",
  "show_hidden": false
}
```
*Note: path = null means "list browsable roots"*

**browse_response**
```json
{
  "type": "browse_response",
  "path": "/home/alex/Documents",
  "entries": [
    {
      "name": "Work",
      "entry_type": "directory",
      "size": null,
      "modified": "2025-01-10T08:30:00Z"
    },
    {
      "name": "report.pdf",
      "entry_type": "file",
      "size": 1048576,
      "modified": "2025-01-12T14:22:00Z"
    }
  ],
  "can_write": false
}
```

**browse_error**
```json
{
  "type": "browse_error",
  "path": "/root",
  "error": "access_denied"
}
```
*Error types: access_denied, not_found, not_directory, permission_denied*

### File Transfer Messages

**push_intent**
```json
{
  "type": "push_intent",
  "transfer_id": "abc123",
  "files": [
    { "name": "report.pdf", "size": 1048576, "hash": "blake3:a1b2c3d4..." },
    { "name": "data.csv", "size": 2097152, "hash": "blake3:e5f6a7b8..." }
  ],
  "total_size": 3145728
}
```

**push_accept**
```json
{
  "type": "push_accept",
  "transfer_id": "abc123",
  "resume_from": { "report.pdf": 524288 }
}
```

**push_reject**
```json
{
  "type": "push_reject",
  "transfer_id": "abc123",
  "reason": "permission_denied"
}
```

**push_progress**
```json
{
  "type": "push_progress",
  "transfer_id": "abc123",
  "file": "report.pdf",
  "bytes_transferred": 786432
}
```

**push_complete**
```json
{
  "type": "push_complete",
  "transfer_id": "abc123",
  "status": "success",
  "files_received": ["report.pdf", "data.csv"]
}
```

**pull_request**
```json
{
  "type": "pull_request",
  "transfer_id": "xyz789",
  "paths": [
    "/home/alex/Documents/report.pdf",
    "/home/alex/Documents/data.csv"
  ]
}
```

**pull_accept**
```json
{
  "type": "pull_accept",
  "transfer_id": "xyz789",
  "files": [
    {
      "path": "/home/alex/Documents/report.pdf",
      "name": "report.pdf",
      "size": 1048576,
      "hash": "blake3:a1b2c3d4..."
    }
  ]
}
```

**pull_reject**
```json
{
  "type": "pull_reject",
  "transfer_id": "xyz789",
  "reason": "permission_denied",
  "failed_paths": ["/home/alex/private/secret.txt"]
}
```

**pull_complete**
```json
{
  "type": "pull_complete",
  "transfer_id": "xyz789"
}
```

### Notification Messages

**notify**
```json
{
  "type": "notify",
  "level": "info",
  "title": "Transfer Complete",
  "message": "Received report.pdf from Alex's Desktop"
}
```
*Levels: info, warning, error*

## Trust Bundle Format (Croc Payload)

```json
{
  "croc_gui_trust": 1,
  "sender": {
    "endpoint_id": "un3p7i8ynct5kqhgp...",
    "name": "Alex's Desktop",
    "version": "1.0.0"
  },
  "capabilities_offered": ["push", "pull", "browse", "status"],
  "relays": ["https://relay.iroh.network"],
  "created_at": "2025-01-15T10:30:00Z",
  "expires_at": "2025-01-15T10:35:00Z",
  "nonce": "a1b2c3d4e5f6"
}
```

---

# Data Models

## Persistence Files

### Identity (identity.json)

```json
{
  "endpoint_id": "un3p7i8ynct5kqhgp...",
  "private_key": "base64-encoded-key",
  "name": "Alex's Desktop",
  "created_at": "2025-01-10T09:00:00Z"
}
```

Location:
- Linux: `~/.local/share/croc-gui/identity.json`
- Windows: `%LOCALAPPDATA%\croc-gui\identity.json`
- macOS: `~/Library/Application Support/croc-gui/identity.json`

### Trusted Peers (peers.json)

```json
{
  "peers": [
    {
      "id": "local-uuid-1",
      "endpoint_id": "abc123...",
      "name": "Living Room PC",
      "added_at": "2025-01-15T10:30:00Z",
      "last_seen": "2025-01-15T14:22:00Z",
      "permissions_granted": {
        "push": true,
        "pull": true,
        "browse": true,
        "status": true
      },
      "their_permissions": {
        "push": true,
        "pull": false,
        "browse": true,
        "status": true
      },
      "allowed_paths": [
        "/home/alex/Documents",
        "/home/alex/Downloads",
        "/home/alex/Shared"
      ],
      "notes": "Main media server"
    }
  ]
}
```

### Settings (settings.json)

```json
{
  "download_dir": "/home/alex/Downloads",
  "port": 8317,
  "croc_relay": null,
  "iroh_relays": [],
  "theme": "system",
  "notifications": true,
  "auto_accept_from_trusted": true,
  "show_hidden_files": false
}
```

### Daemon Config (daemon.json)

```json
{
  "name": "Living Room PC",
  "download_dir": "C:\\Users\\Alex\\Downloads",
  "browsable_paths": [
    "C:\\Users\\Alex\\Documents",
    "C:\\Users\\Alex\\Downloads",
    "C:\\Users\\Alex\\Shared"
  ],
  "sendable_paths": [
    "C:\\Users\\Alex\\Shared"
  ],
  "notifications": true
}
```

---

# Security Model

## Threat Model

### Trust Establishment Security

**Attack:** Intercept croc code and receive bundle instead of intended user

**Mitigations:**
- Croc code shared out-of-band (verbal, secure chat)
- Croc PAKE ensures only code-holder receives bundle
- Bundle expires quickly (5 minutes)
- Iroh handshake validates both EndpointIds
- Trust is explicit: victim notices they never got the bundle

**Residual risk:** Same as any croc transfer - code interception

### Ongoing Communication Security

**Attack:** Man-in-the-middle on Iroh connection

**Mitigations:**
- All Iroh traffic is TLS 1.3 encrypted (QUIC)
- EndpointId = public key, verified on every connection
- Relay cannot read traffic (E2E encrypted)

**Attack:** Impersonate a trusted peer

**Mitigations:**
- Must possess private key corresponding to EndpointId
- Connection rejected if EndpointId doesn't match

### File System Security

**Attack:** Path traversal to access files outside allowed paths

**Mitigations:**
- Canonical path resolution
- Strict prefix matching against allowed_paths
- Reject any path with .. components
- Symlink handling (don't follow outside allowed paths)

**Attack:** Write malicious files to sensitive locations

**Mitigations:**
- Downloads only to configured download_dir
- Filename sanitization
- No executable permissions set on downloaded files

### Revocation

- Either peer can revoke trust unilaterally
- Revoked EndpointId immediately blocked
- No central authority needed
- Revocation persists across restarts

---

# UI/UX Design

## Main Interface Layout

```
CROC WEBGUI                                                    [⚙ Settings]
───────────────────────────────────────────────────────────────────────────

  [ 📤 Send ]  [ 📥 Receive ]  [ 👥 Trusted Peers (3) ]

═══════════════════════════════════════════════════════════════════════════

[Active Tab Content]




───────────────────────────────────────────────────────────────────────────
Active Transfers
  ↑ report.pdf → Living Room PC          45%  ████████░░░░  [Cancel]
  ↓ photos.zip ← Work Laptop             78%  ██████████░░░ [Cancel]
```

## Send Tab

```
Send Files
───────────────────────────────────────────────────────────────────────────

┌───────────────────────────────────────────────────────────────────────┐
│                                                                       │
│                  Drop files here or click to browse                   │
│                                                                       │
└───────────────────────────────────────────────────────────────────────┘

Selected files:
  📄 report.pdf (1.2 MB)                                            [×]
  📄 data.csv (500 KB)                                              [×]

───────────────────────────────────────────────────────────────────────────

Send to:

  ○ Anyone (generate croc code)

  ● Trusted Peer:
    🟢 Living Room PC          487 GB free           [Select]
    🟢 Work Laptop             52 GB free            [Select]
    🔴 Old Desktop             offline

───────────────────────────────────────────────────────────────────────────

[Advanced Options ▼]

[ Send Files ]
```

## Trusted Peers Tab

```
Trusted Peers                                              [+ Add Peer]
───────────────────────────────────────────────────────────────────────────

┌─────────────────────────────────────────────────────────────────────┐
│  🟢 Living Room PC                                                  │
│     Windows 11 • 487 GB free • Direct connection • Now              │
│                                                                     │
│     [📁 Browse]  [📤 Send Files]  [📥 Pull Files]  [⋯]             │
└─────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────┐
│  🟢 Work Laptop                                                     │
│     Ubuntu 24.04 • 52 GB free • Via relay • 2 min ago               │
│                                                                     │
│     [📁 Browse]  [📤 Send Files]  [📥 Pull Files]  [⋯]             │
└─────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────┐
│  🔴 Old Desktop                                                     │
│     Last seen: 3 days ago                                           │
│                                                                     │
│     [⋯ Remove]                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

## Add Peer Flow

```
Add Trusted Peer                                                    [×]
───────────────────────────────────────────────────────────────────────────

Step 1: On the remote machine
─────────────────────────────────

If not installed, install the daemon:
  See: https://github.com/...

Then run:
┌───────────────────────────────────────────────────────────────────────┐
│  croc-daemon receive 7-alpha-beta-gamma                               │
│                                                      [Copy Command]   │
└───────────────────────────────────────────────────────────────────────┘

───────────────────────────────────────────────────────────────────────────

Step 2: Wait for confirmation
─────────────────────────────────

         ⏳ Waiting for peer to accept...

            Code expires in 4:32

───────────────────────────────────────────────────────────────────────────

[Cancel]
```

## File Browser

```
Browse: Living Room PC                                              [×]
───────────────────────────────────────────────────────────────────────────

📁 /home/alex/Documents                                          [↑ Up]

┌─────────────────────────────────────────────────────────────────────┐
│  Name                              Size          Modified           │
├─────────────────────────────────────────────────────────────────────┤
│  📁 Work                           -             Jan 10, 2025       │
│  📁 Projects                       -             Jan 8, 2025        │
│  📁 Archive                        -             Dec 15, 2024       │
│  📄 report.pdf                     1.2 MB        Jan 12, 2025       │
│  📄 notes.txt                      4 KB          Jan 11, 2025       │
│  📄 budget.xlsx                    256 KB        Jan 9, 2025        │
└─────────────────────────────────────────────────────────────────────┘

Selected: report.pdf, notes.txt (2 items, 1.2 MB)

[Pull Selected]
```

---

# Project Structure

```
croc-gui/
├── Cargo.toml                      # Workspace definition
├── README.md
├── LICENSE
├── CHANGELOG.md
│
├── crates/
│   ├── core/                       # Shared library
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       │
│   │       ├── croc/               # Croc integration
│   │       │   ├── mod.rs
│   │       │   ├── executable.rs   # Find croc binary
│   │       │   ├── options.rs      # CrocOptions struct
│   │       │   ├── process.rs      # Subprocess management
│   │       │   └── output.rs       # Output parsing
│   │       │
│   │       ├── iroh/               # Iroh integration
│   │       │   ├── mod.rs
│   │       │   ├── endpoint.rs     # IrohEndpoint
│   │       │   ├── identity.rs     # Keypair persistence
│   │       │   ├── protocol.rs     # Control messages
│   │       │   └── connection.rs   # Connection management
│   │       │
│   │       ├── blobs.rs            # iroh-blobs wrapper
│   │       │
│   │       ├── trust.rs            # Trust establishment
│   │       ├── peers.rs            # TrustedPeer model
│   │       ├── permissions.rs      # Permission system
│   │       │
│   │       ├── browse.rs           # File browsing
│   │       ├── push.rs             # Push transfer logic
│   │       ├── pull.rs             # Pull transfer logic
│   │       │
│   │       ├── transfer.rs         # Transfer model
│   │       ├── transfer_manager.rs # Transfer orchestration
│   │       │
│   │       ├── files.rs            # File utilities
│   │       ├── config.rs           # Configuration
│   │       ├── persistence.rs      # JSON storage
│   │       ├── platform.rs         # Cross-platform abstractions
│   │       ├── status.rs           # System info
│   │       └── error.rs            # Error types
│   │
│   ├── gui/                        # Native desktop app (Slint)
│   │   ├── Cargo.toml
│   │   ├── build.rs                # Slint compilation
│   │   ├── src/
│   │   │   ├── main.rs             # Entry point
│   │   │   ├── app.rs              # App state, event handling
│   │   │   ├── bridge.rs           # Slint ↔ Core bindings
│   │   │   └── callbacks.rs        # UI action handlers
│   │   │
│   │   └── ui/                     # Slint UI files
│   │       ├── main.slint          # Main window
│   │       ├── send.slint          # Send panel
│   │       ├── receive.slint       # Receive panel
│   │       ├── peers.slint         # Trusted peers view
│   │       ├── browser.slint       # File browser
│   │       ├── transfers.slint     # Active transfers list
│   │       └── widgets.slint       # Shared components
│   │
│   └── daemon/                     # Headless daemon
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs             # Entry point, CLI
│           │
│           ├── commands/           # CLI commands
│           │   ├── mod.rs
│           │   ├── run.rs          # daemon run
│           │   ├── receive.rs      # daemon receive <code>
│           │   ├── status.rs       # daemon status
│           │   ├── peers.rs        # daemon peers
│           │   └── config.rs       # daemon config
│           │
│           ├── inbox.rs            # Trust bundle detection
│           ├── handlers.rs         # Protocol handlers
│           ├── notifications.rs    # Desktop notifications
│           └── service.rs          # OS service integration
│
├── scripts/
│   ├── build.sh                    # Cross-platform builds
│   ├── package.sh                  # Create installers
│   └── release.sh                  # Release automation
│
├── install/
│   ├── linux/
│   │   ├── install.sh
│   │   ├── uninstall.sh
│   │   └── croc-gui.service
│   └── windows/
│       ├── install.ps1
│       └── uninstall.ps1
│
└── docs/
    ├── INSTALL.md
    ├── CONFIGURATION.md
    ├── PROTOCOL.md
    └── TROUBLESHOOTING.md
```

---

# Future Considerations

## Post-MVP Features

### Full File Manager (Phase 9)
- Dual-pane interface
- Drag-drop transfers
- Context menus (rename, delete, new folder)
- Keyboard navigation
- Search
- Favorites

### GUI ↔ GUI Trust (Phase 10)
- Symmetric trust flow
- Both sides have full UI
- Adapt protocol for peer type detection

### Advanced Features (Phase 11)
- Watch folders (auto-sync)
- Remote terminal (PTY over Iroh)
- Screen sharing
- Port forwarding
- Clipboard sync
- Chat/messaging

### Mobile (Phase 12)
- Native iOS/Android apps using Slint (or platform-native UI)
- Iroh integration via FFI
- Note: Existing mobile croc clients cover ad-hoc transfers

### Web Target (Phase 13 - Deferred)
- Low priority; may not implement
- Would require separate architecture (WASM + web server)
- Only consider if strong user demand emerges

## Technical Debt to Monitor

- Iroh FFI stability (if ever exposing API to other languages)
- iroh-blobs API changes (track upstream)
- Protocol versioning (add version negotiation early)
- Slint API stability (track upstream, currently pre-1.0)
- Cross-platform service management (consider unified approach)

## Dependencies to Track

| Dependency | Purpose | Stability |
|------------|---------|-----------|
| slint | Native UI framework | Pre-1.0, actively developed |
| iroh | Core networking | Pre-1.0, actively developed |
| iroh-blobs | File transfer | Pre-1.0, actively developed |
| tokio | Async runtime | Stable |
| serde | Serialization | Stable |
| clap | CLI parsing | Stable |

---

# Document History

| Version | Date | Changes |
|---------|------|---------|
| 1.0 | 2025-01-XX | Initial complete plan |

---

*End of Document*
