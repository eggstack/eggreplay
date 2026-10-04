# gRPC-aware views and bounded fault models

M014D adds optional semantic helpers above already-qualified transports.
Neither changes the canonical flow store.

Stage 11 (M015D) qualified the gRPC view against real traffic from an
independent gRPC implementation and made the limits below testable rather than
asserted. What changed in the product: **nothing**. The view is still a
caller-side projection, and no product code path calls it automatically.

Streaming classes, as qualified over HTTP/2:

| Class | Status |
|---|---|
| Unary | qualified |
| Server streaming | qualified |
| Client streaming | qualified |
| Bidirectional, terminated | qualified |
| Bidirectional, un-terminated | **deferred** |

An un-terminated bidirectional call is deferred for a specific reason. The
recording gateway forwards a streaming request body, so the server's replies
do reach the client — but a client that never half-closes produces no terminal
`grpc-status`. The recorded flow is valid and its envelope is whole; the
*missing* `grpc-status` is the signal that the call never completed. A gRPC
client cannot call that a completed call, and replaying it as if it were
complete would be worse than not replaying it.

One more property worth stating plainly, because it surprises people: the gate
recognises a gRPC **response framing**, not a gRPC request. A server may answer
a non-gRPC request with a gRPC content-type — Tonic's `Unimplemented` reply
does exactly that — and `is_grpc_content_type` correctly returns true about
that response. Recognition alone never invents envelopes: an empty body yields
a view with zero messages.

For the full qualification, including the strict/lenient split between
`decode_grpc_payload` (typed errors) and `grpc_view` (degrades to
`decoded: None`), see
`plans/closure/m015d-grpc-over-http2-integration-qualification.md` and
`docs/http2-support.md`.

## gRPC views (`eggreplay_http::grpc`, behind the `grpc` cargo feature)

For qualified HTTP/1.1 or HTTP/2 flows with `application/grpc*` content
types, the view layer parses the 5-byte length-prefixed envelope and
exposes ordered frames (`index`, `compressed`, `length`, raw `payload`),
plus `grpc-status`/`grpc-message` trailers as diagnostics. Findings:

- Compressed frames stay opaque (flag exposed, never decompressed).
- Protobuf payloads decode to canonical JSON only against an explicit
  caller-supplied `FileDescriptorSet` (`decode_grpc_payload`);
  descriptor bytes are bounded (1 MiB), untrusted, and never fetched
  from the network. Unknown message names and undecodable payloads fail
  closed.
- Raw body blobs stay authoritative; views are optional projections
  (`grpc_view`) with stable JSON fields. Decoded output inherits flow
  redactions; there is no separate view selector language.
- Malformed envelopes (truncated header, length overrun, trailing
  bytes) are errors, never silently reinterpreted.

## Authored faults (`ScenarioFault`)

`ScenarioResponse.fault` (absent in pre-M014D fixtures) selects one
deterministic fault applied at replay serving through existing
EggServe lifecycle controls:

| Fault | Behavior |
|---|---|
| `response_head_delay` | Sleep (≤ 30 s) before the head; otherwise identical. |
| `body_chunk_delay` | Stream the body in bounded chunks with inter-chunk sleeps; completes normally. |
| `close_before_response` | Headers flush with the full declared length, then the stream errors and the connection aborts. No scenario bytes are delivered; never a synthetic status. |
| `close_after_bytes` | Offer the rendered prefix (declared length stays the full body), then stream-error + abort. Prefix bytes may or may not arrive before the abort; clean full delivery never happens. |
| `transport_error` | Project a recorded-style semantic error: 502 with the stable `recorded upstream error: {Category:?}` shape replay uses for recorded `FlowOutcome::Error`. |

Bounds are validated by `ScenarioRules::validate` (delays ≤ 30 s,
chunks `1..=16` MiB); categories reuse the `ErrorCategory`/`ErrorPhase`
vocabulary. Arbitrary packet corruption, TCP flag manipulation, and
kernel-level emulation are out of scope (EggChaos/EggBench territory).

Cancellation is safe: dropping a client mid-delay leaves the server
healthy, and the state lock never spans fault sleeps so concurrent
requests keep flowing.
