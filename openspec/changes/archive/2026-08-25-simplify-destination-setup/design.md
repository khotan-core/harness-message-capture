## Context

See `proposal.md` — Why. The state this has to move from:

- `RouteRef.id` is `fnv1a(api_url + "\n" + org_id)`, and that string is the name
  of the queue directory under `spool/`. The org is therefore needed before a
  single record can be queued, and capture does no network.
- `route.json` inside each queue holds the whole `RouteRef`, including the
  absolute `credential_path` of the file that created it. Delivery reads the key
  from that path at send time.
- `verify_org` already fetches `GET /api/v1/me` per route, with a five minute
  cache, reads `organizationId`, compares it to the declared value, and throws
  it away.
- Live evidence from all seven destinations on this machine: `/api/v1/me`
  returns `{"organizationId", "role", "keyId"}` for every key, and the value
  matches the hand-written `KHOTAN_ORG_ID` in all five files that were filled in
  independently. The ingest endpoint rejects a body with no `organization_id`
  (`expected string, received undefined`), so the field stays in the request
  even though the local receiver ignores it.
- On disk right now: 122,129 records across five queues, 335 MB.

## Goals / Non-Goals

**Goals:**
- A repository enrols with an origin and a key.
- A queue can be named before anything about the organization is known.
- No record already queued moves, is renamed, or is re-sent.

**Non-Goals:**
- Changing how records enter or leave a queue. `delivery/spool-queue` and
  `delivery/upload-batching` are untouched.
- Dropping `organization_id` from the request. The endpoint requires it.
- Multi-org keys. One key resolves to exactly one organization.

## Decisions

**Identify a queue by origin plus key fingerprint.** The organization is not
knowable without the network; the key is sitting in the file. Fingerprinting it
gives an identity that capture can compute offline, which is the whole point.
The fingerprint is the FNV-1a already implemented as `key_fingerprint` in
`uploader.rs`, moved next to the route type so one definition serves both.

Alternatives: *origin plus credential path* survives key rotation, but a moved
or renamed checkout then strands its queue — precisely the failure this change
is meant to remove. *Keep the org and resolve it during discovery* puts a
network call in the capture path, which is the one place that must stay cheap
and offline.

Rotation is the known wrinkle and it is benign. A rotated key mints a new queue
identity, so new records land in a new directory. The old directory keeps
draining, because delivery reads the key from the file at send time and the file
now holds the new key for the same organization. The orphan empties itself and
stays empty.

**Adopt existing directories; never rename them.** The dangerous part of any
repartitioning is moving 335 MB of queued records, and it buys nothing. The
identity already lives inside each queue as `route.json`, so the observer can
match a route to a queue by reading metadata instead of computing a path. The
directory name stops being meaningful — it is just where the first write landed.
There are only a handful of queues, so scanning them costs nothing.

The matching rule: a queue belongs to a route when the recorded origin matches
and either the recorded key fingerprint matches, or the queue has no fingerprint
recorded and its organization matches the one the route knows. The second arm is
what adopts queues written before this change. A legacy queue whose route no
longer declares an organization cannot be matched offline; it is adopted the
first time that route resolves its organization, and until then it simply drains
under its own metadata. Nothing is lost in either case.

A one-time rename pass was the alternative. It can half-complete, and a
half-completed rename of a queue directory is the one failure that loses
records.

**Resolve the organization at send time and pin it.** `verify_org` becomes a
function that returns the organization rather than one that only compares it.
The first successful resolution writes it into the queue's metadata, and from
then on it is enforced like a declared value: if the endpoint later reports a
different organization for that queue, the route blocks instead of uploading.

This is a deliberate weakening, and worth naming: the guard moves from
*human-declared* to *first-observed*. It still catches a key swapped after
records were queued, which is the realistic accident on a machine that already
holds seven customers' keys. It no longer catches the wrong key pasted in on day
one, because there is nothing to disagree with. A declared `KHOTAN_ORG_ID` keeps
the stronger guarantee, so the field stays supported and stays documented for
anyone who wants it.

**Re-point a queue whose pinned file died.** Identity is origin plus key, so any
destination file with the same identity is by definition interchangeable with
the one a queue was created from. When the pinned `credential_path` no longer
loads and a discovered destination matches the queue's identity, delivery uses
that file and records the new path. This is what would have kept 11,461 records
moving when one checkout's file was repurposed while its sibling still held the
same key.

**Blocked repositories belong in `status`.** `destination::readiness` already
returns `Blocked(reason)` and the picker already renders it; `status` simply
never asks. It is the same call over the same discovered candidates.

**`--add-repo` / `--remove-repo` merge; `--allow-repo` still replaces.** Changing
what `--allow-repo` means would break the installer and any script using it.

## Risks / Trade-offs

- The cross-customer guard degrades to first-observed when nobody declares an
  org → Declared values are still enforced, the pin still fails closed on a
  later swap, and the README recommends declaring it where it matters.
- A key fingerprint is written into queue metadata → It is a 64-bit FNV of a
  64-character random key, not reversible in any practical sense, and the key
  itself is still never written. It is not logged.
- Key rotation leaves an empty legacy directory behind → It drains itself and
  holds nothing; a later cleanup can remove queues that are empty and unmatched.
- Two checkouts sharing one key still share one queue → Intentional: identical
  credentials mean an identical destination. The re-point rule removes the
  stranding that made this hurt.
- Matching by metadata means reading every queue's `route.json` on append → A
  handful of small files, already read by `routes()` each pass.

## Migration Plan

Nothing to run and nothing to convert. Existing queues keep their directories,
their cursors, and their records; they gain a key fingerprint in their metadata
the next time they are written. A destination file that declares
`KHOTAN_ORG_ID` behaves exactly as it does today. Rollback is the previous
binary: it computes the old directory name, finds the legacy queues where it
left them, and ignores metadata fields it does not know.
