#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Croc GUI Installer for Windows

.DESCRIPTION
    Installs croc-gui and croc-daemon to the system.
    Optionally installs as a Windows service using NSSM.

.PARAMETER InstallService
    If specified, installs croc-daemon as a Windows service.
#>

param(
    [switch]$InstallService
)

$ErrorActionPreference = "Stop"

# Configuration
$InstallDir = "$env:LOCALAPPDATA\croc-gui"
$BinDir = "$InstallDir\bin"
$DataDir = "$InstallDir\data"
$ConfigDir = "$env:APPDATA\croc-gui"

Write-Host "Croc GUI Installer for Windows" -ForegroundColor Green
Write-Host "================================" -ForegroundColor Green
Write-Host ""

# Check for croc
Write-Host -NoNewline "Checking for croc... "
$crocPath = Get-Command croc -ErrorAction SilentlyContinue
if ($crocPath) {
    Write-Host "found ($($crocPath.Source))" -ForegroundColor Green
} else {
    Write-Host "not found" -ForegroundColor Red
    Write-Host ""
    Write-Host "Croc is required. Install it with:"
    Write-Host "  scoop install croc"
    Write-Host "  choco install croc"
    Write-Host "  winget install schollz.croc"
    Write-Host ""
    $continue = Read-Host "Continue without croc? [y/N]"
    if ($continue -ne "y" -and $continue -ne "Y") {
        exit 1
    }
}

# Find binaries
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent (Split-Path -Parent $ScriptDir)

$GuiBin = $null
$DaemonBin = $null

if (Test-Path "$RepoRoot\target\release\croc-gui.exe") {
    Write-Host "Using pre-built binaries from target\release\"
    $GuiBin = "$RepoRoot\target\release\croc-gui.exe"
    $DaemonBin = "$RepoRoot\target\release\croc-daemon.exe"
} elseif (Test-Path ".\croc-gui.exe") {
    Write-Host "Using binaries from current directory"
    $GuiBin = ".\croc-gui.exe"
    $DaemonBin = ".\croc-daemon.exe"
} else {
    Write-Host "Building from source..."
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) {
        Write-Host "Error: cargo not found. Please install Rust first." -ForegroundColor Red
        Write-Host "Visit: https://rustup.rs/"
        exit 1
    }
    
    Push-Location $RepoRoot
    cargo build --release
    Pop-Location
    
    $GuiBin = "$RepoRoot\target\release\croc-gui.exe"
    $DaemonBin = "$RepoRoot\target\release\croc-daemon.exe"
}

# Create directories
Write-Host ""
Write-Host "Creating directories..."
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null

# Copy binaries
Write-Host "Installing binaries to $BinDir..."
Copy-Item $GuiBin "$BinDir\croc-gui.exe" -Force
Copy-Item $DaemonBin "$BinDir\croc-daemon.exe" -Force

# Add to PATH
Write-Host "Adding to PATH..."
$currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($currentPath -notlike "*$BinDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$currentPath;$BinDir", "User")
    $env:Path = "$env:Path;$BinDir"
    Write-Host "  Added $BinDir to user PATH"
} else {
    Write-Host "  Already in PATH"
}

# Create Start Menu shortcut
Write-Host "Creating Start Menu shortcut..."
$StartMenu = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs"
$WScriptShell = New-Object -ComObject WScript.Shell
$Shortcut = $WScriptShell.CreateShortcut("$StartMenu\Croc GUI.lnk")
$Shortcut.TargetPath = "$BinDir\croc-gui.exe"
$Shortcut.WorkingDirectory = $BinDir
$Shortcut.Description = "Croc GUI - File Transfer Application"
$Shortcut.Save()

# Install as service (optional)
if ($InstallService) {
    Write-Host ""
    Write-Host "Installing as Windows service..."
    
    # Check for NSSM
    $nssm = Get-Command nssm -ErrorAction SilentlyContinue
    if (-not $nssm) {
        Write-Host "NSSM not found. Installing via Scoop..." -ForegroundColor Yellow
        
        $scoop = Get-Command scoop -ErrorAction SilentlyContinue
        if (-not $scoop) {
            Write-Host "Please install NSSM manually: https://nssm.cc/" -ForegroundColor Red
            Write-Host "Or install Scoop first: https://scoop.sh/" -ForegroundColor Red
        } else {
            scoop install nssm
            $nssm = Get-Command nssm -ErrorAction SilentlyContinue
        }
    }
    
    if ($nssm) {
        # Remove existing service if present
        & nssm stop croc-daemon 2>$null
        & nssm remove croc-daemon confirm 2>$null
        
        # Install service
        & nssm install croc-daemon "$BinDir\croc-daemon.exe" run
        & nssm set croc-daemon AppDirectory $DataDir
        & nssm set croc-daemon DisplayName "Croc GUI Daemon"
        & nssm set croc-daemon Description "Headless file transfer daemon for Croc GUI"
        & nssm set croc-daemon Start SERVICE_AUTO_START
        & nssm set croc-daemon AppStdout "$DataDir\daemon.log"
        & nssm set croc-daemon AppStderr "$DataDir\daemon.log"
        & nssm set croc-daemon AppRotateFiles 1
        & nssm set croc-daemon AppRotateBytes 1048576
        
        Write-Host "Service installed. Start with: nssm start croc-daemon"
    }
}

Write-Host ""
Write-Host "Installation complete!" -ForegroundColor Green
Write-Host ""
Write-Host "Usage:"
Write-Host "  croc-gui              # Launch the GUI"
Write-Host "  croc-daemon run       # Run the daemon manually"
Write-Host "  croc-daemon status    # Check daemon status"
Write-Host "  croc-daemon receive <code>  # Receive a file"
Write-Host ""
Write-Host "Configuration: $ConfigDir\config.json"
Write-Host "Data directory: $DataDir"

if (-not $InstallService) {
    Write-Host ""
    Write-Host "To install as a Windows service, run:"
    Write-Host "  .\install.ps1 -InstallService"
}



