# harness-message-capture — origin notes

Distilled from a chat on 2026-08-11 that started as research into Paxel (YC's
local coding-session analyzer) and ended in designing an internal, consented
tool for capturing employee AI coding-agent transcripts.

## Where this came from: researching Paxel

[Paxel](https://paxel.ycombinator.com/) is YC's free tool that reads local
Claude Code / Codex CLI / Cursor / opencode / Gemini CLI / VS Code Copilot /
Antigravity session transcripts and generates a "builder profile" (scores
across steering, execution, engineering quality, product thinking, planning).
It's pitched as research into what makes someone build well with agents, and
a Paxel token can be attached to a Startup School application.

Key architecture takeaways from reading its `upload.sh` and
`/data-handling` page directly:

- **Transcripts are plaintext on disk in known paths** — this is the whole
 reason a tool like this is easy to build:
 - Claude Code: `~/.claude/projects/**/*.jsonl`
 - Codex CLI: `~/.codex/sessions/`
 - Cursor: `~/Library/Application Support/Cursor/`, newer versions also
   `~/.cursor/projects/`
 - opencode: SQLite DB under `~/.local/share/opencode`
 - Gemini CLI: `~/.gemini/tmp/*/chats/`
 - VS Code Copilot: `~/Library/Application Support/Code/User/workspaceStorage`
 - Antigravity: `~/.gemini/{antigravity,antigravity-ide,antigravity-cli}/brain/`
- **Two-stage network flow, not one:**
 1. A `docker run` client mounts transcripts read-only and makes many
    per-call HTTPS requests to an LLM proxy (`paxel-llm.ycombinator.com`)
    that forwards to Anthropic/OpenAI/Microsoft Foundry. This is where
    prompt/response content actually leaves the machine, continuously
    during analysis — not batched at the end.
 2. One final `POST /api/v1/results` with derived JSON only (scores,
    narratives, redacted decisions, metadata, git commit stats).
- **The proxy is also the anti-gaming mechanism.** Every proxied LLM call
 returns an HMAC nonce; the final results submission includes the nonces
 and the server verifies them against its own proxy log
 (`no_nonces_matched` rejection otherwise). You can't fabricate a flattering
 profile without real, server-witnessed LLM calls. This is *why* it can't be
 fully local, not just a cost-sharing convenience.
- **Redaction happens client-side, before upload**, against a long list of
 credential patterns (API keys, JWTs, DB connection strings, PEM keys,
 OAuth tokens, etc.), reapplied server-side as defense in depth.
- **Distribution trick:** `curl | bash` downloading an unsigned binary does
 not trigger macOS Gatekeeper quarantine (only browser-style downloads with
 `LSFileQuarantineEnabled` do that), so no Apple signing/notarization is
 needed for a CLI tool. That's the opposite of a double-clicked `.app`.
- Auth is a plain browser device-code flow (`/auth/cli/register` →
 open browser → poll `/auth/cli/poll`), token cached at `~/.paxel/token`,
 sent as `X-YC-Token` on subsequent requests.

## What we designed: an internal version, for employees, consented

Explicitly **not** a stealth/spyware tool — this is for a team that installs
it themselves, knowing what it does, analogous to Paxel itself. The
consent/disclosure line is what separates "internal analytics product" from
something that gets a company into real legal trouble (wiretap-adjacent
statutes, GDPR/CCPA, employee-monitoring notice requirements in some
jurisdictions) — worth a short written policy + sign-off at install, not just
an assumption.

Landed on the simplest viable shape:

- **CLI binary, not a `.app`.** Skips Apple signing/notarization entirely
 (see distribution trick above). Go was the suggested language: single
 static binary, no runtime deps, `fsnotify` for watching, trivial
 cross-compile.
- **Background execution via a LaunchAgent** (`~/Library/LaunchAgents/*.plist`,
 `RunAtLoad` + `KeepAlive`), not `nohup &` — survives logout/reboot.
- **Four components:**
 1. *Watcher* — FSEvents/fsnotify on the transcript dirs above, tail new
    `.jsonl` lines as they're appended.
 2. *Local queue/state* — track per-file byte offsets (or a small SQLite
    spool) so restarts resume instead of re-sending; buffer when offline.
 3. *Redactor* — strip secrets/credentials before anything leaves the
    machine (borrow Paxel's pattern list as a starting point).
 4. *Uploader* — batched HTTPS `POST` with a per-device/employee token,
    retry with backoff.
- **Install UX target:**
 ```bash
 brew install yourco/tap/harness-message-capture
 harness-message-capture enroll --token=...
 harness-message-capture start   # writes + loads the LaunchAgent
 ```
 or a one-line `curl | bash` installer as the no-Homebrew fallback.
- **Permissions:** dotfile paths (`~/.claude`, `~/.codex`) are always
 readable by a plain user process; `~/Library/Application Support/Cursor`
 is normally fine for a non-sandboxed process too. Full Disk Access is the
 fallback if a macOS version starts gating a given path — design so the
 tool reports "couldn't read X" rather than silently collecting nothing.
- **Server side:** minimal — an authenticated `POST /ingest` that checks a
 device token, dedupes on record id, writes to Postgres/object storage.
 Not yet decided whether this lives in this repo, in `khotan`, or elsewhere.

## Prior art (deliberately not reused)

There was an existing, fairly mature implementation of very close to this
tool at `/Users/adeep/Developer/agent-message-capture`
(`github.com/khotan-core/agent-message-capture`, Bun/TypeScript) — Claude/
Codex/Cursor adapters, redaction, an encrypted spool, an uploader, keychain
integration, installer/uninstaller, machine enrollment, versioned contracts.
Decision: start over fresh rather than fork it. The local directory was
deleted on 2026-08-11 (all commits were already pushed to GitHub; a handful
of uncommitted WIP changes to `integrations.ts`/`runtime.ts` were discarded
intentionally). The GitHub repo itself still exists if anything needs to be
referenced later, but it's not the basis for this project.

## Open questions going into this repo

- Language: Go was proposed in the original discussion, not yet confirmed
 for this fresh start.
- Server: stub endpoint needed for end-to-end testing — stack undecided.
- Whether to reuse *any* concepts from the deleted prior art (e.g. its
 contract-versioning approach, or its adapter split per tool) or design
 those pieces from scratch.
