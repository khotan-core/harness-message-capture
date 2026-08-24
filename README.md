# khotan-observer

Tiny macOS background agent that captures local AI coding-agent transcripts
(Cursor, Claude Code, Codex) and ships redacted lines to the Khotan organization
pinned by each customer repository.

Designed to feel barely there: a single ~2 MB Rust binary, event-driven file
watching, durable local spool, and a LaunchAgent that starts at login.

## Install (one line)

```bash
curl -fsSL https://raw.githubusercontent.com/khotan-core/harness-message-capture/main/dist/install.sh | bash
```

This downloads the matching macOS binary from [GitHub Releases](https://github.com/khotan-core/harness-message-capture/releases/latest),
verifies its SHA-256 checksum, and installs it to `~/.local/bin/khotan-observer`.
After that, `khotan-observer update` is the upgrade path.

Pin a version:

```bash
KHOTAN_OBSERVER_VERSION=v0.1.0 \
  curl -fsSL https://raw.githubusercontent.com/khotan-core/harness-message-capture/main/dist/install.sh | bash
```

## Update

```bash
khotan-observer update
khotan-observer update --version v0.1.21
```

That downloads `releases/latest` (or the pinned tag), checks the SHA-256,
replaces `~/.local/bin/khotan-observer` on a new inode, and restarts a
running LaunchAgent. `install` is the same command.

## Choose repositories and run

The installer writes `~/.config/harness-message-capture/config.toml` with
preset poll and batch values. The only setting you choose is which
repositories may upload. A log glossary ships in the binary. Run
`khotan-observer docs`, or open `~/.local/share/khotan-observer/help.md`.

```bash
# Checkbox list of every repository that has a destination file.
# Arrows move, space toggles, type to filter, enter saves, esc cancels.
khotan-observer configure

# Same choice without a prompt, for scripts and machines with no terminal.
khotan-observer configure --allow-repo podium-automation --allow-repo chief-nutrition

# Foreground — live log of allowed repos. Ctrl-C stops and returns to the shell.
khotan-observer run
khotan-observer run --all-logs   # include skip lines for other repos

# Or run as a persistent background LaunchAgent:
khotan-observer start
khotan-observer logs      # follow background activity
khotan-observer status
khotan-observer docs      # what status and log lines mean
khotan-observer stop
khotan-observer uninstall
khotan-observer clear-queue --yes  # permanently discard unsent records

# Local proof sink (optional): receive into a directory, then inspect.
khotan-observer receive --token qa-token
khotan-observer read --tool cursor --limit 20
```

Only one observer can run at a time. If a background LaunchAgent is loaded,
then `run` stops it first. Ctrl-C then exits the observer and returns you
to the shell. You do not need `khotan-observer stop` after a foreground
session. `start` still uses `stop` because that process is not attached to
a terminal. The process lock is released automatically if the process exits
or crashes.

`clear-queue --yes` only discards records waiting in the local delivery queue.
It does not delete the original AI-tool transcripts or reset their offsets.

Running in the foreground shows what it's doing:

```
  khotan-observer  0.1.24

    Sources    claude, codex, cursor
    Allow      podium-automation, chief-nutrition · 2 ready

  ✓ Watching in 3ms  · Ctrl-C to stop

  15:53:05   harness-message-capture   captured 12   uploaded 12
  15:58:05   idle (No new lines this pass · 3,952 files)
```

If GitHub has a newer tagged release, `run` prints a bright-red `ALERT`
after the banner. Capture still starts.

Default `run` prints only repositories on the allow list. Failures for those
repos still print. Skip lines for other folders stay off unless you pass
`--all-logs`.

Each allowed workspace prints on its own line. Counts on that line belong to
that folder only:

```
  16:30:09   harness-message-capture   captured 10   uploaded 10
  16:30:09   khotan   captured 3   uploaded 3
```

If a customer endpoint is unreachable, nothing is lost. That customer's
records stay in its local queue. Other customers keep draining:

```
  15:52:47   podium-automation   captured 5   queued 531   (Host is up in DNS, port is closed)
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

Only workspaces that resolve to a complete repo-local destination are captured:

```dotenv
KHOTAN_API_URL='https://customer.example'
KHOTAN_API_KEY='organization-scoped-key'
KHOTAN_ORG_ID='expected-organization-id'
```

The observer checks `GET /api/v1/me` before delivery and fails closed unless
the key's organization matches `KHOTAN_ORG_ID`. API keys are read at send time;
they are never copied into message records, queue metadata, or logs. Repositories
without a valid destination are skipped and their offsets advance, so adding a
destination later does not retroactively upload old chats.

To choose which repositories upload, run `configure` and tick them in the list,
or edit `allow_repos` in `~/.config/harness-message-capture/config.toml`. The
list shows only repositories that already have a destination file, because no
other repository can upload. A repository you allowed earlier stays on the list
even after its destination file goes away, so saving never drops an entry
silently. Each entry must be the exact folder name. `podium-automation` does not match
`podium-automation-mirror`. An empty list sends nothing. The next scan reads
the file; you do not need to restart.

```toml
allow_repos = [
  "podium-automation",
  "chief-nutrition",
  "dev-serve-robotics",
]
```

Records already queued for a repo that is no longer allowed stay on disk until
`khotan-observer clear-queue --yes`. The observer does not keep retrying them.

## Local inbox reader

The bundled receiver remains useful for inspecting the legacy batch shape:

1. Start the local receiver (writes authenticated batches to a directory):

   ```bash
   khotan-observer receive --bind 127.0.0.1:8787 --token qa-token
   ```

   Default inbox: `~/.local/state/harness-message-capture/inbox/`
   Layout: `{device_id}/{tool}/{project}/{thread_id}/{session_id}.ndjson`

2. Inspect records already written to the inbox:

   ```bash
   khotan-observer read --tool cursor --limit 20
   khotan-observer read --thread 76a56200-c845-4f62-b741-ca6237573ade
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

This tool is intended for consented employee installs. Nothing is uploaded for
a workspace unless its repository contains a complete, organization-verified
Khotan destination, and the workspace matches `allow_repos` in the machine
config. An empty allow list sends nothing. Secrets are scrubbed on-device
before leaving the machine; see `src/redact.rs` for the pattern list.
