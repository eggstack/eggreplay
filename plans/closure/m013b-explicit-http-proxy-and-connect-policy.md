# M013B closure — Explicit HTTP Proxy and CONNECT Policy

M013B is closed on implementation revision `d39f4b7` (working tree pre-closure;
implementation commits follow in the M013B closure commit) with local
verification green. Hosted qualification is deferred to M013F per the M013
closure plan; M013B required tests are local-only by plan.

## Implementation

Leaf crate `eggreplay-intercept` (remains a leaf; no reverse dependencies):

- `src/policy.rs` — transport-neutral `TargetPolicy`, `Rule`, `HostMatch`
  (`ExactDns` / explicit label-boundary `SuffixDns` / `ExactIp`), `PortMatch`
  (`Exact` / bounded `Range` / bounded `Set` / `Any`), `RequestKind`
  (`Plain` / `Connect` / `Any`), `RuleAction`, `ConnectAction::Deny|Tunnel`,
  normalization (DNS lowercase + trailing-dot trim, IPv6 bracket strip,
  userinfo/multiple-authority/empty/port-zero/non-ASCII rejection, 253-char
  host bound, 128-rule / 64-entry port-set bounds), deterministic
  specificity scoring with deny-wins-ties, fail-closed defaults
  (deny unmatched, no allow-all, configurable deny/tunnel CONNECT default).
- `src/headers.rs` — single RFC-oriented `filter_proxy_headers` helper:
  strips `Proxy-Connection`, `Proxy-Authorization` (never forwarded,
  persisted, or logged), `Proxy-Authenticate`, `Connection`-nominated
  headers, `Keep-Alive`, non-trailer `TE`, `Upgrade` (M013B has no upgrade
  support; rejected upstream of recording); preserves end-to-end
  `Transfer-Encoding`, `Trailer`, duplicate headers, and trailers semantics.
- `src/tunnel.rs` — opaque CONNECT relay: `TunnelLimits`
  (max bytes / total duration / idle-no-progress / concurrent), `relay_tunnel`
  via `tokio::io::copy_bidirectional` with direct backpressure, half-close
  propagation, lifecycle cancellation, bounded `TunnelEvent` operational
  metadata only (target/action/error, no secrets, no flows).
- `src/proxy.rs` — `ExplicitProxy` EggServe `Service` (absolute-form
  `http://` recording via existing `record_request_with_session` + EggFetch,
  CONNECT deny/tunnel via Eggress `OutboundConnector`), pure
  `resolve_absolute_target` / `resolve_connect_target` helpers, `ProxyRoute`
  (`direct` / `from_pproxy_uri`, fail-closed, redacted, no fallback),
  `ProxyListenerConfig` + `validate_bind` (loopback by default; non-loopback
  requires explicit `allow_remote` + ingress allow policy, with open-proxy
  warning), `start_explicit_proxy` building the EggServe `Server` on the
  `InterceptionProfile` (`OriginOrAbsolute`, eggserve-owned policy/admission,
  64 conn / 64 tunnels / 64 in-flight, 8 MiB body, 8 KiB target, 60/60/30s
  timeouts).
- `src/lib.rs` — module wiring + re-exports; `InterceptionProfile::with_bind`
  for explicitly validated binds.
- `Cargo.toml` — workspace-only deps (`bytes`, `futures-util`, `http`,
  `http-body`, `http-body-util`; dev `serde_json`); no new registries,
  no `eggserve-core`/PHF/native-TLS/second HTTP stack; `forbid(unsafe_code)`
  retained.

Ownership respected: EggServe owns inbound H1/tunnel handoff; Eggress owns
raw CONNECT route establishment (`pproxy-compat` only); EggFetch owns
upstream semantic HTTP/TLS verification; `eggnet-tls`/`rcgen`/`rustls`
unchanged from M013B0.

## Evidence

Local verification on the implementation tree:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
git diff --check
```

All green. Workspace suite: 188 tests passed (baseline 146 + 19 new
`eggreplay-intercept` lib unit tests + 23 new `proxy_policy` integration
tests; substrate 12 unchanged):

- absolute-form GET/POST/streaming/chunked + trailers recorded with
  duplicate headers/query preserved;
- Host mismatch and origin-form-on-proxy-listener rejected;
- `https://` absolute and `Upgrade` rejected without recording;
- hop-by-hop/proxy stripping + `Proxy-Authorization` sentinel never
  forwarded/persisted/logged;
- exact host/port allow+deny, suffix label-boundary
  (`evil-example.com` vs `example.com`), IPv4/IPv6 CONNECT authority;
- CONNECT deny, direct tunnel, routed tunnel, no-direct-fallback,
  dial-failure-before-200 (502, no 200);
- half-close/backpressure, byte/idle/duration/concurrency limits,
  shutdown-with-active-tunnel drain, non-loopback bind gate,
  tunnel-traffic-produces-no-flow.

Known limitation (carried to M013D, not a reopen): EggServe 0.3.0 strips the
service-set `Trailer` announcement in response normalization, so the proxied
H1 response terminal trailer block is not re-emitted on the wire in M013B;
trailers remain fully persisted in flows and request trailers reach upstream.
Service attaches trailers + announcement correctly for future transports.

## Handoff

M013B satisfies its plan acceptance (safe explicit HTTP/1.1 proxy +
policy-controlled CONNECT deny/tunnel, no TLS interception). M013C becomes
ready; M013D–M013F and M014 remain blocked by dependency order.
