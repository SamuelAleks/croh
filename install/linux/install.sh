#!/bin/bash
#
# Croh Installer for Linux
# This script installs the croh and croh-daemon binaries
#

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Installation paths
INSTALL_DIR="/usr/local/bin"
SERVICE_DIR="$HOME/.config/systemd/user"
DATA_DIR="$HOME/.local/share/croh"
CONFIG_DIR="$HOME/.config/croh"

echo -e "${GREEN}Croh Installer${NC}"
echo "===================="
echo

# Check if running as root (we don't want that for user service)
if [ "$EUID" -eq 0 ]; then
    echo -e "${YELLOW}Warning: Running as root. The daemon will be installed as a user service.${NC}"
    echo "Consider running this script as your regular user."
    read -p "Continue anyway? [y/N] " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

# Check for croc
echo -n "Checking for croc... "
if command -v croc &> /dev/null; then
    echo -e "${GREEN}found${NC} ($(which croc))"
else
    echo -e "${RED}not found${NC}"
    echo
    echo "Croc is required. Install it with one of:"
    echo "  curl https://getcroc.schollz.com | bash"
    echo "  sudo apt install croc"
    echo "  sudo snap install croc"
    echo
    read -p "Continue without croc? [y/N] " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

# Check for binaries in current directory or build them
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

if [ -f "$REPO_ROOT/target/release/croh" ] && [ -f "$REPO_ROOT/target/release/croh-daemon" ]; then
    echo "Using pre-built binaries from target/release/"
    GUI_BIN="$REPO_ROOT/target/release/croh"
    DAEMON_BIN="$REPO_ROOT/target/release/croh-daemon"
elif [ -f "./croh" ] && [ -f "./croh-daemon" ]; then
    echo "Using binaries from current directory"
    GUI_BIN="./croh"
    DAEMON_BIN="./croh-daemon"
else
    echo "Building from source..."
    if ! command -v cargo &> /dev/null; then
        echo -e "${RED}Error: cargo not found. Please install Rust first.${NC}"
        echo "Visit: https://rustup.rs/"
        exit 1
    fi

    cd "$REPO_ROOT"
    cargo build --release
    GUI_BIN="$REPO_ROOT/target/release/croh"
    DAEMON_BIN="$REPO_ROOT/target/release/croh-daemon"
fi

# Create directories
echo
echo "Creating directories..."
mkdir -p "$DATA_DIR"
mkdir -p "$CONFIG_DIR"
mkdir -p "$SERVICE_DIR"

# Install binaries
echo "Installing binaries to $INSTALL_DIR..."
sudo cp "$GUI_BIN" "$INSTALL_DIR/croh"
sudo cp "$DAEMON_BIN" "$INSTALL_DIR/croh-daemon"
sudo chmod +x "$INSTALL_DIR/croh"
sudo chmod +x "$INSTALL_DIR/croh-daemon"

# Install systemd service
echo "Installing systemd user service..."
cp "$SCRIPT_DIR/croh.service" "$SERVICE_DIR/croh@.service"

# Enable linger for user services to run without login
echo "Enabling user service lingering..."
loginctl enable-linger "$USER" 2>/dev/null || true

# Reload systemd
echo "Reloading systemd..."
systemctl --user daemon-reload

echo
echo -e "${GREEN}Installation complete!${NC}"
echo
echo "Usage:"
echo "  croh              # Launch the GUI"
echo "  croh-daemon run       # Run the daemon manually"
echo "  croh-daemon status    # Check daemon status"
echo "  croh-daemon receive <code>  # Receive a file"
echo
echo "To run daemon as a service:"
echo "  systemctl --user start croh@$USER"
echo "  systemctl --user enable croh@$USER  # Start on login"
echo
echo "Configuration: $CONFIG_DIR/config.json"
echo "Data directory: $DATA_DIR"
