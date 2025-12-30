#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Croc GUI Uninstaller for Windows

.DESCRIPTION
    Removes croc-gui and croc-daemon from the system.

.PARAMETER RemoveData
    If specified, also removes configuration and data.
#>

param(
    [switch]$RemoveData
)

$ErrorActionPreference = "Stop"

$InstallDir = "$env:LOCALAPPDATA\croc-gui"
$BinDir = "$InstallDir\bin"
$DataDir = "$InstallDir\data"
$ConfigDir = "$env:APPDATA\croc-gui"

Write-Host "Croc GUI Uninstaller for Windows" -ForegroundColor Red
Write-Host "==================================" -ForegroundColor Red
Write-Host ""

$confirm = Read-Host "This will remove Croc GUI. Continue? [y/N]"
if ($confirm -ne "y" -and $confirm -ne "Y") {
    Write-Host "Cancelled."
    exit 0
}

# Stop and remove service
Write-Host "Stopping service..."
$nssm = Get-Command nssm -ErrorAction SilentlyContinue
if ($nssm) {
    & nssm stop croc-daemon 2>$null
    & nssm remove croc-daemon confirm 2>$null
}

# Remove from PATH
Write-Host "Removing from PATH..."
$currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($currentPath -like "*$BinDir*") {
    $newPath = ($currentPath -split ';' | Where-Object { $_ -ne $BinDir }) -join ';'
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
}

# Remove Start Menu shortcut
Write-Host "Removing Start Menu shortcut..."
$StartMenu = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs"
Remove-Item "$StartMenu\Croc GUI.lnk" -ErrorAction SilentlyContinue

# Remove binaries
Write-Host "Removing binaries..."
Remove-Item $BinDir -Recurse -Force -ErrorAction SilentlyContinue

# Remove data if requested
if ($RemoveData) {
    Write-Host "Removing data and configuration..."
    Remove-Item $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $ConfigDir -Recurse -Force -ErrorAction SilentlyContinue
} else {
    Write-Host ""
    Write-Host "Configuration and data preserved at:"
    Write-Host "  $ConfigDir"
    Write-Host "  $DataDir"
    Write-Host ""
    Write-Host "To remove data, run: .\uninstall.ps1 -RemoveData"
}

Write-Host ""
Write-Host "Uninstallation complete." -ForegroundColor Green



