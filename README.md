# Croc GUI

A native desktop application for [croc](https://github.com/schollz/croc) file transfers, built with Rust and [Slint](https://slint.dev/).

## Features

- **Send files**: Select files and generate a croc code for sharing
- **Receive files**: Enter a code to receive files from anyone
- **Trusted Peers** (coming soon): Direct peer-to-peer connections using [Iroh](https://iroh.computer/)

## Requirements

- [croc](https://github.com/schollz/croc) installed and available in PATH
- Rust 1.75+ (for building from source)

## Building

```bash
# Clone the repository
git clone https://github.com/your-username/croc-gui.git
cd croc-gui

# Build all crates
cargo build --release

# Run the GUI
cargo run --release -p croc-gui

# Run the daemon
cargo run --release -p croc-daemon -- --help
```

## Project Structure

```
croc-gui/
├── crates/
│   ├── core/       # Shared library (croc wrapper, transfer management)
│   ├── gui/        # Native desktop app (Slint UI)
│   └── daemon/     # Headless service (CLI)
└── docs/           # Documentation
```

## Daemon Commands

```bash
# Run the daemon service
croc-daemon run

# Receive a file
croc-daemon receive <code>

# Check status
croc-daemon status

# List trusted peers
croc-daemon peers

# View/modify config
croc-daemon config
croc-daemon config download_dir
croc-daemon config download_dir /path/to/downloads
```

## Configuration

Configuration files are stored in:
- **Linux**: `~/.config/croc-gui/`
- **Windows**: `%APPDATA%\croc-gui\`
- **macOS**: `~/Library/Application Support/croc-gui/`

Environment variables:
- `CROC_PATH`: Path to croc executable
- `CROC_GUI_DOWNLOAD_DIR`: Default download directory

## License

MIT

