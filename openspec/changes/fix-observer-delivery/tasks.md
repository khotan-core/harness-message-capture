## 1. Read the queue by bytes

- [x] 1.1 Read the front of a route by a byte budget and a record ceiling,
      always returning at least one record so an outsized line cannot stall
- [x] 1.2 Keep a malformed queue line a hard error rather than a skipped row
- [x] 1.3 Cover the budget, the always-one rule, and the malformed line in tests

## 2. Make a refusal legible

- [x] 2.1 Carry the operation, the HTTP status, and a clipped, redacted excerpt
      of the response body in every delivery reason
- [x] 2.2 Separate a size refusal from a credential refusal, and keep a size
      refusal on the identity check from shrinking any batch
- [x] 2.3 Cover a refusal reason, a size refusal, and excerpt clipping in tests

## 3. Replace the queue rewrite with a cursor

- [x] 3.1 Record `offset`, `pending`, and `scanned_to` per route, written
      atomically beside the records
- [x] 3.2 Advance the cursor on a delivered batch instead of rewriting the file,
      and release the file once a route empties
- [x] 3.3 Compact only past 8 MiB of delivered prefix
- [x] 3.4 Count pending from the cursor, scanning only newly appended bytes, and
      discard a cursor that disagrees with the file
- [x] 3.5 Cover the cursor advance, incremental counting, and compaction in tests

## 4. Share the pass across customers

- [x] 4.1 Drain one batch per route per cycle with routes running concurrently
      under a scope
- [x] 4.2 Stop starting batches once the pass budget is spent
- [x] 4.3 Halve a route's byte budget on a size refusal, and send one record at a
      time once the budget bottoms out
- [x] 4.4 Park a single record refused for its size, advance past it, and say so
      on the customer's line
- [x] 4.5 Cover fairness with a test that fails if routes drain in label order

## 5. Report while the pass runs

- [x] 5.1 Print per-route delivered and still-queued counts after each cycle past
      the first, through the same allow-list filter as any activity line

## 6. Raise the ceiling and document it

- [x] 6.1 Raise the `batch` preset and lift a stale lower value at load
- [x] 6.4 Give an upload 60 seconds to answer, leaving the identity check at 20
- [x] 6.2 Update the shipped log glossary for the new delivery reasons
- [x] 6.3 `cargo fmt`, `cargo clippy --all-targets`, `cargo test`, and
      `cargo build --release`

## 7. Verify against the real queue

- [x] 7.1 Confirm the cursor counts the live queue correctly and that a second
      listing is cheaper than the first
- [x] 7.2 Run a live pass against the customer endpoints and read back what the
      servers answer: no size refusal appeared, and the stall was the request
      timeout being passed by an oversized body
- [x] 7.4 Confirm no record is skipped or sent twice by comparing every cursor
      against the records physically left in its file
- [ ] 7.3 Ship the release and reinstall, per AGENTS.md
