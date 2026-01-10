#!/usr/bin/env bash
# Launch two Croh GUI instances for testing peer-to-peer functionality.
#
# Instance 1 (Alice): Uses default config/data directories
# Instance 2 (Bob):   Uses /tmp/croh-bob for isolated config/data
#
# Usage:
#   ./scripts/dual-test.sh          # Build and run both instances
#   ./scripts/dual-test.sh --release # Build release and run both instances
#   ./scripts/dual-test.sh --no-build # Skip build, just run

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

BUILD_MODE="debug"
SKIP_BUILD=false

for arg in "$@"; do
    case $arg in
        --release)
            BUILD_MODE="release"
            ;;
        --no-build)
            SKIP_BUILD=true
            ;;
        --help|-h)
            echo "Usage: $0 [--release] [--no-build]"
            echo ""
            echo "Launch two Croh instances for testing Iroh peer-to-peer functionality."
            echo ""
            echo "Options:"
            echo "  --release   Build in release mode"
            echo "  --no-build  Skip cargo build"
            echo ""
            echo "Instance directories:"
            echo "  Alice: Default (~/.config/croh, ~/.local/share/croh)"
            echo "  Bob:   /tmp/croh-bob"
            exit 0
            ;;
    esac
done

cd "$PROJECT_DIR"

# Build if needed
if [ "$SKIP_BUILD" = false ]; then
    echo "Building croh ($BUILD_MODE)..."
    if [ "$BUILD_MODE" = "release" ]; then
        cargo build -p croh --release
        BINARY="target/release/croh"
    else
        cargo build -p croh
        BINARY="target/debug/croh"
    fi
else
    if [ "$BUILD_MODE" = "release" ]; then
        BINARY="target/release/croh"
    else
        BINARY="target/debug/croh"
    fi
fi

if [ ! -f "$BINARY" ]; then
    echo "Error: Binary not found at $BINARY"
    echo "Run without --no-build to compile first."
    exit 1
fi

# Create Bob's directories
BOB_DIR="/tmp/croh-bob"
mkdir -p "$BOB_DIR/config" "$BOB_DIR/data" "$BOB_DIR/downloads"

echo ""
echo "Starting two Croh instances..."
echo "  Alice: Default config (primary instance)"
echo "  Bob:   $BOB_DIR (secondary instance)"
echo ""
echo "To test peer trust:"
echo "  1. In Alice's window, go to Peers tab and click 'Add Peer'"
echo "  2. Copy the trust code shown"
echo "  3. In Bob's window, go to Receive tab and enter the code"
echo "  4. Both instances should now show each other as trusted peers"
echo ""
echo "Press Ctrl+C to stop both instances."
echo ""

# Launch Alice (default config)
echo "Launching Alice..."
RUST_LOG=info "$BINARY" &
ALICE_PID=$!

# Small delay to avoid window overlap
sleep 0.5

# Launch Bob (isolated config)
echo "Launching Bob..."
CROH_CONFIG_DIR="$BOB_DIR/config" \
CROH_DATA_DIR="$BOB_DIR/data" \
CROH_DOWNLOAD_DIR="$BOB_DIR/downloads" \
RUST_LOG=info "$BINARY" &
BOB_PID=$!

# Cleanup on exit
cleanup() {
    echo ""
    echo "Stopping instances..."
    kill $ALICE_PID 2>/dev/null || true
    kill $BOB_PID 2>/dev/null || true
    wait
    echo "Done."
}
trap cleanup EXIT

# Wait for either to exit
wait -n $ALICE_PID $BOB_PID 2>/dev/null || true
