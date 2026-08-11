# khotan-observer

Tiny macOS background agent that captures local AI coding-agent transcripts
(Cursor, Claude Code, Codex) and ships redacted lines to your ingest endpoint.

Designed to feel barely there: a single ~2 MB Rust binary, event-driven file
watching, durable local spool, and a LaunchAgent that starts at login.

## Install (one line)

```bash
curl -fsSL https://raw.githubusercontent.com/khotan-core/harness-message-capture/main/dist/install.sh | bash
```

This downloads the matching macOS binary from [GitHub Releases](https://github.com/khotan-core/harness-message-capture/releases/latest),
verifies its SHA-256 checksum, and installs it to `~/.local/bin/khotan-observer`.

Pin a version:

```bash
KHOTAN_OBSERVER_VERSION=v0.1.0 \
  curl -fsSL https://raw.githubusercontent.com/khotan-core/harness-message-capture/main/dist/install.sh | bash
```

## Configure & run

```bash
# You'll be prompted for the enrollment token (not echoed).
khotan-observer configure --endpoint https://YOUR_INGEST/ingest

# Foreground — live log, Ctrl-C to stop.
khotan-observer run

# Or run as a persistent background LaunchAgent:
khotan-observer start
khotan-observer logs      # follow background activity
khotan-observer status
khotan-observer stop
khotan-observer uninstall

# Local proof sink (optional): receive into a directory, then inspect.
khotan-observer receive --token qa-token
khotan-observer read --tool cursor --limit 20
```

Running in the foreground shows what it's doing:

```
  khotan-observer  0.1.3

    Endpoint   https://ingest.example.com/ingest
    Device     3f88991f7ee379a1589ab29795e43b0c
    Sources    claude, codex, cursor
    Tracking   3,952 transcript files

  ✓ Watching in 3ms  · Ctrl-C to stop

  15:53:05   captured 12   uploaded 12   spool 0
  15:58:05   idle · watching 3,952 files · spool 0
```

If the endpoint is unreachable, nothing is lost — records queue on disk and
the line tells you so:

```
  15:52:47   captured 5   spool 531   ⚠ endpoint unreachable (connection refused) — retrying
```

Background mode writes the same log to
`~/Library/Logs/khotan-observer.log`; `khotan-observer logs` tails it.

## What it captures

Only directories that already exist on the machine are watched:

| Tool        | Path                          |
|-------------|-------------------------------|
| Cursor      | `~/.cursor/projects/**/*.jsonl` |
| Claude Code | `~/.claude/projects/**/*.jsonl` |
| Codex       | `~/.codex/sessions/**/*.jsonl`  |

Every newly appended JSONL line is redacted client-side (API keys, tokens,
connection strings, common `password=` / `api_key=` assignments) before it is
spooled and uploaded. The client stays dumb: it ships redacted raw lines; the
server can parse per-tool semantics later.

## Local QA (inbox receive + read)

Prove end-to-end capture without a production ingest server:

1. Start the local receiver (writes authenticated batches to a directory):

   ```bash
   # Uses the token from config if already configured; otherwise pass --token.
   khotan-observer receive --bind 127.0.0.1:8787 --token qa-token
   ```

   Default inbox: `~/.local/state/harness-message-capture/inbox/`
   Layout: `{device_id}/{tool}/{project}/{session_id}.ndjson`

2. Point the observer at it:

   ```bash
   khotan-observer configure --endpoint http://127.0.0.1:8787/ingest --token qa-token
   khotan-observer run-once   # or: khotan-observer run
   ```

3. Send a Cursor / Claude Code / Codex prompt containing a unique marker
   (e.g. `KHOTAN_QA_MARKER`), then capture again and inspect:

   ```bash
   khotan-observer run-once
   khotan-observer read --tool cursor --limit 20
   # or dump redacted raw JSONL:
   khotan-observer read --raw --limit 5
   ```

`read` renders recognizable user/assistant text when the line shape is known
(Cursor-style, and similar Claude `type` envelopes). Unrecognized harness
events are shown as preserved redacted raw JSONL with provenance — nothing is
dropped just because the local reader can't parse it yet.

Tip: to avoid uploading historical transcripts on first run, baseline offsets
to the current end of every `*.jsonl` under the source roots before the first
`run` / `run-once`.

### Throwaway Python receiver (optional)

If you only need to dump POSTs without using `receive`/`read`:

```bash
python3 - <<'PY'
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(n)
        print(body.decode()[:2000])
        open("/tmp/khotan-observer-received.ndjson", "ab").write(body + b"\n")
        self.send_response(204); self.end_headers()
    def log_message(self, *a): pass
HTTPServer(("127.0.0.1", 8787), H).serve_forever()
PY
```

## Build from source

```bash
cargo build --release
./target/release/khotan-observer status
```

## Release

Push a version tag (or run the **Release** workflow manually):

```bash
git tag v0.1.0
git push origin v0.1.0
```

GitHub Actions builds `aarch64-apple-darwin` and `x86_64-apple-darwin`, then
publishes both binaries plus `.sha256` sidecars to a GitHub Release. The
installer always targets `releases/latest` unless `KHOTAN_OBSERVER_VERSION` is set.

## Privacy & consent

This tool is intended for consented employee installs. Nothing is uploaded until
`configure` is run with a real endpoint and token. Secrets are scrubbed on-device
before leaving the machine; see `src/redact.rs` for the pattern list.
