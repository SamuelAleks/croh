#!/bin/bash
#
# Croc GUI Uninstaller for Linux
#

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

INSTALL_DIR="/usr/local/bin"
SERVICE_DIR="$HOME/.config/systemd/user"
DATA_DIR="$HOME/.local/share/croc-gui"
CONFIG_DIR="$HOME/.config/croc-gui"

echo -e "${RED}Croc GUI Uninstaller${NC}"
echo "====================="
echo

# Confirm
read -p "This will remove Croc GUI and all its data. Continue? [y/N] " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Cancelled."
    exit 0
fi

# Stop service if running
echo "Stopping service..."
systemctl --user stop "croc-gui@$USER" 2>/dev/null || true
systemctl --user disable "croc-gui@$USER" 2>/dev/null || true

# Remove service file
echo "Removing service file..."
rm -f "$SERVICE_DIR/croc-gui@.service"
systemctl --user daemon-reload 2>/dev/null || true

# Remove binaries
echo "Removing binaries..."
sudo rm -f "$INSTALL_DIR/croc-gui"
sudo rm -f "$INSTALL_DIR/croc-daemon"

# Ask about data removal
echo
read -p "Remove configuration and data? [y/N] " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    echo "Removing data..."
    rm -rf "$DATA_DIR"
    rm -rf "$CONFIG_DIR"
fi

echo
echo -e "${GREEN}Uninstallation complete.${NC}"

