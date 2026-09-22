# M014D — gRPC-Aware Views and Bounded Fault-Model Polish

Status: blocked
Depends on: M014B, M010
Parent: M014

## Objective

Add optional semantic helpers above already-qualified transports. Do not make
protobuf/gRPC parsing or fault injection part of the canonical flow store.

## gRPC view

For qualified HTTP/2 flows with gRPC content types:

- parse the 5-byte gRPC message envelope;
- expose ordered message lengths/compression flag;
- optionally decode protobuf only when an explicit caller-supplied descriptor
  set is provided;
- preserve raw body blobs as authority;
- expose grpc-status/grpc-message trailers in derived diagnostics.

Descriptor parsing is bounded and untrusted. No network descriptor lookup.

## Fault models

Extend authored scenario responses with a small deterministic set that can be
implemented through existing EggServe/EggFetch lifecycle controls:

- response-head delay;
- inter-event/body delay (M010);
- connection close before response;
- close after N body bytes;
- explicit recorded semantic transport error where reproducible.

Do not implement arbitrary packet corruption, TCP flag manipulation, or
kernel-level network emulation; EggChaos/EggBench are better authorities for
transport fault/performance work.

## Reports

Derived gRPC/fault diagnostics remain optional projections of canonical flow
and stream-event data. Stable JSON fields and redaction rules apply.

## Tests

Use local h2/gRPC fixtures, bounded descriptor sets, compressed/uncompressed
messages where supported, trailer status, malformed envelope, close-after-N,
delays with cancellation, and deterministic repeated reports.

## Closure

Create `plans/closure/m014d-grpc-and-fault-polish.md`. The M014 umbrella
closes only after M014A–D have explicit closure/support decisions.
