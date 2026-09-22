# ADR 0005 — Versioned Session Extensions

Status: accepted

## Context

Schema 1 deliberately keeps each flow small and semantic: request, response/error,
body references, trailers, timing envelope, route metadata, annotations, and
redaction markers. Later roadmap work needs data that is session-level or
protocol-specific: authored scenario rules, stream-event timing, SSE views,
WebSocket messages, and import/export provenance.

Adding all of that directly to every `Flow` would destabilize the v0.1 record
shape and make older readers silently ignore behavior-changing data.

## Decision

Separate session schema from flow schema before M009.

- Existing schema-1 fixtures remain readable.
- Flow records remain flow schema 1 unless a later migration explicitly changes
  the flow contract.
- A new session schema 2 introduces a bounded manifest extension registry.
- A session extension has a stable name, extension schema version, relative
  fixture path, and a `required_for_replay` flag.
- Behavior-changing extensions are marked required. A reader that does not
  understand a required extension rejects the fixture rather than replaying it
  with degraded semantics.
- Extension files live inside the fixture root and are validated with the same
  path-confinement, symlink, size/count, and crash-safe publication rules as
  flows/blobs.
- Extension payloads may reference content-addressed `blobs/<sha256>`; body
  bytes are never duplicated into JSON solely for extension convenience.
- Unknown optional extensions may be preserved/ignored only when doing so cannot
  change replay behavior.

Initial names:

- `rules` / `rules.json` — M009 scenarios and deterministic templates.
- `stream-events` / `stream-events.jsonl` — M010 timing/SSE/mid-body events.
- `websocket-messages` / `websockets.jsonl` — M011 message semantics.
- `interop-provenance` — M014 lossy import/export metadata when needed.

## Migration

Introduce distinct constants/types for session-schema and flow-schema versions;
do not continue overloading one `SCHEMA_VERSION` for both meanings.

New readers accept session schema 1 and 2. Schema-1 sessions have no extensions.
Writers use schema 1 for ordinary fixtures until an extension is required, or
schema 2 if implementation simplicity strongly favors always writing the new
manifest; either choice must preserve schema-1 read compatibility and be
covered by golden tests.

Old EggReplay binaries encountering a session-schema-2 manifest must reject it
as unsupported rather than silently ignoring extensions.

## Consequences

M009 owns the migration seam and extension registry implementation. M010/M011
must reuse it rather than inventing sidecar discovery conventions. M014 migration
tooling may later materialize explicit on-disk upgrades; M009 only requires
safe read compatibility and new-write behavior.

This ADR does not authorize arbitrary plugin files, unbounded extension payloads,
or executable fixture content.
