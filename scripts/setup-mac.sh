#!/bin/bash
set -e

# One-time setup for ais-monitor on macOS.
# Installs: Homebrew, Azure CLI
# Already-installed tools are skipped automatically.

info()  { echo "[info]  $*"; }
ok()    { echo "[ok]    $*"; }
skip()  { echo "[skip]  $*"; }
warn()  { echo "[warn]  $*"; }

# ── Homebrew ─────────────────────────────────────────────────────────────────
if ! command -v brew &>/dev/null; then
    info "Installing Homebrew..."
    /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
    eval "$(/opt/homebrew/bin/brew shellenv 2>/dev/null || /usr/local/bin/brew shellenv)"
    ok "Homebrew installed"
else
    skip "Homebrew already installed ($(brew --version | head -1))"
fi

# ── Azure CLI ────────────────────────────────────────────────────────────────
if ! command -v az &>/dev/null; then
    info "Installing Azure CLI..."
    brew install azure-cli
    ok "Azure CLI installed ($(az version --query '"azure-cli"' -o tsv))"
else
    skip "Azure CLI already installed ($(az version --query '"azure-cli"' -o tsv 2>/dev/null || az --version | head -1))"
fi

echo ""
echo "Setup complete. Run ./ais-monitor to start the app."
