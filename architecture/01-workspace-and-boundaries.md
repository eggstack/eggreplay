> Deep dive for [overview](overview.md).

# 01 — Workspace and Boundaries

How the seven crates fit together, which dependency edges are invariants, which
feature flags may admit which capability, and the CI lanes that fail the build
when one of those edges is crossed. The boundary rules here are machine-checked;
every one of them has a `cargo tree` or `grep` assertion in
`.github/workflows/ci.yml` that is cited in [CI enforcement lanes](#ci-enforcement-lanes).

---

## Workspace layout

`Cargo.toml:1-11` declares seven members, resolver `3`:

| Crate | Lines | Files | Owns (one line) |
|---|---|---|---|
| `eggreplay-core` | 5,288 | 11 | Semantic flow/conversation models, matcher, scenarios, redaction, error taxonomy; no transport, Tokio, or filesystem. |
| `eggreplay-store` | 3,428 | 1 | `.eggr` directory fixtures: manifest, JSONL flows, content-addressed blobs, extensions. |
| `eggreplay-har` | 2,479 | 1 | Lossy HAR 1.2 import/export with a loss report and session migration helpers. |
| `eggreplay-http` | 21,234 | 16 | Every adapter and orchestration: recording gateway, replay server, regression, protocol policy. |
| `eggreplay-intercept` | 13,490 | 18 | Opt-in explicit H1 proxy, CONNECT policy, CA lifecycle, leaf issuance, HTTPS MITM. |
| `eggreplay-cli` | 6,012 | 6 | Clap surface, exit codes, report emission, fixture inspection. |
| `eggreplay-python` | 1,623 | 6 | PyO3 bindings, asyncio lifecycle, pytest plugin packaging. |
| **Total** | **53,554** | **59** | |

Line counts are all `.rs` files under `crates/`, excluding `target/`.

### Dependency graph

Direction is strictly downward: arrows point at the depended-on crate.

```
  eggreplay-cli        eggreplay-python        eggreplay-intercept
  (bin `eggreplay`)    (cdylib, leaf)          (opt-in leaf)
        |  \                  |  \                    |
        |   \                 |   \                   |
        |    v                v    v                  v
        |  eggreplay-har   eggreplay-http <-----------+
        |        |               |
        +--------+---------------+
                 v               v
           eggreplay-store  ──▶ eggreplay-core
                                 (leaf: no workspace deps)
```

Workspace edges read directly from the manifests:

| Crate | Depends on (workspace crates) | Optional |
|---|---|---|
| `eggreplay-core` | none | — |
| `eggreplay-store` | `core` | — |
| `eggreplay-har` | `core`, `store` | — |
| `eggreplay-http` | `core`, `store` | — |
| `eggreplay-intercept` | `core`, `store`, `http` | — (itself is the optional one) |
| `eggreplay-cli` | `core`, `store`, `har`, `http` | `eggreplay-intercept` (`crates/eggreplay-cli/Cargo.toml:20`) |
| `eggreplay-python` | `core`, `store`, `http` | — |

Two shapes are worth naming explicitly.

**`eggreplay-intercept` is a leaf.** It is the only crate that depends on
`rcgen` (`crates/eggreplay-intercept/Cargo.toml:21`), and it is reachable only
through `eggreplay-cli/intercept`. ADR 0008 makes it "a separate privileged
leaf"; ADR 0003 says no CA generation, trust-store mutation, or certificate
issuance dependency belongs in core or default builds.

**`eggreplay-python` is a leaf cdylib.** `crates/eggreplay-python/Cargo.toml:10-14`
declares `name = "_native"`, `crate-type = ["cdylib"]`, `test = false`,
`doctest = false` — its Rust tests are not a target, and no workspace crate
depends on it. Its only non-workspace specialisms are `pyo3 =0.29.2`
(`abi3-py311`) and `pyo3-async-runtimes =0.29.0` (`:23-24`).

---

## Dependency rules

These are the invariants. Each is stated in `plans/001-architecture-and-boundaries.md`
and each is asserted in CI.

1. **`eggreplay-core` has no transport, no Tokio, no filesystem.** Its only
   dependencies are `chrono`, `serde`, `serde_json`, `sha2`, `thiserror`, `url`,
   `uuid` (`crates/eggreplay-core/Cargo.toml:10-17`). CI rejects
   `eggfetch*`, `eggserve*`, `eggress*`, `tokio`, `hyper`, `tungstenite`, and
   `base64` in the normal-edge tree (`.github/workflows/ci.yml:134`).
2. **`eggreplay-store` is filesystem-only.** It adds `tempfile` and `sha2` to
   the core set (`crates/eggreplay-store/Cargo.toml:11-17`) and is held to the
   identical transport-purity grep (`.github/workflows/ci.yml:135`).
   `exclude = ["tests/fixtures/**"]` (`:9`) keeps golden fixtures out of the
   published package.
3. **`eggreplay-http` owns every adapter and is the only place an optional
   transport dependency is declared.** Each of `base64`, `tokio-tungstenite`,
   `eggserve-primitives`, `eggserve-server`, `eggserve-core`, `eggnet-tls`,
   `eggress-outbound`, `prost-reflect` is `optional = true` and reachable only
   through a feature (`crates/eggreplay-http/Cargo.toml:44,59,61-66`). The
   WebSocket codec in particular must not leak into the direct profile
   (`.github/workflows/ci.yml:140`).
4. **Eggress stays narrow.** Only `eggress-outbound/pproxy-compat` is enabled
   (`crates/eggreplay-http/Cargo.toml:40`, and
   `crates/eggreplay-intercept/Cargo.toml:18`). SSH, QUIC, and extended
   surfaces are asserted absent (`.github/workflows/ci.yml:143`).
5. **`eggreplay-intercept` is opt-in and nobody depends upward on it.** CI loops
   all five other crates plus the Python wheel and fails if `eggreplay-intercept`
   or `rcgen` appears in any of their normal edges
   (`.github/workflows/ci.yml:146-152`).
6. **Product crates must not depend on PyO3.** `core`, `store`, and `http` are
   checked for `pyo3`/`pyo3-async-runtimes`; then the inverse tree
   `cargo tree --workspace --invert eggreplay-python` must not list any
   `eggreplay-{core,store,http,cli}` (`.github/workflows/ci.yml:63-75`).
7. **`eggreplay-python` is a leaf workspace crate** and Rust remains the only
   semantic authority over it — the pure-Python package may not implement
   matching, validation, redaction, scenarios, transport, or regression
   decisions (ADR 0007, `## Decision`).
8. **No milestone copies a sibling protocol implementation.** ADR 0002 is
   explicit: "No milestone copies a sibling protocol implementation merely to
   avoid an integration dependency. If a reusable seam is missing, document the
   gap and either add the narrow seam upstream or defer the feature."
9. **The test-only gRPC oracle never enters a product graph.** `tonic`,
   `tonic-prost`, `prost-types`, `prost` are declared in `[workspace.dependencies]`
   purely as *version sources* for `eggreplay-http`'s `[dev-dependencies]`
   (`Cargo.toml:84-98`, `crates/eggreplay-http/Cargo.toml:77-85`). Because the
   boundary lanes resolve `--edges normal`, which excludes dev edges, a Tonic
   server can qualify gRPC-over-H2 in tests without shipping
   (`.github/workflows/ci.yml:303-340`).

The workspace manifest itself encodes policy as prose. `Cargo.toml:64-70` states
that `eggserve-core` is "the multiprotocol composition layer and is *opt-in
only*" and that the default H1/direct graph "must never pull it, nor the
`eggserve-static` closure it brings with it, nor any H2/QUIC/H3 dependency."

---

## Feature-flag matrix

Only two crates declare features. `core`, `store`, `har`, `intercept`, and
`python` have no `[features]` table at all, which is itself a boundary: they
cannot grow a capability flag.

### `eggreplay-http` (`crates/eggreplay-http/Cargo.toml:10-40`)

| Feature | Default | Admits |
|---|---|---|
| `direct` | **yes** | Nothing by itself; it is the marker profile meaning "plain H1, no optional codec." |
| `eggserve` | no | `eggserve-primitives` + `eggserve-server` — the supported inbound H1 runtime. |
| `websocket` | no | `base64` + `tokio-tungstenite` — RFC 6455 message codec over already-owned streams (cleartext only). |
| `h2` | no | `eggfetch-core/native-http2` — **outbound only**; H1 stays the default policy and the caller must set an explicit `HttpVersionPolicy`. |
| `h2-inbound` | no | `eggserve` + `eggserve-core` + `eggserve-core/http2` — the multiprotocol serving closure. |
| `h2-inbound-tls` | no | `h2-inbound` + `eggserve-core/tls` + `eggnet-tls` — ALPN over TLS with operator-supplied identity material. |
| `grpc` | no | `prost-reflect` — optional protobuf/gRPC derived views. |
| `eggress` | no | `eggress-outbound` with `pproxy-compat` only. |

`default = ["direct"]` (`:11`). Nothing else is default.

### `eggreplay-cli` (`crates/eggreplay-cli/Cargo.toml:45-53`)

| Feature | Default | Admits |
|---|---|---|
| `intercept` | no | `eggreplay-intercept` — the CA/`rcgen` leaf. |
| `h2` | no | Forwards `eggreplay-http/h2`. |
| `h2-inbound` | no | Forwards `eggreplay-http/h2-inbound`. |
| `h2-inbound-tls` | no | `h2-inbound` + forwards `eggreplay-http/h2-inbound-tls`. |

`default = []` (`:46`). The CLI's dependency line on `eggreplay-http`
unconditionally adds `eggserve`, `eggress`, and `websocket`
(`crates/eggreplay-cli/Cargo.toml:19`) — those three are part of the default CLI
graph, which is why the inbound-H2 opt-in must not disturb them
(`.github/workflows/ci.yml:297-301`).

### Profiles that select features without declaring them

| Consumer | Selection | Notes |
|---|---|---|
| `eggreplay-python` | `eggreplay-http` with `default-features = false, features = ["direct","eggserve","eggress","websocket"]` (`crates/eggreplay-python/Cargo.toml:18`) | Explicitly drops `direct`-by-default and re-adds it; no H2 or gRPC view exists in the wheel. |
| `eggreplay-intercept` | `eggreplay-http` with `["eggserve","eggress"]` (`crates/eggreplay-intercept/Cargo.toml:14`) | H1 only. |

### The three claims that must be repeated

- **`h2-inbound` admits `eggserve-core` and the `eggserve-static` closure, and
  must never be default.** ADR 0010's option B — "make `eggserve-core` a
  default dependency to obtain H2" — is *rejected*, because the ordinary
  H1/direct profile "would silently acquire `eggserve-static`, Hyper `http2`,
  and the H2 protocol graph, widening every default build, the Python wheel,
  and the interception graph for a capability that must stay opt-in"
  (ADR 0010, `## Options considered`). The cost is accepted inside the opt-in
  graph only: "`eggserve-static` is a non-optional dependency of
  `eggserve-core`. Any graph that admits Core also admits Static. That closure
  is accepted *only* inside the opt-in H2 graph" (ADR 0010, fact 5).
- **`h2` is outbound-only and independent of `h2-inbound`.** `h2` forwards
  `eggfetch-core/native-http2` and nothing else. An operator may record over H2
  while serving H1, or the reverse (`crates/eggreplay-cli/Cargo.toml:48-50`;
  `crates/eggreplay-http/Cargo.toml:15-19`). CI checks the combination
  `--features eggserve,h2` and asserts the Core/Static closure is still absent
  (`.github/workflows/ci.yml:186`).
- **No HTTP/2 capability is default in any profile.** ADR 0010's consequence
  list states that default, direct, and interception graphs are free of
  `eggserve-core`, `eggserve-static`, and the H2 protocol graph. The one
  permitted exception is documented rather than swept: the `h2` *library* still
  appears in the default and interception graphs through
  `eggress-outbound` → `eggress-protocol-http`; that is pre-existing Eggress
  routing-hop behavior since 1.0.8, is not the multiprotocol serving closure,
  and "is pinned by the boundary checks so it cannot be mistaken for one."

---

## Lint and toolchain policy

| Setting | Value | Where |
|---|---|---|
| Edition | 2024 | `Cargo.toml:15` |
| MSRV | 1.89 (`rust-version`) | `Cargo.toml:16` |
| Toolchain file | `channel = "stable"`, components `rustfmt`, `clippy` | `rust-toolchain.toml:1-3` |
| `unsafe_code` | `forbid` | `Cargo.toml:22` |
| `missing_docs` | `warn` | `Cargo.toml:23` |
| Clippy | `all` + `pedantic` at `warn`; `module_name_repetitions` and `must_use_candidate` allowed | `Cargo.toml:25-29` |
| Release profile | `lto = "thin"`, `codegen-units = 1`, `strip = "symbols"` | `Cargo.toml:31-34` |

CI turns the clippy warning level into an error with `-D warnings`
(`.github/workflows/ci.yml:28`).

**One caveat a reviewer should know.** `[workspace.lints]` is opt-in per crate
via `[lints] workspace = true`. Only four crates declare it:
`eggreplay-cli` (`:55-56`), `eggreplay-har` (`:23-24`), `eggreplay-intercept`
(`:47-48`), and `eggreplay-python` (`:30-31`). `eggreplay-core`,
`eggreplay-store`, and `eggreplay-http` do **not** — so `unsafe_code = forbid`,
`missing_docs`, and clippy pedantic do not currently apply to the three crates
with the most product surface. `cargo clippy --workspace -- -D warnings` still
denies the default warning set everywhere, but the workspace lint table is
narrower there than the manifest suggests.

### Canonical local verification

From `AGENTS.md:8-12`:

```sh
cargo fmt --all -- --check && cargo check --workspace --all-targets --all-features && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features
```

CI runs the same four steps with `--locked` added and, for tests,
`--no-fail-fast` "so one failing suite (e.g. a platform-specific isolation
reproducer) never hides the rest" (`.github/workflows/ci.yml:26-31`).

Python binding development is a separate loop, also from `AGENTS.md:14-17`: an
isolated environment in `crates/eggreplay-python`, pinned tools, then
`maturin develop` and `python -m pytest tests`. "The native extension remains a
leaf workspace crate and Rust product crates must not depend on PyO3."

---

## CI enforcement lanes

`.github/workflows/ci.yml` defines five jobs. `verify` runs a 4-cell matrix
(three OSes on stable, plus Linux on 1.89.0 — the MSRV lane is Linux-only by
design, `.github/workflows/ci.yml:14-18`), giving 14 total hosted jobs, which
matches the "all 14 jobs green" phrasing in the M016/M017 closure records.
The three boundary jobs are the interesting part: they assert architecture, not
behavior.

### `verify` — the compile/test baseline

Asserts the whole workspace compiles and passes under `--all-features` on three
platforms and two toolchains. What breaks: any portability bug, any MSRV
regression, any lint. Why it exists: the boundary claims are only credible if
every profile in the matrix is green, not just the default one.

### `python-bindings` — the Python-free lane

Four cells (`.github/workflows/ci.yml:33-48`): Linux/3.11 on 1.89.0,
Linux/3.14 on stable, macOS/3.11, Windows/3.11. Each builds with
`maturin==1.14.1` + `pytest==8.4.2` + `pytest-asyncio==1.2.0` +
`pytest-xdist==3.8.0` under `uv 0.8.22`, then runs the suite.

The lane that matters architecturally is the inline step *"Ensure semantic
crates remain Python-free"* (`:63-75`). It does two things:

```bash
for package in eggreplay-core eggreplay-store eggreplay-http; do
  if cargo tree -p "$package" --edges normal --prefix none | grep -E '^(pyo3|pyo3-async-runtimes) '; then ... fi
done
if cargo tree --workspace --invert eggreplay-python --edges normal --prefix none \
  | grep -E '^eggreplay-(core|store|http|cli) '; then ... fi
```

What breaks: any PyO3 reference from a semantic crate, and any reverse
dependency from a Rust product crate onto the extension. Why here: the
extension crate is the only place the two language worlds meet, so the invariant
is cheapest to check where a Python interpreter already exists — and the comment
notes tree queries need metadata only, so this could have lived in the
Linux-only boundary job.

The same job also checks wheel and sdist contents for `.venv`,
`.pytest_cache`, `__pycache__`, `fixtures`, `target`, and any `.pyc`/`.pyo`
(`:82-103`).

### `dependency-boundary` — core/store purity, Eggress narrowness, interception absence

Nine assertions, all Linux/stable:

1. `cargo tree -p eggreplay-core --no-default-features` must not contain
   `eggfetch*|eggserve*|eggress*|tokio|hyper|tungstenite|base64` (`:134`).
2. The same grep on `egreplay-store` (`:135`).
3. `cargo check -p eggreplay-http --no-default-features --features direct` (`:137`)
   — the direct H1 build must not require Eggress routing.
4. `cargo check -p eggreplay-http --no-default-features --features websocket` (`:139`)
   — the codec is opt-in and belongs only to the HTTP adapter crate.
5. The `direct` tree must not contain `tungstenite` or `base64` (`:140`).
6. The `eggress` tree must contain `eggress-outbound` (`:142`) — a presence
   check, not just an absence check.
7. The inverse Eggress tree must not contain `ssh|quic|extended` (`:143`).
8. **The interception-absence lane**: a loop over all five other crates failing
   if `eggreplay-intercept` or `rcgen` appears in their normal edges (`:146-152`).
9. `cargo check -p eggreplay-intercept --all-targets --locked` (`:153`) — the
   opt-in crate must still build on its own.

What breaks: a new convenience dependency in `core` or `store`; a codec or
router leaking into the direct profile; anyone "temporarily" wiring
interception into a product crate. Note the grep includes `base64` in the
core/store denylist — the semantic crates are not allowed an ad-hoc encoding
dependency.

### `protocol-boundary` — the M015A/M015E inbound-H2 lane

Eight named steps (`.github/workflows/ci.yml:155-367`). The header comment is
the design statement: the `h2-inbound` feature "is the ONLY thing that may admit
EggServe Core (and therefore `eggserve-static` plus the Hyper/h2 H2 protocol
graph). Every ordinary profile must stay free of that multiprotocol closure,
and outbound H2 must never imply inbound multiprotocol serving."

**a. Ordinary profiles exclude the Core/Static/H3 closure** (`:169-195`). A
`check_no_core` helper wraps `cargo tree --edges normal --prefix none | grep -E
'^(eggserve-core|eggserve-static|eggserve-h3) '` and is invoked for nine
profiles: `workspace default`, `http direct/default`, `http eggserve`,
`http eggserve,h2`, `intercept --all-features`, `cli` **default**, `python`,
`core --no-default-features`, `store --no-default-features`.

The CLI case carries an in-file rationale worth preserving: it uses the
*default* graph, not `--all-features`, because "M015B gives the CLI opt-in
`h2-inbound`/`h2-inbound-tls` features, so `--all-features` is deliberately a
multiprotocol graph and asserting on it would assert against the design. The
default is the supported profile" (`:188-191`).

**b. The opt-in feature is explicit and does pull the closure** (`:196-225`).
For `h2-inbound` and `h2-inbound-tls`: each must `cargo check --all-targets`
*and* each tree must then contain `^eggserve-core `. The same is required for
the CLI's forwarding features. Finally, `http --no-default-features` with no
features must *not* contain `eggserve-core`. So the lane asserts the boundary
in both directions — absence by default, presence on request.

**c. QUIC/H3 stays absent** (`:226-238`). Seven `eggreplay-http` feature
combinations are swept, ending with the maximal
`h2-inbound,h2-inbound-tls,h2,grpc,eggress,websocket`, and each tree must not
contain `quinn`, `eggserve-h3`, `eggress-transport-quic`, or
`eggress-protocol-h3`. Why: ADR 0010 rejects option D — adopting
`eggserve-h3 0.4.0` now that Core is being added — because ADR 0009 already
deferred H3 on missing upstream seams and "H3 is not authorized by this
dependency adoption."

**d. Interception never adopts inbound multiprotocol serving** (`:239-254`).
Two checks: `eggreplay-intercept --all-features` must not contain
`eggserve-core`/`eggserve-static`, and a literal grep of
`crates/eggreplay-intercept/Cargo.toml` must not match `h2-inbound`. The second
is a source grep rather than a graph query because a feature that does not
exist cannot be seen in a tree; "interception stays H1-only on
`eggserve-server`."

**e. The Python binding lane stays free of the multiprotocol closure**
(`:255-264`). `eggreplay-python` must not contain
`eggserve-core|eggserve-static|eggserve-h3` — the wheel cannot gain inbound H2.

**f. Every feature profile of `eggreplay-http` compiles** (`:265-302`). Ten
`--all-targets` profiles: no features, `direct`, `eggress`, `websocket`, `h2`,
`grpc`, `eggserve`, `h2-inbound`, `h2-inbound-tls`, and the maximal
`h2-inbound-tls,grpc,eggress,websocket`; then the CLI at
`--no-default-features` and at `--features h2-inbound-tls`. The comment records
why `--all-targets` and not just the lib: the M015B regression that broke the
`direct` profile "lived partly in unit-test modules — `inbound.rs` and
`render_recorded_headers` referenced optional `eggserve-*` crates without a
gate, and two `mod tests` blocks exercised the serving path unguarded. A
lib-only check missed all of it." The governing principle: "An optional feature
is only a boundary if the profiles on both sides of it still build."

**g. The gRPC oracle never enters a product graph** (`:303-340`). Nine
`check_no_tonic` invocations across workspace default, workspace all-features,
`http` all-features, `http` inbound-H2+TLS, `intercept`, CLI default, CLI
inbound-H2, and `python` all-features; then a positive check that `tonic` *is*
present under `--edges dev`. `prost`/`prost-types` are deliberately not
checked, "so the `grpc` feature has carried `prost-reflect` since M014D, so
their presence is by design. Tonic is the thing that must stay out."

**h. Every inbound-H2 listener and the gRPC view are behind one feature**
(`:341-367`). A source grep requires
`#[cfg(feature = "h2-inbound")]` in `crates/eggreplay-http/src/inbound.rs`, and
two graph checks pin `prost-reflect` to the `grpc` feature. The comment is
about anti-drift: "If that gate were ever dropped, a default build would gain a
second HTTP stack without any manifest change, which is exactly the failure the
`protocol-boundary` lane exists to catch. Asserting it here as well as in the
graph keeps the two checks from drifting apart."

### `interception` — the qualification lane

Runs on all three OSes (`.github/workflows/ci.yml:369-403`). It first proves
the capability stays optional: `cli --no-default-features` checks, an
`intercept --all-targets` check, and an assertion that
`cargo tree -p eggreplay-python` has no `eggreplay-intercept` or `rcgen`. Then
it runs the full suite with `--no-fail-fast` and
`cargo test -p eggreplay-cli --features intercept --locked`. Every step forces
`shell: bash` "where `grep` chaining fails the step spuriously" on Windows
runners that default to pwsh.

### The shape of the enforcement

Three properties make this more than documentation:

- **Bidirectional assertions.** Presence is asserted where absence is (steps
  b and g of `protocol-boundary`), so a feature cannot be deleted from the graph
  while leaving the code gated.
- **Both edges and sources.** `cargo tree` proves the resolved graph; `grep` on
  `inbound.rs` and `crates/eggreplay-intercept/Cargo.toml` proves the gates that
  a tree cannot see.
- **Deliberate negative results recorded inline.** The `--all-features` CLI
  exception, the `h2`-library-via-Eggress exception, and the `--all-targets`
  rationale are all written next to the command they qualify, with the reason
  for the exemption.

`.github/workflows/python-wheels.yml` is the packaging counterpart: five release
targets (manylinux x86_64/aarch64, macOS arm64/x86_64, Windows x86_64), an
sdist job that builds a wheel *from* the sdist, and an `abi3-interpreter-smoke`
job that installs the one manylinux abi3 wheel under CPython 3.11, 3.12, 3.13, and
3.14. It is `workflow_dispatch`-plus-`main`-push gated on
`.github/workflows/python-wheels.yml`, `crates/eggreplay-python/**`, and
`tools/python/**` (`:4-10`).

---

## Change discipline

`AGENTS.md:3-6` sets the operating rule: "Use the plans in `plans/` as the
execution source of truth. Work milestones in dependency order, update
`plans/registry.md` and add a closure record when each milestone closes. Keep
transport ownership in EggFetch, EggServe, and Eggress; do not add a parallel
HTTP stack."

### Layout

`plans/README.md:15-26` defines the convention:

| Directory | Holds |
|---|---|
| `plans/` root | Canonical roadmap and qualification documents (`000`–`004`) |
| `plans/subsystems/` | Subsystem roadmaps |
| `plans/implementation/<workstream>/` | Bounded implementation plans |
| `plans/adrs/` | Architectural decisions |
| `plans/closure/` | Completion evidence |
| `plans/archive/` | Historical superseded material |
| `plans/registry.md` | Execution/status index |

Status vocabulary (`plans/README.md:28-35`): `ready`, `blocked`, `active`,
`implemented`, `closed`, `deferred`. "Plans remain audit artifacts after
implementation. Source presence alone never closes a plan."

### Gate rules

`plans/registry.md:229-244`:

- **blocked → ready** only when every dependency is closed, or the plan
  explicitly permits an implemented-but-not-closed dependency.
- **→ closed** only after implementation, required tests/evidence,
  documentation updates, and a closure record are all present.
- For decomposed milestones, "an umbrella milestone never authorizes skipping
  subplan dependencies"; independent sibling subplans may run concurrently; a
  parent closes only when every required child track has an explicit
  closure/support decision.
- Hosted-CI-gated plans stay open until remote evidence is green.
- Historical closure records are immutable audit artifacts.

### ADRs

Ten ADRs exist. Four govern boundaries directly:

| ADR | Rule it encodes |
|---|---|
| 0002 — Reuse Eggstack Transport Authorities | EggFetch owns outbound, EggServe owns inbound, Eggress owns optional routing; no milestone copies a sibling implementation. |
| 0003 — Interception Is an Optional Acquisition Adapter | Interception is not in the core path or the v0.1 gate; "any later interception milestone requires a separate threat model, certificate lifecycle design, and isolated feature/crate boundary." |
| 0007 — Python Binding Authority and Runtime | Rust is the only semantic authority; one process-wide Tokio runtime through the `pyo3-async-runtimes` bridge; `abi3-py311` over CPython 3.11–3.14. |
| 0010 — Inbound HTTP/2 Serving Boundary | Direct H1 stays on `eggserve-server` + `eggserve-primitives`; inbound H2 is `eggserve-core` behind `h2-inbound`/`h2-inbound-tls` only; no Hyper fork; H3 not authorized. |

ADR 0010's own decision list is the closest thing to a design contract for
this document: "`eggserve-core` is declared in `[workspace.dependencies]` with
`default-features = false` and pulled into `eggreplay-http` as an
`optional = true` dependency. **Nothing else in the workspace may depend on
it.**"

### Closure records

A closure record is a scope table of touched files, a status line naming the
qualifying commit and hosted run, and — when the milestone corrected a prior
belief — an explicit account of the premise that turned out to be wrong. The
M017 record is the model: it opens with the status line
(`closed (qualifying hosted run 37238704257 on 0c48a26, all 14 jobs green)`),
a `## Scope` file table with `**new**`/`**fix**`/`**rewrite**` markers, and
`## The deferral's stated blocker did not exist`, which records that the
gateway "is **already** full-duplex" and cites the upstream source line.

### Before proposing a change

1. Does it add a transport dependency to `core`, or a non-filesystem capability
   to `store`? Stop.
2. Does it add an optional dependency to `eggreplay-http`? It needs a feature
   entry, a comment stating what it admits, and a `protocol-boundary` profile
   check.
3. Does it want `eggserve-core`? It needs an ADR that reopens ADR 0010, and CI
   will fail until `check_no_core` is narrowed *and* the reason is recorded
   inline.
4. Does it add PyO3, `rcgen`, or `eggreplay-intercept` to a product crate? CI
   fails at `ci.yml:63-75` and `ci.yml:146-152`.
5. Does it change a dependency version? `Cargo.toml` pins every Eggstack
   artifact exactly (`=0.2.2`, `=0.4.0`, `=1.0.11`, `=0.2.0`) and states why
   in comments; a bump is a plan with a closure record, not a `cargo update`.
6. Then: registry row, plan under `plans/implementation/`, closure record under
   `plans/closure/`, and a green run of the canonical command.

---

## Review checklist

Boundary-focused questions, in the order a reviewer should ask them.

**Dependency graph**

1. Does `cargo tree -p eggreplay-core --no-default-features --edges normal`
   still contain no `eggfetch*`, `eggserve*`, `eggress*`, `tokio`, `hyper`,
   `tungstenite`, or `base64`? Same question for `store`.
2. Did any crate gain a dependency for convenience that a reviewer would not
   recognise as belonging to its layer?
3. Does any new edge point *up* — a semantic crate depending on an adapter, or a
   product crate depending on `eggreplay-python` or `eggreplay-intercept`?
4. Is a new optional dependency declared in the *consuming* crate with
   `optional = true` and a feature, rather than added as a plain dependency?

**Features**

5. Is the new capability reachable from `default`? If yes, that is a boundary
   break, not a convenience.
6. For `eggreplay-http`, does the new feature's comment say what it admits, and
   does a `check_profile` entry exist in the "every feature profile compiles"
   step with `--all-targets`?
7. Does `h2` stay outbound-only, and does `h2-inbound` remain non-default?
8. Does the change keep `prost-reflect` behind `grpc` and `rcgen` behind
   `eggreplay-intercept`?

**Code**

9. Is `crates/eggreplay-http/src/inbound.rs` still gated on
   `#[cfg(feature = "h2-inbound")]`, and are test modules exercising optional
   paths gated the same way? The M015B regression lived in exactly this place.
10. Is anything a second implementation of a transport concern — a Hyper
    client/server, a CONNECT/SOCKS stack, a TLS verifier, a hand-built X.509
    encoder? ADR 0002 and ADR 0008 both forbid it.
11. Does a new capability fail *closed* when its feature is off, rather than
    silently downgrading to a weaker protocol?

**Process**

12. If a boundary rule genuinely had to change, is there an ADR that reopens
    the prior one, with the options that were rejected and why?
13. Does the change add or widen a `cargo tree` assertion? A new capability
    without a boundary check will regress silently.
14. Is the registry updated, and is there a closure record with the qualifying
    commit and hosted run?
15. Are the CI exemptions still justified inline? Every `||` and every narrower
    profile in `ci.yml` should carry its reason next to it, as the CLI
    default-graph note does.
