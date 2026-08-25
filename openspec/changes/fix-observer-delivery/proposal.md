## Why

Uploads had stalled: 19 records/second at `batch = 200` and nothing at all at
`batch >= 400`, with a queue of 128,768 records across five customers. Two
faults combined.

A batch was sized in records while what the endpoint and the uplink care about
is bytes, and a captured line varies from about 2 KB to over 150 KB. A fixed
count therefore built a body of unpredictable size — around 1 MiB in the calm
stretches of a queue, tens of MiB where large lines cluster. Once a body was big
enough that the upload could not finish inside the client's 20 second request
timeout, the route delivered nothing and reported `No answer in time`, pass
after pass. The reason line also discarded the HTTP status, so a refusal that
did come from the server read as `Key or request refused` and looked like a
credentials problem.

Meanwhile the drain emptied one customer at a time in label order, so a queue
that never emptied kept every later customer at zero.

## What Changes

- Size an upload batch in bytes (900 KiB budget) instead of records, and raise
  the record ceiling to 2000 so small lines still travel in large batches. A
  stale, lower `batch` preset on disk moves up to the current one at load.
- Give an upload 60 seconds to answer instead of 20, measured against a
  near-megabyte batch with four customers uploading at once. Keep the identity
  check at 20 seconds, since it carries no records.
- Report the HTTP status and a short, redactor-scrubbed snippet of the server's
  own words in every delivery reason: `ingest 413 · Body exceeded 1mb limit`.
- Classify a size refusal apart from a credential refusal. The uploader halves
  its byte budget and retries instead of abandoning the route.
- Park a single record that no batch size can carry in
  `state/quarantine/oversize-<route>.ndjson` and step over it, so one outsized
  line cannot hold a customer's queue.
- Keep delivery progress in a per-route `cursor.json` (`offset`, `pending`,
  `scanned_to`). Dropping a delivered batch writes the cursor rather than
  rewriting the queue file, which is compacted only past 8 MiB of delivered
  prefix. Route listing reads `pending` from the cursor and scans only bytes
  appended since it last looked.
- Drain every route once per cycle, all routes concurrently, under a 120 second
  per-pass budget, so label order no longer decides who gets served.
- Print per-route delivery progress during a drain instead of only when the pass
  ends.

## Capabilities

### New Capabilities
- `delivery/upload-batching`: how one upload batch is sized, and how the server's
  answer becomes a reason a person can act on.
- `delivery/spool-queue`: the durable per-route queue, its delivery cursor, and
  how records leave it.
- `delivery/drain-scheduling`: how the observer shares one pass across customers
  and reports what it delivered.

### Modified Capabilities
<!-- None. This is the first change recorded for this project. -->

## Impact

- `src/uploader.rs`: `Upload::TooLarge`, status- and body-bearing reasons.
- `src/spool.rs`: `cursor.json`, `peek_batch`, cursor-stepping `drop_front`,
  compaction, `pending_for`, `quarantine_front`.
- `src/main.rs`: round-robin concurrent `drain` under `std::thread::scope`,
  payload budget and pass budget, incremental progress lines.
- `src/config.rs`: `batch` preset raised to 2000; stale lower values move up.
- `dist/help.md`: the delivery-problem glossary, which ships inside the binary.
- On-disk state gains `spool/<route>/cursor.json` and may gain
  `quarantine/oversize-<route>.ndjson`. Existing queues need no migration: a
  missing cursor counts the file once and starts at offset 0.
