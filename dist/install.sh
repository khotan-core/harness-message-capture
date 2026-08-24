#!/usr/bin/env bash
# khotan-observer installer — download from public GitHub Releases.
#
# One-liner:
#   curl -fsSL https://raw.githubusercontent.com/khotan-core/harness-message-capture/main/dist/install.sh | bash
#
# Optional env overrides:
#   KHOTAN_OBSERVER_VERSION=v0.1.0   # pin a release tag (default: latest)
#   KHOTAN_OBSERVER_BIN_DIR=~/.local/bin
#   KHOTAN_OBSERVER_REPO=khotan-core/harness-message-capture
#
# After install:
#   khotan-observer configure
#   khotan-observer run
#   (OR khotan-observer start if you want it to run in the background)
# Later upgrades:
#   khotan-observer update
#
# Note: a binary fetched via curl is NOT flagged with the macOS quarantine
# attribute, so no Apple code-signing/notarization is required for it to run.
set -euo pipefail

REPO="${KHOTAN_OBSERVER_REPO:-khotan-core/harness-message-capture}"
BIN_DIR="${KHOTAN_OBSERVER_BIN_DIR:-$HOME/.local/bin}"
BIN_NAME="khotan-observer"
BIN_PATH="$BIN_DIR/$BIN_NAME"
VERSION="${KHOTAN_OBSERVER_VERSION:-latest}"

die() { echo "error: $*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }

need curl
need shasum

# --- detect macOS architecture ---------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"
[[ "$os" == "Darwin" ]] || die "only macOS is supported (got $os)"

case "$arch" in
  arm64)  TARGET="aarch64-apple-darwin" ;;
  x86_64) TARGET="x86_64-apple-darwin" ;;
  *)      die "unsupported architecture: $arch" ;;
esac

ASSET="${BIN_NAME}-${TARGET}"
CHECKSUM_ASSET="${ASSET}.sha256"

if [[ "$VERSION" == "latest" ]]; then
  BASE="https://github.com/${REPO}/releases/latest/download"
else
  BASE="https://github.com/${REPO}/releases/download/${VERSION}"
fi

BINARY_URL="${BASE}/${ASSET}"
CHECKSUM_URL="${BASE}/${CHECKSUM_ASSET}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "==> Downloading ${ASSET} (${VERSION})"
curl -fsSL "$BINARY_URL" -o "$tmp/$ASSET"
curl -fsSL "$CHECKSUM_URL" -o "$tmp/$CHECKSUM_ASSET"

echo "==> Verifying checksum"
# Accept either `HASH` or `HASH  filename` formats.
expected="$(awk '{print $1}' "$tmp/$CHECKSUM_ASSET")"
[[ -n "$expected" ]] || die "empty checksum file"
actual="$(shasum -a 256 "$tmp/$ASSET" | awk '{print $1}')"
[[ "$expected" == "$actual" ]] || die "checksum mismatch (expected $expected, got $actual)"

# --- upgrade safely ---------------------------------------------------------
# If a previous version is running, it holds the binary's inode mapped. Writing
# over it in place leaves the kernel's cached code hash out of sync with the
# file, and every later exec dies with SIGKILL ("zsh: killed"). So: stop the
# agent, replace via a fresh inode, and restart only if it was running before.
WAS_RUNNING=0
if launchctl list 2>/dev/null | grep -q "com.khotan.observer"; then
  WAS_RUNNING=1
  echo "==> Stopping running observer before upgrade"
  launchctl unload "$HOME/Library/LaunchAgents/com.khotan.observer.plist" 2>/dev/null || true
  sleep 1
fi

echo "==> Installing to $BIN_PATH"
mkdir -p "$BIN_DIR"
rm -f "$BIN_PATH"                      # new inode, never overwrite in place
cp "$tmp/$ASSET" "$BIN_PATH"
chmod 755 "$BIN_PATH"

# Fail loudly here rather than leaving a binary that gets killed on exec.
if ! "$BIN_PATH" status >/dev/null 2>&1; then
  code=$?
  if [[ $code -eq 137 ]]; then
    die "installed binary was killed on exec (code signature/inode issue)"
  fi
  # Any other non-zero is fine: `status` exits non-zero when not yet configured.
fi

# --- PATH guidance ---------------------------------------------------------
if ! echo ":$PATH:" | grep -q ":$BIN_DIR:"; then
  echo ""
  echo "NOTE: $BIN_DIR is not on your PATH."
  echo "Add this to your shell profile (~/.zshrc or ~/.bashrc):"
  echo "  export PATH=\"$BIN_DIR:\$PATH\""
  echo ""
fi

echo "==> Installed: $BIN_PATH"
"$BIN_PATH" docs --write >/dev/null || true

CONFIG="${HOME}/.config/harness-message-capture/config.toml"

print_configure_why() {
  local prefix="${1:-}"
  echo "${prefix}Select the repositories you want to track."
  echo "${prefix}NOTHING that is not selected here will be tracked."
  echo "${prefix}MESSAGES will NOT be sent for those repositories!"
}

print_next_steps() {
  echo ""
  echo "Next steps:"
  echo ""
  echo "1. Please run khotan-observer configure and select the repositories you want to track."
  print_configure_why "   "
  echo ""
  echo "2. khotan-observer run"
  echo "   (OR khotan-observer start if you want it to run in the background)"
  echo ""
}

allow_list_empty() {
  if [[ ! -f "$CONFIG" ]]; then
    return 0
  fi
  "$BIN_PATH" status 2>/dev/null | grep -q '^allow     : none'
}

open_configure() {
  if [[ -t 0 ]]; then
    "$BIN_PATH" configure
  elif [[ -e /dev/tty ]]; then
    "$BIN_PATH" configure < /dev/tty
  else
    "$BIN_PATH" configure
  fi
}

if allow_list_empty; then
  echo ""
  echo "No repositories are selected yet."
  print_configure_why
  echo ""
  if [[ "${KHOTAN_OBSERVER_SKIP_CONFIGURE:-}" == "1" ]]; then
    echo "==> Skipping configure"
  else
    echo "==> Opening configure"
    open_configure
  fi
fi
echo "==> Allow list: $CONFIG"

if [[ "$WAS_RUNNING" -eq 1 ]]; then
  echo "==> Restarting background observer"
  "$BIN_PATH" start
  echo ""
  echo "Upgrade complete."
  print_next_steps
  exit 0
fi

print_next_steps
