#!/usr/bin/env pwsh
# Launch multiple Croh GUI instances for testing peer-to-peer functionality.
#
# Instance 1: Uses default config/data directories
# Instance 2+: Uses %TEMP%\croh-{name} for isolated config/data
#
# Usage:
#   .\scripts\dual-test.ps1              # Build and run 2 instances (default)
#   .\scripts\dual-test.ps1 -n 3         # Run 3 instances
#   .\scripts\dual-test.ps1 -n 5         # Run 5 instances
#   .\scripts\dual-test.ps1 -Release     # Build release and run
#   .\scripts\dual-test.ps1 -NoBuild     # Skip build, just run
#   .\scripts\dual-test.ps1 -Clean       # Delete configs before launching (fresh start)

param(
    [int]$n = 2,
    [switch]$Release,
    [switch]$NoBuild,
    [switch]$Clean,
    [switch]$Debug,
    [switch]$Trace,
    [string]$Log = "",
    [switch]$Help
)

# Show help
if ($Help) {
    Write-Host "Usage: .\dual-test.ps1 [-n NUM] [-Release] [-NoBuild] [-Clean] [-Debug] [-Trace] [-Log LEVEL] [-Help]"
    Write-Host ""
    Write-Host "Launch multiple Croh instances for testing Iroh peer-to-peer functionality."
    Write-Host ""
    Write-Host "Options:"
    Write-Host "  -n NUM        Number of instances to launch (default: 2, max: 26)"
    Write-Host "  -Release      Build in release mode"
    Write-Host "  -NoBuild      Skip cargo build"
    Write-Host "  -Clean        Delete all config/data files before launching (fresh start)"
    Write-Host "  -Debug        Set log level to debug"
    Write-Host "  -Trace        Set log level to trace"
    Write-Host "  -Log LEVEL    Set custom log level (e.g., 'croh=debug,croh_core=trace')"
    Write-Host "  -Help         Show this help message"
    Write-Host ""
    Write-Host "Instance directories:"
    Write-Host "  Instance 1 (Alice): Default (%APPDATA%\croh, %LOCALAPPDATA%\croh)"
    Write-Host "  Instance 2+ (Bob, Carol, ...): %TEMP%\croh-{name}"
    exit 0
}

# Validate instance count
if ($n -lt 1) {
    Write-Error "Error: -n requires a positive integer"
    exit 1
}
if ($n -gt 26) {
    Write-Error "Error: Maximum 26 instances supported"
    exit 1
}

# Instance names for display (A-Z)
$INSTANCE_NAMES = @(
    "Alice", "Bob", "Carol", "Dave", "Eve", "Frank", "Grace", "Heidi",
    "Ivan", "Judy", "Karl", "Liam", "Mia", "Noah", "Olivia", "Paul",
    "Quinn", "Rose", "Sam", "Tina", "Uma", "Vince", "Wendy", "Xander",
    "Yara", "Zack"
)

# Determine log level
$LOG_LEVEL = "info"
if ($Debug) {
    $LOG_LEVEL = "debug"
} elseif ($Trace) {
    $LOG_LEVEL = "trace"
} elseif ($Log -ne "") {
    $LOG_LEVEL = $Log
}

# Get project directory
$SCRIPT_DIR = Split-Path -Parent $PSCommandPath
$PROJECT_DIR = Split-Path -Parent $SCRIPT_DIR

Set-Location $PROJECT_DIR

# Build if needed
$BUILD_MODE = if ($Release) { "release" } else { "debug" }
$BINARY = "target\$BUILD_MODE\croh.exe"

if (-not $NoBuild) {
    Write-Host "Building croh ($BUILD_MODE)..."
    if ($Release) {
        cargo build -p croh --release
    } else {
        cargo build -p croh
    }
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Build failed"
        exit 1
    }
}

if (-not (Test-Path $BINARY)) {
    Write-Error "Error: Binary not found at $BINARY"
    Write-Error "Run without -NoBuild to compile first."
    exit 1
}

# Set up directories
$PRIMARY_CONFIG_DIR = Join-Path $env:APPDATA "croh"
$PRIMARY_DATA_DIR = Join-Path $env:LOCALAPPDATA "croh"
$TEMP_DIR = $env:TEMP

# Clean configs if requested
if ($Clean) {
    Write-Host "Cleaning config/data directories..."

    # Remove primary instance directories
    if (Test-Path $PRIMARY_CONFIG_DIR) {
        Remove-Item -Recurse -Force $PRIMARY_CONFIG_DIR
        Write-Host "  Removed: $PRIMARY_CONFIG_DIR"
    }
    if (Test-Path $PRIMARY_DATA_DIR) {
        Remove-Item -Recurse -Force $PRIMARY_DATA_DIR
        Write-Host "  Removed: $PRIMARY_DATA_DIR"
    }

    # Remove secondary instance directories
    for ($i = 1; $i -lt $n; $i++) {
        $name = $INSTANCE_NAMES[$i]
        $name_lower = $name.ToLower()
        $instance_dir = Join-Path $TEMP_DIR "croh-$name_lower"
        if (Test-Path $instance_dir) {
            Remove-Item -Recurse -Force $instance_dir
            Write-Host "  Removed: $instance_dir"
        }
    }

    # Create fresh configs with nicknames
    Write-Host "Creating fresh configs with nicknames..."

    # Primary instance (Alice)
    New-Item -ItemType Directory -Force -Path $PRIMARY_CONFIG_DIR | Out-Null
    $downloads_dir = Join-Path $env:USERPROFILE "Downloads"

    $config = @{
        download_dir = $downloads_dir
        default_relay = $null
        theme = "system"
        croc_path = $null
        device_nickname = $INSTANCE_NAMES[0]
        default_hash = $null
        default_curve = $null
        throttle = $null
        no_local = $false
        window_size = @{ width = 700; height = 600 }
        browse_settings = @{
            show_hidden = $false
            show_protected = $false
            exclude_patterns = @("node_modules", ".git", "__pycache__", "*.tmp", "*.swp")
            allowed_paths = @()
        }
        dnd_mode = "off"
        dnd_message = $null
        show_session_stats = $false
        keep_completed_transfers = $true
        security_posture = "balanced"
        guest_policy = @{
            default_duration_hours = 72
            max_duration_hours = 168
            allow_extensions = $true
            max_extensions = 3
            allow_promotion_requests = $true
            auto_accept_guest_pushes = $true
        }
    }

    $config_json = $config | ConvertTo-Json -Depth 10
    $config_path = Join-Path $PRIMARY_CONFIG_DIR "config.json"
    $config_json | Out-File -FilePath $config_path -Encoding UTF8
    Write-Host "  Created: $config_path (nickname: $($INSTANCE_NAMES[0]))"

    # Secondary instances
    for ($i = 1; $i -lt $n; $i++) {
        $name = $INSTANCE_NAMES[$i]
        $name_lower = $name.ToLower()
        $instance_dir = Join-Path $TEMP_DIR "croh-$name_lower"
        $instance_config_dir = Join-Path $instance_dir "config"
        $instance_downloads_dir = Join-Path $instance_dir "downloads"

        New-Item -ItemType Directory -Force -Path $instance_config_dir | Out-Null

        $config = @{
            download_dir = $instance_downloads_dir
            default_relay = $null
            theme = "system"
            croc_path = $null
            device_nickname = $name
            default_hash = $null
            default_curve = $null
            throttle = $null
            no_local = $false
            window_size = @{ width = 700; height = 600 }
            browse_settings = @{
                show_hidden = $false
                show_protected = $false
                exclude_patterns = @("node_modules", ".git", "__pycache__", "*.tmp", "*.swp")
                allowed_paths = @()
            }
            dnd_mode = "off"
            dnd_message = $null
            show_session_stats = $false
            keep_completed_transfers = $true
            security_posture = "balanced"
            guest_policy = @{
                default_duration_hours = 72
                max_duration_hours = 168
                allow_extensions = $true
                max_extensions = 3
                allow_promotion_requests = $true
                auto_accept_guest_pushes = $true
            }
        }

        $config_json = $config | ConvertTo-Json -Depth 10
        $config_path = Join-Path $instance_config_dir "config.json"
        $config_json | Out-File -FilePath $config_path -Encoding UTF8
        Write-Host "  Created: $config_path (nickname: $name)"
    }
}

# Create directories for secondary instances
for ($i = 1; $i -lt $n; $i++) {
    $name = $INSTANCE_NAMES[$i]
    $name_lower = $name.ToLower()
    $instance_dir = Join-Path $TEMP_DIR "croh-$name_lower"
    $instance_config_dir = Join-Path $instance_dir "config"
    $instance_data_dir = Join-Path $instance_dir "data"
    $instance_downloads_dir = Join-Path $instance_dir "downloads"

    New-Item -ItemType Directory -Force -Path $instance_config_dir | Out-Null
    New-Item -ItemType Directory -Force -Path $instance_data_dir | Out-Null
    New-Item -ItemType Directory -Force -Path $instance_downloads_dir | Out-Null
}

Write-Host ""
Write-Host "Starting $n Croh instance(s)..."
Write-Host "  $($INSTANCE_NAMES[0]): Default config (primary instance)"
for ($i = 1; $i -lt $n; $i++) {
    $name = $INSTANCE_NAMES[$i]
    $name_lower = $name.ToLower()
    $instance_dir = Join-Path $TEMP_DIR "croh-$name_lower"
    Write-Host "  ${name}: $instance_dir"
}
Write-Host ""
Write-Host "To test peer trust:"
Write-Host "  1. In one instance, go to Peers tab and click 'Add Peer'"
Write-Host "  2. Copy the trust code shown"
Write-Host "  3. In another instance, go to Receive tab and enter the code"
Write-Host "  4. Both instances should now show each other as trusted peers"
Write-Host ""
Write-Host "Press Ctrl+C to stop all instances."
Write-Host ""

# Array to store process objects
$PROCESSES = @()

# Launch primary instance (default config)
Write-Host "Launching $($INSTANCE_NAMES[0]) (log level: $LOG_LEVEL)..."
$env:CROH_INSTANCE_NAME = $INSTANCE_NAMES[0]
$env:RUST_LOG = $LOG_LEVEL
$process = Start-Process -FilePath $BINARY -PassThru -WindowStyle Normal
$PROCESSES += $process

# Launch secondary instances with isolated configs
for ($i = 1; $i -lt $n; $i++) {
    Start-Sleep -Milliseconds 500  # Small delay to avoid window overlap

    $name = $INSTANCE_NAMES[$i]
    $name_lower = $name.ToLower()
    $instance_dir = Join-Path $TEMP_DIR "croh-$name_lower"
    $instance_config_dir = Join-Path $instance_dir "config"
    $instance_data_dir = Join-Path $instance_dir "data"
    $instance_downloads_dir = Join-Path $instance_dir "downloads"

    Write-Host "Launching $name (log level: $LOG_LEVEL)..."

    $env:CROH_INSTANCE_NAME = $name
    $env:CROH_CONFIG_DIR = $instance_config_dir
    $env:CROH_DATA_DIR = $instance_data_dir
    $env:CROH_DOWNLOAD_DIR = $instance_downloads_dir
    $env:RUST_LOG = $LOG_LEVEL

    $process = Start-Process -FilePath $BINARY -PassThru -WindowStyle Normal
    $PROCESSES += $process
}

# Cleanup function
function Cleanup {
    Write-Host ""
    Write-Host "Stopping $($PROCESSES.Count) instance(s)..."
    foreach ($process in $PROCESSES) {
        if (-not $process.HasExited) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        }
    }
    Write-Host "Done."
}

# Register cleanup on Ctrl+C
$null = Register-EngineEvent -SourceIdentifier PowerShell.Exiting -Action { Cleanup }

try {
    # Wait for any instance to exit
    Write-Host "Waiting for instances to finish (Ctrl+C to stop all)..."
    while ($true) {
        $running = $false
        foreach ($process in $PROCESSES) {
            if (-not $process.HasExited) {
                $running = $true
                break
            }
        }
        if (-not $running) {
            break
        }
        Start-Sleep -Seconds 1
    }
}
finally {
    Cleanup
}
