# M015D — gRPC over HTTP/2 Integration Qualification

Status: **ready**
Depends on: M015C closure, M014D closure
Parent: M015

## Objective

Bind M014D's existing optional gRPC envelope/status projections to real
EggReplay HTTP/2 record/replay/regression traffic.

This is an interoperability/qualification track, not a second gRPC framework.
Raw HTTP bodies and trailers remain authoritative.

## Scope

Use real local gRPC traffic to qualify:

- gRPC content-type recognition over H2;
- ordered 5-byte message envelopes;
- multiple envelopes in one stream;
- compressed-flag reporting without implicit decompression;
- response trailers including `grpc-status` and `grpc-message`;
- caller-supplied descriptor-set decoding within existing bounds;
- recording -> offline replay byte/semantic preservation;
- candidate regression and derived diagnostics;
- cancellation/deadline behavior as observable HTTP/2 stream outcomes.

At least one maintained independent gRPC implementation (preferably a
dev/test-only Rust client/server such as Tonic) must participate in the
qualification. Product runtime dependencies must not gain Tonic merely for
tests.

## Streaming classes

Qualify unary and server-streaming RPCs. Evaluate client-streaming and
bidirectional streaming against the existing flow/stream-event model.

If those streaming classes require new canonical semantics, do not smuggle
them into this plan; record them as deferred with evidence. If the existing
model represents them correctly, add deterministic qualification.

## Descriptor/security rules

Keep the M014D rules:

- descriptor sets are caller supplied;
- no reflection/network descriptor lookup;
- bounded descriptor bytes and message/frame counts;
- malformed protobuf/envelopes fail as derived-view errors, not fixture
  corruption;
- raw body blobs/trailers are the source of truth;
- redaction applies before persistence and derived output must not resurrect
  redacted secrets.

## Non-goals

No generated service implementation, arbitrary method scripting, dynamic
protobuf mutation engine, gRPC-Web, HTTP/2 MITM, or reflection server.

## Acceptance

Create `plans/closure/m015d-grpc-over-http2-integration-qualification.md`
with the exact gRPC client/server oracle, RPC classes exercised, descriptor
evidence, trailer/status behavior, cancellation behavior, and explicit
supported/deferred matrix.

M015E becomes ready only after D closes.
