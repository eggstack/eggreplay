# Interception threat model

M013 interception is an explicit, opt-in local proxy. The operator controls
the listener, target policy, CA identity, fixture location, and outbound route.
The proxy must fail closed when any of those authorities are missing or
invalid. EggServe owns inbound HTTP framing/lifecycle, EggFetch owns upstream
HTTP and TLS verification, and Eggress owns optional outbound route
establishment.

## Assets and trust boundaries

- The CA private key is a local signing authority and stays outside fixture
  data, logs, operational output, and temporary artifacts.
- Issued leaf private keys and TLS session secrets are memory-only.
- Proxy credentials, HTTP authorization/cookie values, and request/response
  bodies are sensitive. Existing redaction and persistence rules apply.
- The client-facing listener, policy file, CA directory, fixture store, and
  upstream route are separate trust boundaries. Client-supplied Host, SNI,
  Forwarded headers, and proxy headers are untrusted.

## Threats and required controls

| Threat | Control |
|---|---|
| Open-proxy exposure | Loopback bind by default; non-loopback requires explicit opt-in and ingress allow policy. No default allow-all target policy. |
| SSRF/internal-network access | Apply the same exact target policy before direct or routed dials; deny by default; constrain ports and bound names. |
| CONNECT/SNI/Host confused deputy | Normalize and compare CONNECT authority, SNI, and decrypted HTTP authority; one intercepted connection cannot change origin. |
| CA compromise or accidental overwrite | Dedicated versioned CA directory, restrictive permissions where supported, create-new publication, explicit import/rotation, public-only export. |
| Key/fixture/log leakage | No private key in metadata or fixtures; redacted diagnostics; scan outputs and staging paths with unique sentinels. |
| Certificate pinning | Pinned clients are expected to reject the generated leaf; operator may explicitly choose opaque tunnel where policy permits. |
| Unsupported mTLS | Client-certificate interception is unsupported and must fail explicitly. |
| Upstream TLS downgrade/bypass | EggFetch verifies upstream certificates and names; client trust in the local CA does not alter upstream roots. No insecure verifier is enabled. |
| Tunnel or leaf-generation denial of service | Bound connections, handshakes, concurrent tunnels, byte/time/idle budgets, input sizes, leaf cache, and policy size. |
| Shutdown/temp-file residue | Stop admission, cancel/drain active tasks, atomically publish CA state, and remove failed staging artifacts. |

## Residual risks and unsupported traffic

Installing the public CA grants the proxy the ability to impersonate names
allowed by interception policy. Trust installation is manual and external to
EggReplay. HTTP/2, HTTP/3/QUIC, WebSocket-over-WSS, client mTLS, transparent
interception, and successful interception by certificate-pinned clients are
outside the initial support claim. Opaque CONNECT tunneling does not create a
semantic HTTP flow.

Windows filesystem ACL guarantees must be stated only to the level proven by
the qualified APIs and hosted tests; Unix mode bits do not imply equivalent
Windows protection. Runtime shutdown cannot revoke certificates already
trusted by a client, so CA removal/revocation guidance is an operator task.

## M013-closure verification (M013F, local portion)

Each threat below was re-verified against the implementation and its tests.
Constant references name the defining location so drift fails the
`resource_bounds` integration test
(`crates/eggreplay-intercept/tests/resource_bounds.rs`).

| Threat | Implementation | Test |
|---|---|---|
| Open-proxy exposure | Loopback bind by default; `validate_bind`/`ProxyListenerConfig::loopback` reject non-loopback without `with_remote_opt_in` + `--allow-non-loopback`; no allow-all target policy (`TargetPolicy` denies unmatched, deny wins ties). | `proxy::loopback_gate_rejects_remote_without_opt_in`, CLI `non_loopback_bind_requires_explicit_opt_in`, `proxy_policy` deny-by-default cases |
| SSRF/internal target access | Same `TargetPolicy` gates direct and Eggress-routed dials; deny by default; bounded ports (`MAX_PORT_SET_SIZE = 64`, port zero rejected); configured routes never fall back to direct (`ProxyRoute::from_pproxy_uri` fails closed, `physical_route` is redaction-safe). | `proxy_policy` routed/no-fallback tests, `policy` specificity/deny-wins tests |
| CONNECT/SNI/Host confusion | `resolve_connect_target` (443 default, userinfo/port-zero rejected) → `resolve_connect`; `check_sni_coherence` (absent OK, present must normalize equal; DNS SNI on IP CONNECT fails); `check_http_authority_coherence` (Host must normalize to CONNECT host+port, default-port equivalence); cross-origin inside one CONNECT → `421` + connection poisoned, no flow. `Forwarded`/`X-Forwarded-*` never consulted. | `mitm` SNI/authority coherence tests, cross-origin `421`/poison tests, `proxy` absolute-target Host-coherence tests |
| Private-key leakage | `CaError`/`LeafError`/`MitmError`/`PolicyFileError` messages carry no key bytes, PEM, or paths (static labels only); `CaAuthority`/`LeafIssuer`/`LeafCertificate`/`MitmConfig` `Debug` redacted; `CaMetadata` public facts only; `ca export` copies the public cert only; leaf keys memory-only, never fixtures. No memory zeroization is promised (key types provide none). | `hardening.rs` sentinel audit (CA key, Proxy-Auth, HTTP Auth/Cookie, body target scanned across fixture tree, JSON, events, stats, errors, metadata, staging), per-module redaction unit tests |
| Temp-file leakage | CA publish stages in a sibling `TempDir` then atomically claims `dir` via `create_dir` (existing dirs never overwritten); staged files created with restrictive Unix modes; staging dir removed on success and failure; failed imports publish nothing. | `failed_import_publishes_nothing_and_echoes_no_key`, CA overwrite/rotation refusal tests |
| CA overwrite/rotation confusion | `create_new`/`import` refuse existing dirs (`AlreadyExists`); `export` refuses existing dest (`ExportDestExists`); rotation creates a distinct identity in a new dir, never mutates the old; handles are immutable snapshots; `open` revalidates fingerprint binding; selection is always explicit `--ca-dir`. | `ca_leaf` create/import/export/rotate refusal + fingerprint-binding tests |
| Route fallback | `ProxyRoute` is direct-or-`pproxy-compat`; malformed routes fail closed with credential-redacted diagnostics; direct and routed dials share one target policy; MITM upstream uses the same route dialer with secure-default TLS (`upstream_tls` custom roots only, no bypass). | `proxy_policy` routed/no-fallback tests, `mitm` dead-route (no flow) test |
| Upstream TLS bypass | Upstream recording owns verification via EggFetch with normal roots/hostname checks; client trust in the local CA affects only the client↔proxy leg; no `danger_accept_invalid_certs` path exists in interception code; leaf chain is `[leaf, ca]` for the client leg only. | `substrate::eggfetch_requires_explicit_ca_and_checks_hostname_and_sni`, MITM wrong-name/unrelated-CA failures, curl `--cacert` interop |
| Cert-cache exhaustion | `LeafIssuer` cache bounded at `MAX_LEAF_CACHE_ENTRIES = 128` with deterministic FIFO eviction; concurrent same-target issuance shares one entry (mutex held across local CPU signing only); expired entries replaced; validity capped by CA expiry (`CaExpiring`). | `leaf` cache-hit/FIFO-eviction/concurrency/expiry tests, pinned in `resource_bounds` |
| Connection/tunnel exhaustion | Listener: `InterceptionProfile` (`max_connections = 64`, `max_in_flight_requests = 64`, `max_active_tunnels = 64`); tunnels/intercepts share a semaphore (`TunnelLimits::max_concurrent`, default 16, hard ceiling 4096); excess CONNECT → `503` + `limit_reached`; diagnostics bounded (`TunnelEventLog::MAX_EVENTS = 256`, 9 fixed failure categories). | `proxy` stats/limit tests, `tunnel` bound tests, pinned in `resource_bounds` |
| Slowloris/no-progress | Tunnel relay bounded by bytes (256 MiB), total duration (300 s), idle/no-progress timeout (60 s); client TLS handshake bounded by `DEFAULT_TLS_HANDSHAKE_TIMEOUT` (10 s, tracks the connect timeout); handshake concurrency shares the tunnel semaphore; decrypted H1 driver runs under the listener profile timeouts (60 s total, 60 s keep-alive idle, 30 s write). | `tunnel` byte-limit/idle-timeout tests, MITM malformed-TLS/h2-only rejection tests, pinned in `resource_bounds` |
| Shutdown with active TLS/tunnel | `shutdown()` stops admission; `wait()` drains; tunnel relays race against request-lifecycle cancellation (cancel → `Shutdown` outcome + bounded event); decrypted MITM drivers get graceful shutdown signal plus a 5 s reclaim so shutdown never hangs; recording sessions shut down and finalize after drain. | `m013e_proxy_stats`/shutdown/finalization tests, MITM cancellation coverage |

Lock discipline (audited, not merely asserted): the leaf-cache mutex
guards fast local CPU signing only (no network or disk I/O under the
lock); `record_request_with_session` streams bodies through independent
sinks outside the flow-log lock and serializes only the final bounded
metadata append; the TLS handshake holds only the per-connection tunnel
permit plus local state. See `MitmInner` docs and `recording.rs`
(`record_request_with_session` docs).

A fail-closed interaction proved during M013F hardening: JSON-path body
redaction (`--redact-json-path`) requires a JSON media type on the
redacted message; non-JSON bodies with redaction requested fail the
recording with `502` rather than persisting unredacted bytes
(`finish_session_sink_redacted`). Operators combining interception with
body redaction must ensure clients declare JSON content types.

## Residual risks and unsupported traffic (M013F restatement)

- Installing the public CA grants the proxy the ability to impersonate
  names allowed by interception policy. Trust installation is manual and
  external; removal/revocation is an operator task (see
  `docs/interception-ca-trust.md`). Runtime shutdown cannot untrust a CA.
- Certificate-pinned applications reject minted leaves by design; use an
  explicit `tunnel` policy (opaque, unrecorded) or the ordinary gateway path.
- HTTP/2 MITM, HTTP/3/QUIC, WSS, client mTLS, non-HTTP TLS, and
  transparent/TUN interception remain unsupported and fail explicitly.
- Unix CA protection is `0700`/`0600` mode bits, enforced and repaired on
  Unix. On Windows, files are created with default sharing and secrecy
  depends on the operator profile-directory ACLs; no Unix-equivalent claim
  is made (see `ca.rs` Permissions docs; Windows hosted coverage pins
  creation/open/export behavior only).
- Leaf serials are unique within the issuing process (counter + seeded
  step); cross-process collision is negligible but not promised.
- `curl` interop is locally qualified (plain + MITM); hosted curl
  availability varies, so curl tests skip where `curl` is absent rather
  than failing. Scripted rustls and EggFetch clients run everywhere.
