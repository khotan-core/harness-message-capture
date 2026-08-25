# AGENTS.md

Agent brief for `harness-message-capture`. Humans read `README.md`.

> **Where this lives.** Development happens in the Khotan platform monorepo at
> `tools/harness-message-capture/`, imported there with `git subtree add`. This
> repository is a **published mirror**: a merge to the monorepo's `main`
> automatically splits the prefix and fast-forwards `main` here. Do not commit
> to this repository directly — an upstream commit the monorepo does not have
> (a release bump is the likely one) makes the next publish fail its ancestry
> check rather than force-push over you.
>
> It remains the **release home**: `dist/install.sh` is curl'd from this `main`,
> `khotan-observer update` resolves against these GitHub Releases, and
> `.github/workflows/release.yml` runs here on a `v*` tag. Publishing is not
> releasing — bump the version in the monorepo, let the merge publish it, then
> tag here to ship binaries.

## Project overview

macOS background agent that tails local AI coding-agent transcripts (Cursor, Claude Code, Codex), redacts secrets on device, and POSTs redacted lines to the Khotan organization pinned by each customer repository.

Crate name: `harness-message-capture`. Binary name: `khotan-observer`. Rust 2021 edition. Single package. `license = "UNLICENSED"` and `publish = false` in `Cargo.toml`. No crates.io publish. macOS only (`aarch64-apple-darwin`, `x86_64-apple-darwin`).

The client stays dumb: it ships redacted raw JSONL plus provenance. The server parses per-tool semantics later. This is a consented employee install, not a stealth collector.

## After the user approves a change

Do this in the same turn. "Push", "ship", "land it", or "LGTM" all mean the full release. Do not stop after `git push origin main`.

1. Commit the approved change.
2. Bump the patch in `Cargo.toml` (`0.1.10` → `0.1.11`). Sync `Cargo.lock` and the version string in `README.md`.
3. Commit `Release vX.Y.Z`.
4. Create an annotated tag `vX.Y.Z` on that commit.
5. Push both: `git push origin main` and `git push origin vX.Y.Z`.
6. Wait until the Release workflow on that tag succeeds and the GitHub Release exists.
7. Reinstall:

```bash
curl -fsSL https://raw.githubusercontent.com/khotan-core/harness-message-capture/main/dist/install.sh | bash
```

A push to `main` does not publish a binary. Only a `v*` tag does. Never leave `Cargo.toml` unbumped. Never leave `~/.local/bin/khotan-observer` on a stale build.

## Architecture

| Module | Role |
| --- | --- |
| `src/main.rs` | CLI: `configure`, `run`, `start`, `stop`, `logs`, `uninstall`, `status`, `docs`, `run-once`, `receive`, `read`, `clear-queue` |
| `src/docs.rs` | Embedded log glossary from `dist/help.md`; `docs` and `docs --write` |
| `src/picker.rs` | Raw-mode checkbox list behind a bare `configure` |
| `src/sources.rs` | Discover transcript roots that exist on disk |
| `src/workspace.rs` | Map a transcript path to a git checkout or worktree |
| `src/destination.rs` | Load `env.khotan.local` / `.env.khotan.local` and verify org |
| `src/capture.rs` | Tail new JSONL bytes; persist offsets |
| `src/redact.rs` | Client-side secret scrub before spool |
| `src/record.rs` | Generic captured-line schema (v2: `thread_id`, `agent_role`, `seq`) |
| `src/spool.rs` | Per-route durable local queue |
| `src/uploader.rs` | `GET /api/v1/me` then `POST /ingest` |
| `src/agent.rs` | LaunchAgent install, start, stop, logs |
| `src/singleton.rs` | One running observer at a time |
| `src/config.rs` | `~/.config/harness-message-capture/config.toml` |
| `src/receiver.rs` | Local proof sink |
| `src/store.rs` / `src/reader.rs` | Inbox layout and local inspect |
| `src/log.rs` | Foreground and LaunchAgent log format |
| `src/update.rs` | Compare this binary to the latest GitHub Release, warn on `run`, and replace `~/.local/bin` via `update` |

Capture path: watch or poll `*.jsonl` → redact → require a complete repo-local destination → respect `allow_repos` → spool → upload. Workspaces without a valid destination are skipped and their offsets advance. Each record carries `thread_id` (the root chat), `agent_role` (`root` or `subagent`), and `seq` (byte offset in the source file).

Transcript roots (only if the directory exists):

| Tool | Path |
| --- | --- |
| Cursor | `~/.cursor/projects/**/*.jsonl` |
| Claude Code | `~/.claude/projects/**/*.jsonl` |
| Codex | `~/.codex/sessions/**/*.jsonl` |

A destination is a repo-local env file (`env.khotan.local` or `.env.khotan.local`) with `KHOTAN_API_URL`, `KHOTAN_API_KEY`, and `KHOTAN_ORG_ID`. The uploader checks `GET /api/v1/me` and fails closed unless the key's organization matches `KHOTAN_ORG_ID`. API keys are read at send time. Do not copy them into records, queue metadata, or logs.

Default search roots that exist: `~/Developer`, `~/Projects`, `~/repos`, `~/code`, `~/conductor/workspaces`, `~/.cursor/worktrees`.

On-disk state:

| Kind | Path |
| --- | --- |
| Config | `~/.config/harness-message-capture/config.toml` |
| Offsets and spool | `~/.local/state/harness-message-capture/` |
| Installed binary | `~/.local/bin/khotan-observer` |
| Log glossary | `~/.local/share/khotan-observer/help.md` |
| LaunchAgent | `~/Library/LaunchAgents/com.khotan.observer.plist` |
| Background log | `~/Library/Logs/khotan-observer.log` |

`allow_repos` empty means send nothing. A name matches the folder leaf exactly (`podium-automation` does not match `podium-automation-mirror`). An absolute path matches only that workspace.

## Setup commands

Need a local Rust stable toolchain (`rustc`, `cargo`).

```bash
cargo build
cargo build --release
./target/release/khotan-observer status
```

The release profile in `Cargo.toml` optimizes for a small always-on binary (`opt-level = "z"`, LTO, one codegen unit, `panic = "abort"`, `strip = true`). Do not overwrite a running `~/.local/bin/khotan-observer` in place. The kernel keeps the old inode mapped and later execs die with SIGKILL.

Installed-binary workflow (user machine):

```bash
khotan-observer configure                        # checkbox list; no TTY prints a hint and exits
khotan-observer configure --allow-repo customer-repo   # scriptable, replaces the list
khotan-observer run
khotan-observer run --all-logs
khotan-observer start
khotan-observer update
khotan-observer logs
khotan-observer status
khotan-observer stop
khotan-observer uninstall
khotan-observer clear-queue --yes
```

`run` is foreground. Ctrl-C stops the observer and returns to the shell. If a LaunchAgent is loaded, `run` stops it first. `start` still uses `stop` because that process is not attached to a terminal. Default `run` prints only allow-list repos. `--all-logs` also prints skip lines.

## Development workflow

No watch mode and no extra package manager. Edit `src/*.rs`, then:

```bash
cargo test
cargo build --release
```

For a local swap of the installed binary, stop the agent first, then replace the file, then start again. Prefer the installer after a tagged release.

Config changes do not need a restart. The next scan reads `config.toml`.

## Testing instructions

Tests live in `#[cfg(test)]` modules next to the code they cover. There is no `tests/` directory and no integration-test crate.

```bash
cargo test
cargo test --lib
cargo test workspace_allowed
./dist/smoke-install.sh
```

`./dist/smoke-install.sh` needs `./target/release/khotan-observer` first. It does not hit the network. It serves a fake GitHub Release locally and checks installer checksum plus install-path logic.

Add or update tests for the code you change. Keep destination, allowlist, redaction, spool, and uploader tests in their own modules.

## Code style

- Rust 2021. `anyhow::Result` at the CLI boundary. No extra error crate.
- One concern per `src/*.rs` file. Do not add a crate when `std` is enough.
- `rustfmt` defaults. No `rustfmt.toml` or `clippy.toml`.
- Keep the binary small. New dependencies need a size reason.
- Do not log API keys, tokens, or raw unredacted transcript lines.
- User-facing strings match the existing `khotan-observer` CLI voice in `src/log.rs` and `print_help`.
- Prose (README, comments, commit messages) follows the Google developer documentation voice and short-sentence mechanics in the workspace rules.

## Build and deployment

A push to `main` does not publish a binary. Only a `v*` tag runs `.github/workflows/release.yml`. That workflow builds both macOS targets and attaches:

- `khotan-observer-aarch64-apple-darwin` (+ `.sha256`)
- `khotan-observer-x86_64-apple-darwin` (+ `.sha256`)

`dist/install.sh` downloads `releases/latest` unless `KHOTAN_OBSERVER_VERSION` is set. It verifies SHA-256, stops a running LaunchAgent, installs to `~/.local/bin/khotan-observer` on a new inode, and restarts if the agent was running.

### Ship after a change lands on `main`

Do this without waiting for a reminder. Do not leave `~/.local/bin/khotan-observer` on a stale build.

1. Read `version` in `Cargo.toml`. Bump the patch (`0.1.5` → `0.1.6`). Keep `Cargo.lock` and the version string in `README.md` in sync.
2. Commit the bump: `Release vX.Y.Z`.
3. Create an annotated tag `vX.Y.Z` on that commit.
4. Push the commit and the tag: `git push origin main` and `git push origin vX.Y.Z`.
5. Wait until the Release workflow on that tag succeeds and the GitHub Release exists.
6. Reinstall:

```bash
curl -fsSL https://raw.githubusercontent.com/khotan-core/harness-message-capture/main/dist/install.sh | bash
```

`cargo build --release` is not a substitute unless the user asks for a local swap.

## Pull request guidelines

- Commit messages: one or two sentences on why, not a file list.
- Title the change by intent (`Fix allowlist prefix match`, `Release v0.1.6`).
- Run `cargo test` before you commit.
- Do not commit secrets, `env.khotan.local`, or `.env.khotan.local`.
- Do not skip hooks. Do not force-push `main`.

## Security and privacy

- Intended for consented employee installs only.
- Redact on device in `src/redact.rs` before spool or upload. Extend that pattern list when you add capture of a new secret shape.
- Fail closed when org verification fails or credentials are missing.
- `clear-queue --yes` discards the local delivery queue only. It does not delete original transcripts or reset offsets.
- The installer fetches an unsigned binary. A `curl | bash` download does not get the macOS quarantine attribute. Do not add a `.app` bundle without a signing plan.

## Common pitfalls

- Two observers cannot run at once. `src/singleton.rs` holds a process lock. The lock drops when the process exits or crashes.
- Replacing the installed binary while it runs causes later `zsh: killed` execs. Stop first, then replace via a new inode.
- An empty `allow_repos` captures nothing. That is intentional.
- Adding a destination later does not retroactively upload old chats. Offsets already advanced.
- Records queued for a repo that is no longer allowed stay on disk until `clear-queue --yes`. The observer does not keep retrying them.
- `NOTES.md` is origin history (Paxel research, prior Bun tool). Do not treat it as current architecture.
