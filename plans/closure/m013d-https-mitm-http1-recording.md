# M013D closure — HTTPS MITM HTTP/1.1 Recording

M013D is closed with local verification green. Hosted qualification is
deferred to M013F per the M013 closure plan; M013D required tests are
local-only by plan.

## Implementation

Leaf crate `eggreplay-intercept` (remains a leaf; no reverse dependencies):

- `src/policy.rs` — extended `ConnectAction` with `Intercept` (deny-wins,
  default-deny preserved; `resolve_connect` maps `Allow` to the configured
  default, so intercept fires only for explicitly allowed targets under an
  intercept default).
- `src/proxy.rs` — `ExplicitProxyConfig.mitm: Option<MitmConfig>` (default
  `None` preserves M013B behavior); `handle_connect` split into
  deny/intercept/tunnel arms; `MitmInner` built against the validated
  listener `RuntimeConfig`.
- `src/mitm.rs` — interception authority: `MitmConfig` (issuer, optional
  upstream trust anchor, redaction/body ceilings; redacted `Debug`),
  `MitmPolicy` (rejects non-intercept defaults via `PolicyNotArmed`),
  `check_sni_coherence` / `check_http_authority_coherence` /
  `check_authority_coherence` (strict one-origin rule, default-port
  equivalence, never consults `Forwarded`), `build_intercept_server_config`
  (chain `[leaf, CA]`, memory-only PKCS#8 key, ring provider, safe defaults,
  no client auth, ALPN `["http/1.1"]` only), `serve_intercepted_connect`
  (leaf issuance before 200; tunnel-concurrency permit held for the whole
  decrypted connection), `accept_decrypted_tls` + `serve_decrypted`
  (handshake timeout, ALPN None-or-`http/1.1`, truthful HTTPS `TlsInfo` /
  `ConnectionContext`, `serve_http1_connection_with_policy` over the TLS
  stream, lifecycle-cancel race + graceful drain; post-200 failures emit
  bounded `intercept` events, never flows), `MitmService` (per-tunnel
  origin-form-only; first cross-origin request yields 421 + connection
  poison; WSS/upgrade yields 400; upstream via `record_request_with_session`
  with `PhysicalRoute{kind: explicit_proxy_mitm}`).
- Upstream invariants: EggFetch normal TLS verification; client trust in the
  EggReplay CA has no effect upstream; no verification bypass exists
  (trust-anchor override only); logical origin/SNI preserved through
  physical proxy routes; route failure never falls back to direct.

Ownership respected: EggServe owns decrypted-stream H1 execution; Eggress
owns routed establishment; EggFetch owns upstream TLS; rcgen/`eggnet-tls`/
`x509-parser` ownership from M013C unchanged; no second HTTP/TLS stack.

## Evidence

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
git diff --check
```

All green. Workspace suite: 256 tests passed (baseline 223 + 10 new
`eggreplay-intercept` lib unit tests + 23 new `mitm` integration tests;
substrate 12, `proxy_policy` 23, `ca_leaf` 11 unchanged):

- HTTPS GET/POST, streaming request/response, trailers (flow-persisted),
  duplicate headers/query, concurrent intercepted clients;
- SNI match / SNI mismatch (fail closed), Host mismatch (fail closed),
  IP SAN interception (SNI-absent allowed);
- upstream untrusted-cert failure, upstream hostname failure (remain
  failures); Eggress-routed success; routed failure with no fallback;
- malformed TLS after CONNECT (tunnel/TLS failure, no flow);
- H2-only client fails at handshake (`no_application_protocol`, no flow);
- non-HTTP TLS fails explicitly; WebSocket Upgrade over MITM rejected;
- redaction sentinel absent durably; CA/leaf keys absent from
  fixture/report/log; cancellation/shutdown with active decrypted
  connection drains; session finalization valid after TLS/protocol failures.
- Two client styles: scripted tokio-rustls (SNI/ALPN control, incl. no-SNI
  IP case) + `eggfetch-core` through `CONNECT` with custom CA.

Notes carried forward (not reopens): Eggress direct dials reject
`localhost` (`ReservedTarget` DNS-rebinding guard; IP literals used for
direct-route tests, local relay hop for DNS); H2-only handshake alert is
stricter than the plan minimum; cross-origin yields 421 + poison (both
allowed outcomes); streamed bodies re-chunk on egress per canonical EggServe
behavior with trailers in the flow record (pre-existing 0.3.0 limitation);
`provenance.mode` stays `eggfetch-native`, MITM mode carried on
`physical_route.kind = "explicit_proxy_mitm"`.

## Handoff

M013D satisfies its plan acceptance (opt-in policy-gated HTTPS MITM H1
recording into the existing flow authority). M013E becomes ready; M013F and
M014 remain blocked by dependency order.
