# khotan-observer log lines

This file ships inside the binary. Run `khotan-observer docs` to print it.
Install also writes a copy to `~/.local/share/khotan-observer/help.md`.

Nothing leaves this machine unless the chat maps to a git repo on the allow
list, and that repo has a complete `env.khotan.local` or `.env.khotan.local`.
A destination file is complete with `KHOTAN_API_URL` and `KHOTAN_API_KEY`.
`KHOTAN_ORG_ID` is optional: the organization is otherwise read from the key at
send time. Declaring it keeps the stronger check that a key pasted into the
wrong repository is caught on the first upload rather than after it resolves.

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

`podium-automation (Server error or rate limit · ingest 503 · upstream timeout)`
— orange. Retry later. Delivery lines carry the HTTP status and the first words
of the server's own answer, so a refusal names its cause.

`podium-automation (Key or request refused · ingest 401 · invalid key · queue
keeps the lines)` — red.

`podium-automation (One record was too big to send · ingest 413 · Body exceeded
1mb limit)` — orange. Batches shrink themselves when a server refuses their
size. A single line no batch size can carry moves to `quarantine/` so the rest
of the queue keeps going.

`podium-automation (Key resolves to a different org than this queue is bound to)`
— red. The endpoint reported a different organization than the one declared in
the destination file, or the one already pinned to this queue. The lines stay
queued rather than going to the wrong organization.

`podium-automation (Dest file gone and no repo with the same key was found)` —
red. The file this queue was pinned to no longer produces a key, and no other
allowed repository on the machine carries the same origin and key to deliver
through instead.

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
