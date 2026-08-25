## Context

See `proposal.md` — Why. Measurements taken from the live queue on one machine,
128,768 records across five customers, shaped every decision below:

| route | avg record | first 200 | first 400 |
| --- | --- | --- | --- |
| the-fish-factory | 2.8 KB | 0.44 MiB | 0.97 MiB |
| chief-nutrition | 2.2 KB | 0.62 MiB | 1.01 MiB |
| pollinate-workspace | 3.1 KB | 0.55 MiB | 0.83 MiB |
| demo | 2.6 KB | 0.70 MiB | 1.17 MiB |
| simon-george-sons | 8.4 KB | 1.64 MiB | 2.82 MiB |

Those averages hide the spread that matters: within one queue a captured line
runs from about 2 KB to over 150 KB, and the largest single record found was
2.6 MB. A fixed record count therefore produces a body of unpredictable size,
and two live passes against the real endpoints showed what that costs. At 900
KiB per request with four customers uploading together, a request measured
about 15 seconds on the wire — inside the old 20 second timeout only by luck,
and two routes lost the rest of their pass to `No answer in time`. Raising the
timeout to 60 seconds removed every timeout from the next pass. No HTTP 413 was
observed in roughly 7,000 delivered records, so the size handling below is a
safety net for a limit the client has not yet met, not a workaround for one it
has.

Constraints the design has to hold: no new crates for a binary that has a size
budget; `std` only; the queue is the durable record, so nothing may be dropped
on a crash; API keys are read at send time and must not reach a log.

## Goals / Non-Goals

**Goals:**
- Delivery that does not depend on knowing the endpoint's body limit.
- Cost per delivered batch that does not grow with the size of the backlog.
- A pass whose wall-clock is shared across customers rather than claimed by one.

**Non-Goals:**
- Raising per-route throughput. The round trip is the endpoint's; the gain here
  is that five routes run at once and none of them is stuck.
- Server-side changes. The client adapts to whatever limit it meets.
- Reordering records. Delivery stays strictly first-in, first-out per route.

## Decisions

**Size batches in bytes, and let the client discover the limit.** A fixed record
count cannot be right for both a 2.2 KB average and an 8.4 KB one, and it is
never right within a single queue whose lines vary by two orders of magnitude. A 900 KiB
budget clears the observed ~1 MiB refusal with room for the request envelope,
and a size refusal halves the budget and retries. Hard-coding a 1 MiB limit was
the alternative; it would break silently against any customer whose gateway
differs, and the halving costs at most a handful of requests to find the real
one. The record ceiling stays as a secondary bound at 2000 so a burst of very
small lines cannot build an unbounded request.

**Park an unsendable record instead of stopping.** Once the budget bottoms out,
requests carry one record, so a further refusal indicts that record and not the
batch. Moving it aside is the only option that keeps the queue moving; failing
the route leaves a customer permanently at zero, and dropping it loses captured
work. The file lands under the existing `quarantine/` directory, named
separately from the v1 legacy queue so the "legacy queue present" signal keeps
its meaning.

**A cursor file per route, not a queue rewrite.** Rewriting a 90 MB file after
every batch made the cost of one delivery proportional to the whole backlog, and
listing routes re-counted 335 MB of lines every pass. The cursor holds `offset`,
`pending`, and `scanned_to`: `offset` is where delivery resumes, and
`scanned_to` is what makes counting incremental — a listing counts only the
bytes appended past it. Compaction still happens, but only once the delivered
prefix passes 8 MiB, which turns a per-batch cost into a rare one. A separate
index file or a database was the alternative; both are more state to corrupt for
a queue whose file is already the source of truth. The cursor is advisory: if it
disagrees with the file — shorter than `scanned_to`, or pointing past the end —
it is discarded and the file is recounted.

**One batch per route per cycle, routes concurrent, under a scope.** Fairness
and concurrency together answer the starvation: a cycle serves everyone, and
running the cycle's routes at once means a slow endpoint delays only its own
customer. `std::thread::scope` borrows the route list and the spool without
`Arc`, which keeps the change inside `std`. A single worker pool over a shared
queue would drain more smoothly but needs shared mutable state and interleaves
one route's batches with another's; the barrier at the end of each cycle costs
the slowest route's round trip and buys a merge point where progress can be
printed in label order. The 120 second pass budget bounds how long capture waits
behind delivery.

**Progress lines start at the second cycle.** The end-of-pass line already
reports the first cycle, so printing from cycle one would double every line in
the common case where a pass drains in one go.

## Risks / Trade-offs

- Throughput is bounded by the uplink, not by the batch: two live passes moved
  about 29 records per second across four customers → Concurrency keeps every
  customer moving rather than multiplying a fixed pipe; the backlog drains in
  roughly an hour of running.
- A gateway that caps below the byte floor forces one record per request →
  Progress still happens, and the reason line now carries the status and the
  server's words, so the cap is visible rather than guessed at.
- A quarantined record is delivered by nobody → It is written intact and never
  deleted, the activity line says it happened, and it can be replayed by hand.
- A cycle is only as fast as its slowest route → Bounded by the request timeout,
  and it is what makes the merge point and ordered progress lines possible.
- An echoed secret in a server error body could reach the log → The excerpt goes
  through the same redaction as captured lines, and is clipped to one short line.
- Concurrent routes multiply outbound requests → One request in flight per
  customer, which is the same per-customer load as before.

## Migration Plan

No migration step and no state to convert. A queue with no `cursor.json` counts
its records once and starts delivering from the first one; the first listing
after the upgrade pays a single full count per route and later listings do not.
An older binary run against a queue that has a cursor ignores it and rewrites
the file as it always did, and the cursor self-heals on the next read because
the file is shorter than it recorded. A `batch` preset on disk below the current
one is raised at load, so an install that was hand-tuned during the outage comes
back to the preset without anyone editing a config file.

Rollback is the previous binary; nothing on disk has to be undone.
