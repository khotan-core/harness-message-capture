## Why

Enrolling a repository takes three secrets when the endpoint only needs two.
`KHOTAN_ORG_ID` carries nothing the API key does not already determine: a live
check against all seven destinations on this machine showed `GET /api/v1/me`
returning the org for every key, matching the hand-written value in all five
files that were filled in independently. The field exists as a fail-closed
assertion — it catches one customer's key pasted into another customer's repo —
but it costs a lookup that a person has to do by hand, and two repositories on
this machine sat silently unenrolled because it was missing.

Silently is the second problem. `status` reports only the routes that work, so a
repository with a half-filled destination file is indistinguishable from one
that was never set up. The interactive picker does show the reason, but nothing
points anybody at it, and `configure --allow-repo` replaces the whole allow list
rather than adding to it, so growing the list means retyping every entry.

The third problem is what the org id does to the queue. A queue is named
`hash(api_url, org_id)`, so two repositories sharing an org share one queue, and
that queue is pinned to whichever destination file created it. When one repo's
file was later repurposed, 11,461 records were stranded behind it even though a
sibling checkout held a working key for the same org.

## What Changes

- **BREAKING** for new queues only: derive a route's identity from the origin
  and a fingerprint of its API key rather than from the origin and the org id.
  Existing queue directories are adopted by their recorded metadata and are
  never renamed, so nothing already queued moves or is lost.
- Make `KHOTAN_ORG_ID` optional. A destination file with `KHOTAN_API_URL` and
  `KHOTAN_API_KEY` is complete. The org is read from `/api/v1/me` — the call the
  uploader already makes before every batch — and pinned into the queue's
  metadata the first time it resolves.
- Keep the cross-customer guard: a declared `KHOTAN_ORG_ID` is still enforced,
  and once an org is pinned to a queue, a later disagreement blocks the route
  instead of uploading.
- Re-point a queue at a live destination file that carries the same identity
  when the file it was pinned to stops resolving, so one repurposed file cannot
  strand records another checkout could deliver.
- Report repositories that have a destination file but cannot upload, with the
  reason, in `status`.
- Add `configure --add-repo` and `--remove-repo`, leaving `--allow-repo` as the
  replace-everything form it already is.

## Capabilities

### New Capabilities
- `delivery/destination-identity`: what a destination file must carry, how a
  route's org is established and enforced, and how a queue is identified.
- `setup/repo-selection`: how a person sees which repositories are enrolled,
  which cannot be, and why, and how they change that list.

### Modified Capabilities
<!-- None. `delivery/spool-queue` and `delivery/upload-batching` from
     fix-observer-delivery are untouched: this change alters how a queue is
     named and how credentials are resolved, not how records enter or leave it. -->

## Impact

- `src/destination.rs`: optional `KHOTAN_ORG_ID`, `RouteRef.org_id` becomes
  optional, identity derived from origin plus key fingerprint, readiness and
  diagnostics updated.
- `src/uploader.rs`: `verify_org` returns the resolved org instead of only
  comparing it; the batch body always carries a concrete org.
- `src/spool.rs`: route metadata gains the key fingerprint and a pinned org;
  queue directories are matched by recorded identity rather than by name.
- `src/main.rs`: `status` lists blocked repositories; `configure` gains
  `--add-repo` and `--remove-repo`.
- `dist/help.md` and `README.md`: two required keys, not three.
- No on-disk migration. Queues written by the current binary keep their
  directories, their cursors, and their records.
