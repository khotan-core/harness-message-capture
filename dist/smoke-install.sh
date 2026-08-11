#!/usr/bin/env bash
# Offline smoke test for the installer checksum + install path logic.
# Does not hit the network. Run from repo root:
#   ./dist/smoke-install.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_SRC="$ROOT/target/release/khotan-observer"
[[ -x "$BIN_SRC" ]] || { echo "build release binary first: cargo build --release" >&2; exit 1; }

fake_release="$(mktemp -d)"
install_root="$(mktemp -d)"
trap 'rm -rf "$fake_release" "$install_root"' EXIT

arch="$(uname -m)"
case "$arch" in
  arm64)  TARGET="aarch64-apple-darwin" ;;
  x86_64) TARGET="x86_64-apple-darwin" ;;
  *) echo "unsupported arch: $arch" >&2; exit 1 ;;
esac

ASSET="khotan-observer-${TARGET}"
cp "$BIN_SRC" "$fake_release/$ASSET"
shasum -a 256 "$fake_release/$ASSET" | awk '{print $1}' > "$fake_release/${ASSET}.sha256"

# Tiny local HTTP server that serves the fake release assets.
python3 - "$fake_release" <<'PY' &
import http.server, os, sys
os.chdir(sys.argv[1])
http.server.ThreadingHTTPServer(("127.0.0.1", 8765), http.server.SimpleHTTPRequestHandler).serve_forever()
PY
SERVER_PID=$!
trap 'kill $SERVER_PID 2>/dev/null || true; rm -rf "$fake_release" "$install_root"' EXIT
sleep 0.3

# Patch the installer URLs via env by wrapping curl? Easier: invoke a
# surgically overridden copy that points BASE at our local server.
patched="$install_root/install.sh"
sed \
  -e "s|https://github.com/\${REPO}/releases/latest/download|http://127.0.0.1:8765|g" \
  -e "s|https://github.com/\${REPO}/releases/download/\${VERSION}|http://127.0.0.1:8765|g" \
  "$ROOT/dist/install.sh" > "$patched"
chmod +x "$patched"

KHOTAN_OBSERVER_BIN_DIR="$install_root/bin" bash "$patched"

[[ -x "$install_root/bin/khotan-observer" ]] || { echo "FAIL: binary not installed" >&2; exit 1; }
"$install_root/bin/khotan-observer" 2>&1 | head -1 | grep -q "khotan-observer" \
  || { echo "FAIL: binary did not print help banner" >&2; exit 1; }

echo "PASS: offline installer smoke test"
