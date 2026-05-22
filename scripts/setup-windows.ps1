#Requires -Version 5.1
<#
.SYNOPSIS
    AIS Monitor - Windows dependency installer.

.DESCRIPTION
    Downloads and installs the Azure CLI required by AIS Monitor.
    Already-installed tools at the correct version are skipped.
    Requires an internet connection and administrator privileges.
#>
param(
    [switch]$NoPrompt
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$AZ_CLI_VERSION  = '2.65.0'
$AZ_URL          = "https://azcliprod.blob.core.windows.net/msi/azure-cli-${AZ_CLI_VERSION}-x64.msi"
$TMP = $env:TEMP

# ── Helpers ────────────────────────────────────────────────────────────────────
function Write-Step { param([string]$msg) Write-Host "`n>> $msg" -ForegroundColor Cyan }
function Write-Ok   { param([string]$msg) Write-Host "  OK  $msg" -ForegroundColor Green }
function Write-Skip { param([string]$msg) Write-Host "  --  $msg (already installed)" -ForegroundColor DarkGray }
function Write-Warn { param([string]$msg) Write-Host "  !!  $msg" -ForegroundColor Yellow }
function Write-Fail { param([string]$msg) Write-Host "  XX  $msg" -ForegroundColor Red }

function Refresh-Path {
    $machine = [System.Environment]::GetEnvironmentVariable('PATH', 'Machine')
    $user    = [System.Environment]::GetEnvironmentVariable('PATH', 'User')
    $env:PATH = "$machine;$user"
}

function Get-AzCliVersion {
    try {
        $out = az --version 2>&1 | Out-String
        if ($out -match 'azure-cli\s+(\d+\.\d+\.\d+)') { return $Matches[1] }
    } catch { }
    return $null
}

function Install-Msi {
    param([string]$Url, [string]$Label)
    $file = Join-Path $TMP ([System.IO.Path]::GetFileName($Url))
    Write-Host "  Downloading $Label ..."
    try {
        $wc = New-Object System.Net.WebClient
        $wc.DownloadFile($Url, $file)
        $sizeMB = [math]::Round((Get-Item $file).Length / 1MB, 1)
        Write-Host "  Downloaded $sizeMB MB - installing (silent) ..."
        $proc = Start-Process msiexec -ArgumentList "/i `"$file`" /qn /norestart" -Wait -PassThru -NoNewWindow
        if ($proc.ExitCode -ne 0 -and $proc.ExitCode -ne 3010) {
            throw "msiexec exited with code $($proc.ExitCode)"
        }
        Refresh-Path
    } finally {
        if (Test-Path $file) { Remove-Item $file -Force }
    }
}

# ── Elevation check ───────────────────────────────────────────────────────────
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host "Requesting administrator privileges..." -ForegroundColor Yellow
    Start-Process powershell "-ExecutionPolicy Bypass -File `"$PSCommandPath`"" -Verb RunAs
    exit
}

Write-Host ""
Write-Host "  AIS Monitor - Windows Setup" -ForegroundColor White
Write-Host "  ===========================" -ForegroundColor DarkGray
Write-Host ""
Write-Host "  Azure CLI  $AZ_CLI_VERSION"
Write-Host ""

# ── Azure CLI ──────────────────────────────────────────────────────────────
Write-Step "Azure CLI $AZ_CLI_VERSION"
$azVer = Get-AzCliVersion
if ($azVer -eq $AZ_CLI_VERSION) {
    Write-Skip "az $azVer"
} else {
    if ($azVer) { Write-Warn "Found az $azVer - will replace with $AZ_CLI_VERSION" }
    Install-Msi $AZ_URL "Azure CLI $AZ_CLI_VERSION"
    Refresh-Path
    $azVer = Get-AzCliVersion
    if ($azVer) { Write-Ok "az $azVer" } else { Write-Fail "az not found after install" }
}

# ── Summary ───────────────────────────────────────────────────────────────────
Refresh-Path
Write-Host ""
Write-Host "  ===========================" -ForegroundColor DarkGray
Write-Host ""

$azVer = Get-AzCliVersion
if ($azVer) {
    Write-Host "  OK  az         $azVer" -ForegroundColor Green
} else {
    Write-Host "  XX  az         not found" -ForegroundColor Red
}

Write-Host ""
Write-Host "  Setup complete. Launch ais-monitor.exe to start." -ForegroundColor Green
Write-Host ""

if (-not $NoPrompt) { Read-Host "Press Enter to close" }
