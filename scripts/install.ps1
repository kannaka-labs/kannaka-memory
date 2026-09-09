# Kannaka Constellation Installer — native Windows (PowerShell)
#
# Usage from PowerShell:
#   iwr -useb https://install.ninja-portal.com/kannaka.ps1 | iex
# or:
#   .\install.ps1
#
# Downloads kannaka.exe + kannaka-tui.exe from GitHub Releases, installs
# them to %LOCALAPPDATA%\Programs\kannaka, adds that directory to user
# PATH (no admin needed), then runs `kannaka init` if first install.
#
# Companion to scripts/install.sh (Unix / Git Bash). Use this one when the
# target machine doesn't have Git Bash and wants `kannaka` + `kannaka-tui`
# on the native cmd.exe / PowerShell PATH.

$ErrorActionPreference = "Stop"

$Repo = "kannaka-labs/kannaka-memory"
$InstallDir = if ($env:KANNAKA_INSTALL_DIR) { $env:KANNAKA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\kannaka" }
$Version = if ($env:KANNAKA_VERSION) { $env:KANNAKA_VERSION } else { "latest" }

Write-Host "Kannaka Constellation Installer"
Write-Host "  Target:  $InstallDir"
Write-Host "  Version: $Version"
Write-Host ""

# Arch detection — Windows kannaka is x86_64-only today.
$arch = "x86_64"
if ([Environment]::Is64BitOperatingSystem -eq $false) {
    Write-Error "Kannaka requires a 64-bit Windows install."
    exit 1
}

# Build download URLs.
if ($Version -eq "latest") {
    $KannakaUrl = "https://github.com/$Repo/releases/latest/download/kannaka-windows-$arch.exe"
    $TuiUrl     = "https://github.com/$Repo/releases/latest/download/kannaka-tui-windows-$arch.exe"
} else {
    $KannakaUrl = "https://github.com/$Repo/releases/download/v$Version/kannaka-windows-$arch.exe"
    $TuiUrl     = "https://github.com/$Repo/releases/download/v$Version/kannaka-tui-windows-$arch.exe"
}

# Ensure install dir.
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# Download kannaka.exe (required).
$KannakaTmp = Join-Path $env:TEMP "kannaka-install.exe"
Write-Host "  Downloading kannaka.exe: $KannakaUrl"
Invoke-WebRequest -Uri $KannakaUrl -OutFile $KannakaTmp -UseBasicParsing
Move-Item -Force $KannakaTmp (Join-Path $InstallDir "kannaka.exe")
Write-Host "  Installed: kannaka.exe"

# Download kannaka-tui.exe (optional — older releases don't have it).
$TuiTmp = Join-Path $env:TEMP "kannaka-tui-install.exe"
try {
    Write-Host "  Downloading kannaka-tui.exe: $TuiUrl"
    Invoke-WebRequest -Uri $TuiUrl -OutFile $TuiTmp -UseBasicParsing -ErrorAction Stop
    Move-Item -Force $TuiTmp (Join-Path $InstallDir "kannaka-tui.exe")
    Write-Host "  Installed: kannaka-tui.exe (chat-first terminal dashboard)"
} catch {
    Write-Host "  Note: kannaka-tui not available for this release (skipping)."
}

# Add install dir to USER PATH if missing.
$userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($userPath -notlike "*$InstallDir*") {
    Write-Host "  Adding $InstallDir to user PATH"
    $newPath = if ([string]::IsNullOrEmpty($userPath)) { $InstallDir } else { "$userPath;$InstallDir" }
    [Environment]::SetEnvironmentVariable("PATH", $newPath, "User")
    # Also update current session so the user can run `kannaka` immediately
    # without opening a new shell.
    $env:PATH = "$env:PATH;$InstallDir"
    Write-Host "  PATH updated. New shells will pick this up automatically."
} else {
    Write-Host "  $InstallDir already on PATH."
}

# First-install detection: run `kannaka init` if ~/.kannaka doesn't exist.
$DataDir = Join-Path $env:USERPROFILE ".kannaka"
if (-not (Test-Path $DataDir)) {
    Write-Host ""
    Write-Host "  First install detected. Running setup..."
    & (Join-Path $InstallDir "kannaka.exe") init
}

Write-Host ""
Write-Host "Installed:"
Write-Host "    kannaka              # wave-interference memory CLI"
Write-Host "    kannaka-tui          # chat-first terminal dashboard"
Write-Host ""
Write-Host "Optional companion (separate install):"
Write-Host "    kannaktopus          # ghost-frequency multi-agent orchestrator"
Write-Host "      npm install -g kannaktopus    # requires Node 18+"
Write-Host ""
Write-Host "Try:"
Write-Host "    kannaka --help"
Write-Host "    kannaka status"
