# M013 — Explicit Proxy Acquisition and Optional HTTPS Interception Closure

Status: closed

M013 and its full subplan decomposition (M013A, M013B0, M013B, M013C, M013D,
M013E, and M013F) are closed on implementation revision
`5efc6f9c892bb5b1c2e84330a0c40a38f1de0de7`. The qualifying hosted Actions run
on the M013 closure commit is
[36456906216](https://github.com/eggstack/eggreplay/actions/runs/36456906216)
(revision `2bcf4933209dc56a60f2ca7b6982df2266832720`); the earlier
implementation-SHA run
[36211265347](https://github.com/eggstack/eggreplay/actions/runs/36211265347)
on `5efc6f9` is the same matrix with only docs additions in between. The
closure commit run passed every job: `verify` (Ubuntu stable, Ubuntu Rust 1.89,
macOS stable, Windows stable), `interception` (Ubuntu, macOS, Windows), the
`dependency-boundary` job, the four `python-bindings` matrix lanes, and the
`python-abi3-cross-version` job. M013F is the sole host-qualification task and
its local hardening plus hosted CI lane pass on the same revision closes the
umbrella.

## Subplan closures and decomposition

M013 is decomposed under ADR 0008 into a substrate preflight, the EggServe 0.3
adoption stage, the explicit-proxy/CONNECT plan, the CA/leaf lifecycle, the
opt-in MITM HTTP/1.1 recording plan, and the CLI/policy/operator UX plan; M013F
finalizes hardening and qualification:

- [M013A substrate, dependency, and threat preflight](m013a-interception-substrate-and-threat-preflight.md)
  — qualified on `147655a` (run 36011801512); substrate constants and the
  initial threat model live here.
- [M013B0 EggServe 0.3 adoption and absolute-form qualification](m013b0-eggserve-0-3-adoption-and-absolute-form-qualification.md)
  — qualified on `86cf2ff` (run 36094432787); pins `eggserve-server 0.3.0` and
  `eggserve-primitives 0.2.1` and qualifies the caller-owned absolute-form H1
  service seam.
- [M013B explicit HTTP proxy and CONNECT policy](m013b-explicit-http-proxy-and-connect-policy.md)
  — local 188-test suite; runtime profile, target policy, header filter, and
  tunnel relay.
- [M013C CA lifecycle and leaf issuance](m013c-ca-lifecycle-and-leaf-issuance.md)
  — local 223-test suite; ECDSA-P256 CA + bounded leaf cache.
- [M013D HTTPS MITM HTTP/1.1 recording](m013d-https-mitm-http1-recording.md)
  — local 256-test suite; CONNECT/SNI/Host authority coherence, in-memory
  decrypted H1 driver.
- [M013E CLI, policy, and operator experience](m013e-cli-policy-and-operator-experience.md)
  — local 288-test suite; `proxy` / `ca` namespaces, versioned policy file,
  bounded `ProxyStats`.
- **M013F hardening and qualification — this record plus runs 36211265347
  (implementation SHA `5efc6f9`) and 36456906216 (closure commit
  `2bcf493`).**

## Implementation surface

Leaf crate `eggreplay-intercept` (still a leaf — no ordinary product graph or
Python extension depends on it):

- `src/policy.rs` — transport-neutral `TargetPolicy`: most-specific-rule wins,
  deny wins ties, unmatched targets deny. `ConnectAction` resolves to deny /
  tunnel / intercept; intercept only fires for explicitly allowed targets
  under an intercept default.
- `src/policy_file.rs` — versioned declarative policy format
  `eggreplay-intercept-policy/v1`: exact / suffix / IP hosts, exact / range /
  set / any ports, plain / connect / any kinds, deny / tunnel / intercept
  actions. Bounded 64 KiB / 128 rules, mixed tunnel+intercept listeners fail
  closed, unknown fields / versions / actions rejected.
- `src/headers.rs` — proxy-only/hop-by-hop filter that strips `Proxy-Authorization`
  and similar framing from the upstream leg.
- `src/tunnel.rs` — opaque `CONNECT` relay with explicit byte (256 MiB),
  total-duration (300 s), idle/no-progress (60 s), and concurrency (16/4096)
  budgets; `TunnelEventLog` capped at 256 entries.
- `src/ca.rs` — dedicated CA lifecycle in caller-selected directories
  (`metadata.json + ca-cert.pem + ca-key.pem`, never `.eggr`): ECDSA-P256
  via `ring` through `rcgen`, `BasicConstraints CA:true` with pathlen 0,
  KU `digitalSignature/keyCertSign/cRLSign`, no SAN, 63-bit positive
  process-unique serials. `eggnet-tls::parse_identity_pem` pairing on
  import, self-signed-root + `CA:true` + `keyCertSign` enforced. Unix
  `0700`/`0600`/`0644` enforced on open with an explicit `repair_ca_permissions`;
  Windows behavior is documented as unprovable-without-new-OS-deps (default
  sharing, no enforcement, repair unsupported). Staged atomic publish in a
  sibling temp dir; `create_dir` claims `dir` so existing CA dirs are never
  overwritten. Public-only `export_ca_cert`, no private key export.
- `src/leaf.rs` — bounded in-memory issuer (cache ≤ 128, FIFO eviction,
  expiry-aware reuse). Exact-SAN-only leaves (`NoCa`, `digitalSignature`,
  `serverAuth` EKU, AKI, default 7-day / max 30-day / min 1-hour, always
  capped by CA expiry). One `tokio` mutex guards the cache and only the fast
  local signing step (no network or disk I/O under the lock).
- `src/mitm.rs` — opt-in HTTPS MITM for policy-approved `CONNECT` targets:
  `MitmConfig`, `MitmPolicy` (rejects non-intercept defaults), strict
  SNI / Host / CONNECT coherence (`check_sni_coherence`,
  `check_http_authority_coherence`, `check_authority_coherence`); one CONNECT
  is one origin, cross-origin yields 421 + connection poison. Leaf issuance
  precedes `200`; ALPN `["http/1.1"]` only; chain `[leaf, CA]`; truthful
  HTTPS `TlsInfo` + `ConnectionContext`; upstream via EggFetch (verified
  separately) with `physical_route.kind = "explicit_proxy_mitm"`.
- `src/proxy.rs` — `ExplicitProxy` / `ExplicitProxyHandle`; `InterceptionProfile`
  centralizes EggServe bounds (`max_connections = 64`,
  `max_active_tunnels = 64`, `max_in_flight_requests = 64`,
  `connection_total_timeout = 60 s`, `keep_alive_idle = 60 s`,
  `response_write_timeout = 30 s`, body ceiling 8 MiB, target ceiling 8 KiB);
  `ProxyListenerConfig::loopback` rejects non-loopback binds without
  `with_remote_opt_in` + `--allow-non-loopback`; nine fixed failure categories.
- `src/lib.rs` — module wiring, `forbid(unsafe_code)`, the `substrate`
  constants block pinning EggServe 0.3.0 / EggServe-primitives 0.2.1 /
  Eggress 1.0.8 / eggnet-tls 0.2.0 / rustls 0.23.45 / tokio-rustls 0.26.2 /
  rcgen 0.13.2 / x509-parser 0.16.0 / time 0.3.55.

`eggreplay-cli` adds the `intercept` Cargo feature (default off) with the
`proxy record` / `proxy validate` / `ca init|import|inspect|export|rotate`
namespaces. Feature-off builds emit a stable capability message and exit
code 2; the all-features CI lane exercises both modes.

## Hardening evidence (M013F)

The M013F hardening targets are pinned by tests in
`crates/eggreplay-intercept/tests/`:

- `hardening.rs` — unique sentinels for the CA key, `Proxy-Authorization`,
  HTTP `Authorization`/`Cookie`, and one JSON body redaction target. Scans
  the fixture tree, command stdout/stderr (via live proxy + live MITM
  exchanges), JSON envelopes (events, stats, errors), metadata, panic
  surfaces, and CA staging paths. Private key paths are also redacted from
  diagnostics.
- `resource_bounds.rs` — pins every M013F-listed bound:
  `MAX_POLICY_RULES = 128`, `MAX_HOST_PATTERN_LEN = 253`,
  `MAX_PORT_SET_SIZE = 64`, `MAX_POLICY_FILE_BYTES = 64 KiB`,
  `MAX_PEM_FILE_BYTES = 64 KiB`, `MAX_METADATA_BYTES = 64 KiB`,
  `MAX_CA_SUBJECT_CN_CHARS = 128`, `MAX_LEAF_CACHE_ENTRIES = 128`,
  `MAX_LEAF_VALIDITY_HOURS = 720`, `DEFAULT_TUNNEL_MAX_BYTES = 256 MiB`,
  `DEFAULT_TUNNEL_MAX_DURATION = 300 s`, `DEFAULT_TUNNEL_IDLE_TIMEOUT = 60 s`,
  `DEFAULT_TUNNEL_MAX_CONCURRENT = 16`, `DEFAULT_TLS_HANDSHAKE_TIMEOUT = 10 s`,
  `MAX_CONCURRENT_TLS_HANDSHAKES_UNDER_DEFAULT_LIMITS = 16`,
  `TunnelEventLog::MAX_EVENTS = 256`, `PROXY_FAILURE_CATEGORIES = 9`,
  `InterceptionProfile` defaults, the inherited `eggreplay-core` /
  `eggreplay-store` ceilings, and every substrate version pin.
- `curl_interop.rs` — independent client interop with the `curl` CLI
  (plain HTTP and HTTPS CONNECT+MITM). Each test skips cleanly when `curl` is
  absent, so minimal hosted images stay green while qualifying images prove
  the path. Schannel curl uses `--ssl-no-revoke` because M013 mints test
  leaves without CRL/OCSP; the flag is probed before use so OpenSSL/Rustls
  curl builds never see an unknown option.
- `tls_shutdown_isolation.rs` — raw `rustls` and minimal `Hyper` exchanges
  over plaintext and TLS; isolates graceful vs abrupt shutdown and verifies
  exact-byte transfer. Each platform failure names the faulty layer.
- `substrate.rs` — substrate proofs: published EggServe H1 driver over
  decrypted TLS, Eggress raw CONNECT with no direct fallback, cancelled
  opaque relay release, EggFetch normal TLS verification with custom roots,
  hostname verification, and SNI through the Eggress route adapter, plus
  the qualified `InterceptionProfile` (`Http1RequestTargetMode::OriginOrAbsolute`
  with EggServe-owned bounds).

Lock discipline (audited, not merely asserted): the leaf-cache mutex guards
fast local CPU signing only (no network or disk I/O under the lock);
`record_request_with_session` streams bodies through independent sinks
outside the flow-log lock and serializes only the final bounded metadata
append; the TLS handshake holds only the per-connection tunnel permit plus
local state. The full threat-model verification table is in
`docs/interception-threat-model.md`.

## Dependency and supply-chain audit

- Pinned substrate (`eggreplay-intercept/src/lib.rs` `substrate` constants,
  same values pinned in `Cargo.toml` and the lockfile):
  `eggfetch-core 0.2.0`, `eggserve-primitives =0.2.1`, `eggserve-server =0.3.0`,
  `eggress-outbound =1.0.8` (only `pproxy-compat`), `eggnet-tls =0.2.0`,
  `rustls =0.23.45`, `tokio-rustls =0.26.2`, `rcgen =0.13.2`,
  `x509-parser 0.16.0`, `time 0.3.55`.
- Eggress 1.0.10 remains upstream-pinned and is **not** adopted; the
  M013 closure does not depend on it. `Cargo.lock` pins `eggress-outbound`
  exactly at 1.0.8.
- `cargo audit` is clean against the locked graph.
- `eggreplay-intercept` is the only workspace member that pulls `rcgen` and
  the interception dependencies; the `dependency-boundary` CI job verifies
  `eggreplay-core`, `eggreplay-store`, `eggreplay-http`, `eggreplay-cli`
  (default features), and `eggreplay-python` contain neither
  `eggreplay-intercept` nor `rcgen` in their default trees.
- `eggreplay-cli` exposes an `intercept` feature whose only added
  dependency is `eggreplay-intercept`; default builds stay
  CA-dependency-free.
- No EggServe-core / EggServe-static dependency was added; no second
  Hyper client/server, SOCKS/CONNECT stack, TLS verifier, or hand-built
  X.509 implementation was added (generation/signing is `rcgen`'s,
  pairing is `eggnet-tls`'s, read-only certificate-property checks are
  `x509-parser`'s).

## Interoperability clients

- `curl` (plain HTTP and HTTPS CONNECT+MITM, schannel-aware revoke flag).
- Scripted `tokio-rustls` client with SNI / ALPN control (incl. the no-SNI
  IP case used by `mitm::tests`).
- `eggfetch-core` through `CONNECT` with a custom CA
  (`hardening::mitm_records_without_persisting_secrets`,
  `mitm::tests` upstream-success / upstream-hostname-failure coverage).
- `tokio-rustls` and minimal `Hyper` peer harnesses for transport-isolation
  evidence.

All interop tests are hermetic and loopback-only. No public Internet is
required.

## Platform / permission qualification

- **Ubuntu stable + Ubuntu Rust 1.89** — `verify (ubuntu-latest, stable)`,
  `verify (ubuntu-latest, 1.89.0)`, `interception (ubuntu-latest)` all
  green in run 36211265347. Unix CA directories verified at `0700` and
  CA keys at `0600` (`unix_permissions_are_enforced_and_repaired`).
- **macOS stable** — `verify (macos-latest, stable)` and
  `interception (macos-latest)` all green in run 36211265347.
- **Windows stable** — `verify (windows-latest, stable)` and
  `interception (windows-latest)` all green in run 36211265347. Windows
  behavior is documented at the level the qualified APIs actually prove
  (files are created, handles open, export works); no Unix-equivalent ACL
  claim is made. The mitigation is in `docs/interception-ca-trust.md`.

## Verification (local, qualifying revision `5efc6f9`)

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked --no-fail-fast
cargo audit
git diff --check
```

All green on `5efc6f9`:

- 22 suites passed, 316 tests passed, 0 failed (`--no-fail-fast`).
- `eggreplay-intercept` (164 tests: 64 lib unit + 11 `ca_leaf` + 2
  `curl_interop` + 7 `hardening` + 3 `m013e_proxy_stats` + 24 `mitm` + 23
  `proxy_policy` + 9 `resource_bounds` + 12 `substrate` + 9 `tls_shutdown_isolation`).
- `eggreplay-cli --features intercept` (32 tests).
- `cargo audit` clean.

## Full hosted evidence (qualifying runs)

All thirteen jobs in
[run 36456906216](https://github.com/eggstack/eggreplay/actions/runs/36456906216)
on the closure commit `2bcf493` passed:

| Job | Result |
|---|---|
| `verify (ubuntu-latest, stable)` | success |
| `verify (ubuntu-latest, 1.89.0)` | success |
| `verify (macos-latest, stable)` | success |
| `verify (windows-latest, stable)` | success |
| `interception (ubuntu-latest)` | success |
| `interception (macos-latest)` | success |
| `interception (windows-latest)` | success |
| `dependency-boundary` | success |
| `python-bindings (ubuntu-latest, 3.11, 1.89.0)` | success |
| `python-bindings (ubuntu-latest, 3.14, stable)` | success |
| `python-bindings (macos-latest, 3.11, stable)` | success |
| `python-bindings (windows-latest, 3.11, stable)` | success |
| `python-abi3-cross-version` | success |

The earlier run
[36211265347](https://github.com/eggstack/eggreplay/actions/runs/36211265347)
on `5efc6f9` is the same matrix with only docs additions in between, so it
also qualifies the same code surface.

The `interception` lane is the dedicated M013F qualification lane: minimum-feature
graphs (`eggreplay-cli --no-default-features` and `eggreplay-intercept`
alone), the Python wheel interception-free invariant, the full
`eggreplay-intercept` qualification (CA lifecycle, CONNECT policy, MITM
recording, hardening/secret audit, resource bounds, curl interop —
curl-gated tests skip where curl is absent), and
`eggreplay-cli --features intercept`.

## M013 support matrix

The M013 claim if hosted qualification passes (now proven by runs
36211265347 and 36456906216):

| Capability | M013 |
|---|---|
| Explicit HTTP/1.1 proxy absolute-form recording | supported |
| CONNECT deny | supported |
| CONNECT opaque passthrough | supported |
| HTTPS MITM HTTP/1.1 recording | supported, opt-in/policy-gated |
| Direct + narrow Eggress-routed upstream | supported |
| Manual CA initialize/import/export/rotate | supported |
| Automatic OS/browser trust installation | unsupported |
| HTTP/2 MITM | unsupported/deferred M014B |
| HTTP/3/QUIC interception | unsupported/deferred |
| WSS WebSocket interception | unsupported |
| client mTLS interception | unsupported |
| certificate-pinned clients | expected to fail unless configured passthrough |
| transparent/TUN interception | unsupported |

No matrix row may expand without corresponding tests.

## Known limitations and residual risks

- Trust installation is manual and external; CA removal/revocation is an
  operator task (see `docs/interception-ca-trust.md`). Runtime shutdown
  cannot untrust a CA that a client already trusts.
- Certificate-pinned applications reject minted leaves by design; use an
  explicit `tunnel` policy (opaque, unrecorded) or the ordinary gateway
  path.
- HTTP/2 MITM, HTTP/3/QUIC, WSS, client mTLS, non-HTTP TLS, and transparent/TUN
  interception remain unsupported and fail explicitly.
- Unix CA protection is `0700`/`0600` mode bits, enforced and repaired on
  Unix. On Windows, files are created with default sharing and secrecy
  depends on the operator profile-directory ACLs; no Unix-equivalent
  claim is made.
- Leaf serials are unique within the issuing process; cross-process
  collision is negligible but not promised.
- `curl` interop is locally qualified (plain + MITM); hosted curl
  availability varies, so `curl_interop` skips where `curl` is absent
  rather than failing. Scripted `rustls` and EggFetchCore clients run
  everywhere.
- A fail-closed interaction proved during M013F hardening: JSON-path
  body redaction requires a JSON media type on the redacted message;
  non-JSON bodies with redaction requested fail the recording with `502`
  rather than persisting unredacted bytes
  (`finish_session_sink_redacted`). Operators combining interception with
  body redaction must ensure clients declare JSON content types.
- M013 mint leaves without CRL/OCSP plumbing (no plan entry). Schannel
  curl therefore disables only revocation checking via
  `--ssl-no-revoke`; the flag is schannel-only and is probed before use.

## Acceptance

M013 closes only if interception remains optional, the CA key remains
outside fixtures/diagnostics, proxy targeting is fail-closed, opaque
tunnels are not misrepresented as HTTP, upstream TLS remains verified, and
the advertised HTTP/1.1 support works cross-platform with bounded
lifecycle/resource behavior. All conditions are met by the M013A–M013E
closure trees and M013F's qualifying evidence above.

## Handoff

M013 and M013F are closed. M014 (compatibility program), M014A (HAR
interchange), and M014B (HTTP/2 qualification) become ready; M014C
(HTTP/3 feasibility) and M014D (gRPC + bounded faults) remain blocked on
M014B per their declared dependencies. The default Python wheel remains
interception-free; an opt-in interception distribution for Python is a
separate decision after M014's tracks close.