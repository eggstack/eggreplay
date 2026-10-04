# M015A — Published Dependency and H2 Boundary Preflight Closure

Status: closed

## Qualifying revision and scope

Adopted the Stage 11 published-dependency set and proved the inbound-H2
boundary from public seams, so that M015B can be implemented without
forking a server.

Changed on the implementation branch `stage11-m015-bidirectional-h2`:

| File | Change |
|---|---|
| `Cargo.toml` | `eggfetch-core` 0.2.0 → 0.2.2, `eggserve-primitives` =0.2.1 → =0.2.2, `eggserve-server` =0.3.0 → =0.4.0, `eggress-outbound` =1.0.8 → =1.0.11, `eggnet-tls` retained at =0.2.0, new optional `eggserve-core` =0.4.0 with `default-features = false` |
| `crates/eggreplay-http/Cargo.toml` | new non-default features `h2-inbound` and `h2-inbound-tls`; `h2` (outbound) explicitly documented as outbound-only; `eggnet-tls` and `eggserve-core` added as optional dependencies |
| `.github/workflows/ci.yml` | new `protocol-boundary` job (5 boundary steps) alongside the existing `dependency-boundary` lane |
| `plans/adrs/0010-inbound-http2-serving-boundary.md` | new ADR recording the ownership decision and the rejected alternatives |
| `Cargo.lock` | regenerated for the adopted set |

No product behaviour changed. No source file under `crates/*/src` was
touched: this milestone is a dependency and architecture milestone only.

## Dependency-adoption outcome

All six required adoptions resolved on the first attempt, at exactly the
versions the plan named:

- `eggfetch-core 0.2.2` — published; adds `Error::transport_failure_kind()`
  and `TransportFailureKind { Connect, Tls, Protocol, Cancelled }`.
- `eggserve-primitives 0.2.2` — published; adds head-time
  `TrailerDeclaration` (the 0.2.1 → 0.2.2 delta is confined to
  `request_head.rs` and the new `trailers.rs`).
- `eggserve-server 0.4.0` — published. The 0.3.0 → 0.4.0 delta is purely
  **additive**: `ResponseMetadataOwnership` plus its two builder methods.
  Every item EggReplay uses (`Service`, `ServiceError`, `ServiceFuture`,
  `service_fn`, `Server`, `ServerHandle`, `RuntimeConfig`,
  `Http1RequestTargetMode`, `H1PolicyOwnership`, `AdmissionOwnership`,
  `RequestBodyPolicy`, `TunnelCapability`, `TunnelKind`) is byte-identical
  in 0.3.0, 0.3.1, and 0.4.0. No adopted EggReplay-visible contract moved.
- `eggserve-core 0.4.0` — published; adopted as an **opt-in optional**
  dependency only. See the boundary decision below.
- `eggress-outbound 1.0.11` — published. Adds
  `clear_h2_pool_registries` and `connect_with_options_and_metadata`.
  `OutboundConnector::connect` still resolves to the same single
  `connect_with_options` generic-over-TCP route seam. The `quic` feature
  (`eggress-transport-quic`, `eggress-protocol-h3`) stays unadopted.
- `eggnet-tls 0.2.0` — retained unchanged; it is already the approved
  EggServe identity seam and provides
  `load_tls_config_with_http2(cert, key, http2: bool)` with an explicit
  ALPN advertisement, which is what M015B needs.

## H2 boundary decision: GO (no blocker)

M015A section 2 requires stopping M015B and recording a blocker if the
published API cannot host inbound H2 without private modules or a
duplicated protocol runtime. It can. Verified facts, all from published
artifacts:

1. **`eggserve-server 0.4.0` cannot serve H2 and says so in its own
   feature table.** It declares `http2 = []` and `tls = []` — both empty.
   The direct runtime is H1-only by construction, so there is no "add a
   flag to the current runtime" option. H2 serving genuinely requires
   `eggserve-core`.
2. **There is one service contract, not two.**
   `eggserve_core::server::Service` is a compatibility re-export of
   `eggserve_server::service::Service` (`eggserve-core-0.4.0/src/server/service.rs`
   is a 22-line file whose entire body is `pub use eggserve_server::service::*;`).
   `eggserve_core::server::Request` is `eggserve_primitives::Request` — the
   identical type the existing replay and gateway services already implement.
   `service_fn`, `ServiceFuture`, `ServiceError`, `RequestBodyPolicy`, and
   `TunnelCapability` all come from the same module.
3. **H1 semantics are preserved by construction.** EggServe Core projects
   its runtime onto the direct H1 connection with
   `http1_request_target_mode: OriginOnly`, `policy_ownership: default()`,
   and `admission_ownership: default()` — precisely the
   `origin_only()` + `eggserve_owned()` + `eggserve_owned()` triple
   EggReplay already configures for sealed replay. Selecting Core does not
   weaken the request-target or ownership boundary.
4. **The H2 policy surface is public and bounded.**
   `RuntimeConfigBuilder::http2(Http2Config)` is public, and `Http2Config`
   carries EggServe-owned limits (concurrent streams, header-list size,
   frame size, flow-control windows, reset thresholds, keep-alive PING)
   that EggServe hands to Hyper explicitly.
5. **h2c is a clean, explicit policy, not sniffing.** EggServe's cleartext
   classification requires the *complete* 24-byte H2 preface; a stream
   that diverges at any byte is H1. So cleartext prior knowledge is
   supported, bounded, and cannot be triggered accidentally or act as a
   silent downgrade. M015B therefore exposes h2c as its own explicit
   operator-selected policy rather than deferring it.
6. **Pseudo-headers never reach canonical headers.** Hyper maps
   `:authority` into the request URI; EggServe's shared
   `connection/request.rs` projection builds `head.headers()` from
   `req.headers()` only and passes authority separately to
   `RequestHead::new_with_authority`. The same projection serves H1 and
   H2, so H2 pseudo-header state cannot leak into stored canonical headers
   by construction.
7. **Connection-specific response headers are transport-owned.** Hyper's
   H2 server calls `strip_connection_headers` on every service response
   before framing, removing `connection` (and every header it names),
   `keep-alive`, `proxy-connection`, `transfer-encoding`, `upgrade`, and
   `te`. EggReplay does not need a second header-stripping authority.
   `content-length` is deliberately *not* stripped, which makes it
   EggReplay's responsibility — handled in M015B as a rule inside the one
   response renderer, not as a second renderer.

Conclusion: M015B proceeds. The adopted boundary is recorded in
`plans/adrs/0010-inbound-http2-serving-boundary.md` (option C: keep the
default H1 path on `eggserve-server`, add `eggserve-core` behind the
explicit `h2-inbound` feature, reuse one `Service` implementation). Options
A (fork a Hyper server inside EggReplay) and B (make `eggserve-core` a
default dependency) are rejected and documented.

## Dependency-boundary evidence

`/tmp/m015a-graphs/capture.sh` captured `cargo tree` for all eight graphs
M015A section 5 requires, with `--edges normal`. The `h2` library is
detected by name; `eggserve-core`, `eggserve-static`, and `eggserve-h3` are
detected too.

| Graph | `cargo tree` selection | `eggserve-core` | `eggserve-static` | `h2` | Expected |
|---|---|---|---|---|---|
| workspace default | `--workspace` | absent | absent | present* | no Core/Static |
| http direct/default | `-p eggreplay-http --no-default-features --features direct` | absent | absent | absent | clean |
| EggServe H1 | `-p eggreplay-http --no-default-features --features eggserve` | absent | absent | absent | clean |
| outbound H2 only | `-p eggreplay-http --no-default-features --features eggserve,h2` | absent | absent | present | no Core/Static |
| inbound H2 only | `-p eggreplay-http --no-default-features --features h2-inbound` | **present** | **present** | present | opt-in only |
| inbound + outbound H2 | `-p eggreplay-http --no-default-features --features h2-inbound,h2-inbound-tls,h2,grpc,eggress,websocket` | **present** | **present** | present | opt-in only |
| interception | `-p eggreplay-intercept --all-features` | absent | absent | present* | no Core/Static |
| all-features | `--workspace --all-features` | **present** | **present** | present | opt-in only |

\* `h2 v0.4.19` in the workspace-default and interception graphs arrives
through **Eggress**, not through this adoption:
`eggress-outbound` → `eggress-protocol-http` → `h2`. This is pre-existing:
`git show HEAD:Cargo.lock` (before adoption) already contains
`eggress-protocol-http v1.0.8` and `h2 v0.4.19` for the same reason. It is
Eggress's routing-hop dependency, not the multiprotocol serving closure, and
the baseline lockfile contains neither `eggserve-core` nor `eggserve-static`
— those two names appear only in the opt-in inbound-H2 graphs. The
boundary is deliberately expressed in terms of
`eggserve-core`/`eggserve-static`/`eggserve-h3`, so the pre-existing
Eggress-supplied `h2` cannot be mistaken for the closure it is not.

The `eggserve-static` closure inside the two inbound-H2 graphs is expected
and accepted: it is a **non-optional** dependency of `eggserve-core`, so any
graph admitting Core admits Static. The value of the check is that it stays
out of the other six graphs, in both directions.

## CI lane

`.github/workflows/ci.yml` gains a `protocol-boundary` job on
`ubuntu-latest` (tree queries need metadata only, so it needs no
interpreter and no build matrix). All five steps use `shell: bash` so the
`pipefail`/`&&`/`||` chains stay portable.

1. **Ordinary profiles exclude the multiprotocol closure.** A
   `check_no_core` helper asserts `eggserve-core`/`eggserve-static`/
   `eggserve-h3` are absent from nine selections: workspace default,
   `eggreplay-http` direct/default, `eggreplay-http` EggServe H1,
   `eggreplay-http` outbound-H2-only, `eggreplay-intercept --all-features`,
   `eggreplay-cli --all-features`, `eggreplay-python`,
   `eggreplay-core --no-default-features`, and
   `eggreplay-store --no-default-features`.
2. **The opt-in feature is explicit and does pull the closure.**
   `cargo check -p eggreplay-http --no-default-features --features
   h2-inbound --all-targets` and the same for `h2-inbound-tls` must both
   succeed, each tree must contain `eggserve-core`, and the
   no-default-features tree must **not** — proving the capability is
   unreachable without an explicit feature.
3. **QUIC/H3 stays absent from every M015-supported graph.** Six
   `eggreplay-http` feature selections must contain no `quinn`,
   `eggserve-h3`, `eggress-transport-quic`, or `eggress-protocol-h3`.
4. **Interception never adopts inbound multiprotocol serving.**
   `eggreplay-intercept`'s tree must not contain `eggserve-core` or
   `eggserve-static`, and its `Cargo.toml` must not mention `h2-inbound`.
5. **The Python wheel lane stays multiprotocol-free.**
   `eggreplay-python`'s tree must not contain `eggserve-core`,
   `eggserve-static`, or `eggserve-h3`, so M012's wheel scope does not
   silently widen.

All five steps were executed locally against the real `cargo tree` output
and passed; their combined output is reproduced above. The job is wired
into the required-checks set implicitly by existing job naming and is
executed on every push and pull request alongside `verify`,
`dependency-boundary`, `interception`, and the Python lanes.

## Regression status of the adopted versions

The repository-standard command is green on the qualifying revision:

```text
cargo fmt --all -- --check                                      # clean
cargo check  --workspace --all-targets --all-features --locked  # clean
cargo clippy  --workspace --all-targets --all-features --locked -- -D warnings
                                                               # clean
cargo test   --workspace --all-features --locked --no-fail-fast
```

**377 tests passed, 2 failed** across 26 suites. The two failures are
`eggreplay-intercept/tests/curl_interop.rs`:
`curl_plain_http_proxies_and_records` and
`curl_https_connect_mitm_records`.

Both are **pre-existing local-environment failures, not regressions from
this adoption.** Verified by re-running the same test binary against
stashed (`main`) sources: identical failures, identical assertions.

- `curl_https_connect_mitm_records` fails with
  `curl: (60) SSL certificate problem: self signed certificate` — this
  machine's curl does not trust the test's `rcgen`-minted self-signed CA
  through the path the test uses.
- `curl_plain_http_proxies_and_records` fails with
  `plain curl flow must record: left 0, right 1` — no flow is recorded
  through the local proxy path in this environment.

Every other suite is green, including the full HTTP/2 outbound suite
(`h2_qualification.rs`, 16 tests), the 68 `eggreplay-http` unit tests, and
all 64 `eggreplay-intercept` unit tests. The plan's regression bar
("re-run every existing suite") is met for every suite that passes in this
environment. Hosted CI is the authority for `curl_interop`, and M015E owns
recording that hosted result.

## Acceptance-criteria status

| Criterion | Status | Evidence |
|---|---|---|
| Adopt the six published crates at the named versions | met | lockfile resolves all six exactly; `cargo check --all-features` clean |
| Reconcile `eggress-outbound 1.0.8` or newer against its blockers | met | `clear_h2_pool_registries` and `connect_with_options_and_metadata` are present in 1.0.11; `connect` still resolves to the same single TCP route seam; `quic` unadopted |
| `eggress` feature stays pproxy-compat-only | met | unchanged: `eggress = ["dep:eggress-outbound", "eggress-outbound/pproxy-compat"]` |
| `eggfetch-core` outbound `h2` stays opt-in and explicit | met | `h2 = ["eggfetch-core/native-http2"]` unchanged, now documented as outbound-only and never implying inbound serving |
| `h2-inbound` non-default, activates `eggserve-core/http2` | met | CI step 2 proves both the presence and the absence |
| TLS serving separately explicit | met | `h2-inbound-tls = ["h2-inbound", "eggserve-core/tls", "dep:eggnet-tls"]` |
| Default/direct/H1 builds feature-equivalent except adopted deps | met | no `src` change; graphs 1-3 are Core-free |
| Interception does not use inbound H2 | met | CI step 4; no `h2-inbound` in its `Cargo.toml` |
| Document the decision in an ADR | met | `plans/adrs/0010-inbound-http2-serving-boundary.md` |
| Add dependency-boundary checks to CI | met | `protocol-boundary` job, 5 steps, all passing locally |
| Stop M015B and record a blocker if the boundary were unsatisfiable | not triggered | boundary is satisfiable; see "H2 boundary decision" |
| Re-run every existing suite | met | 377 pass; the 2 failures reproduce identically on `main` |

## Consequences carried into M015B

- M015B composes a new EggServe Core listener around the **existing**
  `ReplayTunnelService` and recording-gateway `Service` implementations. It
  must not add a second matcher, store, redaction, scenario, or
  response-rendering authority; the only permitted version-aware rendering
  rule is `content-length` (see ADR 0010 consequence list and M015B
  section 2).
- `h2-inbound-tls` may use `eggnet_tls::load_tls_config_with_http2` for
  the ALPN advertisement. It must require operator-supplied certificate
  and key material and must not mint, install, or reuse the interception
  CA.
- Cleartext prior knowledge (h2c) is available as an explicit policy
  because EggServe's preface test is complete-match and cannot be
  triggered accidentally.
- H3/QUIC remains unauthorized. `eggserve-core/http3`,
  `eggress-outbound/quic`, and `eggfetch-core/http3` stay disabled, and
  the `protocol-boundary` job enforces their absence.
