# ais-monitor-tui Windows installer.
#
# Usage (one-liner, paste into PowerShell):
#
#   Invoke-WebRequest https://raw.githubusercontent.com/Bennekrouf/ais-monitor/master/scripts/install-tui.ps1 -UseBasicParsing | Invoke-Expression
#
# What this does:
#   1. Resolves the current build on mayorana.ch.
#   2. Downloads `ais-monitor-tui-x86_64-pc-windows-msvc.exe` into
#      `%USERPROFILE%\bin\ais-monitor-tui.exe`.
#   3. Removes the "downloaded from internet" mark-of-the-web so SmartScreen
#      doesn't pop on first run.
#   4. Adds `%USERPROFILE%\bin` to the user PATH if it isn't already.
#   5. Reports whether `az` (Azure CLI) is installed; offers a winget install
#      command if not.
#   6. Prints the next step.
#
# Idempotent: rerunning re-downloads the latest binary and re-checks PATH.
# No admin rights required (writes only to the user profile + user PATH).

$ErrorActionPreference = "Stop"
$Repo = "Bennekrouf/ais-monitor"
$Asset = "ais-monitor-tui-x86_64-pc-windows-msvc.exe"
$InstallDir = Join-Path $env:USERPROFILE "bin"
$Target = Join-Path $InstallDir "ais-monitor-tui.exe"

# Detect whether we were launched into our own window (powershell -File ...,
# or right-click Run with PowerShell) vs. piped into an already-open shell
# (Invoke-Expression). In the standalone case the window closes the instant
# the script exits, hiding any errors — so we Pause at the end.
$Standalone = -not [string]::IsNullOrEmpty($MyInvocation.MyCommand.Path)

function Info($msg)  { Write-Host "==> $msg" -ForegroundColor Cyan }
function Warn($msg)  { Write-Host "!!  $msg" -ForegroundColor Yellow }
function Ok($msg)    { Write-Host "OK  $msg" -ForegroundColor Green }
function Bad($msg)   { Write-Host "XX  $msg" -ForegroundColor Red }

function Pause-IfStandalone {
    if ($Standalone) {
        Write-Host ""
        Write-Host "Press Enter to close this window..." -ForegroundColor DarkGray
        try { [void](Read-Host) } catch {}
    }
}

# ── Wrap everything in a top-level try/finally so errors are SEEN, not flashed
# off-screen on the way out. Every failure path falls through to the pause.
try {

# ── 1. Resolve the download URL ──────────────────────────────────────────────
# Binaries are served from mayorana.ch, where `latest/` is a stable path that
# release CI overwrites. That removes the GitHub API round-trip this used to
# do, along with its rate limits and its "asset missing from the release"
# failure mode - a missing build is now just a 404 on the download below.
$DlBase = "https://mayorana.ch/downloads/ais-monitor"
$url    = "$DlBase/latest/$Asset"

# Version lookup is cosmetic and must never block the install.
$tag = $null
try {
    $manifest = Invoke-RestMethod -Uri "$DlBase/latest/latest.json" -UseBasicParsing
    $tag = $manifest.tag
} catch { }
if ($tag) { Ok "Latest release: $tag" } else { Info "Downloading the current build..." }

# ── 2. Download ──────────────────────────────────────────────────────────────
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Info "Downloading $Asset to $Target ..."
# `Invoke-WebRequest` is fine for a single binary; -UseBasicParsing keeps it
# light even on Windows PowerShell 5.1.
try {
    Invoke-WebRequest -Uri $url -OutFile $Target -UseBasicParsing
} catch {
    Bad  "Could not download $Asset from $url"
    Bad  "$($_.Exception.Message)"
    Bad  ""
    Bad  "A 404 here means this release did not build the Windows TUI binary."
    Bad  "Workarounds:"
    Bad  "  1. Push a new tag so CI builds it."
    Bad  "  2. cargo install --git https://github.com/$Repo ais-monitor-tui"
    throw "download-failed"
}
Ok   "Saved $([math]::Round((Get-Item $Target).Length / 1MB, 1)) MB."

# ── 3. Clear mark-of-the-web (avoids SmartScreen on first run) ───────────────
try {
    Unblock-File -Path $Target -ErrorAction Stop
    Ok "Cleared SmartScreen mark."
} catch {
    Warn "Could not Unblock-File ($($_.Exception.Message))."
    Warn "First run may show a SmartScreen prompt - click 'More info > Run anyway'."
}

# ── 4. Ensure %USERPROFILE%\bin is on user PATH ──────────────────────────────
$userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if (-not $userPath) { $userPath = "" }
$onPath = $userPath.Split(';') | Where-Object { $_ -eq $InstallDir }
if (-not $onPath) {
    Info "Adding $InstallDir to user PATH..."
    $newPath = if ($userPath) { "$InstallDir;$userPath" } else { $InstallDir }
    [Environment]::SetEnvironmentVariable("PATH", $newPath, "User")
    Ok   "User PATH updated. Open a new terminal for it to take effect."
} else {
    Ok   "$InstallDir already on PATH."
}

# ── 5. Azure CLI check ──────────────────────────────────────────────────────
$azCmd = Get-Command az -ErrorAction SilentlyContinue
if ($azCmd) {
    Ok "Azure CLI found: $($azCmd.Source)"
} else {
    Warn "Azure CLI (az) not found. Install it with:"
    Write-Host "      winget install -e --id Microsoft.AzureCLI" -ForegroundColor Yellow
    Write-Host "    then run: az login" -ForegroundColor Yellow
}

# ── 6. Next steps ───────────────────────────────────────────────────────────
Write-Host ""
Write-Host "----------------------------------------------------------" -ForegroundColor DarkGray
Ok "ais-monitor-tui $tag installed."
Write-Host ""
Write-Host "Next:" -ForegroundColor White
Write-Host "  1. Open a NEW Windows Terminal tab (so PATH is reloaded)." -ForegroundColor White
Write-Host "  2. Sign in once:  " -NoNewline -ForegroundColor White
Write-Host "az login" -ForegroundColor Cyan
Write-Host "  3. Launch:        " -NoNewline -ForegroundColor White
Write-Host "ais-monitor-tui" -ForegroundColor Cyan
Write-Host ""
Write-Host "If your machine has no browser (Server Core, SSH session), use:" -ForegroundColor DarkGray
Write-Host "  ais-monitor-tui --device-code" -ForegroundColor DarkGray
Write-Host "----------------------------------------------------------" -ForegroundColor DarkGray

} catch {
    # Already-printed errors throw a known string; only print unexpected ones.
    $known = @("github-unreachable", "asset-missing")
    if ($_.Exception.Message -notin $known) {
        Bad ""
        Bad "Installer aborted: $($_.Exception.Message)"
        if ($_.ScriptStackTrace) {
            Write-Host $_.ScriptStackTrace -ForegroundColor DarkGray
        }
    }
} finally {
    Pause-IfStandalone
}
