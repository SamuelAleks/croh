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
#   ./scripts/dual-test.sh --tile       # Tile windows side-by-side (KDE Wayland only)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

BUILD_MODE="debug"
SKIP_BUILD=false
CLEAN_CONFIGS=false
TILE_WINDOWS=false
NUM_INSTANCES=2
LOG_LEVEL="info"

# Instance names for display (A-Z)
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
    "Karl"
    "Liam"
    "Mia"
    "Noah"
    "Olivia"
    "Paul"
    "Quinn"
    "Rose"
    "Sam"
    "Tina"
    "Uma"
    "Vince"
    "Wendy"
    "Xander"
    "Yara"
    "Zack"
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
        --tile)
            TILE_WINDOWS=true
            shift
            ;;
        --debug)
            LOG_LEVEL="debug"
            shift
            ;;
        --trace)
            LOG_LEVEL="trace"
            shift
            ;;
        --log)
            LOG_LEVEL="$2"
            shift 2
            ;;
        -n|--instances)
            NUM_INSTANCES="$2"
            if ! [[ "$NUM_INSTANCES" =~ ^[0-9]+$ ]] || [ "$NUM_INSTANCES" -lt 1 ]; then
                echo "Error: -n requires a positive integer"
                exit 1
            fi
            if [ "$NUM_INSTANCES" -gt 26 ]; then
                echo "Error: Maximum 26 instances supported"
                exit 1
            fi
            shift 2
            ;;
        --help|-h)
            echo "Usage: $0 [-n NUM] [--release] [--no-build] [--clean] [--tile] [--debug] [--trace] [--log LEVEL]"
            echo ""
            echo "Launch multiple Croh instances for testing Iroh peer-to-peer functionality."
            echo ""
            echo "Options:"
            echo "  -n, --instances NUM  Number of instances to launch (default: 2, max: 26)"
            echo "  --release            Build in release mode"
            echo "  --no-build           Skip cargo build"
            echo "  --clean              Delete all config/data files before launching (fresh start)"
            echo "  --tile               Tile windows side-by-side on current monitor (KDE Wayland only)"
            echo "  --debug              Set log level to debug"
            echo "  --trace              Set log level to trace"
            echo "  --log LEVEL          Set custom log level (e.g., 'croh=debug,croh_core=trace')"
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

    # Remove ALL secondary instance directories (not just the ones we're launching)
    # This ensures a clean slate even if you previously ran with more instances
    for ((i=1; i<${#INSTANCE_NAMES[@]}; i++)); do
        name="${INSTANCE_NAMES[$i]}"
        name_lower=$(echo "$name" | tr '[:upper:]' '[:lower:]')
        instance_dir="/tmp/croh-$name_lower"
        if [ -d "$instance_dir" ]; then
            rm -rf "$instance_dir"
            echo "  Removed: $instance_dir"
        fi
    done

    # Create fresh configs with nicknames set
    echo "Creating fresh configs with nicknames..."

    # Primary instance (Alice)
    mkdir -p "$PRIMARY_CONFIG_DIR"
    cat > "$PRIMARY_CONFIG_DIR/config.json" << EOF
{
  "download_dir": "$HOME/Downloads",
  "default_relay": null,
  "theme": "system",
  "croc_path": null,
  "device_nickname": "${INSTANCE_NAMES[0]}",
  "default_hash": null,
  "default_curve": null,
  "throttle": null,
  "no_local": false,
  "window_size": { "width": 700, "height": 600 },
  "browse_settings": {
    "show_hidden": false,
    "show_protected": false,
    "exclude_patterns": ["node_modules", ".git", "__pycache__", "*.tmp", "*.swp"],
    "allowed_paths": []
  },
  "dnd_mode": "off",
  "dnd_message": null,
  "show_session_stats": false,
  "keep_completed_transfers": true,
  "security_posture": "balanced",
  "guest_policy": {
    "default_duration_hours": 72,
    "max_duration_hours": 168,
    "allow_extensions": true,
    "max_extensions": 3,
    "allow_promotion_requests": true,
    "auto_accept_guest_pushes": true
  }
}
EOF
    echo "  Created: $PRIMARY_CONFIG_DIR/config.json (nickname: ${INSTANCE_NAMES[0]})"

    # Secondary instances
    for ((i=1; i<NUM_INSTANCES; i++)); do
        name="${INSTANCE_NAMES[$i]}"
        name_lower=$(echo "$name" | tr '[:upper:]' '[:lower:]')
        instance_dir="/tmp/croh-$name_lower"
        mkdir -p "$instance_dir/config"
        cat > "$instance_dir/config/config.json" << EOF
{
  "download_dir": "$instance_dir/downloads",
  "default_relay": null,
  "theme": "system",
  "croc_path": null,
  "device_nickname": "$name",
  "default_hash": null,
  "default_curve": null,
  "throttle": null,
  "no_local": false,
  "window_size": { "width": 700, "height": 600 },
  "browse_settings": {
    "show_hidden": false,
    "show_protected": false,
    "exclude_patterns": ["node_modules", ".git", "__pycache__", "*.tmp", "*.swp"],
    "allowed_paths": []
  },
  "dnd_mode": "off",
  "dnd_message": null,
  "show_session_stats": false,
  "keep_completed_transfers": true,
  "security_posture": "balanced",
  "guest_policy": {
    "default_duration_hours": 72,
    "max_duration_hours": 168,
    "allow_extensions": true,
    "max_extensions": 3,
    "allow_promotion_requests": true,
    "auto_accept_guest_pushes": true
  }
}
EOF
        echo "  Created: $instance_dir/config/config.json (nickname: $name)"
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

# File to remember the last-used screen for tiling
TILE_SCREEN_FILE="/tmp/croh-tile-screen"

# Function to tile windows using KWin scripting (KDE Wayland)
tile_windows_kwin() {
    if [ "$TILE_WINDOWS" != true ]; then
        return
    fi

    # Check if we're on KDE Wayland
    if [ "$XDG_SESSION_TYPE" != "wayland" ] || [ -z "$KDE_FULL_SESSION" ]; then
        echo "Warning: --tile only works on KDE Wayland. Skipping window tiling."
        return
    fi

    # Check for qdbus
    local QDBUS=""
    if command -v qdbus6 &>/dev/null; then
        QDBUS="qdbus6"
    elif command -v qdbus-qt5 &>/dev/null; then
        QDBUS="qdbus-qt5"
    elif command -v qdbus &>/dev/null; then
        QDBUS="qdbus"
    else
        echo "Warning: qdbus not found. Cannot tile windows."
        return
    fi

    # Read preferred screen from file, or use current active screen
    local PREFERRED_SCREEN=""
    if [ -f "$TILE_SCREEN_FILE" ]; then
        PREFERRED_SCREEN=$(cat "$TILE_SCREEN_FILE" 2>/dev/null)
    fi

    # Get current active screen as fallback
    local CURRENT_SCREEN=$($QDBUS org.kde.KWin /KWin org.kde.KWin.activeOutputName 2>/dev/null)

    if [ -n "$PREFERRED_SCREEN" ]; then
        echo "Tiling windows on remembered screen: $PREFERRED_SCREEN"
    else
        PREFERRED_SCREEN="$CURRENT_SCREEN"
        echo "Tiling windows on current screen: $PREFERRED_SCREEN"
    fi

    # Wait for windows to appear
    sleep 2

    # Create a temporary KWin script to tile windows
    local SCRIPT_FILE="/tmp/croh-tile-windows.js"
    cat > "$SCRIPT_FILE" << KWINSCRIPT
// KWin script to tile Croh windows side-by-side on a specific screen
(function() {
    var preferredScreenName = "$PREFERRED_SCREEN";
    var crohWindows = [];

    // Find all Croh windows
    var clients = workspace.windowList();
    for (var i = 0; i < clients.length; i++) {
        var client = clients[i];
        if (client.caption && client.caption.indexOf("Croh") !== -1) {
            crohWindows.push(client);
        }
    }

    if (crohWindows.length === 0) {
        console.log("No Croh windows found");
        return;
    }

    // Find the preferred screen, or fall back to active screen
    var targetScreen = null;
    var screens = workspace.screens;
    for (var i = 0; i < screens.length; i++) {
        if (screens[i].name === preferredScreenName) {
            targetScreen = screens[i];
            break;
        }
    }

    // Fall back to active screen if preferred not found
    if (!targetScreen) {
        targetScreen = workspace.activeScreen;
        console.log("Preferred screen '" + preferredScreenName + "' not found, using active screen: " + targetScreen.name);
    }

    var screenGeom = targetScreen.geometry;
    console.log("Using screen: " + targetScreen.name + " geometry: " + JSON.stringify(screenGeom));

    // Calculate window dimensions
    var numWindows = crohWindows.length;
    var windowWidth = Math.floor(screenGeom.width / numWindows);
    var windowHeight = screenGeom.height;

    // Windows are already in creation order (Alice, Bob, Carol, etc.)
    // No sorting needed - we launched them in order with delays

    // Position each window
    for (var i = 0; i < crohWindows.length; i++) {
        var client = crohWindows[i];
        var x = screenGeom.x + (i * windowWidth);
        var y = screenGeom.y;

        // Move and resize
        client.frameGeometry = {
            x: x,
            y: y,
            width: windowWidth,
            height: windowHeight
        };

        console.log("Positioned " + client.caption + " at " + x + "," + y + " size " + windowWidth + "x" + windowHeight);
    }

    // Save the screen name we used (write to a temp file that the shell script will read)
    // We output this so the shell script can save it
    console.log("TILE_SCREEN_USED:" + targetScreen.name);
})();
KWINSCRIPT

    # Load and run the script
    local SCRIPT_ID=$($QDBUS org.kde.KWin /Scripting org.kde.kwin.Scripting.loadScript "$SCRIPT_FILE" 2>/dev/null)
    if [ -n "$SCRIPT_ID" ] && [ "$SCRIPT_ID" != "0" ]; then
        $QDBUS org.kde.KWin /Scripting org.kde.kwin.Scripting.start 2>/dev/null
        sleep 0.5
        # Unload the script
        $QDBUS org.kde.KWin /Scripting org.kde.kwin.Scripting.unloadScript "croh-tile-windows" 2>/dev/null || true

        # Save the current active screen for next time (the windows are now on it)
        # We use the screen where Alice ended up (first window)
        local USED_SCREEN=$($QDBUS org.kde.KWin /KWin org.kde.KWin.activeOutputName 2>/dev/null)
        if [ -n "$PREFERRED_SCREEN" ]; then
            # Use the preferred screen we targeted
            echo "$PREFERRED_SCREEN" > "$TILE_SCREEN_FILE"
        elif [ -n "$USED_SCREEN" ]; then
            echo "$USED_SCREEN" > "$TILE_SCREEN_FILE"
        fi

        echo "Windows tiled successfully on $PREFERRED_SCREEN (remembered for next time)"
    else
        echo "Warning: Failed to load KWin script. Windows not tiled."
    fi

    rm -f "$SCRIPT_FILE"
}

# Launch primary instance (default config)
echo "Launching ${INSTANCE_NAMES[0]} (log level: $LOG_LEVEL)..."
CROH_INSTANCE_NAME="${INSTANCE_NAMES[0]}" \
RUST_LOG="$LOG_LEVEL" "$BINARY" &
PIDS+=($!)

# Launch secondary instances with isolated configs
for ((i=1; i<NUM_INSTANCES; i++)); do
    sleep 0.5  # Small delay to avoid window overlap

    name="${INSTANCE_NAMES[$i]}"
    name_lower=$(echo "$name" | tr '[:upper:]' '[:lower:]')
    instance_dir="/tmp/croh-$name_lower"

    echo "Launching $name (log level: $LOG_LEVEL)..."
    CROH_INSTANCE_NAME="$name" \
    CROH_CONFIG_DIR="$instance_dir/config" \
    CROH_DATA_DIR="$instance_dir/data" \
    CROH_DOWNLOAD_DIR="$instance_dir/downloads" \
    RUST_LOG="$LOG_LEVEL" "$BINARY" &
    PIDS+=($!)
done

# Tile windows if requested (runs in background)
tile_windows_kwin &

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
