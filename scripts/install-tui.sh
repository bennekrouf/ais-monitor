#!/usr/bin/env sh
# ais-monitor-tui macOS / Linux installer.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Bennekrouf/ais-monitor/master/scripts/install-tui.sh | sh
#
# What it does:
#   1. Detects platform → picks the right release asset.
#   2. Downloads it to /usr/local/bin (or $HOME/.local/bin if unprivileged).
#   3. chmods +x. On macOS, strips the quarantine xattr so Gatekeeper doesn't
#      block first run.
#   4. Reports whether `az` is on PATH.

set -eu

REPO="Bennekrouf/ais-monitor"
PREFIX="${PREFIX:-/usr/local}"

os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
    Darwin/arm64)     target="aarch64-apple-darwin" ;;
    Darwin/x86_64)    target="aarch64-apple-darwin" ;;  # only arm64 published; runs under Rosetta
    Linux/x86_64)     target="x86_64-unknown-linux-gnu" ;;
    *)
        echo "Unsupported platform: $os/$arch" >&2
        echo "Fallback: cargo install --git https://github.com/$REPO ais-monitor-tui" >&2
        exit 1
        ;;
esac

asset="ais-monitor-tui-$target"
echo "==> Querying latest release…"
tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
[ -n "$tag" ] || { echo "Could not parse latest release tag." >&2; exit 1; }
url="https://github.com/$REPO/releases/download/$tag/$asset"
echo "    Latest: $tag"

# Pick a writable directory: try PREFIX/bin, fall back to $HOME/.local/bin.
SUDO=""
if [ -w "$PREFIX/bin" ] 2>/dev/null; then
    install_dir="$PREFIX/bin"
elif command -v sudo >/dev/null 2>&1 && [ -d "$PREFIX/bin" ]; then
    install_dir="$PREFIX/bin"
    SUDO="sudo"
else
    install_dir="$HOME/.local/bin"
    mkdir -p "$install_dir"
fi
BIN="$install_dir/ais-monitor-tui"

echo "==> Downloading $asset → $BIN"
tmp=$(mktemp)
curl -fSL --progress-bar "$url" -o "$tmp"
$SUDO mv "$tmp" "$BIN"
$SUDO chmod +x "$BIN"

# macOS: clear quarantine so Gatekeeper allows first run.
if [ "$os" = "Darwin" ]; then
    $SUDO xattr -d com.apple.quarantine "$BIN" 2>/dev/null || true
fi

echo "✓ Installed: $BIN"

# Path sanity check.
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) echo "!  $install_dir is not on PATH — add it to your shell rc." ;;
esac

# az check.
if command -v az >/dev/null 2>&1; then
    echo "✓ Azure CLI: $(command -v az)"
else
    echo "!  Azure CLI not found. Install with your package manager (brew install azure-cli, etc.)"
fi

echo
echo "Next:"
echo "  az login"
echo "  ais-monitor-tui"
echo
echo "Headless / SSH? Use:  ais-monitor-tui --device-code"
