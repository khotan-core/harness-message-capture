## 1. Make the organization optional in a destination

- [x] 1.1 Parse `KHOTAN_ORG_ID` as optional and make `RouteRef` carry an
      optional organization
- [x] 1.2 Treat a file with an origin and a key as ready, and stop naming
      `KHOTAN_ORG_ID` in the blocked reason
- [x] 1.3 Cover a file with two keys, a file with three, and a file missing the
      API key in destination tests

## 2. Identify a queue by origin and credential

- [x] 2.1 Move the key fingerprint next to the route type so one definition
      serves discovery and upload
- [x] 2.2 Derive a new route's identity from the origin and the key
      fingerprint, never from the organization
- [x] 2.3 Record the fingerprint, and the organization once known, in queue
      metadata; never write the key itself
- [x] 2.4 Cover that two origins, and two keys on one origin, stay in separate
      queues

## 3. Find a queue by what it recorded, not by its name

- [x] 3.1 Match a route to an existing queue by recorded origin plus recorded
      fingerprint, falling back to recorded organization when a queue predates
      the fingerprint
- [x] 3.2 Create a queue named from the new identity only when nothing matches,
      and never rename an existing directory
- [x] 3.3 Adopt a legacy queue by writing the fingerprint into it the first time
      its route is matched
- [x] 3.4 Cover an upgrade against a queue directory named the old way,
      asserting its records still deliver and its cursor is untouched

## 4. Resolve and pin the organization

- [x] 4.1 Return the resolved organization from the identity check instead of
      only comparing it, and always send a concrete organization in the batch
- [x] 4.2 Pin the organization to the queue the first time it resolves
- [x] 4.3 Block the route when the endpoint reports an organization that
      disagrees with a declared or pinned one, keeping the records queued
- [x] 4.4 Report the identity check's own failure when a route has no
      organization and the endpoint will not say
- [x] 4.5 Cover resolve-then-pin, a declared mismatch, and a pinned mismatch
      against a fake endpoint

## 5. Re-point a queue whose destination file died

- [x] 5.1 When the pinned credential path no longer loads, deliver through a
      discovered destination with the same identity and record the new path
- [x] 5.2 Keep blocking, with the reason, when nothing matches
- [x] 5.3 Cover the repurposed-file case that stranded 11,461 records

## 6. Show and change the enrolled list

- [x] 6.1 List repositories blocked by their destination file, with reasons, in
      `status`, and say so plainly when there are none
- [x] 6.2 Report an allow-list entry that matches no repository with a
      destination
- [x] 6.3 Add `configure --add-repo` and `--remove-repo` that merge with the
      stored list, leaving `--allow-repo` replacing as it does now
- [x] 6.4 Cover add, remove, both at once, a duplicate add, an absent remove,
      and that the replace form still replaces

## 7. Documentation and release

- [x] 7.1 Update `README.md` and `dist/help.md`: two required keys, the third
      optional and what declaring it buys
- [x] 7.2 `cargo fmt`, `cargo clippy --all-targets`, `cargo test`, and
      `cargo build --release`
