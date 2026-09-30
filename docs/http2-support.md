# HTTP/2 support (experimental outbound)

M014B qualifies HTTP/2 record and regression-candidate execution through
EggFetch as **experimental**. HTTP/1.1 remains the default and only
generally-supported transport.

## What is supported

- Outbound H2 via EggFetch `native-http2` (ALPN `h2` over TLS), enabled
  with the `eggreplay-http/h2` cargo feature and an explicit
  `HttpVersionPolicy` (`Http2Only` or `Auto`) on the caller-constructed
  client. Default builds exclude the feature; default clients stay H1.
- Concurrent streams on one connection, request/response trailers (stored
  as flow trailers plus `StreamEventKind::Trailers`), large streaming
  bodies with backpressure, per-stream cancellation without corrupting
  siblings, target remapping, strict/practical matching, scenarios, M010
  stream-event timing, regression diffing, graceful GOAWAY handling, and
  routed H2-over-TLS through an Eggress TCP route (SNI/ALPN stay in
  EggFetch; the dialer moves raw TCP bytes only).
- H2 observations carry a diagnostic `("transport", "http-version:h2")`
  flow annotation. It is never a match dimension: replay selection and
  regression comparison are version-neutral.

## What is not supported

- Inbound H2 serving (EggServe replay/gateway): the qualified EggServe
  direct runtime is H1-only. H2-recorded fixtures replay over H1 (direct
  same-origin match) or drive H1 scenario replay.
- H2 interception (MITM): needs its own ALPN/caller-owned-connection
  evidence (out of scope).
- Cleartext prior-knowledge (`h2c`): not exposed. `Http2Only` against a
  cleartext endpoint fails closed instead of downgrading.
- No HTTP/1 connection-specific header (`Connection`, `Keep-Alive`,
  `Proxy-Connection`, `Transfer-Encoding`, `Upgrade`, non-`trailers` TE)
  leaks into H2 semantics. EggFetch strips them; `h2::check_h2_headers`
  rejects them at the EggReplay boundary so H2 callers fail fast.

## Dependency posture

No version moves: `eggfetch-core 0.2.0`, `eggserve-primitives 0.2.1`,
`eggserve-server 0.3.0`, `eggress-outbound 1.0.8`, `eggnet-tls 0.2.0`.
M014B enables only the already-published `native-http2` capability on the
qualified EggFetch line. EggServe 0.4.0 keeps H1 as the supported
transport with H2/H3 opt-in experimental, and H2 serving composition
lives outside the adopted direct-runtime closure, so inbound H2 stays
unsupported. See `plans/closure/m014b-http2-qualification.md`.
