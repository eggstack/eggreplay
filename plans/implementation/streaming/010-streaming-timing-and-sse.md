# M010 — Streaming Timing, Mid-Body Events, and SSE

Status: ready
Depends on: M009
Roadmap stage: 6

## Objective

Add optional stream-event timing and SSE-aware views while retaining raw body
bytes as the canonical payload authority. Ordinary replay/test behavior remains
immediate and timing-insensitive unless explicitly enabled.

## Storage

Reuse ADR 0005's extension registry. Add a required-when-used
`stream-events` extension, preferably JSONL keyed by flow id and body
direction.

Each event is bounded and ordered and may represent:

- DATA boundary: body offset/length and relative monotonic delta;
- trailers;
- terminal clean EOF;
- terminal semantic error/mid-body abort;
- optional response-head/start markers needed for timing assertions.

Do not duplicate DATA bytes in the event file; body refs remain authoritative.

Relative timing uses capture-local monotonic durations. Wall-clock timestamps
must not drive replay scheduling.

## Capture

Extend the existing frame observers rather than adding another HTTP stack.

Record event metadata while DATA/trailer frames already pass through
EggFetch/EggServe adapters. Ensure event capture does not introduce unbounded
per-frame allocation; coalesce only under an explicit bounded policy.

Represent mid-body failure with the exact delivered-byte offset plus existing
typed error category/phase where possible.

## Replay timing modes

Add explicit modes:

- `immediate` (default);
- `recorded`;
- `scaled:<factor>` with validated finite bounds.

Recorded/scaled modes reproduce relative delays between semantic events, not
TCP packet timing. Enforce a maximum per-delay and maximum total replay delay.

Cancellation/shutdown must interrupt sleeps immediately.

## Concurrent timeline replay

Add an optional scheduler that starts flows according to recorded relative
start offsets with bounded maximum concurrency. Tie-break identical offsets by
fixture order.

Do not make timeline replay the default regression scheduler.

## SSE derived view

Parse `text/event-stream` bodies into a derived, bounded event view for
inspection/diff:

- data lines;
- event name;
- id;
- retry;
- comments where requested.

Raw HTTP body bytes remain authoritative. Invalid SSE syntax must be reportable
without corrupting or replacing the raw body.

Support SSE comparison policies such as exact ordered event semantics and
explicit ignored fields. Timing comparisons remain opt-in.

## Regression

Extend reports with stream-event findings only when enabled:

- early/late terminal error;
- event-count/order changes;
- SSE semantic differences;
- explicit cadence thresholds.

Do not compare exact timestamps. Keep stable JSON/JUnit projections.

## Limits

Add explicit caps for event records per flow, aggregate event records,
serialized event metadata, total scheduled delay, SSE line length, SSE event
count, and diagnostic output.

## Tests

Cover immediate compatibility, recorded/scaled timing, cancellation during
delay, mid-body abort replay, trailers after delayed data, concurrent timeline
ordering, event-count bounds, large bodies without payload duplication, valid
and malformed SSE, multiline SSE data, ignored SSE fields, and deterministic
report ordering across repeated runs.

## Closure

Create `plans/closure/m010-streaming-timing-and-sse.md`. M011 remains blocked
until this event/timing model closes because WebSocket message timing must
reuse it rather than inventing a second scheduler.
