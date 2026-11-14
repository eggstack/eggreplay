# HTTP/2 support (experimental, outbound and inbound)

HTTP/2 is supported as an **experimental, opt-in** capability for outbound
requests, inbound serving, and gRPC. HTTP/1.1 remains the default; no default
build enables HTTP/2.

## What is supported

### Outbound

- H2 via EggFetch `native-http2` (ALPN `h2` over TLS), enabled with the
  `eggreplay-http/h2` cargo feature and an explicit `HttpVersionPolicy`
  (`Http2Only` or `Auto`) on the caller-constructed client. Default builds
  exclude the feature; default clients stay H1.
- The CLI exposes this as `--outbound-version auto|http1|http2` on `serve`,
  `replay`, `record`, and `test` (note the `http1`/`http2` spellings).
  **`auto` maps to HTTP/1.1**, not to EggFetch's `Auto`, so an upstream release
  cannot silently change the protocol of an existing invocation.
- Concurrent streams on one connection, request/response trailers (stored as
  flow trailers plus `StreamEventKind::Trailers`), large streaming bodies with
  backpressure, per-stream cancellation without corrupting siblings, target
  remapping, strict/practical matching, scenarios, stream-event timing,
  regression diffing, graceful GOAWAY handling, and routed H2-over-TLS through
  an Eggress TCP route (SNI/ALPN stay in EggFetch; the dialer moves raw TCP
  bytes only).

### Inbound

- H2 serving is opt-in behind the `h2-inbound` and `h2-inbound-tls` cargo
  features. `eggserve-core` is an optional, `default-features = false`
  workspace dependency reachable only through them, so the default, direct, H1,
  interception, and Python profiles stay free of the multiprotocol closure.
- There is **one service and two runtimes**. EggServe's H1 and H2 drivers both
  project onto the same `eggserve_primitives::Request` and the same
  `eggserve_server::service::Service`, so the matcher, store, redaction,
  scenario engine, and renderer are shared. The protocol is a listener
  property, not a second code path. The only protocol-aware rendering rule is
  `content-length`, dropped on H2 only.
- `--inbound http1` (default; `h1` is an alias) or `--inbound http2` / `h2c`
  (cleartext prior knowledge). Cleartext HTTP/2 is an explicit, supported policy
  rather than an accident: a client selects HTTP/2 by speaking the 24-byte
  preface, so a request can never silently fall back.
- `--inbound-tls-cert` / `--inbound-tls-key` supply an **operator identity** and
  are what enable ALPN-negotiated HTTP/2. No CA is minted and no insecure mode
  exists. `--inbound h2-tls` is a recognised name that is deliberately
  **rejected**, so TLS is selected by supplying identity material rather than by
  naming the protocol. `--h2-max-concurrent-streams` bounds concurrency.
- An H1-recorded fixture replays over H2 and an H2-recorded fixture replays
  over H1. Both directions are qualified.
- The machine-readable status payloads of `serve` and `record` report the
  serving protocol.

### gRPC over H2

- Unary, server streaming, client streaming, and **both** terminated and
  un-terminated bidirectional calls are recorded, replayed, and regressed.
- The gRPC view is a caller-side **derived projection** over the recorded raw
  body and trailers, which stay authoritative. There is no gRPC branch in the
  matcher, the store, or the renderer, and no product path calls the view
  automatically.
- Descriptor sets are **caller supplied**, bounded, and never fetched or
  resolved. A malformed or oversized one fails as a derived-view error without
  touching the fixture.
- The compressed flag is reported; nothing is implicitly decompressed.

### Un-terminated bidirectional gRPC

For an un-terminated bidirectional call, a client that never half-closes has no
terminal `grpc-status`, even if the server has sent replies. The fixture records
why capture ended in a terminal `Error` stream event. Replaying it does not
produce a successful call: the live
client reports `Unknown` and the replayed client reports `Internal`. EggReplay
preserves this difference. See [`grpc-and-faults.md`](grpc-and-faults.md).

## What is not supported

- **H2 interception (MITM).** `eggreplay-intercept` never adopts the
  multiprotocol serving layer; it stays on `eggserve-server` and H1.
- **HTTP/3 / QUIC** on any path. There is no Eggress QUIC route connector or H3
  serving seam, and EggFetch's `http3` safety is unreviewed.
- **WSS** and **extended-CONNECT WebSockets**. The replay handshake path still
  requires HTTP/1.1.
- **A generic reverse proxy.**

## Invariants worth knowing

- **The recorded scheme participates in matching.** An `https`-acquired flow
  served cleartext is refused, not relaxed. This is a bounded refusal, not a
  silent mismatch. A gateway flow records the **upstream** authority it was
  configured to reach, not the inbound `:authority`, so a replay client must be
  at the recorded origin.
- **Protocol annotations are observational.** The
  `("transport", "http-version:h2")` annotation is preserved in the fixture and
  read by nothing. Replay selection and regression comparison are
  version-neutral; this is what makes cross-protocol replay safe.
- **A candidate records the baseline request.** Remapping a candidate's
  destination is a transport concern: the observed flow keeps the recorded
  request verbatim, and the remapped path shows up in provenance and the
  physical route. A remap therefore cannot silently rewrite what a later report
  compares against.
- **No HTTP/1 connection-specific header** (`Connection`, `Keep-Alive`,
  `Proxy-Connection`, `Transfer-Encoding`, `Upgrade`, non-`trailers` TE) leaks
  into H2 semantics. EggFetch strips them; `h2::check_h2_headers` rejects them
  at the EggReplay boundary so H2 callers fail fast.
- **A configured Eggress route never falls back to direct.** A route that
  cannot be reached fails the call.

## Operational limits

- A request with a declared `content-length` above the configured body limit is
  rejected with **413** before its body is read. An oversized body with no
  declared length is detected while streaming and may surface as **500**.
- An **incomplete** request still consumes a single-use candidate. A later
  request for the same recorded path finds no candidate, but the failure stays
  local to that stream and does not close the connection.
- A TLS peer that sends `:scheme: http` is rejected with **400** before matching.
