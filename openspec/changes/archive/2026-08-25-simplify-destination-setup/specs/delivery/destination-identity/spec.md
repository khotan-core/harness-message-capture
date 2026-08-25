## Purpose

Defines what a repository must declare to send its chats to a customer, how the
observer establishes which organization those chats belong to, and how a
customer's queue is told apart from every other customer's.

## ADDED Requirements

### Requirement: A destination needs an origin and a key

A destination file SHALL be complete when it carries `KHOTAN_API_URL` and
`KHOTAN_API_KEY`. `KHOTAN_ORG_ID` SHALL be optional. A file missing either
required value SHALL NOT produce a route, and the reason SHALL name the values
that are absent.

#### Scenario: A file carries only the origin and the key

- **WHEN** a repository holds a destination file with `KHOTAN_API_URL` and
  `KHOTAN_API_KEY` and no `KHOTAN_ORG_ID`
- **THEN** the repository is treated as ready to upload

#### Scenario: A file is missing the key

- **WHEN** a destination file has no `KHOTAN_API_KEY`
- **THEN** the repository is reported as blocked, naming `KHOTAN_API_KEY`

### Requirement: The organization is resolved from the key

When a route has no organization recorded, the observer SHALL take the
organization from the identity check it already performs against the endpoint
before uploading, and SHALL record it against that route's queue. Every upload
SHALL carry a concrete organization, because the endpoint requires one.

#### Scenario: The first upload for an undeclared organization

- **WHEN** a route with no declared or recorded organization uploads for the
  first time
- **THEN** the organization the endpoint reports for that key is used for the
  upload and recorded against the queue

#### Scenario: A later pass after the organization was recorded

- **WHEN** the same route uploads again
- **THEN** the recorded organization is used, and no extra identity request is
  made beyond the one the uploader already performs

#### Scenario: The endpoint will not say who the key belongs to

- **WHEN** the identity check fails for a route with no recorded organization
- **THEN** nothing is uploaded for that route and the failure is reported with
  its reason

### Requirement: A declared or recorded organization is enforced

The observer SHALL refuse to upload when the organization the endpoint reports
for a key differs from one declared in the destination file, and equally when it
differs from one already recorded against that queue. Records SHALL stay queued.

#### Scenario: The wrong key is placed in a repository

- **WHEN** a destination file declares one organization and its key resolves to
  another
- **THEN** the route is blocked, the mismatch is reported, and the queued
  records are kept

#### Scenario: A key is swapped after records were queued

- **WHEN** a route's key is replaced with one belonging to a different
  organization than the queue recorded
- **THEN** the route is blocked rather than delivering that queue to the new
  organization

### Requirement: A queue is identified by its origin and credential

A queue SHALL be identified by its endpoint origin together with a fingerprint
of the API key that reaches it, and SHALL NOT depend on the organization, so a
queue can exist before the organization is known. A fingerprint SHALL NOT be
reversible to the key, and the key itself SHALL NOT be written to queue
metadata.

#### Scenario: A repository is enrolled before its organization is known

- **WHEN** records are captured for a destination whose organization has not
  been resolved yet
- **THEN** they are queued for that destination without a network request

#### Scenario: Two customers share one machine

- **WHEN** two repositories point at different origins or carry different keys
- **THEN** their records are held in separate queues and are never sent in one
  request

### Requirement: Existing queues keep their records

A queue written before this change SHALL continue to be found, drained, and
appended to. The observer SHALL match a queue to a route by the identity
recorded in that queue's metadata rather than by the name of its directory, and
SHALL NOT rename or rewrite existing queue directories.

#### Scenario: An upgrade finds queues named the old way

- **WHEN** the observer starts against queues whose directories were named from
  the organization
- **THEN** every one of them is matched to its route, and its pending records
  are delivered unchanged

### Requirement: A route re-points at a working destination file

When the destination file a queue was pinned to no longer produces credentials,
and another repository on the machine offers a destination with the same
identity, the observer SHALL deliver that queue using the working file.

#### Scenario: The pinned file is repurposed

- **WHEN** the file a queue was created from loses its Khotan keys while a
  sibling checkout still carries the same origin and key
- **THEN** the queue delivers through the sibling's file instead of stalling

#### Scenario: No working file is left

- **WHEN** no repository offers a destination matching a queue's identity
- **THEN** the route is reported as blocked and its records stay queued
