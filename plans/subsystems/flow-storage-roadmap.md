# Flow and Storage Roadmap

Status: active roadmap
Owners: M002, then M010/M011/M014

## Canonical schema

Schema 1 defines Session, Flow, HttpRequest, FlowOutcome::{Response, Error}, HttpResponse, ordered multi-value headers/query values, trailers, body references, timestamps, protocol metadata, route metadata, provenance, annotations, and typed redaction markers.

Persist relative timing with integer durations/offsets where deterministic comparison matters.

## Bodies

The store streams bytes to a same-filesystem temporary blob while hashing/counting, then atomically publishes by SHA-256. Distinguish absent body from empty body. Optional application-visible body-event spans/timestamps are schema-capable but may be absent in v0.1.

## Sessions

Record capture mode, schema/tool version, start time, redacted source/target descriptions, normalization/redaction profile identifiers, and ordered flow records. Overlapping flow lifetimes are representable for later concurrency replay.

## Evolution

Readers reject unknown future schemas unless an explicit migration path exists. Writers emit one current schema. Never reinterpret an existing field incompatibly.

M010 extends event timing/SSE views; M011 attaches WebSocket messages; M014 handles import/export provenance and packed transport.

## Non-goals

No SQLite in v0.1, no global fixture daemon, no packet capture, and no mutation of authoritative body bytes merely to create a pretty diff.
