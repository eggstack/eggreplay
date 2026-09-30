# gRPC-aware views and bounded fault models

M014D adds optional semantic helpers above already-qualified transports.
Neither changes the canonical flow store.

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
