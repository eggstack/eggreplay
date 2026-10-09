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
| Bidirectional, un-terminated | qualified (M017) |

An un-terminated bidirectional call is qualified, with an honest scope. The
recording gateway forwards a streaming request body, so the server's replies do
reach the client — but a client that never half-closes produces no terminal
`grpc-status`. The recorded flow is valid, its envelope is whole, and the
*missing* `grpc-status` is the signal that the call never completed.

What the fixture also records is **why** it stopped. The outbound deadline
surfaces as a response-body failure, which the recorder turns into a terminal
`Error` stream event (category `timeout`) and uses to suppress the clean `End`
event. So both facts are in the recording: the call did not finish, and it was
cut off by a deadline.

Replaying such a fixture is safe. A gRPC client never observes a successful
call. The live and replayed clients do see *different* failure codes, which is
worth knowing:

- **Live** — the gateway ends the downstream response cleanly after the outbound
  timeout, so the client gets 200, a partial body, and no trailers, which Tonic
  reports as `Unknown`.
- **Replay** — the recorded stream events say the outbound leg was cut off, and
  replay reproduces that termination downstream, so the body read fails and
  Tonic reports `Internal`.

Both are failures. Whether replay should instead reproduce the *downstream*
client experience differs from the recorded upstream truncation was an open
question; it is now decided. See "Un-terminated bidi replay semantics" below.

> M015D deferred this class on the belief that replaying a status-less response
> "would be worse than not replaying it". M017 investigated, found the stated
> blocker did not exist (the gateway is already full-duplex), and replaced the
> prediction with an observed client outcome. See
> `plans/closure/m017-unterminated-bidi-grpc.md`.

## Un-terminated bidi replay semantics

**Decision (M019): replay reproduces the recorded upstream truncation, and the
`Unknown` / `Internal` asymmetry is documented rather than smoothed over.**

The alternative — reproduce the downstream client experience, so a replayed
client sees `Unknown` exactly as a live one did — was considered and rejected
for this milestone. The reasoning:

- **A replay that lies about the recorded cause is a worse failure than a
  differing error code.** The fixture records *why* the call stopped: a terminal
  `Error` stream event with category `timeout`, which suppresses the clean
  `End`. Replaying the termination preserves that fact. Terminating cleanly
  downstream would produce a client experience matching the live run while
  erasing the only durable evidence of the deadline cut-off, so a later reader
  of the fixture could not tell a truncated call from a completed one.
- **Error-code parity is a convenience, not a contract.** `Unknown` and
  `Internal` are both non-success. No gRPC client branches on the specific
  value to decide whether the call worked; they branch on success. Making them
  agree would buy nothing a retry-aware client could use.
- **Reproducing the live symptom would require knowing the live symptom.** The
  live path's `Unknown` is an artifact of where the gateway happened to end the
  downstream response relative to the outbound deadline — a race, not a
  recorded property. Encoding it would pin a race into a deterministic replay.

This closes the question, not the asymmetry: the differing codes remain real and
remain recorded. The safety property is unchanged and still pinned — the call
never replays as success. Changing this is a replay-semantics change, not a
corrective, and needs its own milestone.

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
