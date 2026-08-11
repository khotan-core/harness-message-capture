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
#   khotan-observer configure --endpoint https://your-ingest/ingest
#   khotan-observer run
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

echo "==> Installing to $BIN_PATH"
mkdir -p "$BIN_DIR"
install -m 755 "$tmp/$ASSET" "$BIN_PATH"

# --- PATH guidance ---------------------------------------------------------
if ! echo ":$PATH:" | grep -q ":$BIN_DIR:"; then
  echo ""
  echo "NOTE: $BIN_DIR is not on your PATH."
  echo "Add this to your shell profile (~/.zshrc or ~/.bashrc):"
  echo "  export PATH=\"$BIN_DIR:\$PATH\""
  echo ""
fi

"$BIN_PATH" >/dev/null 2>&1 || true
echo "==> Installed: $BIN_PATH"
echo ""
echo "Next steps:"
echo "  1. khotan-observer configure --endpoint https://YOUR_INGEST/ingest"
echo "     (you'll be prompted for the enrollment token)"
echo "  2. khotan-observer run          # foreground, easy to QA"
echo "     or: khotan-observer start    # background LaunchAgent"
echo ""
