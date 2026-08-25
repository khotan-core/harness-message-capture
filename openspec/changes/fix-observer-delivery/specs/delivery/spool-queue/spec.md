## Purpose

Holds captured records on disk per customer until they are delivered, and
remembers how far delivery has got without rereading or rewriting the queue.

## ADDED Requirements

### Requirement: Delivery progress is recorded beside the queue

Each route's queue SHALL carry a delivery cursor recording the byte offset of
the first undelivered record, the number of records still pending, and how far
into the queue file the pending count has been taken. Recording a delivered
batch SHALL update that cursor and SHALL NOT require rewriting the queue file.

#### Scenario: A delivered batch leaves the queue

- **WHEN** a batch at the front of a queue is delivered
- **THEN** the cursor moves past those records, the pending count drops by the
  number delivered, and the queue file is left byte-for-byte as it was

#### Scenario: A queue with no cursor is adopted

- **WHEN** a queue written before cursors existed is read
- **THEN** every record in it counts as pending and delivery starts from its
  first record

### Requirement: Delivered bytes are reclaimed on a threshold, not on every batch

The observer SHALL rewrite a queue file to drop its delivered prefix only once
that prefix is large, and SHALL reclaim the whole file once a route has nothing
left pending.

#### Scenario: The delivered prefix grows past the threshold

- **WHEN** the delivered prefix of a queue passes the compaction threshold
- **THEN** the file is rewritten without it, the cursor returns to the start of
  the file, and the records still pending are unchanged

#### Scenario: A route empties

- **WHEN** the last pending record of a route is delivered
- **THEN** the queue file's space is released and the route reports nothing
  pending

### Requirement: Counting what is pending does not reread the whole queue

Listing routes and their pending counts SHALL read the recorded count and
examine only the bytes appended since that count was last taken.

#### Scenario: A pass follows an earlier pass

- **WHEN** routes are listed after records have been appended since the last
  listing
- **THEN** only the newly appended records are counted, and they are added to
  the count already recorded

#### Scenario: A partially written record is at the end of the file

- **WHEN** the last line of a queue file has no line ending yet
- **THEN** it is not counted until it is complete, and it is counted exactly
  once afterwards

### Requirement: A corrupt queue line stops that route rather than being skipped

Reading records for delivery SHALL fail loudly on a queue line that cannot be
parsed, so a corrupt line can never shift which records are treated as
delivered.

#### Scenario: A queue file holds a line that is not a record

- **WHEN** the front of a queue cannot be parsed
- **THEN** the read fails and the route reports that a queued file is unreadable
