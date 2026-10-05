# HTTP/2 support (experimental, outbound and inbound)

HTTP/2 is a qualified **experimental** tier in both directions. HTTP/1.1
remains the default and the only multiprotocol-free profile: no HTTP/2
capability is enabled in any default build.

- **Outbound** H2 record and regression-candidate execution is qualified under
  M014B and re-qualified on the Stage 11 dependency line under M015C.
- **Inbound** H2 serving — the recording gateway and the offline replay
  server — is qualified under M015B, with the end-to-end semantic matrix in
  M015C and the hardening matrix in M015E.
- **gRPC over H2** is qualified as an experimental tier under M015D.

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
  remapping, strict/practical matching, scenarios, M010 stream-event timing,
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

Qualified under M017, with an honest scope. The gateway is already
full-duplex, so server replies do reach the client; but a client that never
half-closes produces no terminal `grpc-status`. The recorded flow is valid and
its envelope is whole — the **missing status is the signal** that the call never
completed, and the fixture also records *why* it stopped, as a terminal `Error`
stream event with the classified category.

Replaying such a fixture is safe: a gRPC client never observes a successful
call. Live and replayed clients do report different failure codes (`Unknown`
live, `Internal` on replay), and that asymmetry is recorded rather than smoothed
over. See `docs/grpc-and-faults.md` and
`plans/closure/m017-unterminated-bidi-grpc.md`.

## What is not supported

- **H2 interception (MITM).** `eggreplay-intercept` never adopts the
  multiprotocol serving layer; it stays on `eggserve-server` and H1.
- **HTTP/3 / QUIC** on every path — deferred per ADR 0009. No Eggress QUIC
  route connector, no H3 serving seam, and EggFetch's `http3` safety unreviewed.
- **WSS** and **extended-CONNECT WebSockets**. The replay handshake path still
  requires HTTP/1.1.
- **A generic reverse proxy.**

## Invariants worth knowing

- **The recorded scheme participates in matching.** An `https`-acquired flow
  served cleartext is refused, not relaxed — a bounded refusal, not a silent
  mismatch. A gateway flow records the **upstream** authority it was configured
  to reach, not the inbound `:authority`, so a replay client must be addressed
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

## Known limitations found during hardening

These are recorded rather than hidden. They are properties of the adopted
EggServe runtime or of the canonical model, not of a test.

- `H2Limits::max_header_list_size` is enforced inbound but **not advertised**:
  the SETTINGS frame still carries EggServe's own 16384, so a client sizes its
  header block by a number the operator did not choose.
- An oversized request body surfaces as **500**, not 413. Bounded and safe, but
  not the most informative status.
- An **incomplete** request still consumes a single-use candidate, so a later
  request for the same recorded path finds nothing left. The failure is
  stream-local; it is not a connection failure.
- `eggfetch_core::Timeout::from_secs` sets `pool`, `connect`, `write`, and
  `read` but **not** `total`. The `read` budget is "time between response body
  chunks", so an upstream that accepts a request and then sends nothing is
  unbounded by `from_secs` alone. EggReplay therefore populates `total`
  explicitly: the CLI's `--timeout-secs` bounds the whole transaction and is
  **unset by default**, so no existing invocation's behaviour changed. A
  timed-out transaction is recorded as an `ErrorCategory::Timeout` flow outcome
  rather than a command-level error, so the command still exits `0`.
- Route failures are attributed. A typed `DialError` is classified into the
  taxonomy (`ConnectionRefused`, `Unreachable`, …) instead of collapsing to
  `Other`; this was the M016 fix, and it is why
  `ErrorCategory::ConnectionRefused` is reachable in the product at all.
- A TLS peer that claims `:scheme: http` is refused with **400** before the
  matcher runs. That is the right place to catch it, but it is worth knowing.
- The regression authority compares `date`, which is origin-generated and
  second-granular. A comparison about protocol *semantics* should normalize
  that one field, as the pre-existing outbound-H2 qualification does.

## Dependency posture

`eggfetch-core 0.2.2`, `eggserve-primitives 0.2.2`, `eggserve-server 0.4.0`,
`eggserve-core 0.4.0` (optional, inbound only), `eggnet-tls 0.2.0`,
`eggress-outbound 1.0.11`. H2 on the outbound side is the already-published
`native-http2` capability; H2 on the inbound side is EggServe Core's H2 driver,
adopted in Stage 11 as an opt-in dependency.

The gRPC oracle (Tonic 0.14.6) used to qualify gRPC is a **dev-dependency** of
`eggreplay-http` only and enters no product graph.

## Evidence

- `plans/adrs/0010-inbound-http2-serving-boundary.md` — the ownership decision
- `plans/closure/m015a-published-dependency-and-h2-boundary-preflight.md`
- `plans/closure/m015b-inbound-http2-gateway-and-replay.md`
- `plans/closure/m015c-http2-end-to-end-semantic-and-regression-qualification.md`
- `plans/closure/m015d-grpc-over-http2-integration-qualification.md`
- `plans/closure/m015e-h2-hardening-hosted-qualification-and-closure.md`
- `plans/closure/m015-bidirectional-http2-and-transport-baseline.md`
- `plans/closure/m014b-http2-qualification.md` — the original outbound tier
- `plans/closure/m014c-http3-feasibility-and-qualification.md` and
  `plans/adrs/0009-http3-integration-boundary.md` — the H3 deferral
