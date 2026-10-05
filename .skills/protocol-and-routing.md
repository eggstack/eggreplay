# Protocol and Routing Tiers

Use when touching HTTP/2, gRPC views, the WebSocket codec, or Eggress routing.

## The tier model

Every capability is one of three tiers, and the tier is a *claim*, not a
description:

- **default** — enabled in a plain `cargo build`; HTTP/1.1 only.
- **supported** — qualified and reachable behind a feature boundary.
- **experimental** — qualified against independent peers on loopback behind a
  feature boundary. Qualified is not the same as unverified, but it is not
  default either.

"Experimental" never means "shipped by default", and a capability row may only
be expanded with corresponding tests. When you change what is supported, update
the matrices in `README.md`, `docs/http2-support.md`, and
`architecture/07-protocol-and-routing-tiers.md` together — they are three views
of one claim and they have drifted before.

## One service, two runtimes

The central design fact: EggServe Core re-exports the *same* service and
request types as `eggserve-server`, so `--inbound http2` hands the existing
service implementation to a different runtime. The matcher, store, redaction,
scenario engine, and renderer are shared. **The protocol is a listener
property, not a second code path.**

This is why you must never fork a protocol-specific matcher, renderer, or
validation path. If inbound H2 seems to need its own copy of something, the
right answer is to parameterize the shared one. The only protocol-aware
rendering rule is `content-length`, which is dropped on H2 only.

## Feature boundaries

| Capability | Feature | Tier |
|---|---|---|
| Outbound H2 record/regression | `eggreplay-http/h2` | experimental |
| Inbound H2 gateway + replay (h2c) | `eggreplay-http/h2-inbound` | experimental |
| Inbound H2 over TLS (ALPN) | `eggreplay-http/h2-inbound-tls` | experimental |
| gRPC derived views | `eggreplay-http/grpc` | experimental |
| WebSocket codec | `eggreplay-http/websocket` | supported |
| Eggress routing | `eggreplay-http/eggress` | supported |
| H2 MITM | — | unsupported |
| H3 / QUIC | — | deferred (ADR 0009) |
| WSS / extended-CONNECT WS | — | unsupported |

`h2` (outbound) and `h2-inbound` are **independent**. Do not make one imply the
other. `h2-inbound` is the only feature that may admit `eggserve-core`, and it
is never default in any profile — not the library, not the CLI, not the Python
wheel, not the interception graph.

`inbound.rs` must keep its `#[cfg(feature = "h2-inbound")]` gate. If that gate
is dropped, a default build gains a second HTTP stack with no manifest change
at all, which is precisely what the `protocol-boundary` CI lane exists to catch.

## gRPC is a derived projection

There is no gRPC branch in the matcher, the store, or the renderer. A gRPC call
is an HTTP/2 request with a content-type and a body. The gRPC view is a
**caller-side projection** over the recorded raw body and trailers, which stay
authoritative, and no product code path invokes it automatically.

- Descriptor sets are **caller supplied**. Nothing fetches or resolves one;
  there is no reflection. Bytes are bounded (1 MiB), untrusted, and a malformed
  set fails as a derived-view error **without touching the fixture**.
- Compressed frames stay opaque: the flag is exposed, nothing is decompressed.
- Malformed envelopes (truncated header, length overrun, trailing bytes) are
  errors, never silently reinterpreted.
- The gate recognises a gRPC **response framing**, not a gRPC request — Tonic's
  own `Unimplemented` reply to a non-gRPC request is correctly recognised.
  Recognition alone never invents envelopes: an empty body yields zero messages.
- `tonic` is a **dev-dependency of `eggreplay-http` only**. It qualifies gRPC
  against an independent implementation and must never enter a product graph.

## Un-terminated bidirectional gRPC (M017)

Qualified, with an honest scope. The gateway is full-duplex: it forwards a
streaming request body, so server replies do reach the client. But a client
that never half-closes produces no terminal `grpc-status`. The recorded flow is
valid, its envelope is whole, and the **missing status is the signal** that the
call never completed.

The fixture also records *why* it stopped: the recorder turns a response-body
failure into a terminal `Error` stream event (category `timeout`) and
suppresses the clean `End` event.

Live and replayed clients see *different* failure codes, and this is recorded
rather than smoothed over: live ends the downstream response cleanly after the
outbound timeout so Tonic reports `Unknown`; replay reproduces the recorded
termination downstream so the body read fails and Tonic reports `Internal`.
Both are failures. The safety property is that a gRPC call never replays as
success.

## WebSocket boundary

ADR 0006 defines a WebSocket conversation as a **separate required extension**
keyed to the initiating HTTP Upgrade flow. EggReplay uses EggFetch's owned
post-101 stream and EggServe's generic tunnel IO; the codec may parse messages
over those already-owned streams but must never create its own HTTP/TCP/TLS
stack.

M011 does not claim WSS, inbound TLS interception, H2 extended CONNECT, H3,
negotiated extensions, or frame-layout fidelity. `append-new` does not acquire
new conversations. A recorded 101 must never replay as an ordinary static HTTP
response.

## Eggress routing

Direct is always the default (`--route direct`). The optional `EggressDialer`
delegates **listener-free TCP route establishment** using only
`eggress-outbound/pproxy-compat` as a construction grammar. EggFetch keeps
ownership of logical Host/SNI, TLS, framing, pooling, and body semantics — the
dialer moves raw TCP bytes only.

- **A configured route never falls back to direct.** A route that cannot be
  reached fails the call. This is a safety property, not a preference.
- Unsupported or failed chains fail closed with credential-redacted
  diagnostics. Map typed Eggress facts into the error taxonomy; never parse
  display strings. M016 fixed a defect where every route failure was categorised
  `Other` because `map_fetch_error` had no `CustomTransport` arm, which made
  `ErrorCategory::ConnectionRefused` unreachable in the product.
- `physical_route` is recorded in flows for redaction-safe provenance.
- No H3 claim. No SSH/QUIC/extended Eggress surfaces.

## HTTP/3 is deferred, not pending

ADR 0009 documents the missing seams: no Eggress QUIC route connector, no H3
serving seam, and EggFetch's `http3` safety unreviewed. The `protocol-boundary`
CI lane asserts QUIC/H3 is absent from every supported graph. Do not
reintroduce an H3 row as "in progress" without reopening that ADR.

## Architecture References

- [`architecture/07-protocol-and-routing-tiers.md`](../architecture/07-protocol-and-routing-tiers.md)
  — the tier model, H2/gRPC/WebSocket/Eggress modules, H3 deferral.
- [`architecture/05-http-replay-and-serving.md`](../architecture/05-http-replay-and-serving.md)
  — inbound protocol policy and the shared service composition.
- [`architecture/08-interception.md`](../architecture/08-interception.md) — why
  interception stays H1-only.
- [`docs/http2-support.md`](../docs/http2-support.md) — the operator-facing
  support matrix and known limitations.
