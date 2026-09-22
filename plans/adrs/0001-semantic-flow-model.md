# ADR 0001 — Semantic Flow Model, Not Wire Capture

Status: accepted

## Decision

EggReplay's canonical artifact is a semantic protocol flow: HTTP request, response-or-error, bodies, trailers, timing/protocol metadata, and annotations.

Application-visible body events may be recorded, but TCP segmentation, TLS records, kernel packet timing, and exact HTTP/1 chunk boundaries are not canonical.

## Consequences

Byte-identical bodies can be asserted; packet/chunk-perfect reproduction cannot be claimed. Future WebSocket messages attach to the initiating flow. Import loss must be explicit. Fault simulation begins at semantic/stream level rather than arbitrary malformed packets.
