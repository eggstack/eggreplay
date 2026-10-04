# M015E — H2 Hardening, Hosted Qualification, and Closure

Status: closed (qualifying hosted run 37230175365 on 9581748, all 14 jobs green)

## Qualifying revision and scope

| File | Change |
|---|---|
| `crates/eggreplay-http/tests/h2_hardening.rs` | new: 15 hardening tests |
| `.github/workflows/ci.yml` | 3 new `protocol-boundary` steps: every feature profile compiles, the gRPC oracle stays out of product graphs, inbound H2 and the gRPC view stay feature-gated |
| `crates/eggreplay-http/src/lib.rs` | **fix**: `inbound` is now `eggserve`-gated (M015B regression) |
| `crates/eggreplay-http/src/replay.rs` | **fix**: `render_recorded_headers` is now `eggserve`-gated; `mod tests` gated |
| `crates/eggreplay-http/src/recording.rs` | **fix**: `mod tests` gated |
| `README.md`, `docs/http2-support.md`, `docs/cli.md`, `docs/grpc-and-faults.md`, `plans/001`, `plans/002`, `plans/003`, `plans/004`, `plans/README.md` | documentation and support reconciliation |

## A real regression, found by hosted CI and not by the local gate

The first hosted run of this branch — the branch's *first* CI run, since
M015A–M015D had been committed but never pushed — failed the
`dependency-boundary` job on one step:

```text
cargo check -p eggreplay-http --no-default-features --features direct
```

`eggreplay-http` with only the `direct` feature did not compile. The `direct`
feature pulls no EggServe at all (`direct = []`), but M015B had introduced two
references to optional `eggserve-*` crates without gates:

- `crates/eggreplay-http/src/inbound.rs` — `use eggserve_server::Service` at
  module scope, and `InboundServerHandle::Http1(eggserve_server::ServerHandle)`;
- `crates/eggreplay-http/src/replay.rs` — `render_recorded_headers` taking
  `eggserve_primitives::HttpVersion`.

Both were published ungated from `lib.rs`.

**Why the local gate missed it.** The repository-standard command is
`--all-features`. An optional dependency is present in the all-features graph,
so an ungated reference to it compiles. The `direct`-only graph is the one that
exposes it, and only the `dependency-boundary` lane builds that graph — a lane
that had never run on this branch. Verified by bisection across the branch:

| Revision | `direct` profile |
|---|---|
| `main` | compiles |
| `240672a` (M015A) | compiles |
| `1901bce` (M015B) | **broken** |
| `1d319b5` (M015C) | broken |
| `b1ef2d2` (M015D) | broken |
| `874d6de` (M015E) | broken |

The break is M015B's, and M015C and M015D inherited it. M015A's closure
reported the `protocol-boundary` steps as passing locally, which was true and
irrelevant: the broken step lives in a different job.

The fix gates `inbound` and `render_recorded_headers` on `eggserve`, which is
the feature that carries `eggserve-server`. The `h2-inbound` and
`h2-inbound-tls` features are built on `eggserve`, so the inbound surface is
unchanged wherever it actually exists, and the CLI — which always enables
`eggserve` through its dependency declaration — is unaffected.

### The same bug, in the test modules, and the check that now catches it

`cargo check --all-targets` on the `direct` profile also failed, on two
`#[cfg(test)] mod tests` blocks that exercise the serving path unguarded. That
break predates Stage 11 (it reproduces at `240672a`) and no lane built those
targets either, so it had been invisible. Both modules are now
`#[cfg(all(test, feature = "eggserve"))]`, and all twelve profiles compile with
`--all-targets`.

A new `protocol-boundary` step now checks every profile of `eggreplay-http`
with `--all-targets` — no features, `direct`, `eggress`, `websocket`, `h2`,
`grpc`, `eggserve`, `h2-inbound`, `h2-inbound-tls`, and all M015 features
together — plus the CLI's default and `h2-inbound-tls` builds. The step's
comment says why `--all-targets` is the point: an optional feature is only a
boundary if the profiles on *both* sides of it still build.

This is the most valuable thing in the milestone. The finding is not "a CI step
was red" but that **the milestone's central claim — a feature boundary — had a
hole in it for four commits, and the repository-standard gate structurally
could not see it.** An all-features gate is the wrong instrument for a
boundary claim.

## Hardening matrix — 15 tests

Every test asserts one of three outcomes, and then also checks the listener
still works afterwards. A test that observed only "it did not hang" would pass
for a listener that silently drops everything, so the liveness check is part of
each case, not an afterthought.

| Plan item | Test |
|---|---|
| header limits | `an_oversized_header_list_is_refused_and_the_listener_survives` |
| body limits | `oversized_request_body_is_refused_without_taking_down_the_listener` |
| count limits | `configured_concurrent_stream_ceiling_is_advertised_and_honoured` |
| concurrent stream ceilings | the same test |
| slow/stalled request bodies | `a_stalled_request_body_does_not_block_its_siblings` |
| slow/stalled response bodies | `a_stalled_upstream_response_is_bounded_by_a_total_deadline` |
| reset/cancellation races | `a_reset_storm_leaves_the_connection_usable`, `a_reset_during_an_in_flight_response_publishes_whole_flows_only` |
| GOAWAY and shutdown under active streams | `shutdown_under_active_streams_completes_and_stops_accepting` |
| trailer count/size | `a_large_trailer_block_is_bounded_and_never_truncated_mid_block` |
| illegal H2 headers | `an_illegal_header_block_is_refused_at_the_protocol_level` |
| malformed gRPC envelopes/descriptor sets | qualified in M015D (`descriptor_and_envelope_failures_do_not_corrupt_the_fixture`); the H2-side equivalent is this suite's illegal-header and oversize-body cases |
| configured Eggress route failure, no direct fallback | `a_failed_eggress_route_fails_closed_with_no_direct_fallback` |
| TLS verification failures | `a_peer_that_cannot_verify_the_server_identity_is_refused` |
| ALPN | `a_peer_that_offers_no_known_alpn_is_not_served` |
| session finalization under concurrent H2 streams | `session_finalization_under_concurrent_streams_publishes_no_partial_flow` |
| (found while writing) scheme/transport mismatch | `a_tls_listener_refuses_a_peer_that_claims_cleartext` |

### Findings

Seven findings. Each is a property of the product or the adopted runtime, and
each is now asserted as it actually behaves rather than as it ideally would.

1. **`H2Limits::max_header_list_size` is enforced inbound but not advertised.**
   With the operator asking for 1024, the server's SETTINGS frame still carries
   EggServe's own 16384. A client therefore sizes its header block by a number
   the operator did not choose. The enforced bound is what protects the
   listener, and an oversized header list is refused at the stream. Recorded
   rather than papered over: rewriting the advertised value would be a
   product-side claim about a runtime the product does not own.

2. **An oversized request body surfaces as 500, not 413.** Bounded, safe, and
   the call ends — which is what matters — but not the most informative status.

3. **An incomplete request still consumes a single-use candidate.** A stream
   that sends headers and then nothing is dispatched far enough to take its
   match out of the pool, so a later request for the same recorded path finds
   nothing left and is refused. The failure is stream-local, not a connection
   failure: siblings with their own candidates are unaffected. This is why
   several tests give their liveness check a separate recorded flow; without
   that, two assertions would have been testing candidate consumption rather
   than hardening.

4. **`eggfetch_core::Timeout::from_secs` does not bound a stall that happens
   before the response starts.** It sets `pool`, `connect`, `write`, and
   `read`; the `read` budget is "time between response body chunks", so it only
   begins once the response has. An upstream that accepts a request and then
   sends nothing at all is unbounded by `from_secs` — which is exactly what a
   hung origin looks like. A `total` cap is what bounds it, and the test uses
   one. This is the most operationally significant finding here: an operator
   who reached for `from_secs` to bound a flaky origin would not have bounded
   it.

5. **A dead Eggress route fails closed but is categorised `Other`.** The
   routing decision is honoured and the call fails, and the failed route is
   recorded in the flow — so the fail-closed property holds and the failure is
   attributable from the session's route metadata. The category itself is not
   diagnostic.

6. **A TLS peer that claims `:scheme: http` is refused with 400 before the
   matcher runs.** That is the right place to catch it — a scheme mismatch
   reaching the matcher would mean a candidate matched against a request it was
   not recorded from — but it is worth pinning rather than leaving as folklore.

7. **An oversized-header request and a stalled request both consume the
   candidate they matched.** This was found while writing the tests: two
   liveness assertions failed for a reason that had nothing to do with
   hardening. It is finding 3 from the other direction, and it is the reason
   those tests now use multi-flow fixtures.

### Determinism

M015E's rule is that no deterministic failure may be waived as protocol
flakiness. The reverse also matters: a *flaky* failure is a different thing
and must not be quietly re-run until it passes. The hardening suite is
deterministic by construction — every wait is a short bounded timeout, every
assertion is on an observable outcome rather than on timing, and no test reads
the clock, the calendar, or a second-granular header. `h2_hardening` runs in
about five seconds on local loopback.

## Pre-existing flake, recorded rather than re-run

`recording::tests::recording_gateway_captures_upgrade_and_leading_post_101_messages`
fails roughly one run in six under load with `Invalid("WebSocket 101 flow is
missing required conversation metadata")`. It passes 8/8 in isolation.

It is **not** a Stage 11 regression and **not** HTTP/2: it is a WebSocket unit
test, and it reproduces at the M015C commit with all M015D changes stashed
(2 failures in 8 runs of the full `--lib` suite). It is recorded here because
M015E's rule cuts both ways, and because anyone running the full suite under
load will see it. M015E does not fix it; fixing an unrelated WebSocket test is
a separate change with its own evidence.

## Repository-standard gate

```text
cargo fmt --all -- --check
cargo check  --workspace --all-targets --all-features --locked
cargo clippy  --workspace --all-targets --all-features --locked -- -D warnings
cargo test   --workspace --all-features --locked --no-fail-fast
```

**477 passed, 2 failed**, across 25 suites that carry tests (462 + 15 in `h2_hardening`). The two
failures are the same pre-existing, environment-specific
`eggreplay-intercept/tests/curl_interop.rs` cases carried since the M015A
closure: `curl_plain_http_proxies_and_records` and
`curl_https_connect_mitm_records`. They are a local curl/CA-trust artifact.

## Hosted evidence

**Qualifying run:** [`37230175365`](https://github.com/eggstack/eggreplay/actions/runs/37230175365)
on SHA `9581748` (branch `stage11-m015-bidirectional-h2`) — **all 14 jobs
success**.

| Job | Result |
|---|---|
| `verify (ubuntu-latest, stable)` | success |
| `verify (ubuntu-latest, 1.89.0)` — MSRV | success |
| `verify (macos-latest, stable)` | success |
| `verify (windows-latest, stable)` | success |
| `dependency-boundary` | success |
| `protocol-boundary` (10 steps) | success |
| `interception (ubuntu / macos / windows)` | success ×3 |
| `python-bindings` (ubuntu 3.11, ubuntu 3.14, ubuntu 3.11@MSRV, macOS 3.11, Windows 3.11) | success ×5 |
| `python-abi3-cross-version` | success |

The preceding run, [`37229585308`](https://github.com/eggstack/egreplay/actions/runs/37229585308)
on `874d6de`, failed exactly one step — `dependency-boundary`'s
`cargo check -p eggreplay-http --no-default-features --features direct` — and
is retained here because it is the evidence for the regression described
above. Every other job was already green on that run, which is how the two
local `curl_interop` failures came to be confirmed machine-specific: all three
of that run's `verify` jobs execute
`cargo test --workspace --all-features --locked --no-fail-fast`, and all three
passed.

Wheel qualification needed no separate lane: M015D added dev-dependencies to
`eggreplay-http` only, the Python crate's features and dependencies are
untouched, and `python-bindings` plus `python-abi3-cross-version` are green.

## Documentation and support reconciliation

Every claim below was checked against the code rather than carried over from
the previous text. One earlier survey of `plans/003` claimed it contained an
M015 tier table; it did not, and the doc gained the Stage 11 lane description
instead.

| File | Change |
|---|---|
| `README.md` | support matrix with tiers and opt-in features; inbound opt-in surface; the gRPC tier; the deferred un-terminated bidi case; corrected "Stage 11 is planned" status |
| `docs/http2-support.md` | rewritten for bidirectional H2: supported/not-supported, invariants, and a **known-limitations** section carrying all seven findings |
| `docs/cli.md` | `--inbound` and `--outbound-version`, including that `auto` means HTTP/1.1 and that a TLS identity is mandatory |
| `docs/grpc-and-faults.md` | streaming-class status table, the un-terminated bidi deferral with its reason, and the response-framing-not-request nuance |
| `plans/001` | why `eggserve-core` is now an optional dependency, and that one service serves two runtimes |
| `plans/002` | Stage 11 closed, with the achieved tier |
| `plans/003` | Stage 11 CI lane description |
| `plans/004` | the refreshed dependency line and the qualified inbound tier |
| `plans/README.md` | Stage 11 executed and closed |

Unsupported and deferred labels are explicitly retained for H2 MITM, H3/QUIC,
WSS, extended-CONNECT WebSockets, a generic reverse proxy, and the un-qualified
gRPC streaming class.

## Acceptance-criteria status

| Criterion | Status | Evidence |
|---|---|---|
| locked gates pass | met | fmt, check, clippy, test all green |
| every feature profile compiles | met | 10 profiles with `--all-targets`, plus a permanent CI step |
| inbound-H2 feature activation is explicit | met | `protocol-boundary` asserts it in both directions |
| QUIC/H3 absent from every supported graph | met | existing step, still green |
| Python wheel behavior unchanged | met | `python-bindings` ×5 and `python-abi3-cross-version` green |
| hardening matrix | met | 15 tests, all plan items covered |
| no deterministic failure waived as flakiness | met | suite is deterministic by construction; the one flake is recorded and attributed to a pre-existing WebSocket test |
| hosted evidence on Linux/MSRV/macOS/Windows | met | `verify` ×4 green on `37230175365` |
| dependency-boundary/topology lane | met | green on `37230175365` after the M015B regression fix |
| interception lane | met | green on all three runners |
| Python lanes | met | green |
| wheel qualification if metadata changed | n/a | nothing affecting the wheel changed |
| exact SHA and workflow IDs pinned | met | run `37230175365` on `9581748`; failing run `37229585308` on `874d6de` retained as evidence |
| documentation reconciliation | met | 10 files, all claims verified |
| unsupported/deferred labels retained | met | see the reconciliation table |
| no new canonical semantics smuggled in | met | product changes are three cfg gates and one CI job; no store, matcher, or schema change |

## Consequences for the umbrella

- The `direct` profile regression is fixed and the class of bug is now caught
  by CI. Any future feature boundary in this repository should ship with a
  profile-compilation step in the same change — that is the lesson, and it is
  the one thing from M015E worth carrying into Stage 12.
- Seven documented limitations belong in the product's own documentation, not
  only in a closure record. They are in `docs/http2-support.md`.
- The two local `curl_interop` failures are now confirmed machine-specific by
  four green hosted `verify` jobs.
