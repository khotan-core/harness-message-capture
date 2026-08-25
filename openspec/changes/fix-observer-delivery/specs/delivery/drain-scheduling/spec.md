## Purpose

Shares one delivery pass fairly across every customer the machine uploads for,
and shows what each of them received while the pass is still running.

## ADDED Requirements

### Requirement: Every customer gets a turn in every cycle

A delivery pass SHALL send at most one batch per route per cycle and SHALL take
another cycle while any route still has records. No route SHALL have to empty
before another route is served, and the order routes are named in SHALL NOT
decide who is served first.

#### Scenario: One customer has far more queued than another

- **WHEN** a pass drains a customer with several batches queued alongside a
  customer with one
- **THEN** the second customer's batch is delivered before the first customer's
  last batch

#### Scenario: A route can deliver nothing

- **WHEN** a route is refused, or its queue is unreadable
- **THEN** that route leaves the pass with its reason recorded and the remaining
  routes keep taking turns

### Requirement: Routes are drained at the same time

Routes SHALL upload concurrently within a cycle, so one slow endpoint delays
only its own customer.

#### Scenario: Two customers are ready at once

- **WHEN** two routes both have records to send
- **THEN** their requests are in flight together rather than one after the other

### Requirement: A pass gives delivery a bounded share of time

A delivery pass SHALL stop starting new batches once it has run past its time
budget, leaving the remaining records queued, so capturing newly written
transcript lines is not postponed indefinitely by a large backlog.

#### Scenario: A backlog is larger than one pass

- **WHEN** the queues still hold records when the pass budget is spent
- **THEN** the pass ends, the undelivered records stay queued, and the next pass
  resumes them

### Requirement: Progress is reported while the pass runs

The observer SHALL report what each route has delivered as the pass proceeds,
not only when it ends, and SHALL apply the same allow-list filtering to those
lines as to any other activity line.

#### Scenario: A drain runs for several cycles

- **WHEN** a route delivers a batch in a cycle after the first
- **THEN** a line for that route reports what it just delivered and how many of
  its records are still queued

#### Scenario: A drain finishes in one cycle

- **WHEN** every route empties in the first cycle
- **THEN** the pass prints its usual end-of-pass line and no duplicate progress
  line
