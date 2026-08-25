## Purpose

Decides how much of a customer's queue travels in one upload request, and turns
the endpoint's answer into a reason a person reading the log can act on.

## ADDED Requirements

### Requirement: A batch is bounded by bytes, not only by records

The observer SHALL bound each upload request by the serialized size of the
records it carries, and SHALL apply the record ceiling only as a secondary
limit. A single record larger than the whole byte budget SHALL still be sent on
its own rather than held back.

#### Scenario: Wide records fill the budget before the record ceiling

- **WHEN** the front of a queue holds records whose combined size would pass the
  byte budget before the record ceiling is reached
- **THEN** the request carries only the records that fit inside the budget

#### Scenario: One record is wider than the whole budget

- **WHEN** the record at the front of the queue is larger than the byte budget
- **THEN** that record is sent alone rather than leaving the queue stalled

### Requirement: A refusal names its status and the server's own words

Every delivery failure reported to the log SHALL name the operation and the HTTP
status, and SHALL include a short excerpt of the response body when the server
sent one. The excerpt SHALL be trimmed to a single short line and SHALL pass
through client-side redaction before it can reach a log file.

#### Scenario: The endpoint refuses a request

- **WHEN** the ingest endpoint answers 403 with a body explaining the refusal
- **THEN** the reason on the activity line carries `ingest 403` and the first
  words of that body

#### Scenario: The server body is long

- **WHEN** the response body is longer than one short log line
- **THEN** the excerpt is clipped and marked as clipped

### Requirement: A size refusal is not a credential refusal

The observer SHALL treat an HTTP 413, or another non-retryable status whose body
says the request was too large, as a statement about the request size rather
than about the key. On such a refusal it SHALL retry the route with a smaller
byte budget instead of abandoning it. A size refusal on the identity check SHALL
NOT shrink any batch, because that request carries no records.

#### Scenario: The endpoint rejects an oversized body

- **WHEN** an upload is answered with 413
- **THEN** the route's byte budget is reduced and the records stay queued for
  another attempt, and the route is not reported as refused

#### Scenario: A server error is still retried as a server error

- **WHEN** an upload is answered with 500, 503, or 429
- **THEN** the route is reported as a retryable server error, not as a size
  problem

### Requirement: A record no batch size can carry is set aside

When a request carrying exactly one record is refused for its size, the observer
SHALL move that record to a quarantine file under its state directory and
advance past it, so the rest of that customer's queue keeps moving. The record
SHALL NOT be deleted, and the move SHALL be reported on the customer's activity
line.

#### Scenario: A single outsized record blocks the front of a queue

- **WHEN** one record alone is refused for its size
- **THEN** that record is written to quarantine, the queue advances to the next
  record, and the activity line says a record was too big to send
