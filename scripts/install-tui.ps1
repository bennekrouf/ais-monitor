# ais-monitor-tui Windows installer.
#
# Usage (one-liner, paste into any modern PowerShell):
#
#   iwr https://raw.githubusercontent.com/Bennekrouf/ais-monitor/master/scripts/install-tui.ps1 | iex
#
# What this does:
#   1. Fetches the latest GitHub Release.
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

function Info($msg)  { Write-Host "==> $msg" -ForegroundColor Cyan }
function Warn($msg)  { Write-Host "!!  $msg" -ForegroundColor Yellow }
function Ok($msg)    { Write-Host "✓   $msg" -ForegroundColor Green }
function Bad($msg)   { Write-Host "✗   $msg" -ForegroundColor Red }

# ── 1. Resolve latest release ────────────────────────────────────────────────
Info "Querying latest ais-monitor release…"
try {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" `
                                 -UseBasicParsing `
                                 -Headers @{ "User-Agent" = "ais-monitor-installer" }
} catch {
    Bad  "Could not reach GitHub: $($_.Exception.Message)"
    Bad  "If you're behind a corporate proxy, set HTTPS_PROXY and retry."
    exit 1
}
$tag = $release.tag_name
$url = $release.assets | Where-Object { $_.name -eq $Asset } | Select-Object -ExpandProperty browser_download_url
if (-not $url) {
    Bad  "Release $tag has no asset named '$Asset'."
    Bad  "Possible: the Windows TUI binary wasn't built for this release."
    Bad  "Fallback: cargo install --git https://github.com/$Repo ais-monitor-tui"
    exit 1
}
Ok   "Latest release: $tag"

# ── 2. Download ──────────────────────────────────────────────────────────────
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Info "Downloading $Asset to $Target …"
# `Invoke-WebRequest` is fine for a single binary; -UseBasicParsing keeps it
# light even on Windows PowerShell 5.1.
Invoke-WebRequest -Uri $url -OutFile $Target -UseBasicParsing
Ok   "Saved $([math]::Round((Get-Item $Target).Length / 1MB, 1)) MB."

# ── 3. Clear mark-of-the-web (avoids SmartScreen on first run) ───────────────
try {
    Unblock-File -Path $Target -ErrorAction Stop
    Ok "Cleared SmartScreen mark."
} catch {
    Warn "Could not Unblock-File ($($_.Exception.Message))."
    Warn "First run may show a SmartScreen prompt — click 'More info → Run anyway'."
}

# ── 4. Ensure %USERPROFILE%\bin is on user PATH ──────────────────────────────
$userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if (-not $userPath) { $userPath = "" }
$onPath = $userPath.Split(';') | Where-Object { $_ -eq $InstallDir }
if (-not $onPath) {
    Info "Adding $InstallDir to user PATH…"
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
Write-Host "──────────────────────────────────────────────────────────" -ForegroundColor DarkGray
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
Write-Host "──────────────────────────────────────────────────────────" -ForegroundColor DarkGray
