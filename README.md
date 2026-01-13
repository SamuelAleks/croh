# Croh

A native desktop application for secure file transfers between trusted devices, built with Rust and [Slint](https://slint.dev/).

Croh combines two transfer modes:
- **Ad-hoc transfers** via [croc](https://github.com/schollz/croc) - Share files with anyone using human-readable codes
- **Trusted peer connections** via [Iroh](https://iroh.computer/) - Persistent, encrypted peer-to-peer networking with NAT traversal

![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux-blue)
![License](https://img.shields.io/badge/License-MIT-green)
[![CI](https://github.com/SamuelAleks/croh/actions/workflows/ci.yml/badge.svg)](https://github.com/SamuelAleks/croh/actions/workflows/ci.yml)

## Features

### File Transfers
- **Send files**: Select files and generate a croc code for sharing with anyone
- **Receive files**: Enter a code to receive files from any croc client
- **Push to peers**: Send files directly to trusted peers without codes
- **Pull from peers**: Request files from trusted peers' shared directories
- **Remote browsing**: Browse a trusted peer's filesystem and select files to pull
- **Speed testing**: Test connection speed between peers

### Screen Streaming (Remote Desktop)
- **View peer screens**: Stream a trusted peer's display in real-time
- **Remote control**: Send mouse and keyboard input to control peer's screen
- **Multiple backends**: DRM/KMS, Portal (Wayland), X11 SHM, DXGI (Windows)
- **Hardware acceleration**: Optional FFmpeg support for H.264 encoding
- **Adaptive quality**: Automatic adjustment based on bandwidth
- **Permission-based**: Separate permissions for view-only vs control access

### Trusted Peers
- **Trust establishment**: Secure handshake using croc for initial key exchange
- **Persistent connections**: Iroh handles NAT traversal and reconnection automatically
- **Peer status**: See which peers are online with connection quality indicators
- **Guest peers**: Temporary access with configurable auto-expiry (24h-1 week)
- **Peer promotion**: Guests can request permanent trusted status

### Peer Networks
- **Network groups**: Organize trusted peers into named networks
- **Peer introductions**: Introduce peers to each other with three-way consent
- **Network settings**: Control introductions and member visibility per network

### Peer-to-Peer Chat
- **Text messaging**: Send messages to trusted peers
- **Typing indicators**: See when peers are typing
- **Delivery receipts**: Track message delivery and read status
- **Offline queuing**: Messages queue when peers are offline
- **Message sync**: Conversation history syncs when peers reconnect

### Configuration
- **10+ themes**: Dark, Light, Dracula, Nord, Solarized, Gruvbox, Catppuccin, and more
- **Security postures**: Relaxed, Balanced, or Cautious presets
- **Do Not Disturb**: Silent or Busy modes with custom messages
- **Relay preferences**: Control direct vs relay connections
- **Browse settings**: Configure which directories peers can access

## Requirements

- [croc](https://github.com/schollz/croc) installed and available in PATH (for ad-hoc transfers)

### Optional Dependencies

**Linux (for screen streaming):**
- `libpipewire-0.3-dev` - Required for Portal/Wayland screen capture
- `CAP_SYS_ADMIN` capability - For DRM/KMS capture backend (best performance)

**FFmpeg support (optional):**
- `ffmpeg` development libraries for H.264 hardware encoding

## Installation

### Windows

**Option 1: Install Script (Recommended)**
```powershell
# Run PowerShell as Administrator
.\install\windows\install.ps1

# To also install as a Windows service:
.\install\windows\install.ps1 -InstallService
```

**Option 2: Manual**
```powershell
cargo build --release
copy target\release\croh.exe C:\Tools\
copy target\release\croh-daemon.exe C:\Tools\
```

### Linux

**Option 1: Install Script (Recommended)**
```bash
chmod +x install/linux/install.sh
./install/linux/install.sh
```

**Option 2: Manual**
```bash
cargo build --release
sudo cp target/release/croh /usr/local/bin/
sudo cp target/release/croh-daemon /usr/local/bin/

# (Optional) Install systemd service
cp install/linux/croh.service ~/.config/systemd/user/croh@.service
systemctl --user daemon-reload
systemctl --user enable croh@$USER
systemctl --user start croh@$USER
```

### Building from Source

```bash
git clone https://github.com/yourusername/croh.git
cd croh

# Build all crates
cargo build --release

# Run the GUI
cargo run --release -p croh

# Run the daemon
cargo run --release -p croh-daemon -- --help
```

## Usage

### GUI Application

```bash
croh
```

The GUI provides four main tabs:

- **Send**: Select files, choose destination (croc code or trusted peer), and send
- **Receive**: Enter a croc code to download files, or accept incoming pushes
- **Peers**: Manage trusted peers, view status, send messages, browse files
- **Settings**: Configure download directory, theme, security, and transfer options

### Daemon (Headless)

```bash
# Run the daemon service
croh-daemon run

# Receive a file (auto-accept)
croh-daemon receive 7-alpha-beta-gamma

# Check daemon status
croh-daemon status

# List trusted peers
croh-daemon peers

# View/set configuration
croh-daemon config
croh-daemon config download_dir /path/to/downloads
```

### Running as a Service

**Linux (systemd)**
```bash
systemctl --user start croh@$USER
systemctl --user enable croh@$USER
systemctl --user status croh@$USER
journalctl --user -u croh@$USER -f
```

**Windows (NSSM)**
```powershell
nssm start croh-daemon
nssm stop croh-daemon
```

### Screen Streaming Setup (Linux)

Screen streaming requires additional setup depending on the capture backend:

**Portal/Wayland (Recommended for desktop use)**
```bash
# Install PipeWire development libraries
sudo apt install libpipewire-0.3-dev  # Debian/Ubuntu
sudo pacman -S pipewire               # Arch

# No additional setup needed - Portal prompts for permission on first use
# Restore tokens are cached so subsequent sessions skip the dialog
```

**DRM/KMS (Best performance, requires privileges)**
```bash
# Grant CAP_SYS_ADMIN capability to the binary
sudo setcap cap_sys_admin+ep /usr/local/bin/croh-daemon
```

**Input Injection (for remote control)**
```bash
# Add user to input group
sudo usermod -aG input $USER

# Create udev rule for uinput access
echo 'KERNEL=="uinput", MODE="0660", GROUP="input"' | \
  sudo tee /etc/udev/rules.d/99-uinput.rules
sudo udevadm control --reload-rules

# Log out and back in for group changes to take effect
```

## Architecture

```
croh/
├── crates/
│   ├── core/               # Shared library (croh-core)
│   │   ├── croc/           # Croc subprocess wrapper
│   │   ├── iroh/           # Iroh P2P networking
│   │   │   ├── endpoint.rs # Connection management
│   │   │   ├── protocol.rs # Control messages
│   │   │   ├── handshake.rs# Trust establishment
│   │   │   ├── transfer.rs # Push/pull file transfers
│   │   │   ├── browse.rs   # Remote directory browsing
│   │   │   └── speedtest.rs# Connection speed testing
│   │   ├── screen/         # Screen capture & streaming
│   │   │   ├── drm.rs      # Linux DRM/KMS backend
│   │   │   ├── portal.rs   # Linux Portal/Wayland backend
│   │   │   ├── x11.rs      # Linux X11 SHM backend
│   │   │   ├── dxgi.rs     # Windows DXGI backend
│   │   │   ├── encoder.rs  # Frame encoding (PNG, Zstd)
│   │   │   ├── ffmpeg/     # H.264 encoding (optional)
│   │   │   ├── viewer.rs   # Client-side frame buffer
│   │   │   └── input.rs    # Remote input injection
│   │   ├── chat/           # Peer-to-peer messaging
│   │   ├── config.rs       # Configuration management
│   │   ├── peers.rs        # Trusted peer storage
│   │   ├── networks.rs     # Peer network groups
│   │   ├── trust.rs        # Trust bundle handling
│   │   └── transfer.rs     # Transfer state machine
│   ├── gui/                # Native desktop app (croh)
│   │   ├── src/app.rs      # Application state & callbacks
│   │   └── ui/main.slint   # UI definition
│   └── daemon/             # Headless service (croh-daemon)
│       └── commands/       # CLI subcommands
├── install/
│   ├── linux/              # Linux install scripts & systemd service
│   └── windows/            # Windows install scripts
└── misc/                   # Reference documents (gitignored)
```

## Configuration

Configuration files are stored in:
- **Linux**: `~/.config/croh/config.json`
- **Windows**: `%APPDATA%\croh\config.json`
- **macOS**: `~/Library/Application Support/croh/config.json`

### Config Options

| Option | Description |
|--------|-------------|
| `download_dir` | Directory for received files |
| `default_relay` | Custom croc relay (null = default) |
| `theme` | UI theme (dark, light, dracula, nord, etc.) |
| `croc_path` | Path to croc executable (null = auto-detect) |
| `device_nickname` | Custom device name for display |
| `default_hash` | Hash algorithm: md5, xxhash, imohash |
| `default_curve` | Encryption curve: p256, p384, p521, siec |
| `throttle` | Bandwidth limit (e.g., "1M", "500K") |
| `no_local` | Force relay-only transfers |
| `security_posture` | relaxed, balanced, or cautious |
| `dnd_mode` | Do Not Disturb: off, silent, busy |
| `relay_preference` | normal, direct-preferred, relay-only, disabled |

### Environment Variables

| Variable | Description |
|----------|-------------|
| `CROC_PATH` | Path to croc executable |
| `CROH_DOWNLOAD_DIR` | Default download directory |
| `RUST_LOG` | Log level (debug, info, warn, error) |

## Trust Model

### Two-Tier Peer System

| Tier | Duration | Network Access | Can Be Introduced |
|------|----------|----------------|-------------------|
| **Trusted** | Permanent | Full | Yes |
| **Guest** | Temporary (configurable) | Limited | No |

### Security Postures

| Setting | Relaxed | Balanced | Cautious |
|---------|---------|----------|----------|
| Guest duration | 1 week | 3 days | 24 hours |
| Auto-accept pushes | Yes | Yes (trusted only) | No |
| Network introductions | Auto-accept | Notify | Require approval |

### Trust Establishment Flow

1. Initiator generates a trust bundle and sends it via croc
2. Receiver gets the bundle and connects via Iroh using the endpoint ID
3. Both peers exchange TrustConfirm messages with nonce verification
4. Trust is stored locally with configurable permissions

## Protocol

Croh uses a custom protocol over Iroh's QUIC connections:

- **ALPN**: `croh/control/1` for control messages, `croh/blobs/1` for file data
- **Format**: Length-prefixed JSON (4-byte big-endian length + JSON payload)
- **Encryption**: TLS 1.3 via QUIC, verified by Iroh endpoint IDs

### Message Types

- Trust: `TrustConfirm`, `TrustComplete`, `TrustRevoke`
- Transfer: `PushOffer`, `PullRequest`, `TransferProgress`, `TransferComplete`
- Browse: `BrowseRequest`, `BrowseResponse`
- Status: `StatusRequest`, `StatusResponse`, `DndStatus`
- Chat: `ChatMessage`, `ChatDelivered`, `ChatRead`, `ChatTyping`
- Guest: `ExtensionRequest`, `PromotionRequest`
- Network: `IntroductionOffer`, `IntroductionRequest`, `IntroductionComplete`
- Screen: `ScreenStreamRequest`, `ScreenStreamResponse`, `ScreenFrame`, `ScreenInput`

## Debugging

```bash
# Enable debug logging
RUST_LOG=debug cargo run -p croh

# Enable backtrace on panics
RUST_BACKTRACE=1 cargo run -p croh

# Run tests
cargo test -p croh-core --lib
cargo test -p croh-core --test iroh_integration
```

VS Code/Cursor debug configurations are provided in `.vscode/launch.json`.

## Current Status

### Complete
- Croc send/receive with all options
- Native file picker and transfer progress
- Iroh endpoint and identity management
- Trust establishment with NAT traversal
- Push/pull file transfers between peers
- Remote directory browsing
- Peer-to-peer chat with receipts
- Peer networks with introductions
- Guest peers with expiry
- 10+ color themes
- Security posture presets
- Screen streaming with multiple capture backends
- Remote input handling (mouse/keyboard)
- FFmpeg integration for hardware-accelerated encoding

### In Progress
- Connection status indicators
- Transfer resume after interruption
- Full permissions enforcement

### Planned
- Full file manager with dual-pane interface
- Clipboard sync between peers
- Watch folders for auto-sync

## License

MIT

## Acknowledgments

- [croc](https://github.com/schollz/croc) - Simple and secure file transfer
- [Slint](https://slint.dev/) - Native UI framework
- [Iroh](https://iroh.computer/) - Peer-to-peer networking
