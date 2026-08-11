#!/usr/bin/env bash
# harness-message-capture installer.
#
# Usage (from a checkout):
#   HMC_ENDPOINT=https://your-server/ingest HMC_TOKEN=hmc_xxx ./dist/install.sh
#
# Or one-shot from GitHub once releases exist:
#   HMC_BINARY_URL=https://github.com/ORG/harness-message-capture/releases/latest/download/hmc-aarch64-apple-darwin \
#   HMC_ENDPOINT=https://your-server/ingest HMC_TOKEN=hmc_xxx \
#   bash -c "$(curl -fsSL https://raw.githubusercontent.com/ORG/harness-message-capture/main/dist/install.sh)"
#
# Note: a binary fetched via curl/bash is NOT flagged with the macOS quarantine
# attribute, so no Apple code-signing/notarization is required for it to run.
set -euo pipefail

BIN_DIR="${HMC_BIN_DIR:-$HOME/.local/bin}"
BIN_PATH="$BIN_DIR/hmc"

: "${HMC_ENDPOINT:?set HMC_ENDPOINT to your ingest URL}"
: "${HMC_TOKEN:?set HMC_TOKEN to this machine's enrollment token}"

mkdir -p "$BIN_DIR"

if [ -n "${HMC_BINARY_URL:-}" ]; then
  echo "==> Downloading binary from $HMC_BINARY_URL"
  curl -fsSL "$HMC_BINARY_URL" -o "$BIN_PATH"
  chmod +x "$BIN_PATH"
elif command -v cargo >/dev/null 2>&1; then
  echo "==> Building from source with cargo (release)"
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  ( cd "$SCRIPT_DIR" && cargo build --release )
  cp "$SCRIPT_DIR/target/release/hmc" "$BIN_PATH"
else
  echo "error: no HMC_BINARY_URL set and cargo not found — cannot obtain binary" >&2
  exit 1
fi

echo "==> Enrolling"
"$BIN_PATH" enroll --endpoint "$HMC_ENDPOINT" --token "$HMC_TOKEN"

echo "==> Installing background LaunchAgent"
"$BIN_PATH" install

echo "==> Done. Status:"
"$BIN_PATH" status
