#!/usr/bin/env bash
# Launch multiple Croh GUI instances for testing peer-to-peer functionality.
#
# Instance 1: Uses default config/data directories
# Instance 2+: Uses /tmp/croh-{name} for isolated config/data
#
# Usage:
#   ./scripts/dual-test.sh              # Build and run 2 instances (default)
#   ./scripts/dual-test.sh -n 3         # Run 3 instances
#   ./scripts/dual-test.sh -n 5         # Run 5 instances
#   ./scripts/dual-test.sh --release    # Build release and run
#   ./scripts/dual-test.sh --no-build   # Skip build, just run
#   ./scripts/dual-test.sh --clean      # Delete configs before launching (fresh start)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

BUILD_MODE="debug"
SKIP_BUILD=false
CLEAN_CONFIGS=false
NUM_INSTANCES=2

# Instance names for display (extend as needed)
INSTANCE_NAMES=(
    "Alice"
    "Bob"
    "Carol"
    "Dave"
    "Eve"
    "Frank"
    "Grace"
    "Heidi"
    "Ivan"
    "Judy"
)

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --release)
            BUILD_MODE="release"
            shift
            ;;
        --no-build)
            SKIP_BUILD=true
            shift
            ;;
        --clean)
            CLEAN_CONFIGS=true
            shift
            ;;
        -n|--instances)
            NUM_INSTANCES="$2"
            if ! [[ "$NUM_INSTANCES" =~ ^[0-9]+$ ]] || [ "$NUM_INSTANCES" -lt 1 ]; then
                echo "Error: -n requires a positive integer"
                exit 1
            fi
            if [ "$NUM_INSTANCES" -gt 10 ]; then
                echo "Error: Maximum 10 instances supported"
                exit 1
            fi
            shift 2
            ;;
        --help|-h)
            echo "Usage: $0 [-n NUM] [--release] [--no-build] [--clean]"
            echo ""
            echo "Launch multiple Croh instances for testing Iroh peer-to-peer functionality."
            echo ""
            echo "Options:"
            echo "  -n, --instances NUM  Number of instances to launch (default: 2, max: 10)"
            echo "  --release            Build in release mode"
            echo "  --no-build           Skip cargo build"
            echo "  --clean              Delete all config/data files before launching (fresh start)"
            echo ""
            echo "Instance directories:"
            echo "  Instance 1 (Alice): Default (~/.config/croh, ~/.local/share/croh)"
            echo "  Instance 2+ (Bob, Carol, ...): /tmp/croh-{name}"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Use --help for usage information"
            exit 1
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

# Set up directories
PRIMARY_CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/croh"
PRIMARY_DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/croh"

# Clean configs if requested
if [ "$CLEAN_CONFIGS" = true ]; then
    echo "Cleaning config/data directories..."
    rm -rf "$PRIMARY_CONFIG_DIR" "$PRIMARY_DATA_DIR"
    echo "  Removed: $PRIMARY_CONFIG_DIR"
    echo "  Removed: $PRIMARY_DATA_DIR"

    for ((i=1; i<NUM_INSTANCES; i++)); do
        name="${INSTANCE_NAMES[$i]}"
        name_lower=$(echo "$name" | tr '[:upper:]' '[:lower:]')
        instance_dir="/tmp/croh-$name_lower"
        rm -rf "$instance_dir"
        echo "  Removed: $instance_dir"
    done
fi

# Create directories for secondary instances
for ((i=1; i<NUM_INSTANCES; i++)); do
    name="${INSTANCE_NAMES[$i]}"
    name_lower=$(echo "$name" | tr '[:upper:]' '[:lower:]')
    instance_dir="/tmp/croh-$name_lower"
    mkdir -p "$instance_dir/config" "$instance_dir/data" "$instance_dir/downloads"
done

echo ""
echo "Starting $NUM_INSTANCES Croh instance(s)..."
echo "  ${INSTANCE_NAMES[0]}: Default config (primary instance)"
for ((i=1; i<NUM_INSTANCES; i++)); do
    name="${INSTANCE_NAMES[$i]}"
    name_lower=$(echo "$name" | tr '[:upper:]' '[:lower:]')
    echo "  $name: /tmp/croh-$name_lower"
done
echo ""
echo "To test peer trust:"
echo "  1. In one instance, go to Peers tab and click 'Add Peer'"
echo "  2. Copy the trust code shown"
echo "  3. In another instance, go to Receive tab and enter the code"
echo "  4. Both instances should now show each other as trusted peers"
echo ""
echo "Press Ctrl+C to stop all instances."
echo ""

# Array to store PIDs
declare -a PIDS

# Launch primary instance (default config)
echo "Launching ${INSTANCE_NAMES[0]}..."
RUST_LOG=info "$BINARY" &
PIDS+=($!)

# Launch secondary instances with isolated configs
for ((i=1; i<NUM_INSTANCES; i++)); do
    sleep 0.5  # Small delay to avoid window overlap

    name="${INSTANCE_NAMES[$i]}"
    name_lower=$(echo "$name" | tr '[:upper:]' '[:lower:]')
    instance_dir="/tmp/croh-$name_lower"

    echo "Launching $name..."
    CROH_CONFIG_DIR="$instance_dir/config" \
    CROH_DATA_DIR="$instance_dir/data" \
    CROH_DOWNLOAD_DIR="$instance_dir/downloads" \
    RUST_LOG=info "$BINARY" &
    PIDS+=($!)
done

# Cleanup on exit
cleanup() {
    echo ""
    echo "Stopping ${#PIDS[@]} instance(s)..."
    for pid in "${PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    wait
    echo "Done."
}
trap cleanup EXIT

# Wait for any instance to exit
wait -n "${PIDS[@]}" 2>/dev/null || true
