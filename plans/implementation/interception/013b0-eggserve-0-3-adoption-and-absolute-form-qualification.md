# M013B0 — EggServe 0.3 Adoption and Absolute-Form Qualification

Status: ready
Depends on: M013A closure + published EggServe Plan-286 artifacts
Parent milestone: M013
Unblocks: M013B
Architecture: ADR 0008

## Objective

Adopt the published EggServe direct-runtime line that contains the absolute-form
service seam required by M013B, requalify every existing EggReplay EggServe
integration against the source-incompatible 0.3 contract, and prove the
forward-proxy request-target metadata from EggReplay itself before proxy
behavior is implemented.

This is a dependency/adaptation milestone. It must not implement target policy,
HTTP forwarding, CONNECT relay, CA lifecycle, or MITM.

M013B remains blocked until this plan closes.

## Published upstream baseline

EggServe Plan 286 is the publication authority.

Adopt exactly:

- `eggserve-primitives = 0.2.1`;
- `eggserve-server = 0.3.0`.

Keep unless separately justified:

- `eggnet-tls = 0.2.0`;
- EggFetch 0.2.0;
- Eggress 1.0.8;
- rustls 0.23.45;
- tokio-rustls 0.26.2;
- rcgen 0.13.2.

Do not add `eggserve-core`, `eggserve-static`, or an EggServe Tower feature
to EggReplay.

The relevant upstream registry evidence is
`release/plan-286-embedding-contract-publication-closure.md`, which proves
the exact 0.3.0 server artifact with primitives 0.2.1 from crates.io-only
consumers.

## Why a dedicated migration is required

EggServe 0.3 is not merely the previous server plus absolute-form support.
Plans 280–286 introduced source-visible direct-runtime policy ownership and
admission ownership, a projected `H1ConnectionPolicy`, typed runtime rejection
presentation, and updated runtime-state semantics.

EggReplay currently pins:

```text
eggserve-primitives = 0.2.0
eggserve-server     = 0.2.1
```

and M013A's caller-owned TLS proof uses a direct
`RuntimeConfig { ..Default::default() }` construction plus
`RuntimeState::new`.

The ordinary replay/recording gateways already use the safer builder APIs, but
the entire EggServe-facing surface must be requalified before M013B starts.

## A. Dependency migration

Update the workspace exact pins:

```toml
eggserve-primitives = { version = "=0.2.1" }
eggserve-server = { version = "=0.3.0" }
```

Regenerate `Cargo.lock`.

Retain exact pre-1.0 bounds. Do not use `0.3` or a caret range for the server
in this migration.

Verify with `cargo tree` that:

- `eggreplay-http` and `eggreplay-intercept` resolve server 0.3.0 and
  primitives 0.2.1;
- no older parallel EggServe server/primitives copy remains;
- no `eggserve-core`, `eggserve-static`, or PHF-family dependency enters
  EggReplay through this change;
- the default Python wheel remains interception/CA-generation free.

## B. Preserve existing replay/recording defaults

Existing non-interception EggReplay servers must keep EggServe's hardened
default ownership.

For ordinary replay/recording/WebSocket gateways, explicitly qualify:

- `Http1RequestTargetMode::OriginOnly`;
- `H1PolicyOwnership::eggserve_owned()`;
- `AdmissionOwnership::eggserve_owned()`;
- existing `max_request_body_bytes` behavior;
- existing `max_active_tunnels` behavior;
- `disable_connection_total_timeout()` for WebSocket-enabled long-lived
  connections;
- existing `ServerHandle` shutdown/wait behavior.

Do not opt ordinary EggReplay flows into absolute-form acceptance.

Add a regression proving an absolute-form request to the ordinary
record/replay listener is still rejected before the EggReplay service is
invoked.

The migration must not change matching, flow storage, redaction, timing,
WebSocket, scenario, or regression semantics.

## C. Modernize the M013A caller-owned H1 proof

Update `crates/eggreplay-intercept/tests/substrate.rs` to follow EggServe
0.3's published direct-embedding pattern.

Preferred shape:

1. construct `RuntimeConfig` through `RuntimeConfig::builder()`;
2. call `runtime.h1_connection_policy()` once;
3. construct `RuntimeState::try_new(&runtime)` so invalid config is returned
   rather than panicking;
4. use
   `serve_http1_connection_with_policy(..., Arc<H1ConnectionPolicy>, ...)`
   for the caller-owned TLS stream;
5. preserve truthful `ConnectionContext` TLS metadata.

Avoid direct public struct literals for `RuntimeConfig` in EggReplay-owned
code unless a test specifically needs to prove literal compatibility.

The existing M013A closure remains immutable historical evidence for the old
baseline. M013B0's closure becomes the current EggServe dependency authority.

## D. Qualify the absolute-form service seam

Add a focused EggReplay interception test using the published 0.3 API.

Start an EggServe H1 listener with:

```rust
RuntimeConfig::builder()
    .http1_request_target_mode(Http1RequestTargetMode::OriginOrAbsolute)
    ...
```

and a minimal test service.

Send raw local HTTP/1 requests and prove:

### Absolute request

```text
GET http://example.test:8080/a?b=1 HTTP/1.1
Host: example.test:8080
Connection: close
```

reaches the service and exposes:

- `RequestTargetForm::Absolute`;
- scheme `http`;
- URI authority `example.test:8080`;
- path `/a`;
- query `b=1`;
- full semantic raw target;
- canonical request authority coherent with the URI/Host pair.

### Validation

Prove:

- Host/URI authority mismatch returns 400 before service invocation;
- full absolute-target limit returns 414 before service invocation;
- ordinary origin-form still behaves according to EggServe's
  `OriginOrAbsolute` contract;
- CONNECT authority-form remains the tunnel path and is not converted into an
  absolute-form ordinary request;
- duplicate non-Host headers remain ordered in the canonical request.

Do not implement EggReplay proxy target-policy decisions in this test.

## E. Streaming body/trailer proof

M013B needs absolute-form requests to compose with EggReplay's streaming
recording path, not only zero-body GETs.

Add a local absolute-form POST with:

- chunked body;
- terminal trailer;
- duplicate end-to-end headers.

Use `RequestBodyPolicy::Stream` and prove the EggServe canonical request
delivers:

- expected target metadata;
- bounded body bytes;
- terminal trailers;
- original duplicate-header order.

Do not persist a fixture or forward upstream yet. That belongs to M013B.

## F. Runtime ownership decision for M013B

Record the initial M013B ownership profile explicitly.

Use EggServe-owned defaults for:

- handler deadline;
- request-body deadline;
- keep-alive idle deadline;
- response write-progress deadline;
- global request-body ceiling;
- semantic request-target ceiling;
- service-call admission;
- tunnel admission.

Rationale:

- EggReplay target authorization is not a substitute for EggServe's parser or
  semantic length ceiling;
- the existing body service policy composes safely with the EggServe global
  ceiling;
- M013B already needs one concurrent-tunnel cap, and EggServe's
  `max_active_tunnels` can be that single authority;
- there is no need to introduce External ownership merely because 0.3 exposes
  it.

EggReplay M013B will own separate relay byte, total-duration, and
idle/no-progress limits because those govern the accepted outbound relay, not
EggServe's admission semaphore.

If implementation later requires per-rule/per-target tunnel admission distinct
from the global cap, stop and amend the plan before selecting
`AdmissionOwner::External`; do not stack two hidden semaphores.

## G. Interception configuration helper

Add a small internal constructor/helper in `eggreplay-intercept` for the
EggServe runtime profile M013B will consume.

The helper should centralize:

- loopback bind supplied by caller;
- `OriginOrAbsolute`;
- request-body ceiling;
- request-target ceiling;
- service/tunnel admission bounds;
- total-connection/idle/write bounds;
- default EggServe ownership.

Do not expose a broad public EggReplay proxy API yet.

Tests should assert this profile's ownership fields so a later EggServe
upgrade cannot silently alter interception semantics through new defaults.

## H. Dependency/version diagnostics

Update the M013 substrate version constants:

- EggServe primitives 0.2.1;
- EggServe server 0.3.0.

Keep the constants test-only/diagnostic in purpose; do not duplicate Cargo's
dependency resolver as runtime policy.

Update architecture/research/dependency comments that still describe
EggServe 0.2.1 as the current M013 baseline.

Do not rewrite the historical M013A closure record.

## I. Cross-platform regression matrix

The version bump touches the server used by ordinary replay/recording and the
interception leaf, so qualification must cover more than focused M013 tests.

Required local/focused cases:

- ordinary recording gateway;
- ordinary replay server;
- append-new replay path;
- WebSocket record/replay lifecycle;
- M013A TLS caller-owned stream;
- Eggress route/no-fallback substrate;
- new absolute-form metadata;
- Host mismatch;
- target 414;
- absolute-form streaming/trailers;
- shutdown with active tunnel/WebSocket where existing tests cover it.

Then run the full workspace suite.

## J. Hosted qualification

Require one exact implementation revision green on the existing matrix:

- Ubuntu stable;
- Ubuntu Rust 1.89;
- macOS stable;
- Windows stable;
- dependency-boundary;
- existing Python/abi3 lanes.

If the repository has a dedicated wheel workflow triggered by dependency
changes, run its normal required smoke as well; do not republish Python
artifacts.

The dependency-boundary lane must prove the default Python package still has no
`eggreplay-intercept`/rcgen dependency.

## K. Documentation and handoff

Update after implementation:

- `Cargo.toml` dependency comments;
- `docs/architecture.md` current dependency state;
- `plans/004-research-and-compatibility-baseline.md`;
- `plans/README.md`;
- top-level README status;
- M013B's blocker section;
- registry.

Create:

`plans/closure/m013b0-eggserve-0-3-adoption-and-absolute-form-qualification.md`

The closure records:

- exact EggServe versions;
- lockfile resolution;
- source adaptation summary;
- ownership profile;
- focused test evidence;
- hosted run IDs/URLs and platform test counts;
- dependency-tree evidence;
- any remaining M013B limitations.

Only after that closure:

- mark M013B0 closed;
- remove the obsolete EggServe-publication blocker from M013B;
- mark M013B ready;
- leave M013C–M013F blocked.

## Required verification

At minimum:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo audit
git diff --check
```

Run the existing dependency-boundary and Python package checks used by CI.

No public Internet is required for behavioral tests.

## Acceptance criteria

- [ ] EggReplay resolves exactly `eggserve-primitives 0.2.1` and
      `eggserve-server 0.3.0`.
- [ ] no duplicate old EggServe server/primitives remain in the lock graph.
- [ ] no core/static/PHF dependency enters the direct EggReplay path.
- [ ] ordinary replay/recording remains `OriginOnly` and EggServe-owned.
- [ ] existing recording/replay/WebSocket behavior remains green.
- [ ] M013A caller-owned TLS proof uses the validated 0.3 policy path.
- [ ] interception preflight explicitly enables `OriginOrAbsolute`.
- [ ] absolute target form/scheme/authority/path/query are observed from the
      canonical service request.
- [ ] Host mismatch and full-target 414 fail before service.
- [ ] CONNECT remains tunnel-form behavior.
- [ ] absolute-form streamed body + trailers reach the service correctly.
- [ ] M013B ownership profile has exactly one tunnel-admission authority.
- [ ] default Python wheel remains interception-free.
- [ ] Rust 1.89 and the full hosted platform matrix pass.
- [ ] closure evidence exists before M013B becomes ready.

## Non-goals

- No explicit-proxy target-policy implementation.
- No hop-by-hop header filtering.
- No outbound HTTP forwarding.
- No CONNECT relay implementation.
- No CA/leaf issuance.
- No HTTPS MITM.
- No adoption of EggServe external policy/admission ownership without concrete
  evidence.
- No EggServe Core/Static dependency.
- No Python interception API.
