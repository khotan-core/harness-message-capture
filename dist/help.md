# khotan-observer log lines

This file ships inside the binary. Run `khotan-observer docs` to print it.
Install also writes a copy to `~/.local/share/khotan-observer/help.md`.

Nothing leaves this machine unless the chat maps to a git repo on the allow
list, and that repo has a complete `env.khotan.local` or `.env.khotan.local`.

## Activity lines

`captured N` means new transcript lines were queued.

`uploaded N` means those lines were sent to the customer endpoint.

`skipped N` means lines were not sent.

`queued N` means lines wait on disk because the endpoint did not accept them.

`idle · watching N files` means no new lines this pass.

## Skip reasons

`empty-window · no repo on this machine, nothing sent`

The chat folder is not a checkout under the search roots. Common causes are a
chat with no repo, or a folder that moved.

`podium-automation · dest file broken, nothing sent`

The dest file is missing fields, or `env.khotan.local` and `.env.khotan.local`
disagree. The observer does not advance the offset. It retries.

`empty-window · matched two folders, nothing sent`

Two checkouts encode to the same chat folder name. The observer does not guess.
It does not advance the offset.

A skip with no extra clause means the repo is not on the allow list, or it has
no dest file. That is expected. Nothing was sent.

## Status lines

`Routes: 0 customer destination(s)` means no allowed repo has a complete dest
file.

`allow: none` means the allow list is empty, so nothing uploads.

## Commands

```
khotan-observer configure --allow-repo <folder>
khotan-observer start
khotan-observer docs
khotan-observer status
```
