# khotan-observer log lines

This file ships inside the binary. Run `khotan-observer docs` to print it.
Install also writes a copy to `~/.local/share/khotan-observer/help.md`.

Nothing leaves this machine unless the chat maps to a git repo on the allow
list, and that repo has a complete `env.khotan.local` or `.env.khotan.local`.

Deliveries print in green. Warnings print in orange. Errors print in red.

Each record names its root chat as `thread_id` and marks `agent_role` as
`root` or `subagent`. `seq` is the byte offset of that line in the source file.

Default `run` and `run-once` print only repositories on the allow list.
Their captures, uploads, and failures still print. Skip lines for other
folders stay off. To print every skip line, run `khotan-observer run --all-logs`.

## Activity lines

Each workspace prints on its own line. The name in parentheses is the
transcript source: `cursor`, `claude`, or `codex`. Counts on that line
belong to that folder and source only.

`usi (cursor)   skipped 432   queued 200   (Repo is real, but not on the allow list)`

`usi` is the folder name. Those 432 Cursor lines were not sent. The 200
queued lines are leftovers from when that folder was allowed.

`dev-serve-robotics   queued 188   (Host is up in DNS, port is closed)`

The endpoint did not accept those 188 lines. They stay on disk.

`captured N` means new transcript lines were written to the local queue.

`uploaded N` means those lines were sent to the customer endpoint.

`skipped N` means lines were not sent.

`queued N` means lines wait on disk because the endpoint did not accept them.

`+N more   skipped M` means more skip-only folders were folded off this pass.

`idle (No new lines this pass · N files)` means the watcher is up and quiet.

## Startup warns (orange)

`No Cursor, Claude, or Codex folders`

None of the transcript roots exist on this machine.

## Startup alerts (red)

`ALERT  Newer observer v0.1.17 is out (this binary is 0.1.16)`

GitHub Releases has a newer tagged build than this binary. Capture still
runs. The line prints as a bright-red bar so it cannot hide in skip noise.
Reinstall to pick up the new binary.

## Skip reasons (orange)

`empty-window (cursor)   skipped 12   (Chat has no project folder)`

The chat is not tied to a checkout. Cursor names that window `empty-window`.

`podium-mirror (cursor)   skipped 40   (Repo is real, but not on the allow list)`

The folder exists. You did not select it in `configure`.

`podium-automation (claude)   skipped 8   (Repo found, dest file missing fields or conflicts)`

The dest file is missing fields, or `env.khotan.local` and `.env.khotan.local`
disagree. The observer does not advance the offset. It retries.

`empty-window (cursor)   skipped 4   (Same encoded path matches two checkouts)`

Two checkouts encode to the same chat folder name. The observer does not guess.
It does not advance the offset.

## Delivery problems

`podium-automation (Host is up in DNS, port is closed)` — orange. Retry later.

`podium-automation (No answer in time)` — orange. Retry later.

`podium-automation (DNS failed)` — orange. Retry later.

`podium-automation (Server error or rate limit)` — orange. Retry later.

`podium-automation (Key or request refused. Queue keeps the lines)` — red.

`podium-automation (Key's org does not match KHOTAN_ORG_ID)` — red.

`podium-automation (Dest file gone or URL/org changed after queue)` — red.

`podium-automation (The /api/v1/me body was not usable)` — red.

`podium-automation (A queued file is unreadable)` — red.

`podium-automation (Send worked, local delete failed)` — red.

`podium-automation (Disk write to the spool failed)` — red.

`observer (Progress file did not write)` — red.

## Commands

```
khotan-observer configure
khotan-observer start
khotan-observer update
khotan-observer docs
khotan-observer status
```
