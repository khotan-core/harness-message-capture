# System design review — khotan-observer

**Date:** 2026-08-25 · **Version reviewed:** 0.1.26 plus ~1,900 uncommitted lines
(`fix-observer-delivery`) · **Scope:** 8,421 LOC across 19 modules

Six parallel audits — capture, agent coverage, delivery, setup/lifecycle,
testing, architecture/OpenSpec — cross-checked against the live state of one
production machine. Findings carry file and line references against the working
tree as of the date above.

---

## 1. Verdict

The observer does **not** capture all sessions and turns. Three separate code
paths read transcript lines, mark them consumed, and drop them. The loss is
unrecoverable because a single byte offset does double duty as *"I have read
this"* and *"I have decided about this."*

Delivery is durable in the right place — the queue is fsynced before offsets
move — but the cursor beside it is not, there is no idempotency key, no retry
backoff, and no disk cap. Setup needs a hand-placed API key per repo per
machine. A single foreground `run` permanently disables background capture with
nobody notified.

The engineering that is present is good: two `expect()` calls in 8,400 lines, a
clean acyclic module graph, fail-closed defaults throughout, and a spool cursor
design that turned an O(backlog) rewrite into O(1). The problem is that none of
it is enforced — no CI has ever run a test, the suite fails roughly 1 run in 4,
and the entire `openspec/` tree is untracked in git.

---

## 2. Live evidence

Measured on one machine, not inferred from code.

| Observation | Value | Source |
| --- | --- | --- |
| Records stuck in one customer queue, zero ever delivered | **11,461** (36 MB) | `spool/a9c2918d615c5a06/cursor.json` → `{"offset":0,"pending":11461}` |
| Cursor worktree transcript bytes read, discarded, offset advanced | **474,285** | 4 files, `sgs-fresh-connector`, on the allow list |
| Files of `openspec/` tracked in git | **0** | `git ls-files openspec` |
| `cargo test` runs failed with no code change | **2 of 8** | reproduced locally; agent measured 4 of 12 |
| CI workflows that run a test | **0** | `release.yml` is the only workflow |
| LaunchAgent plist | **absent** | background capture simply off |
| Cursor transcripts queued to be discarded | **63** (5.9 MB) | `~/.cursor/projects/empty-window/` |
| Idle CPU cost, almost all filesystem metadata | **~2.4% of a core** | ~179,000 stats per pass |
| Transcript corpus on disk | **2,516 files / 1.28 GB** | `.claude` 335, `.cursor` 1,540, `.codex` 641 |
| `offsets.json` | **434 KB / 2,497 keys** | single JSON blob, rewritten per pass |

Two live root causes worth stating plainly:

**The stuck queue.** `/Users/…/pollinate-workspace/env.khotan.local` was
overwritten and now contains only `DASHBOARD_DATABASE_URL`. The new
`same_identity` repair path exists for exactly this case, but it requires
`key_fingerprint` on *both* routes — and every `route.json` on disk was written
by v0.1.26 with `org_id` as a plain string and no fingerprint. The fix cannot
rescue any queue that exists today.

**The Cursor encoding bug.** Cursor and Claude mangle project paths differently
and `encoded_path` applies one encoding to both:

```
Cursor writes:  Users-coreyberther-cursor-worktrees-sgs-fresh-connector-6xt6   (dot dropped)
Claude writes: -Users-coreyberther-.cursor-worktrees-sgs-fresh-connector-6xt6  (dot kept)
encoded_path() emits for cursor:
                Users-coreyberther-.cursor-worktrees-...                       ✗ never matches
```

`~/.cursor/worktrees` is one of only two search roots in that machine's config.

---

## 3. Decisions taken

Recorded 2026-08-25. These settle the forks that shape the change set in §5.

| # | Decision | Consequence |
| --- | --- | --- |
| 1 | **Capture everything.** Lines that cannot be routed are staged locally and routed at drain time. | Onboarding a repo delivers its history retroactively. A routing bug becomes a re-run, not a data-loss incident. Transcripts now sit on disk for repos that were never onboarded — this changes the privacy story and makes §5.6 load-bearing. Bounded by the same cap-and-evict policy as decision 6; no time-based TTL for now. |
| 2 | **Machine-level credentials, one per org.** Multiple orgs per machine must remain supported. `allow_repos` keeps doing the scoping. | Deletes the fingerprint, org-pinning and queue-repointing machinery. |
| 3 | **The Khotan server can change.** Four requirements, see §4. | Idempotency, partial-accept, heartbeat and a published size limit all become available. |
| 4 | **`[lib]` target and dev-dependencies are acceptable.** | Dev-dependencies are never compiled into `cargo build --release`, so this costs zero shipped bytes. Unlocks `tests/`, `--lib`, `--doc` and coverage. |
| 5 | **Adopt `clap`.** | Measured +133 KB on a 2.5 MB binary (+5.3%) for `default-features = false`; deletes ~330 lines and ~20 tests. |
| 6 | **Cap and evict oldest, loudly.** | Applies to both the staging log and the spool. Silent truncation is not permitted. |
| 7 | **Employee transparency is a requirement.** | `queue --show`, a local `history` digest, `uninstall --purge`. |
| 8 | **Take the cheap coverage wins, defer SQLite.** | `archived_sessions` + both `history.jsonl`; `Tool` enum refactor. opencode/Zed have 0 rows; Cursor's chat DB is 87% redundant. |
| 9 | **Loosen redaction over-matching; the server does not re-scrub.** | The client is the only line of defence. Loosening must be surgical — suppress only clear *references* (`process.env.X`, `os.environ[…]`, `${VAR}`), never literals. **Recommendation to the server team: re-scrub on ingest as defence in depth.** |
| 10 | **Write off the 11,461 stuck records.** | No recovery migration. But every deployed observer carries queues in the same pre-fingerprint shape, so §5.4 must define legacy-queue behaviour explicitly: drain what resolves, drop what cannot, print exactly what was dropped. |

---

## 4. Required Khotan server changes

### (a) Dedupe on a client record id

The client will add `id` to every record: `sha256(tool|session_id|seq|line)`,
deliberately excluding the capture timestamp so a re-read produces the same id.
The server needs a unique constraint on it and an insert that ignores conflicts.

This is what makes at-least-once delivery safe. Without it, every crash
mid-pass and every cursor reset double-writes.

### (b) Partial-accept response on `/ingest`

Today any 2xx means "all good" and the client advances the cursor over the whole
batch — so a server that accepts 900 of 1000 records silently loses 100 and the
client reports success.

```json
{ "accepted": 900, "rejected": [{ "id": "…", "reason": "schema" }] }
```

The client advances by `accepted` only and reports rejection reasons on the
customer's activity line.

### (c) `POST /api/v1/heartbeat`

The highest-value item in this review.

```json
{
  "device_id": "…",
  "version": "0.1.27",
  "agent_loaded": true,
  "routes": [
    { "org_id": "…", "pending": 0, "last_success_ms": 0, "last_error": null }
  ],
  "staged_bytes": 0,
  "quarantined": 0
}
```

Sent daily **even when every queue is empty** — that is the entire point. An
idle observer currently makes zero network calls, so a dead observer and a quiet
one are indistinguishable forever. Alert server-side on any device silent for
more than 48 hours.

### (d) Publish the ingest body limit in `/api/v1/me`

```json
{ "org_id": "…", "max_ingest_bytes": 1048576 }
```

The client currently guesses at 900 KiB and, on a 413, halves and retries —
rediscovering the limit every pass and after every restart. Publishing it turns
that machinery into a fallback rather than the normal path.

---

## 5. Change set

Seven OpenSpec changes, in dependency order.

### 5.1 `establish-test-foundation`

Prerequisite for everything else. `git add openspec/` first — the specs
currently exist only on one machine's disk.

Fix the flaky singleton test, add `ci.yml` on push and PR, clear the two clippy
warnings, split `src/lib.rs` + thin `src/main.rs`, add the three constructor
seams (`Offsets::at`, `Paths`, `Observer::tick`), add `tempfile`, write the
end-to-end test, correct the `cargo test --lib` instruction in AGENTS.md. Adopt
`clap` here, before the CLI grows three new subcommands.

Findings: T-01 … T-10, A-05, A-06, G-01.

### 5.2 `capture-everything-locally`

Split the offset into `read_to` and routing state; add the staging log. Fix the
Cursor path encoding, stop advancing on unroutable / undestined / unresolvable
lines, fix the >4 MB stall, make `offsets.json` atomic with inode identity, make
`seq` monotonic, surface swallowed per-file errors, prune dead offset keys,
cap-and-evict. Plus redaction: the six missing pattern families and the surgical
loosening from decision 9.

New capabilities: `capture/staging-log`, `capture/offset-integrity`,
`capture/redaction`.
Findings: C-01 … C-12, P-01, P-04, A-04.

### 5.3 `secure-durable-delivery`

Pin redirects off and force HTTPS, one shared pooled `ureq::Agent`, fsync the
cursor, repair a partial trailing line, per-route `clear-queue`, exponential
backoff with a negative identity cache, keep the lane on a retryable failure,
record `id` and partial-accept handling, `proxy-from-env` and `native-certs`,
bounded error excerpt.

Modifies `delivery/spool-queue`, `delivery/upload-batching`; adds
`delivery/failure-backoff`.
Findings: D-01 … D-15, P-02, P-03.

### 5.4 `machine-level-credentials`

Per-org credentials in `~/.config/harness-message-capture/`, written by
`configure`; `allow_repos` keeps doing the scoping. Deletes fingerprint,
pinning and re-pointing machinery. Defines legacy-queue behaviour explicitly per
decision 10.

Heavily modifies `delivery/destination-identity`; adds `setup/credentials`.
Findings: S-01, D-06, D-07, D-08.

### 5.5 `report-fleet-health`

Heartbeat; per-route `status` with pending, last error, last success and
version; real crash-loop detection; `run` restores the LaunchAgent on exit via
an RAII guard plus a signal handler; `ThrottleInterval`; log rotation; propagate
the error context the drain currently throws away.

New capabilities: `observability/heartbeat`, `observability/status`.
Findings: O-01 … O-05, S-06, S-09, S-11.

### 5.6 `show-what-was-collected`

`queue --show`, a local append-only `history` digest of what was sent (thread
ids, counts, timestamps, no content), `uninstall --purge`, and a rewritten
`help.md` and privacy section reflecting that unrouted transcripts now stage
locally. Load-bearing because of decision 1.

New capability: `setup/transparency`.
Findings: S-07, S-08, S-02, S-03, S-13, S-14.

### 5.7 `cover-more-agent-sources`

`Tool` enum plus a per-tool decision struct so a missed match arm fails to
compile rather than silently losing data. Add `~/.codex/archived_sessions/`,
`~/.claude/history.jsonl`, `~/.codex/history.jsonl`. Re-run `sources::discover()`
on the poll tick so a newly installed agent is picked up without a restart.

New capability: `capture/agent-sources`.
Findings: V-01 … V-06, A-02, A-03.

### Not scheduled

Architecture cleanups worth doing opportunistically inside the changes above:
A-01 (extract `drain.rs` from `main.rs`), A-07 (split `destination.rs`), A-08
(`DefaultHasher` persisted to disk), A-09 (`panic = "abort"` dead error path),
A-10 (primitive obsession), A-11 (gate the receiver behind a feature), A-12,
S-04, S-05, S-10, S-12, S-15, S-16, G-02 … G-06.

---

## 6. Findings

Severity: **C**ritical / **H**igh / **M**edium / **L**ow.
Action: what the fix is — create, update, or delete.
★ = confirmed firing on the reviewed machine.

### Capture completeness

**C-01 · C · update ★ — Cursor path encoding is wrong, so every worktree session is read and thrown away**
`src/workspace.rs:113-120`, `src/capture.rs:181-189`, `src/main.rs:682-685`
`encoded_path` only does `replace('/', "-")` plus a leading-dash trim. Cursor
additionally strips the leading dot from hidden path segments and converts
spaces to dashes. 8 of 55 Cursor project dirs fail to resolve; 4 are worktrees.
Resolution failure returns `Ok(None)`, which sets `advance_unrouted = true`,
which commits the offset.
*Fix:* tool-specific encoding, or encode each candidate both ways and match
either. Independently, stop advancing on an unresolved workspace.

**C-02 · C · update — An allow-listed repo with no destination file is skipped silently, offset advanced, zero log lines**
`src/capture.rs:169-171`, `src/destination.rs:219`, `src/main.rs:677-687`
`destination::resolve` returns `Ok(None)` when no env file is found; the match
arm folds that into `(route, None, true)` — no warning, offset advances.
*Fix:* emit a `RouteWarning` and set `advance_unrouted = false`.

**C-03 · C · update ★ — "Chat has no project folder" destroys data on a resolution failure, not a policy decision**
`src/capture.rs:181-189`
63 transcripts / 5.9 MB in `empty-window`, plus 12 further unresolvable slugs
including 10 `var-folders-…-T-<uuid>` dirs. The skip line is suppressed unless
`--all-logs` is passed.
*Fix:* `advance_unrouted = false`. An unresolvable path is a bug to fix, not
consent to delete.

**C-04 · H · update — A single line over 4 MB stalls that file forever, silently**
`src/capture.rs:15`, `src/capture.rs:130-136`
A 4 MiB window containing no newline returns `Ok(None)` on every pass forever,
with no log line. Largest line seen today is 1.35 MB; a large tool result
crosses 4 MB easily.
*Fix:* grow the read or park the record with an explicit warning.

**C-05 · H · update — `offsets.json` is written non-atomically, and a corrupt read silently resets every offset to zero**
`src/capture.rs:26-29`, `src/capture.rs:33-39`
`fs::write` truncates then writes; `unwrap_or_default()` swallows a parse
failure and starts from an empty map, re-uploading all 1.28 GB. The spool beside
it already does tmp + rename correctly.
*Fix:* tmp → `sync_data` → rename → fsync dir. Log loudly instead of defaulting.

**C-06 · H · update — Offsets are keyed by path string only, with no inode identity**
`src/capture.rs:114`, `src/capture.rs:119-122`
The only staleness check is `len < offset`. A delete-and-recreate at the same
path with `new_len >= old_offset` silently skips the first `old_offset` bytes.
Inode reuse is invisible.
*Fix:* key on `(dev, ino)`, or store `{offset, inode, len_at_commit}`.

**C-07 · H · create — No record id, so the server has no idempotency key**
`src/record.rs:10-29`, `src/capture.rs:209`
Record shape is `{schema, tool, project, session_id, thread_id, agent_role, seq,
captured_at_ms, line}` — no id. `captured_at_ms` changes on every re-capture, so
a server-side hash of the whole record cannot catch a duplicate.
*Fix:* `id = sha256(tool|session_id|seq|line)`, excluding the timestamp. See §4a.

**C-08 · M · update — `seq` is a byte offset that resets to 0 on truncation**
`src/capture.rs:208`, `src/capture.rs:120-122`
After any shrink or recreate the next line in the same session gets `seq = 0`,
colliding with already-shipped records.
*Fix:* monotonic per-session counter, or a `generation` field.

**C-09 · M · update — `collect_new` swallows every per-file error with no log**
`src/capture.rs:100`
`Ok(None) | Err(_) => continue` catches stat, open, seek and read failures. A
permissions change stops capture for that file permanently, silently.
*Fix:* separate `Err` from `Ok(None)`; rate-limited warning plus a counter.

**C-10 · M · update — `run-once` permanently loses everything past 4 MB per file**
`src/main.rs:460-472`, `src/capture.rs:15`
One pass only; the remainder is deferred to a next pass that never comes.
*Fix:* loop until a pass yields no new bytes.

**C-11 · L · update — `offsets.json` grows forever and never prunes deleted transcripts**
`src/capture.rs:41-51`
No removal method. Also: the README's "3,952 files" idle line is `offsets.len()`,
not files walked — the real per-pass footprint is ~8× larger.
*Fix:* prune missing paths during `save`; fix the idle line's wording.

**C-12 · H · create — The offset means two different things, which is why every loss above is permanent**
`src/capture.rs:144-189`, `src/main.rs:682-685`
One `u64` encodes both "I have read these bytes" and "I have decided what to do
about them." Routing at capture time makes a decision taken under a bug
indistinguishable from a correct one.
*Fix:* split them — see decision 1 and §5.2.

### Agent coverage

**V-01 · H · update ★ — Codex archived sessions sit one directory above the watched root**
`src/sources.rs:15-19`
`~/.codex/archived_sessions/` holds 6 rollouts / 9.3 MB in the identical format.
If Codex archives before the daemon tails to EOF, the tail is lost outright.
*Fix:* one more tuple in `sources.rs`.

**V-02 · M · update — `sources::discover()` runs once at startup**
`src/main.rs:502`, `src/main.rs:512-515`
`cfg` reloads every iteration; `srcs` does not. A newly installed agent is
invisible until restart, with no warning.
*Fix:* re-discover on the poll-timeout branch and register new watches.

**V-03 · M · update — Tool identity is a bare string driving five behavioural decisions in three modules**
`src/sources.rs:7`, `src/workspace.rs:51`, `src/workspace.rs:113-119`,
`src/capture.rs:245-255`, `src/reader.rs:81-113`
Nothing fails to compile if a match arm is missed; you get a wrong label or an
unresolvable workspace at runtime, which lands in the data-discarding path.
*Fix:* `Tool` enum plus a struct of per-tool functions on `Source`.

**V-04 · M · create — Prompt-only stores are free to capture and are not captured**
`src/sources.rs:47`
`~/.claude/history.jsonl` (1 MB, 2,719 lines) and `~/.codex/history.jsonl`
(635 KB, 1,573 lines) are already JSONL and already tailable.
*Fix:* add a `format` discriminant to `Source`, then add both.

**V-05 · L · update — A missing transcript root produces no signal unless all three are missing**
`src/sources.rs:22`, `src/main.rs:526-528`
*Fix:* report each missing root in `status`.

**V-06 · L · create — Cursor's SQLite chat store holds 3 sessions with no JSONL trace**
`src/sources.rs:47`
87% redundant with `agent-transcripts/`. The 7.0 GB legacy `state.vscdb` is
stale since 2026-07-16. opencode and Zed both have zero rows.
*Fix:* defer; byte-offset tailing cannot address SQLite. Document the limit.

### Delivery and durability

**D-01 · C · update — A redirect carries the live API key to any host, over cleartext**
`src/uploader.rs:76`, `src/uploader.rs:144`, `src/destination.rs:369`
`ureq::get`/`post` free functions build a default `Agent`: `redirects(5)`,
`https_only(false)`. ureq's redirect filter strips only `content-length`,
`cookie` and `authorization` — **not** `x-api-key`. `normalize_api_url` permits
plain `http://`.
*Fix:* one shared agent with `.redirects(0)` and `.https_only(true)`; treat 3xx
as `Blocked`. Also fixes the fresh TLS handshake per batch.

**D-02 · H · update — `cursor.json` is atomic but not durable**
`src/spool.rs:435-441`, `src/spool.rs:428-433`, `src/spool.rs:493-503`
No `sync_data()` on the tmp file, no dir fsync — while the queue append *is*
fsynced. After a power loss the cursor can be present-but-empty; the parse error
is swallowed and `offset` returns 0, re-uploading up to 8 MiB.
*Fix:* create → write_all → sync_data → rename → fsync dir. Same after
compaction's rename.

**D-03 · H · update — A partial trailing line wedges a route forever, and the only recovery destroys every customer's queue**
`src/spool.rs:476-478`, `src/spool.rs:299-300`, `src/main.rs:910`,
`src/main.rs:1140-1149`
The next append writes a complete record after the partial one; `read_until`
then returns an unparseable line and `peek_batch` hard-errors on every pass
forever. `clear-queue --yes` `remove_dir_all`s the entire spool.
*Fix:* truncate back to the last newline before appending; quarantine-and-step
on a malformed front line; add `clear-queue <label>`.

**D-04 · H · create — No backoff anywhere**
`src/main.rs:963-964`, `src/main.rs:543-565`, `src/uploader.rs:105-107`
`Turn::Stop` only removes the lane for the current pass, and a pass runs per
debounced fs event. The `VERIFIED` cache stores successes only. A rotated key on
an active repo produces 1–3 request pairs per second per route indefinitely.
*Fix:* per-route exponential backoff with jitter (30 s → 30 min) held across
passes; cache negative identity results.

**D-05 · H · create — No disk cap on the spool, and de-allowed queues are immortal**
`src/spool.rs`, `src/main.rs:787`
`drain` filters by `route_allowed`, so a de-allowed repo's queue is never
drained and never deleted. Nothing bounds growth; compaction reclaims only the
delivered prefix.
*Fix:* per-route byte cap with oldest-first eviction and a reported drop count;
a total cap; age out long-de-allowed queues. Per decision 6.

**D-06 · H · update ★ — A legacy queue whose destination file dies can never be re-pointed**
`src/destination.rs:341-347`, `src/spool.rs:54-57`, `src/main.rs:883-890`
`same_identity` requires `key_fingerprint` on both routes; no `route.json` on
disk has one. The lazy adoption path needs the file to still exist *and* still
declare the now-optional `KHOTAN_ORG_ID`. This is the 11,461-record stall.
*Fix:* superseded by §5.4. Per decision 10, define legacy-queue behaviour rather
than building a recovery migration.

**D-07 · M · update — The serde migration breaks the documented rollback**
`src/destination.rs:18-19`, `src/spool.rs:172-174`
New → old fails: `"org_id": null` cannot deserialize into `String`, and both
`routes()` and `resolve_queue_dir` `continue` past an unparsable metadata file,
so the queue becomes invisible. `design.md` claims rollback is safe.
*Fix:* write `""` rather than `null`, or correct the claim.

**D-08 · M · update — Two repos sharing one key collapse into one queue**
`src/destination.rs:422`, `src/main.rs:787`, `src/destination.rs:118-120`
De-allowing repo A stops the drain of a queue that also holds repo B's records.
All of B's traffic reports under A's label.
*Fix:* superseded by §5.4.

**D-09 · M · update — Delivery success is HTTP status only**
`src/uploader.rs:153-157`, `src/main.rs:940`
Any non-error response advances the cursor over the whole batch; the body is not
read on success.
*Fix:* partial-accept handling per §4b.

**D-10 · M · update — The learned byte budget is in-memory only**
`src/main.rs:790`, `src/main.rs:835-837`
Rediscovered every pass and after every restart; `Turn::Parked` throws it away
mid-pass.
*Fix:* persist in `cursor.json`; grow back on sustained success. Largely
obviated by §4d.

**D-11 · M · update — Quarantine is silent, unbounded, and invisible to `status`**
`src/spool.rs:349-375`, `src/spool.rs:407-421`, `src/main.rs:447-454`
`has_quarantine()` matches only `legacy-spool-*`. No bound, no count, no replay
tool — though `design.md` promises replay by hand. The largest queued records
are 2.7 / 2.6 / 2.4 MB, all headed here.
*Fix:* count in `status`, bound the file, add a replay subcommand.

**D-12 · M · update — One transient failure costs a route the rest of the pass**
`src/main.rs:963`, `src/main.rs:842`
`Upload::Retry` maps to `Turn::Stop`, dropping the lane for the remaining 120 s.
*Fix:* keep the lane and skip it for a few cycles.

**D-13 · M · update — No proxy support, and TLS trust is a compiled-in bundle**
`Cargo.toml:15`, `src/uploader.rs:239`
`proxy-from-env` and `native-certs` are off. `transport_reason` discards the
underlying error, so the operator sees only "Upload failed."
*Fix:* enable both; keep a short redacted form of the real error.

**D-14 · L · update — Appends and compaction are safe only by accident of call ordering**
`src/main.rs:663-697`, `src/spool.rs:493-503`
No lock on any spool directory. Compaction copies to EOF and renames; a record
appended after the copy pointer passes EOF is unlinked with the old inode.
*Fix:* document the invariant and take an `fs2` lock during compaction.

**D-15 · L · update — Failed mid-append duplicates; `.tmp` files leak; the error excerpt regexes up to 10 MiB**
`src/spool.rs:149-157`, `src/spool.rs:65-75`, `src/uploader.rs:204-213`
*Fix:* truncate back to the pre-append length on failure; clean stale `.tmp` on
open; `take(4096)` before scrubbing.

### Security and privacy

**P-01 · H · update — Redaction misses several very common secret shapes**
`src/redact.rs:11-32` — tested empirically against the 17 patterns.

| Input | Result |
| --- | --- |
| `sk-proj-abcdEFGH1234ijklMNOP…` | **plain** — pattern breaks on the dash after `proj` |
| `github_pat_11ABCDEFG0abc…` | **plain** — only `gh[pousr]_` covered |
| `VERCEL_TOKEN=aBcD1234eFgH…` | **plain** — keyword list has no bare `token` |
| PEM private key in a JSON string | **header only** — base64 body survives verbatim |
| `ASIAQWERTYUIOPASDFGH` (AWS STS) | **plain** — only `AKIA` covered |
| `xapp-1-A012345-6789-abcdef` | **plain** — `xapp-` and `xoxe` missing |
| `AKIA…` / `ghp_…` / `xoxb…` / JWT / `Bearer` | redacted |

*Fix:* add `sk-proj-`, `github_pat_`, `(ASIA|ABIA|ACCA)`, `xapp-`, `xoxe`,
`glpat-`, a multiline PEM block pattern plus its `\n`-escaped variant, and bare
`token` / `credential` / `private[_-]?key` to the keyword alternation.
**The server does not re-scrub, so these patterns are the only line of defence.**

**P-02 · M · update — The server error excerpt would log an echoed key that is not shaped like `mk_*`**
`src/redact.rs:15-17`, `src/uploader.rs:145`
A response of `{"error":"unknown credential a1b2c3…"}` is logged verbatim.
*Fix:* also scrub any exact substring match of the key in flight.

**P-03 · L · update — `key_fingerprint` is FNV-1a of the API key on disk**
`src/destination.rs:58-65`, `src/spool.rs:52-53`
Acceptable, not ideal. FNV gives essentially zero work factor for
confirmation-of-guess and is trivially collidable, while `queue_matches` treats
fingerprint equality as identity. The code fails closed on a directory-name
collision but not on a fingerprint collision.
*Fix:* superseded by §5.4, which removes fingerprints.

**P-04 · L · update — Redaction over-matches ordinary code**
`src/redact.rs:15-16`
`apiKey: process.env.FOO` in a code block becomes `[REDACTED]`.
*Fix:* per decision 9, loosen surgically — suppress only clear references
(`process.env.X`, `os.environ[…]`, `${VAR}`), never literals.

### Setup and lifecycle

**S-01 · H · create — Per-repo credential files are the wrong shape for the stated goal**
`src/destination.rs:8-9`, `src/config.rs:9-31`
Nothing writes `env.khotan.local` — it is read-only in the codebase. No `login`,
`enroll` or `init`. Coverage is opt-in per repo per machine: a fresh clone of an
onboarded repo captures nothing and nothing reports it. N employees × M repos =
N×M copies of the org key in working trees.
*Fix:* per decision 2 — see §5.4.

**S-02 · C · update — A malformed config silently empties the allow list and issues a new `device_id`**
`src/main.rs:209`, `src/config.rs:64-73`
`Config::load().unwrap_or(Config::fresh(random_id()?))` — `NotFound` and "TOML
is malformed" are the same branch. The machine looks like a new install and its
history is orphaned. Reproduced.
*Fix:* distinguish the two; bail loudly, back the file up, never re-roll
`device_id`.

**S-03 · H · update ★ — Search roots are frozen at first config write**
`src/config.rs:49-62`, `src/config.rs:95-99`, `src/main.rs:121-125`
`default_search_roots()` filters by `is_dir()` at creation time and `render()`
always writes the resolved list; `#[serde(default)]` only fires when the key is
absent. `--search-root` is rejected as "a preset."
*Fix:* recompute on load and union with stored extras; add
`configure --add-search-root`; omit from `render()` when it equals the default.

**S-04 · H · update — A tag that doesn't match `Cargo.toml` breaks updates permanently, fleet-wide**
`.github/workflows/release.yml:26-51`, `src/update.rs:268`, `src/update.rs:80`
`update` re-downloads the identical binary forever and every machine shows a
permanent red `ALERT` that reinstalling cannot clear. `workflow_dispatch` checks
out the dispatched ref, not `inputs.tag`.
*Fix:* assert tag == crate version in the workflow; fix `workflow_dispatch`.

**S-05 · H · update — `update` can leave the machine with no binary and no agent**
`src/update.rs:299-325`
Unload → `remove_file(dest)` → `fs::copy`. A failed copy propagates through
`result?` and the observer is dead and stays dead.
*Fix:* stage at `dest.parent()/khotan-observer.new` and `rename`; restart the old
binary on error.

**S-06 · C · update ★ — A single foreground `run` permanently disables background monitoring**
`src/main.rs:478`, `src/agent.rs:133-140`
`run` calls `launchctl unload` and nothing reloads it — no `Drop`, no signal
handler (`grep -rn "ctrlc|SIGINT|SIGTERM" src/` → none). The README says "You do
not need `khotan-observer stop` after a foreground session" and never mentions
re-running `start`. Combined with O-01, this is the compound failure most
threatening the stated goal.
*Fix:* RAII guard plus SIGINT/SIGTERM handler restoring the agent, or refuse
`run` while an agent is loaded unless `--takeover` is passed.

**S-07 · H · update — `uninstall` removes one file and leaves captured transcript content behind**
`src/agent.rs:171-179`

| Artifact | Removed |
| --- | --- |
| LaunchAgent plist | yes |
| binary, config incl. `device_id` | no |
| **spool — queued transcript content** | no |
| offsets, inbox, log, `help.md` | no |

*Fix:* `uninstall --purge`; plain `uninstall` prints exactly what it left.

**S-08 · H · create — No transparency command**
`src/main.rs:40-62`, `src/main.rs:1123`
`read` inspects the proof-receiver inbox, not the spool. Delivered records are
deleted locally, so there is no reviewable history. Neither README nor `help.md`
names the spool path.
*Fix:* `queue --show` and `history`. Per decision 7.

**S-09 · M · update — `KeepAlive` with no `ThrottleInterval` turns any startup error into an infinite crash loop**
`src/agent.rs:45-46`, `src/agent.rs:53-56`
Restarts every 10 s forever, ~1.1 MB/day into an unrotated log. `panic = "abort"`
takes the same path. The plist is otherwise well tuned (`ProcessType:
Background`, `LowPriorityIO`, `Nice 10`).
*Fix:* `ThrottleInterval` 60; rotate the log at ~10 MB; consider
`KeepAlive: { SuccessfulExit: false, Crashed: true }`.

**S-10 · M · update — GitHub API rate limits silently disable the only staleness signal**
`src/update.rs:10-11`, `src/update.rs:49-57`, `src/main.rs:529`
60/hr is per source IP — shared across an office NAT, and burned by one
crash-looping agent alone. On 403 the failure is swallowed by `.ok()?`. Note
`install.sh` uses the un-rate-limited redirect, so installing works while
updating breaks.
*Fix:* use the same redirect; check once per 24 h, cached; special-case 403/429.

**S-11 · M · update — `stop` reports success even when the unload failed**
`src/agent.rs:142-149`, `src/agent.rs:91-96`, `src/agent.rs:181-187`
`unload_loaded` discards `.output()` and returns `Ok(())` unconditionally.
`load -w`/`unload` are deprecated in favour of `bootstrap`/`bootout`.
*Fix:* check the status and surface stderr; migrate the verbs.

**S-12 · M · update — `receive` and `read` silently default when a flag's value is missing**
`src/main.rs:1005-1016`, `src/main.rs:1070-1093`, `src/main.rs:140-148`
`receive --token qa --bind` binds the default with no complaint. Unknown
subcommands exit **0**. No `--version`, no per-subcommand `--help`.
*Fix:* resolved by adopting `clap` (decision 5).

**S-13 · M · update — `curl | bash` never starts anything, and its next-step commands don't resolve**
`dist/install.sh:102-131`, `:150-161`, `:93-99`
Prints bare `khotan-observer …` right after warning that `~/.local/bin` is not
on PATH. Never runs `start`. Auto-opens `configure` before any destination file
can exist. The SIGKILL guard at `:93-99` is dead code — inside `if ! cmd; then`,
`$?` is the negated status.
*Fix:* offer the PATH export or print qualified commands; lead with `start`;
capture the status code before the `if`.

**S-14 · L · update — Stale `hmc enroll` error string**
`src/config.rs:69` — names a binary and subcommand that do not exist; fires on
`status`, `run` and `run-once`.
*Fix:* "run `khotan-observer configure` first".

**S-15 · L · update — Unsigned binary plus a README link that triggers Gatekeeper**
`README.md:22`, `dist/install.sh:19-20`
The `curl` path dodges quarantine; a browser download does not, and on macOS 15+
there is no right-click bypass.
*Fix:* remove the Releases link from the install section, or sign and notarize.

**S-16 · L · update — Release workflow uses a floating toolchain, runner and action refs**
`.github/workflows/release.yml:19`, `:30-32`, `:45`
`--locked` is used, which is good; the rest floats, in a workflow holding
`contents: write`.
*Fix:* pin the toolchain version, `macos-15`, and SHA-pin all four actions.

### Observability and fleet health

**O-01 · C · create ★ — No heartbeat: a dead observer is indistinguishable from a quiet one, forever**
`src/main.rs:784-794`, `src/uploader.rs:22`, `src/uploader.rs:124`
Both network calls fire only for lanes built from non-empty queues, so an idle
observer makes zero network calls. The server cannot distinguish a healthy quiet
day, a crash loop, an empty allow list, an unfinished `configure`, an
uninstalled tool, and a machine that has been off for a month.
*Fix:* §4c. Highest-leverage item in this review.

**O-02 · M · update — `status` cannot debug a stuck queue**
`src/main.rs:446`, `src/spool.rs:254-258`, `src/spool.rs:219`
One machine-wide aggregate. No per-route counts, no last error, no last success,
no version — though `pending_for(route)` and `routes()` both already exist.
*Fix:* per-route table; persist `last_error`/`last_success` into `route.json`.

**O-03 · M · update — `status` reports "running" for a crash-looping agent**
`src/agent.rs:181-187`, `src/main.rs:377-385`
`is_loaded()` only checks that the label exists; `launchctl list` shows `-` in
the PID column for a registered-but-dead service.
*Fix:* parse PID and `LastExitStatus`.

**O-04 · M · update — Every carefully built error context is discarded where a human reads it**
`src/main.rs:910`, `src/main.rs:880-890`, `src/capture.rs:100`
`destination.rs:354`, `spool.rs:300` and `capture.rs:115` all build context; the
drain replaces it with a fixed string.
*Fix:* propagate the `anyhow::Error` into `Turn::Stop`'s reason — it already
formats with `{e:#}` at `main.rs:32`.

**O-05 · L · update — A DST transition makes every log timestamp wrong until restart**
`src/log.rs:11-21`
The offset comes from forking `date +%z` once and caching it forever.
*Fix:* re-resolve every few hours, or read `TZ`/`localtime` directly.

### Testing and CI

**T-01 · C · update ★ — `cargo test` fails 1 run in 4 with no code change**
`src/singleton.rs:60`, `src/log.rs:13`, `src/update.rs:196`
`log::` and `update::` are the only modules that spawn subprocesses.
`Command::spawn` forks and the child inherits the flock'd descriptor —
`O_CLOEXEC` releases at `exec`, not at `fork`. Skipping both makes the flake
vanish; skipping either alone does not. Production is unaffected.
*Fix:* bounded-retry the third acquire, or serialize the test.

**T-02 · C · create ★ — No CI runs the tests. Ever.**
`.github/workflows/release.yml`
The only workflow triggers on `v*` tags: checkout → install Rust → build →
package → upload. No test, clippy or fmt step, and no push/PR workflow at all.
Measured cost of the missing job: ~12 s cold.
*Fix:* `ci.yml` on push and PR running fmt, clippy `-D warnings`,
`test --locked`, and `smoke-install.sh`. Gate the release job on it.

**T-03 · H · create — No `[lib]` target, so integration tests are structurally impossible**
`Cargo.toml:10-12`, `src/capture.rs:467-470`
`cargo test --lib` → `error: no library targets found`. AGENTS.md:130 tells
contributors to run it; it has never worked. `capture.rs:467` constructs an
`Offsets` by reaching into private fields, legal only from inside that file.
*Fix:* `src/lib.rs` + thin `src/main.rs`. Size-neutral under LTO.

**T-04 · H · create — The core pipeline function has no test and cannot get one**
`src/main.rs:637-643`, `src/capture.rs:18-21`
`scan_and_ship` is fully parameterized; the blocker is that `Offsets` exposes
only `load()`, which reads the developer's real 434 KB `offsets.json`.
*Fix:* `pub fn Offsets::at(path)`. Three lines.

**T-05 · H · create — No end-to-end test**
`src/capture.rs:449`, `src/main.rs:1271`, `src/receiver.rs`
Capture tests stop at `collect_new`; drain tests start at `spool.append`.
Redaction is tested on strings only. `receiver.rs` is a complete working HTTP
sink used by nothing but its own three tests.
*Fix:* temp jsonl with a fake key → `scan_and_ship` → `receiver::serve` on
`127.0.0.1:0` → assert landed, `[REDACTED]`, pending 0, offset advanced.

**T-06 · M · create — Three missing seams keep most of the code untestable**

| Seam | Replaces | Unblocks |
| --- | --- | --- |
| `Offsets::at(path)` | `Offsets::load()` | `scan_and_ship` |
| `struct Paths { home }` | `config::home()` | Config, sources, Spool, docs, singleton, agent |
| `Observer::tick()` | the loop body in `watch` | notify, debounce, poll fallback, heartbeat |
| `trait Transport` | direct `ureq::` calls | status classification as a pure function |
| `trait Clock` | inline `Instant::now()` | the 120 s drain budget |
| `trait Launchctl` | `Command::new("launchctl")` | start/stop/is_loaded/uninstall |

**T-07 · M · create — Temp-dir cleanup is a tail statement, not RAII**
`src/spool.rs:511-519`, `src/capture.rs:340-348`
Nine near-identical helpers clean up in the last statement of the test body,
which a panicking assertion skips. No `impl Drop` in test code. Two leaked
artifacts were present on the reviewed machine. `singleton.rs:56` keys on
`process::id()`, which is not unique across a run.
*Fix:* `tempfile` as a dev-dependency — zero shipped bytes.

**T-08 · L · delete — A test is keeping dead production code alive**
`src/log.rs:83`, `src/log.rs:561`, `src/main.rs:690`
`attributed` is used only by its own test. Plus a `collapsible_if`. Task 6.3 of
the in-flight change claims clippy is clean; both warnings are live.
*Fix:* delete both, then turn on `-D warnings`.

**T-09 · L · update — `smoke-install.sh` is run by nothing and has its own flakiness**
`dist/smoke-install.sh:27`, `:35`
The only coverage of `install.sh`. Hardcoded port 8765; `sleep 0.3` as the
readiness barrier.
*Fix:* bind port 0, poll for readiness, wire into CI.

**T-10 · L · create — `src/sources.rs` has zero tests**
53 LOC, three functions, no `#[cfg(test)]` module. `walk` is a hand-rolled
recursive walker with a depth cap that silently swallows permission errors.

### Architecture and performance

**A-01 · H · create — `main.rs` holds the drain algorithm beside the arg parsers**
`src/main.rs:736-974`

| Lines | Responsibility | Move to |
| --- | --- | --- |
| 65–205, 991–1137 | help + arg parsing | `src/cli.rs` |
| 207–370 | configure + picker glue | `src/commands/configure.rs` |
| 372–457 | status rendering | `src/commands/status.rs` |
| 459–472, 576–734 | Pass, report, scan_and_ship | `src/pass.rs` |
| 474–574 | run loop | `src/runloop.rs` |
| **736–974** | **drain orchestration** | **`src/drain.rs`** |
| 1–63 | dispatch | stays |

**A-02 · H · update ★ — `WorkspaceIndex::discover` runs twice per pass**
`src/main.rs:659`, `src/main.rs:782`, `src/workspace.rs:80-110`
~60,700 readdir + ~179,000 stat per pass. `scan_candidates` prunes six literal
names to depth 8 and does three stats per directory. Measured: `status` ≈ 1.0 s
wall, 90% kernel. At `poll_secs = 45` that is ~2.4% of a core continuously — and
the 45 s interval is a floor only when idle: on any notify event the loop
debounces 300 ms and runs the whole pass, so during active coding passes run
back-to-back at roughly one per 1.3 s.
*Fix:* hoist the index into `drain` (one line, halves it); cache with a 5-minute
TTL; rate-limit the event path to a 5 s minimum; stop descending past a `.git`.

**A-03 · M · update — Destination files and transcript cwds are re-read every pass**
`src/workspace.rs:124-146`, `src/destination.rs:222-241`, `src/main.rs:880`
`transcript_cwd` re-reads and re-parses up to 256 KB of every active
Claude/Codex transcript per pass, for a value that never changes.
`env.khotan.local` is resolved per file per pass via an upward walk plus a full
read, again in `discover_routes`, and again per batch in `resolve_credentials`.
Index resolution is O(files × candidates) with a fresh `String` per comparison.
*Fix:* memoize by (path, inode); per-pass route map; a slug `HashMap` per index.

**A-04 · M · update — Unbounded worst-case memory; redaction is 17 full scans per line**
`src/capture.rs:95-104`, `src/redact.rs:37-45`
`collect_new` accumulates a `CapturedFile` for every changed file before
returning, with no cap on file count — theoretical ceiling ~20 GB after a long
stop. `scrub` loops 17 regexes with a fresh allocation per hit; a 150 KB line is
≥2.5 MB of scanning. (The regexes *are* compiled once in a `OnceLock`, and
Rust's engine cannot backtrack catastrophically.)
*Fix:* cap total bytes per pass; replace `Vec<Regex>` with a `RegexSet`
pre-filter.

**A-05 · M · update — Four hand-rolled arg parsers with three conventions**

| Build | Size | Delta |
| --- | --- | --- |
| baseline, no clap | 286,080 B | — |
| clap 4 + derive | 436,176 B | +150,096 B |
| clap 4, `default-features = false` | 419,520 B | +133,440 B |
| **current shipped binary** | **2,503,200 B** | **+5.3% for minimal clap** |

Measured under this project's exact release profile, full 14-subcommand surface.

**A-06 · M · update — `anyhow` leaks throughout the domain**
`src/config.rs:1`, `src/capture.rs:7`, `src/spool.rs:4`, `src/uploader.rs:37-54`
AGENTS.md says "at the CLI boundary"; 13 of 19 modules import it. `uploader` is
the one that got it right, with typed `Upload`/`OrgOutcome` enums.
*Fix:* adopt uploader's pattern, or amend AGENTS.md. Do not leave the rule
stated and unfollowed.

**A-07 · M · update — `destination.rs` is a second god module**
728 lines over six concerns. FNV-1a is implemented twice within the file
(`:59-63` and `:249-257`) while the doc comment claims one definition serves all.
*Fix:* split into `allowlist.rs` + `envfile.rs` + `route.rs`.

**A-08 · M · update — A third hash, and this one is not stable across Rust releases**
`src/store.rs:67`
`content_key` uses `DefaultHasher`, documented as unstable, and the value is
persisted to disk for cross-run dedup.
*Fix:* use the FNV from `destination.rs`.

**A-09 · M · update — `panic = "abort"` leaves a dead error path and a user-facing defect**
`Cargo.toml:26`, `src/main.rs:809`, `src/picker.rs:366-402`
`handle.join().unwrap_or(Turn::Stop(…))` is unreachable under abort.
`RawMode::drop` does not run, so a panic during `configure` leaves the terminal
in raw mode.
*Fix:* drop the unreachable fallback or drop `panic = "abort"`; pair with S-09.

**A-10 · L · update — Primitive obsession**
`src/record.rs:12`, `src/main.rs:705`, `src/main.rs:879-881`
No `Tool` enum. `BTreeMap<String, (usize, Option<(log::Tone, String)>)>`.
`Credentials` is unwrapped one line after creation.

**A-11 · L · delete — The local receiver is 340 lines of hand-rolled HTTP shipped to employee laptops**
`src/receiver.rs:31-340`, `:47-56`, `:101`
Single-threaded accept loop; a `content_length` parse failure silently becomes 0.
*Fix:* gate behind `#[cfg(feature = "receiver")]`, off by default. Keep it for
tests and QA builds — it is the ideal target for T-05.

**A-12 · L · update — Computed work discarded, thread churn, misplaced doc comment**
`src/log.rs:441`, `src/main.rs:469`, `src/main.rs:801-804`, `src/config.rs:143`
`log::idle`'s second parameter is `_spool`, ignored — but computing it runs a
full spool scan. `drain` spawns a fresh scoped thread per lane per cycle.

### OpenSpec

**G-01 · C · create ★ — The entire `openspec/` tree is untracked in git**
`git ls-files openspec` → 0 files. `.gitignore` contains only `/target`,
`**/*.rs.bk`, `.DS_Store` — it was never excluded, just never added. Every spec,
both main capabilities, the in-flight change and the archived
`2026-08-25-simplify-destination-setup` exist only on one machine's disk.

**G-02 · H · create — Two capabilities are specced; roughly fifteen ship**

| Capability | Spec |
| --- | --- |
| destination identity | present |
| repo selection / status | present |
| spool · upload batching · drain scheduling | delta only, unmerged |
| **redaction** | absent |
| capture / tailing + offsets | absent |
| sources / agent coverage | absent |
| workspace mapping | absent |
| launchagent lifecycle | absent |
| update / self-replace | absent |
| config · receiver · reader · docs | absent |

`openspec validate --specs` attests to 8 requirements while ~20 are live.
Redaction — the entire privacy guarantee — has no spec at all.

**G-03 · H · update — `fix-observer-delivery` is not archivable: four process blockers, zero substance blockers**
Five requirements were spot-checked against the code and all five are correctly
implemented — the always-send-one rule, incremental cursor counting, the pass
deadline, identity-check size refusals, and label-order fairness. It fails on
process: ~1,900 lines uncommitted on top of an already-tagged v0.1.26;
`openspec/` untracked; task 6.3 claims clippy is clean and it is not; the three
delta capabilities are not merged into `openspec/specs/`.

**G-04 · M · update — The change's own artifacts disclose that its main machinery has never run in production**
`design.md:22-24`, `tasks.md:55-57`
"No HTTP 413 was observed in roughly 7,000 delivered records." The fix that
actually resolved the stall was the one-line `20s → 60s` timeout — task **6.4**,
appended out of order between 6.1 and 6.2. The entire size-refusal →
budget-halving → quarantine path rests on unit tests alone.
*Fix:* state it in the proposal; exercise the path against a server that refuses
on size before trusting it.

**G-05 · M · update — `openspec/config.yaml` is entirely commented-out boilerplate**
Only `schema: spec-driven` is set; there is no `openspec/project.md`. The
toolchain has zero knowledge of the constraints `design.md` treats as
load-bearing.
*Fix:* fill `context` from AGENTS.md; add `rules.specs` requiring security
capabilities to spell out the fail-closed direction, `rules.tasks` requiring
sequential numbering, and `operations.archive` requiring the work to be
committed and released first.

**G-06 · L · update — Archived changes are never machine-validated again**
`openspec validate --all --strict` covers exactly 3 items. Validating the
archived change by name returns `Unknown item`. (The archive itself was executed
correctly — all 8 requirements and 18 scenarios carried across byte-identical
apart from the expected header transform.)

---

## 7. Done well — do not regress these

- **Panic discipline.** Two `expect()` calls in 8,421 lines of non-test code
  (`main.rs:494`, `main.rs:616`), both provably unreachable. No `unwrap()`, no
  `panic!` outside tests.
- **Offsets are never committed before durable spooling.** `read_file` takes
  `&Offsets` and returns `next_offset` for the caller to commit; `spool.append`
  `sync_data()`s before returning. This is the hard part and it is right.
- **Partial trailing lines on the capture side.** `rposition` for the last `\n`
  plus `next_offset = offset + last_nl + 1` means a partial line is never
  emitted truncated and never double-sent.
- **Ambiguous workspace encodings fail closed** (`workspace.rs:73-75`), as do
  conflicting env files, route-id hash collisions, and `same_identity` when a
  fingerprint is absent.
- **Claude and Codex read the workspace from file *content*, not the path**
  (`workspace.rs:51-55`), which sidesteps the dash-mangling problem entirely for
  two of three tools.
- **Subagent detection is empirically correct.** All 44 Claude transcripts
  containing `"isSidechain":true` live under a `subagents/` directory; Cursor
  uses the identical layout.
- **The spool cursor design.** `{offset, pending, scanned_to}` with atomic
  writes, advisory self-healing semantics, and compaction only past 8 MiB turned
  an O(backlog) rewrite per batch into O(1). The best-engineered part of the
  codebase.
- **`uploader.rs`'s typed error model** — `Upload`/`OrgOutcome` with `TooLarge`
  distinguished from `Blocked`, and a heuristic for servers that answer 400 for
  an oversized body.
- **Round-robin drain under `thread::scope` with a deadline**, with a fairness
  test that genuinely fails on the old behaviour.
- **The API key never reaches a record, queue metadata, or a log.** Verified by
  grep; there is a test asserting the secret is absent from the request body,
  and a cross-tenant leak test.
- **`normalize_api_url` rejects userinfo, paths, queries and whitespace.**
- **Test names are behavioural sentences** that read as a spec, several encoding
  the bug they replaced.
- **`picker.rs` is the testability model** — `State::new(rows)` +
  `state.apply(&[u8]) -> Outcome` is a pure state machine with I/O isolated in
  `draw()` and a real `impl Drop`. Every other module should look like this.
- **The LaunchAgent plist is well tuned** — `ProcessType: Background`,
  `LowPriorityIO`, `Nice 10`.
- **Binary size discipline pays off** — 2.4 MB stripped, with every std-only
  decision documented at its site.
- **Comments explain why, not what**, nearly always carrying the motivating
  incident.
- **OpenSpec format compliance is perfect** across all 7 spec files — 28
  requirements, 56 scenarios, correct hashtag depth, `--all --strict` green.

---

## 8. Method

Six subagent audits run in parallel over the working tree, each required to cite
file and line for every claim and to distinguish verified findings from
inference. Claims marked ★ were independently reproduced against live machine
state — spool contents, `offsets.json`, `launchctl list`, repeated `cargo test`
runs, `git ls-files`, and timed `khotan-observer status`. Binary size figures for
`clap` were measured by building the full CLI surface under this project's exact
release profile, not estimated.
