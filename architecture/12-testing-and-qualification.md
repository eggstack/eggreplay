> Deep dive for [overview](overview.md).

# 12 — Testing and Qualification

EggReplay's central claim is not that it records HTTP traffic. It is that a
named capability is *supported*, and that a reader can check. Everything in
this document exists to make that claim falsifiable: one canonical local gate,
a test topology where each file proves a specific tier, CI lanes that assert
architecture as well as behaviour, and a closure record per milestone that
cites a commit SHA, a hosted run ID, and a per-suite test count.

The rule that generates all of it is in `plans/003-qualification-and-release-strategy.md:57`:
"Source presence alone never establishes support." And in
`plans/README.md:26`: "Plans remain audit artifacts after implementation. Source
presence alone never closes a plan."

---

## The fast gate

`AGENTS.md:8-12` is the one supported local verification command:

```text
cargo fmt --all -- --check && cargo check --workspace --all-targets --all-features && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features
```

CI runs the same four stages with `--locked` added to every cargo step, and
`--no-fail-fast` on tests (`.github/workflows/ci.yml:26-31`):

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked --no-fail-fast
```

`--no-fail-fast` is a correctness requirement, not a convenience. `cargo test`
halts at the first failing test binary by default, which truncates the run —
`plans/closure/m016-post-m015-corrective.md:205-208` records that an earlier
M015 test count was wrong for exactly that reason and was corrected.

Policy, from `docs/testing.md:3-7`: tests are **local-only** and use loopback
fixtures; "no routine test depends on public Internet." Network behaviour is
qualified through the delegated EggFetch / EggServe / Eggress surfaces, and
core and store tests need no network runtime at all. `plans/003:90` restates it
as a hard rule: "No test may mutate the system/browser trust store or require
public Internet."

The Python binding loop is separate, from `AGENTS.md:14-17`: an isolated
environment in `crates/eggreplay-python`, pinned tools, then `maturin develop`
and `python -m pytest tests`.

> Documentation drift, verified: `docs/architecture.md:85-86,94` still states
> `eggserve-server 0.3.0` / `eggserve-primitives 0.2.1` / Eggress `1.0.8`,
> while the workspace manifest pins `eggserve-server =0.4.0`,
> `eggserve-primitives =0.2.2`, `eggress-outbound =1.0.11`
> (`Cargo.toml:72-74`). The M015A refresh is recorded correctly in
> `plans/004-research-and-compatibility-baseline.md:106-108` and in
> `plans/registry.md:176-178`; the `docs/architecture.md` dependency-state
> section is the stale copy.

---

## Test topology

Nineteen integration test files, 19,196 lines, plus unit tests inside each
crate. The naming convention is the plan or corrective that produced the file
(`m013e_*`, `m015b_*`) when the suite qualifies one milestone, and a capability
name otherwise. A file's existence is a claim: it says that tier reached the
point of needing a dedicated qualification suite.

| Path | Lines | Tier / what it proves |
|---|---|---|
| `crates/eggreplay-http/tests/v01_qualification.rs` | 798 | v0.1 corrective requalification (C005). Test names are `qual_NN_…`, one per required item in the C005 list of 1–30 — `qual_03_04_repeated_headers_and_query_preserved`, `qual_05_multiple_set_cookie_preserved_and_redacted`, `qual_07_head_and_204_body_suppression_delegated`, `qual_10_11_connection_refused_and_dns_classified`, `qual_12_tls_to_plaintext_fails_safely`, `qual_13_response_head_timeout_classified`, `qual_15_consumption_modes`, `qual_17_practical_ignores_volatile_headers`, `qual_23_target_remapping_preserves_path_query`. Four EggFetch/Eggress substrate rows are separate. |
| `crates/eggreplay-http/tests/h2_qualification.rs` | 1,059 | M014B outbound H2 over EggFetch `native-http2` (ALPN `h2` on local TLS). `alpn_h2_records_with_version_annotation`, `hyper_client_interop_h2`, `raw_h2_crate_interop`, `routed_h2_via_eggress_tcp`, `grpc_view_over_h2_recorded_flow`, `cleartext_prior_knowledge_fails_closed`. |
| `crates/eggreplay-http/tests/h2_inbound_serving.rs` | 1,845 | M015B inbound H2 serving. Gated `#![cfg(all(feature = "eggserve", feature = "h2-inbound-tls"))]`. 30 tests: ALPN and cleartext policy, request projection into the matcher, trailers both directions, per-stream consumption, shutdown, plus an H1 regression matrix on the H2-enabled graph. |
| `crates/eggreplay-http/tests/h2_end_to_end.rs` | 2,589 | M015C end-to-end H2 semantics. Gated on `h2`, `h2-inbound-tls`, `eggress`, `websocket`. The `rowN_…` tests are the falsifiability core: `row1` H2 client → gateway → direct H2 upstream, `row2` the same via an Eggress route, `row6_h1_acquired_fixture_replays_over_h2`, `row7_h2_acquired_fixture_replays_over_h1`, `version_annotations_are_observational_not_matching_dimensions`. |
| `crates/eggreplay-http/tests/h2_hardening.rs` | 1,837 | M015E hostile-peer matrix. Gated on `h2`, `h2-inbound-tls`, `eggress`. 15 tests, each asserting a *bounded refusal*, an *unaffected sibling*, or a *valid fixture* — and then that the listener still works. |
| `crates/eggreplay-http/tests/grpc_integration.rs` | 2,256 | M015D gRPC over H2, 16 tests, gated on `h2`, `h2-inbound`, `grpc`. The first test, `tonic_oracle_round_trips_before_eggreplay_is_involved`, qualifies the harness itself. |
| `crates/eggreplay-http/tests/scenario_faults.rs` | 323 | M014D bounded fault model over H1: head delay, chunked prefix streaming, close-before-response, truncate-after-N, recorded-style 502 projection, cancellation, deterministic reports. |
| `crates/eggreplay-cli/tests/cli_contracts.rs` | 920 | C004 subprocess contracts: routes, exit classes (2/3/…/5), JSON + JUnit reports, `inspect --bodies` bounding and redaction, SSE/stream comparison opt-in. |
| `crates/eggreplay-cli/tests/har_migrate.rs` | 551 | M014A HAR import/export and fixture migration as subprocess contracts: loss report, duplicate preservation, redaction before publication, transactional and idempotent migration, fail-closed on a future extension. |
| `crates/eggreplay-cli/tests/m013e_operator.rs` | 282 | M013E operator surface against the built binary. Split by build: capability-failure assertions run only *without* `intercept` (`#[cfg(not(feature = "intercept"))]`, lines 117/127), interception assertions only *with* it. Also asserts `source_contains_no_automatic_trust_mutation` and that non-loopback record needs an explicit gate. |
| `crates/eggreplay-cli/tests/m015b_inbound_serving.rs` | 345 | M015B flag surface and fail-closed behaviour. Split three ways by `h2-inbound` / `h2-inbound-tls` (lines 109, 265, 325): `http2_policy_is_refused_without_the_feature`, `unknown_inbound_policy_is_refused`, `tls_material_is_refused_without_the_feature`, `status_payload_never_exposes_key_material`. |
| `crates/eggreplay-intercept/tests/substrate.rs` | 874 | M013A published transport/TLS substrate preflight: the EggServe H1 driver over a decrypted TLS stream, Eggress raw CONNECT through a local HTTP proxy with no fallback, explicit-CA and SNI/hostname enforcement, absolute-form listener profile. |
| `crates/eggreplay-intercept/tests/proxy_policy.rs` | 1,377 | M013B explicit proxy and CONNECT policy, 23 tests: absolute-form recording, hop-by-hop stripping, `Proxy-Authorization` never persisted, deny-before-200, exact/suffix policy boundaries, IPv4/IPv6/port dimensions, backpressure, half-close, byte/idle/duration/concurrency limits, drain on shutdown, non-loopback bind opt-in. |
| `crates/eggreplay-intercept/tests/ca_leaf.rs` | 441 | M013C CA lifecycle and leaf issuance, 12 tests: initialize/reopen identity, never-overwrite, import copy-and-detach, tampered-cert fingerprint binding, Unix permission enforcement/repair, Windows behaviour documented rather than faked (`#[cfg(windows)]` at line 274), export contains only the public certificate, rotation, cache key, no key leakage. |
| `crates/eggreplay-intercept/tests/mitm.rs` | 1,883 | M013D HTTPS MITM H1 recording, 24 tests: bodies, trailers, large bidirectional streaming, IP-literal with no SNI, SNI/host/DNS mismatch rejection and tunnel poisoning, upstream TLS failures preserved as failures, Eggress routing with logical origin intact and no direct fallback, `mitm_does_not_negotiate_h2`, websocket-upgrade refusal, redaction before durable publication, drain and session finalization. |
| `crates/eggreplay-intercept/tests/m013e_proxy_stats.rs` | 221 | M013E counters and shutdown/finalization: one denied and one allowed plain request plus one denied `CONNECT` over raw TCP, then the Ctrl-C path via `shutdown`/`wait`/session `finish`. |
| `crates/eggreplay-intercept/tests/curl_interop.rs` | 369 | M013F independent-client interop, 2 tests: `curl_plain_http_proxies_and_records`, `curl_https_connect_mitm_records`. |
| `crates/eggreplay-intercept/tests/hardening.rs` | 731 | M013F secret audit, 7 tests. Unique sentinels for the CA private key, `Proxy-Authorization`, `Authorization`/`Cookie`, and one JSON redaction target must never reach fixtures, diagnostics, events, errors, metadata, or staging residue — and private key *paths* must be absent from routine diagnostics. |
| `crates/eggreplay-intercept/tests/resource_bounds.rs` | 170 | M013F bound pinning, 9 tests, one per bound family (policy, CA input, leaf, tunnel, TLS handshake, admission/diagnostics, inherited session, listener profile, substrate versions). |
| `crates/eggreplay-intercept/tests/tls_shutdown_isolation.rs` | 325 | TLS shutdown and transport isolation, 9 tests: raw `rustls` exact byte transfer (1,024 / 131,190 / 300,118 bytes) under graceful `close_notify` vs abrupt drop, and minimal Hyper H1 over plaintext and over `rustls` — i.e. the substrate works without any EggFetch glue. |

`crates/eggreplay-har/tests/` holds only a golden corpus
(`minimal.har`, `duplicates.har`, `binary-and-error.har`), consumed by
`crates/eggreplay-har/src/lib.rs:2350-2362`. `crates/eggreplay-python/tests/`
holds one file, `test_preflight.py` (1,033 lines, 43 top-level `test_`
functions), covering native import round-trip, stub/runtime export agreement,
asyncio bridge and cancellation, bounded body readers, fixture-error
redaction, the pytest plugin (VCR decorator, read-only shared fixtures,
`xdist` workers, writer-lock semantics, path-escape refusal) and interpreter-exit
behaviour.

---

## Independence and oracles

A capability is qualified against an *independent* implementation. If the
product's client is also the test's client, a shared bug reads as a pass.

| Oracle | Independent of | Where it appears |
|---|---|---|
| Tonic 0.14.6 | EggReplay's gRPC view | `grpc_integration.rs`, dev-dependency of `eggreplay-http` only |
| Hyper H1 + H2 client stacks | EggFetch | `h2_inbound_serving.rs`, `h2_end_to_end.rs`, `tls_shutdown_isolation.rs` |
| raw `h2` crate framing | Hyper | `h2_inbound_serving.rs`, `h2_qualification.rs`, `h2_end_to_end.rs` |
| `curl` | the scripted rustls and EggFetch clients | `curl_interop.rs` |
| `eggfetch-core` client | EggReplay's MITM path | `mitm.rs::mitm_interoperates_with_eggfetch_client_through_proxy` |
| `rcgen` | any real CA | test-owned identities in `substrate.rs`, `h2_*.rs` |

Tonic is the load-bearing case because gRPC is the one place where self-testing
would be trivially easy and worthless. It is a **dev-dependency of
`eggreplay-http` only** (`crates/eggreplay-http/Cargo.toml:81-82`), and the
workspace manifest explains why at `Cargo.toml:85-91`: CI's boundary lanes
resolve `--edges normal`, which excludes dev edges, so a Tonic-qualified
server/client can drive qualification "without entering any shipped dependency
graph." The suite's own doc comment makes the same point
(`grpc_integration.rs:13-20`).

Two more properties keep the oracles honest:

- **The harness is qualified before the product is involved.**
  `tonic_oracle_round_trips_before_eggreplay_is_involved` runs the Tonic
  server and client end to end first; without it, a bug in the hand-written
  per-method glue would be indistinguishable from an EggReplay defect, "and the
  blame would land on the wrong crate" (`grpc_integration.rs:27-33`).
- **`curl` tests skip rather than fail when `curl` is absent** from `PATH`
  (`curl_interop.rs` module doc), so a minimal hosted image stays green while
  a qualifying image proves the command-line path. The CI comment at
  `.github/workflows/ci.yml:397` says so explicitly.

Certificates are test-owned and generated in-process by `rcgen`
(`crates/eggreplay-har/tests` aside, it is a dev-dependency of both
`eggreplay-http` and `eggreplay-intercept`), and no test installs trust into
an OS or browser store — `m013e_operator.rs::source_contains_no_automatic_trust_mutation`
asserts the source contains no such mutation, and README lists "Automatic
OS/browser trust installation" as unsupported.

---

## CI matrix

`.github/workflows/ci.yml` defines six jobs producing 14 hosted cells, which is
what closure records mean by "all 14 jobs green".

| Job | Matrix / runner | What runs |
|---|---|---|
| `verify` (`:8-31`) | 4 cells: `ubuntu-latest`+stable, `ubuntu-latest`+1.89.0, `macos-latest`+stable, `windows-latest`+stable. MSRV is Linux-only by explicit exclude (`:13-18`) | fmt, check, clippy `-D warnings`, `cargo test --workspace --all-features --locked --no-fail-fast` |
| `python-bindings` (`:33-103`) | 4 cells: ubuntu/3.11/1.89.0, ubuntu/3.14/stable, macos/3.11/stable, windows/3.11/stable | `maturin develop` + `pytest tests -q` under `uv 0.8.22` with `maturin==1.14.1`, `pytest==8.4.2`, `pytest-asyncio==1.2.0`, `pytest-xdist==3.8.0`; the named step *"Ensure semantic crates remain Python-free"* (`:63`); `maturin build --locked`; and on ubuntu/3.11 a wheel + sdist build and the named step *"Check wheel and source archive contents"* (`:82`) |
| `python-abi3-cross-version` (`:105-124`) | ubuntu, single job | *"Build abi3 wheel under CPython 3.11"* → *"Install the same wheel under CPython 3.14"* → *"Smoke the installed wheel under CPython 3.14"*. One artifact, two interpreters |
| `dependency-boundary` (`:126-153`) | ubuntu, stable, Linux-only | nine assertions; see below |
| `protocol-boundary` (`:155-367`) | ubuntu, stable, Linux-only | eight named steps; see below |
| `interception` (`:369-403`) | 3 cells: ubuntu, macos, windows | `cargo check -p eggreplay-cli --no-default-features`, `cargo check -p eggreplay-intercept --all-targets --locked`, a Python-wheel interception-free tree query, then `cargo test -p eggreplay-intercept --all-features --locked --no-fail-fast` and `cargo test -p eggreplay-cli --features intercept --locked`. Pinned as its own job so the interception evidence stays attributable to one job (`:371-374`); every step sets `shell: bash` because Windows runners default to pwsh where `grep` chaining fails the step spuriously (`:387-388`) |

The separate `.github/workflows/python-wheels.yml` is the release wheel
pipeline, not per-PR CI: it triggers on `workflow_dispatch` and on pushes to
`main` touching the workflow, the Python crate, or `tools/python/**`
(`:3-10`). Its `wheel` job builds a release abi3 wheel on five targets
(manylinux-x86_64, manylinux-aarch64, macos-arm64, macos-x86_64,
windows-x86_64) and, per target, runs *"Verify native runner architecture"*,
*"Build abi3 wheel"*, *"Inspect wheel contents and tags"*,
*"Clean install, replay, regression, and pytest plugin smoke"*, and *"Record
artifact hash"* before uploading. `source-distribution` builds a wheel *from*
the sdist and inspects it. `abi3-interpreter-smoke` then takes the
manylinux-x86_64 artifact and smokes that same wheel on CPython 3.11, 3.12,
3.13 and 3.14. `plans/003:69-74` is the rule this implements: "Cross-compiled
artifacts without runtime execution are build evidence only."

**C006 — the Windows lane that had to be earned.** The `verify` Windows cell
is not decoration. `plans/closure/c006-windows-hosted-ci-qualification.md`
records the qualifying implementation `9b9cc95` (run `35774531684`) and a
superseded attempt-1 run `35769424825` on `b59dca1` that passed Linux, macOS
and `dependency-boundary` while **failing** the Windows test step with six CLI
contract tests at `SessionWriter::finish` returning `PermissionDenied` code 5.
The cause was real: Windows denies renaming a directory with open files, so the
flow-log and manifest handles had to close before the staging rename. C006
stayed open until the fix landed. The record also publishes the count
asymmetry honestly — 55 tests on Windows versus 56 elsewhere, the single delta
being the `#[cfg(unix)]` symlink-construction test, with the portable
`symlink_metadata` rejection path unchanged (`c006:84-93`).

---

## Boundary and packaging lanes

Three lanes assert architecture rather than behaviour. For the lane-by-lane
walk see [`01-workspace-and-boundaries.md`](01-workspace-and-boundaries.md)
("CI enforcement lanes"); summarised here:

| Lane | Assertions |
|---|---|
| `python-bindings` | `eggreplay-core`, `-store`, `-http` pull no `pyo3`/`pyo3-async-runtimes`; no Rust product crate depends on `eggreplay-python` (`--invert`, `--edges normal`); wheel and sdist contain no `.venv`, `.pytest_cache`, `__pycache__`, `fixtures`, `target`, `.pyc`, `.pyo` (`:63-103`) |
| `dependency-boundary` | `core` and `store` are free of `eggfetch*|eggserve*|eggress*|tokio|hyper|tungstenite|base64`; the `direct` profile builds without Eggress; `websocket` is opt-in; `direct` has no WebSocket codec; the `eggress` profile *does* contain `eggress-outbound` and the inverse tree has no `ssh|quic|extended`; **interception absence** — no ordinary crate may acquire `eggreplay-intercept` or `rcgen` in normal edges; the optional crate still builds alone (`:134-153`) |
| `protocol-boundary` | eight named steps: ordinary profiles exclude the EggServe Core/Static/H3 closure (9 profiles); the opt-in `h2-inbound`/`h2-inbound-tls` features do pull it, on both `eggreplay-http` and the forwarding CLI features, and are never defaults; **QUIC/H3 absence** across seven `eggreplay-http` graphs; **interception never adopts multiprotocol serving** (and its `Cargo.toml` must not mention `h2-inbound`); the Python wheel stays multiprotocol-free; every feature profile compiles with `--all-targets`; **the gRPC oracle never enters a product graph** (9 `check_no_tonic` profiles, and `tonic` must be present under `--edges dev`); `inbound.rs` is `#[cfg(feature = "h2-inbound")]`-gated and `prost-reflect` sits behind `grpc` (`:169-367`) |

The `--all-targets` in the "every feature profile compiles" step is not
decoration either. M015E found that the repository-standard `--all-features`
gate **structurally could not see** a real regression: M015B had left ungated
references to optional `eggserve-*` crates in `inbound.rs` and
`replay.rs`, so the `direct`-only graph broke while the all-features graph
stayed green, and two `#[cfg(test)] mod tests` blocks did too. The
`dependency-boundary` lane caught it, and M015A's local pass had been "true and
irrelevant: the broken step lives in a different job"
(`plans/closure/m015e-…:53-58`). The generalisation M015E draws: "an all-features
gate is the wrong instrument for a boundary claim."

---

## The qualification and closure model

A capability passes four stages, and the registry (`plans/registry.md:229-244`)
defines what each transition requires.

| Stage | Meaning |
|---|---|
| **implemented** | Code exists; closure evidence is incomplete. Source presence is *not* support. |
| **locally qualified** | The `AGENTS.md` gate passes, with a recorded per-suite count. |
| **hosted-qualified** | A green hosted run exists on a named SHA; every required job, not just the relevant one. |
| **closed** | Implementation + required tests/evidence + documentation updates + a closure record, all present. |

Registry rules, verbatim in substance (`plans/registry.md:229-232,241-244`):

- A plan moves **blocked → ready** only when every dependency is closed, or the
  plan explicitly permits an implemented-but-not-closed dependency.
- A plan moves to **closed** only after implementation, required tests and
  evidence, documentation updates, and a closure record are present.
- For decomposed milestones, an umbrella never authorizes skipping subplan
  dependencies; a parent closes only when all required child tracks have
  explicit closure/support decisions.
- Hosted-CI-gated plans stay **open** until the required remote evidence is
  green.
- Historical closure records remain immutable audit artifacts.

The status vocabulary is in `plans/README.md:31-38` (`ready`, `blocked`,
`active`, `implemented`, `closed`, `deferred`). The discipline is in
`plans/003:61-65`: "A decomposed milestone closes only after its subplans have
closure records and the final gate records one qualifying implementation SHA,
hosted platform/MSRV results, exact dependency versions, resource/security
evidence, and an explicit support/limitation matrix."

**Why records are immutable.** `plans/closure/README.md:3-7`: "Closure
documents are evidence records, not implementation plans… Do not rewrite
historical closure records to make later state appear contemporaneous. Add a
corrective closure record when needed." That is why M015E carries a supersession
annotation from M016, why M016's record annotates M015E's, and why M017 exists
as a new record rather than an edit to M015D. It is also why the registry keeps
provenance: the M017 record names three prior runs on the branch (`:217-219`)
rather than only the qualifying one.

---

## What a closure record contains

`plans/closure/m017-unterminated-bidi-grpc.md` is the worked example, and it is
worth reading in full because it shows what rigor looks like here.

**Header and provenance.** `Status: closed (qualifying hosted run 37238704257
on 0c48a26, all 14 jobs green)`, plus a link to the milestone's research note
(`:1-5`).

**A scope table** naming every file touched and whether the change was new, fix,
or rewrite (`:9-14`).

**A correction to the prior milestone's stated blocker.** M015D deferred
un-terminated bidirectional gRPC because closing it "needed either a gateway
that forwards request DATA while response DATA is still arriving… both are new
canonical semantics." The record shows the premise did not hold: hyper's
`ResponseFuture` resolves on response *headers* while the connection task pumps
the request body, so the gateway was already full-duplex (`:16-37`, citing
`hyper-util-0.1.21/src/client/legacy/client.rs:754`). The real defect was one
layer down — three sites hardcoded `category: "other".into()` and discarded the
error, so a deadline cut-off, a reset, and a protocol violation recorded
identically (`:44-68`).

**Exact local results, with a per-suite table** (`:141-181`): the four gate
commands, then `483 passed / 2 failed` and a 26-row breakdown. The arithmetic is
shown: "483 = M016's 479 + 4 new tests." The gRPC suite "stays at 16: one
deferral test was rewritten rather than added."

**The failures are named, not waived** (`:183-185`): the two failures are
`curl_interop.rs` — "the same pre-existing, machine-specific pair carried
through from M015. They reproduce at the Stage 11 commit with all M016 changes
stashed, neither file is in this milestone's diff, and they are green on all
four hosted `verify` jobs. They are recorded, not waived as flakiness." That
sentence is the whole policy in three clauses: reproduce them, prove they
predate the change, prove the platform says otherwise.

**A full 14-row hosted job table** (`:192-208`) — all fourteen named, not "CI
green" — with a paragraph on why `protocol-boundary` mattered more than usual
this time (`:209-215`), and a provenance list of superseded runs.

**A disproved prediction, recorded rather than smoothed over** (`:79-119`).
This is the most valuable part. The milestone's own research predicted replay
would serve a clean 200 with no `grpc-status`, so tonic would report
`Code::Unknown`. "**That prediction was wrong, and observing it is the most
useful thing this milestone produced.**" The observed asymmetry:

| | Live | Replay |
|---|---|---|
| Client sees | 200, partial body, no trailers, clean end | partial body, then a broken stream |
| tonic reports | `Code::Unknown` | `Code::Internal` |

The record then reports two *harness* corrections found on the way (the gateway
helper set no `total` deadline, so the call was not ended by the deadline at
all and hyper tore the stream down with `RST_STREAM(INTERNAL_ERROR)`; and the
recorded request came from a raw H2 peer that sent no `user-agent`, so the
strict profile 404'd until the test used `practical` — "the correct tool for the
difference, not a loosening of the claim"). And the conclusion:

> **Both are failures, and neither is a false success.** That is the property
> this milestone pins, and it is what makes the support-matrix row defensible.

It also carries a counting note worth internalising (`:205-208` in the M016
record): `cargo test` halts at the first failing binary by default, truncating
the run at 371, which is why `--no-fail-fast` is required to see the true total.

**Non-goals held**, with reasons (`:125-139`) — notably "No synthesized
`grpc-status`", because writing `DEADLINE_EXCEEDED` into the trailers "would
fabricate an outcome the upstream never sent, hiding the very signal that makes
the fixture truthful."

**An evidence-quality section** (`:221-233`) that sorts every claim three ways:
*verified by running* (the recorded category, the stream events, the
client-visible outcome), *verified by reading source* (the hyper full-duplex
finding, the tonic status mapping, from the local cargo registry), and *not
verified* — whether a non-Tonic gRPC client renders the truncation the same way,
and whether replay's asymmetry is the behaviour a maintainer wants to keep. The
record says M017 does not open that question; it records it.

---

## Non-goals and negative claims

The project asserts what it does **not** support as carefully as what it does.
`docs/non-goals.md` is the v0.1 statement: TLS interception, MITM certificate
management, WSS and WebSocket extensions, authored scenarios,
record-on-miss/pass-through, streaming timing profiles, Python bindings, HAR
interchange, and broad H2/H3 qualification were all outside v0.1; "opaque binary
payloads cannot be semantically redacted; fixtures remain sensitive"; support
claims are limited to the tested HTTP/1.1 direct path and the optional Eggress
TCP dialer. Several of those have since moved to supported — which is the point:
the document is versioned to a milestone, not aspirational.

The README carries three support matrices with a tier column and an opt-in
feature column: M013 interception (`README.md:60-75`), HTTP/2 / HTTP/3
(`:78-91`), and the gRPC notes. The convention is stated plainly at `README.md:93-95`:

> "Experimental" means qualified against independent peers on local loopback and
> opt-in behind a feature boundary — not "unverified". No HTTP/2 capability is a
> default in any profile.

"Experimental" is therefore a *tested* tier. It is not a hedge. What it does
assert: independent peers, loopback, opt-in, non-default.

Three rules keep negative claims honest:

- **A row may not expand without corresponding tests.** `plans/003:57`: "Source
  presence alone never establishes support." The M017 support-row move from
  *deferred* to *supported (experimental)* is `plans/registry.md:207-212`, and
  it is justified by named tests, not by an implementation existing.
- **A negative claim is tested as an assertion, not just a doc line.** QUIC/H3
  absence is a `cargo tree` check over seven graphs; the interception-absence
  lane loops over five crates; `mitm_does_not_negotiate_h2` and
  `mitm_rejects_websocket_upgrade_as_unsupported` are tests in
  `mitm.rs`; `http2_policy_is_refused_without_the_feature` is a test. The
  `protocol-boundary` step that greps `crates/eggreplay-intercept/Cargo.toml`
  for `h2-inbound` is the mechanical form of "interception never adopts
  multiprotocol serving."
- **Deferred is a recorded decision, not a silence.** HTTP/3 is deferred per
  ADR 0009 with documented missing seams (`plans/004:114-116`), and the README
  matrix says "deferred (ADR 0009)" in the tier column so a reader can tell
  the difference between "not attempted" and "attempted and declined".

---

## Review checklist

Evidence-quality questions, in the order to ask them.

1. **Is the oracle independent?** Would this test still pass, or fail, the same
   way if the *product* were the thing being changed on both sides? A test
   where EggReplay's client is also the test's client proves self-consistency,
   not support. Check that the suite uses Tonic, Hyper, raw `h2`, or `curl` —
   and that the harness itself is qualified first where the glue is
   hand-written.
2. **Is the oracle actually absent from the product graph?** For any new test
   dependency: does CI prove it stays out? A dev-dependency is a boundary claim
   and needs a `--edges normal` / `--edges dev` pair, as Tonic has.
3. **Is the evidence reproducible from a named SHA and run ID?** A closure
   record that says "CI green" without the run, the SHA, and the full job list
   is not auditable. Require all 14 names, not a summary.
4. **Are known failures recorded rather than suppressed?** Is the exact test
   name present, does it predate the change, and does hosted evidence prove it
   platform-specific? "Recorded, not waived as flakiness" is the standard; a
   `#[ignore]` or a loosened assertion fails this.
5. **Are the counts honest?** Does the record account for every suite, and does
   it use `--no-fail-fast` numbers? A total that was obtained from a run that
   halted at the first failing binary is wrong even when the individual numbers
   look plausible.
6. **Is the evidence quality graded?** Every substantive claim should be
   *verified by running*, *verified by reading source*, or *not verified* —
   and the third bucket should be non-empty on a research-heavy milestone.
7. **Are disconfirming observations preserved?** A prediction that the test
   disproved is the highest-value content in the record. Check that the record
   reports the observed behaviour, not the predicted one, and that the
   conclusion follows from the observation.
8. **Do the non-goals hold, and were they tested?** An unsupported capability
   should fail closed and have a test proving it. Look for the *positive* test
   (`mitm_does_not_negotiate_h2`) and the *negative* test
   (`http2_policy_is_refused_without_the_feature`); both are needed.
9. **Does a support-matrix row match the tests that exist today?** Re-derive the
   row from the test names, not from the previous record. Rows drift upward
   silently — the gRPC un-terminated-bidi row is the recent example, and
   `docs/testing.md` is already behind the manifest on dependency pins.
10. **Does the claim need a boundary lane that does not exist yet?** A new
    capability without a `cargo tree` assertion will regress silently; M015B
    proved that an all-features gate cannot see a feature-boundary break.
