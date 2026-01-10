# Croh

A native desktop application for [croc](https://github.com/schollz/croc) file transfers, built with Rust and [Slint](https://slint.dev/).

![Material Dark Theme](https://img.shields.io/badge/Theme-Material%20Dark-6750A4)
![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux-blue)
![License](https://img.shields.io/badge/License-MIT-green)

## Features

- **Send files**: Select files and generate a croc code for sharing
- **Receive files**: Enter a code to receive files from anyone
- **Settings**: Configure download directory, theme, and relay
- **Trusted Peers** (coming soon): Direct peer-to-peer connections using [Iroh](https://iroh.computer/)

## Screenshots

The application uses a Material Dark theme with:
- Send tab with drag-and-drop file picker
- Receive tab with code entry
- Settings panel for configuration
- Real-time transfer progress

## Requirements

- [croc](https://github.com/schollz/croc) installed and available in PATH

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
# Build
cargo build --release

# Copy binaries to a folder in your PATH
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
# Build
cargo build --release

# Install binaries
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
# Clone the repository
git clone https://github.com/croh/croh.git
cd croh

# Build all crates (debug)
cargo build

# Build release binaries
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

The GUI provides:
- **Send Tab**: Select files and share the generated code
- **Receive Tab**: Enter a code to download files
- **Peers Tab**: Manage trusted peers (coming soon)
- **Settings Tab**: Configure download directory, relay, theme

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

# View configuration
croh-daemon config

# Set configuration
croh-daemon config download_dir /path/to/downloads
```

### Running as a Service

**Linux (systemd)**
```bash
# Start the service
systemctl --user start croh@$USER

# Enable at login
systemctl --user enable croh@$USER

# Check status
systemctl --user status croh@$USER

# View logs
journalctl --user -u croh@$USER -f
```

**Windows (NSSM)**
```powershell
# Install with NSSM (done by install script with -InstallService)
nssm start croh-daemon
nssm stop croh-daemon
```

## Project Structure

```
croh/
├── Cargo.toml              # Workspace definition
├── crates/
│   ├── core/               # Shared library
│   │   └── src/
│   │       ├── croc/       # Croc subprocess wrapper
│   │       ├── config.rs   # Configuration management
│   │       ├── transfer.rs # Transfer state tracking
│   │       └── ...
│   ├── gui/                # Native desktop app (Slint)
│   │   ├── src/
│   │   └── ui/             # Slint UI files
│   └── daemon/             # Headless service
│       └── src/
│           └── commands/   # CLI commands
├── install/
│   ├── linux/              # Linux install scripts & service
│   └── windows/            # Windows install scripts
└── .vscode/                # VS Code/Cursor debug configs
```

## Configuration

Configuration files are stored in:
- **Linux**: `~/.config/croh/config.json`
- **Windows**: `%APPDATA%\croh\config.json`
- **macOS**: `~/Library/Application Support/croh/config.json`

### Config File Format

```json
{
  "download_dir": "/home/user/Downloads",
  "default_relay": null,
  "theme": "dark",
  "croc_path": null
}
```

### Environment Variables

| Variable | Description |
|----------|-------------|
| `CROC_PATH` | Path to croc executable |
| `CROH_DOWNLOAD_DIR` | Default download directory |
| `RUST_LOG` | Log level (debug, info, warn, error) |

## Debugging

### VS Code / Cursor

Debug configurations are provided in `.vscode/launch.json`:

1. Install the **CodeLLDB** extension
2. Set breakpoints in Rust code
3. Press `F5` to start debugging

### Command Line

```bash
# Enable debug logging
RUST_LOG=debug cargo run -p croh

# Enable backtrace on panics
RUST_BACKTRACE=1 cargo run -p croh
```

## Roadmap

- [x] Phase 0.1-0.8: Core functionality (send, receive, settings)
- [x] Phase 0.9: Service integration
- [ ] Phase 0.10: Testing & verification
- [ ] Phase 1+: Iroh integration for trusted peers

## License

MIT

## Acknowledgments

- [croc](https://github.com/schollz/croc) - The underlying file transfer tool
- [Slint](https://slint.dev/) - Native UI framework
- [Iroh](https://iroh.computer/) - P2P networking (future)
