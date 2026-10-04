# M017 — Un-Terminated Bidirectional gRPC: Classified Body Errors, and an Observed Replay Contract

Status: closed (qualifying hosted run 37238704257 on 0c48a26, all 14 jobs green)

Research: `plans/research/m017-bidirectional-grpc-deferral-research.md`

## Scope

| File | Change |
|---|---|
| `crates/eggreplay-core/src/error.rs` | **new**: `ErrorCategory::as_str`, `ErrorPhase::as_str`, pinned against serde |
| `crates/eggreplay-http/src/recording.rs` | **fix**: `classify_body_error` at both response-body error sites; `classify_dial_error` extracted and shared; three new unit tests |
| `crates/eggreplay-http/tests/grpc_integration.rs` | **rewrite**: the deferral test becomes `an_unterminated_bidi_call_records_its_cut_off_and_replays_faithfully`; `start_replay_with_matcher`; the gateway helper applies a real `total` deadline |
| `docs/grpc-and-faults.md`, M015 / M015D / M016 closures, registry, research doc | support-matrix reclassification and supersession notes |

## The deferral's stated blocker did not exist

M015D deferred this class on the ground that closing it needed "either a
gateway that forwards request DATA while response DATA is still arriving, or a
canonical model that records cross-direction ordering on one stream — both are
new canonical semantics."

**The gateway is already full-duplex.** EggFetch hands the streaming request
body to `hyper_util::client::legacy::Client`, whose `ResponseFuture` resolves on
response *headers* while the connection task pumps the request body concurrently
(`hyper-util-0.1.21/src/client/legacy/client.rs:754`). The transport was never
the blocker.

What was true, and narrower: the *recorder* never observes the interleaving.
`recording.rs:189` awaits the response before the frame loop at `:217`, and the
request tee runs under hyper's task. The two `Instant` origins (`:430`,
`:468`) are never compared.

**The cut-off was already recorded.** The outbound deadline surfaces as a
response-body *error*, and the recorder already pushed a terminal
`StreamEventKind::Error` while suppressing `End`. M015D's test only asserted on
trailers, so it never saw this.

## The real defect: body errors were `other` — the M016 defect, one layer down

Three sites hardcoded `category: "other".into()`, and two of them discarded the
error outright:

```rust
Err(error) => {
    push_stream_event(..., StreamEventKind::Error {
        offset: response_offset,
        category: "other".into(),   // hardcoded
        phase: "body".into(),
    })?;
    let _ = error;                  // discarded
    response_failed = true;
    break;
}
```

So a deadline cut-off, a connection reset, and a protocol violation all
recorded identically. This is the same defect M016 fixed for dial errors, where
every `DialErrorKind` collapsed to `(Other, Other)` and made
`ErrorCategory::ConnectionRefused` unreachable in the product. It survived one
layer down because nobody looked there.

`classify_body_error` now maps `eggfetch_core::Error` onto the same vocabulary
`map_fetch_error` uses, with the phase forced to `Body` — except a deadline,
which reports `Timeout` so a cut-off call is distinguishable from a reset. The
`CustomTransport` arm is shared with `map_fetch_error` through
`classify_dial_error`, so a route failure reads the same from either layer
rather than two tables drifting apart.

**The request tee keeps `Other` deliberately.** Its error is an opaque inbound
`eggserve` body error with no EggFetch category to consult. Claiming one would
be a guess, and a wrong guess in a recording is worse than an honest `Other`.
That is now a comment at the site, not an absence of one.

`as_str()` is pinned against serde in a core unit test, so a wire-name drift
between the string form and the serialized form fails the build rather than
producing a stream event the JSON decoder would not read back.

## What the tests actually found — a correction to the research

The research predicted that replay would serve a clean 200 with no
`grpc-status`, and that tonic would therefore report `Code::Unknown`. **That
prediction was wrong, and observing it is the most useful thing this milestone
produced.**

Getting there required fixing the test harness twice, and both fixes are worth
recording:

1. **The gateway helper applied no `total` deadline.** It used
   `Timeout::from_secs`, which sets the per-phase budgets but not `total` — the
   exact M016 finding. With no `total`, the call was not ended by the deadline
   at all: hyper tore the stream down with `RST_STREAM(INTERNAL_ERROR)` and the
   recorded category was `protocol`, not `timeout`. Setting `total` is what
   makes "the outbound timeout ends the call" the real mechanism, and it is
   what this test claims to exercise. The helper now does.

2. **The recorded request came from a raw H2 peer that sent no `user-agent`**,
   so a Tonic client replaying it differed by a header the strict profile treats
   as significant, and the replay 404'd. `start_replay_with_matcher` was added
   so this test can use the `practical` profile, which ignores exactly those
   volatile headers while still requiring an exact body. That is the correct
   tool for the difference, not a loosening of the claim.

With the harness correct, the observed outcome:

| | Live | Replay |
|---|---|---|
| Client sees | 200, partial body, no trailers, clean end | partial body, then a broken stream |
| tonic reports | `Code::Unknown` | `Code::Internal` |

Replay reproduces the recorded terminal `Error` event as a
`TimedStreamStep::Error` (`replay.rs:832-845`), so the body read fails. The
recorded truncation is *upstream* reality, and replay applies it to the
downstream response too.

**Both are failures, and neither is a false success.** That is the property
this milestone pins, and it is what makes the support-matrix row defensible. The
research's core conclusion — that replaying this fixture is safe — held; its
mechanism did not, and the doc, the research note, and the test all say so.

Whether replay *should* reproduce a downstream client experience that differs
from the recorded upstream truncation is a real open question. M017 does not
open it; it records it.

## Non-goals, held

- **No synthesized `grpc-status`.** Writing `DEADLINE_EXCEEDED` into the
  trailers is the smallest available change and it fabricates an outcome the
  upstream never sent, hiding the very signal that makes the fixture truthful.
- **No cross-direction ordering.** `delta_ns` is per-direction with independent
  origins (`stream.rs:68`; origins at `recording.rs:430` and `:468`), and
  `validate()` iterates the two vectors independently (`stream.rs:152`). Making
  it shared is a genuine canonical milestone — schema v2, additive field,
  cross-cutting consumers — but an un-terminated call has no ending to be
  faithful about, so it is not this milestone's job.
- **No change to the recorder's half-duplex observation.** Same schema-v2 work.
- **No change to `grpc.rs`**, which already degrades a missing status to
  `status: None` and is not in the record/replay path.
- No change to `Flow::validate()`, the store validator, or exit codes.

## Local gate

```text
cargo fmt --all -- --check                                     OK
cargo check --workspace --all-targets --all-features --locked  OK
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings  OK
cargo test --workspace --all-features --locked --no-fail-fast  483 passed / 2 failed
```

483 = M016's 479 + 4 new tests (2 in `eggreplay-core`, 2 in `eggreplay-http`).
The gRPC suite stays at 16: one deferral test was rewritten rather than added.

| Suite | Passed | Failed |
|---|---|---|
| `eggreplay-cli` unittests | 12 | 0 |
| `cli_contracts.rs` | 12 | 0 |
| `har_migrate.rs` | 11 | 0 |
| `m013e_operator.rs` | 8 | 0 |
| `m015b_inbound_serving.rs` | 8 | 0 |
| `eggreplay-core` unittests | 39 | 0 |
| `eggreplay-har` unittests | 7 | 0 |
| `eggreplay-http` unittests | 80 | 0 |
| `grpc_integration.rs` | 16 | 0 |
| `h2_end_to_end.rs` | 23 | 0 |
| `h2_hardening.rs` | 15 | 0 |
| `h2_inbound_serving.rs` | 30 | 0 |
| `h2_qualification.rs` | 16 | 0 |
| `scenario_faults.rs` | 7 | 0 |
| `v01_qualification.rs` | 16 | 0 |
| `eggreplay-store` unittests | 64 | 0 |
| `ca_leaf.rs` | 11 | 0 |
| `curl_interop.rs` | 0 | **2** |
| `hardening.rs` | 7 | 0 |
| `m013e_proxy_stats.rs` | 3 | 0 |
| `mitm.rs` | 24 | 0 |
| `proxy_policy.rs` | 23 | 0 |
| `resource_bounds.rs` | 9 | 0 |
| `substrate.rs` | 12 | 0 |
| `tls_shutdown_isolation.rs` | 9 | 0 |
| `eggreplay-intercept` unittests | 21 | 0 |
| **Total** | **483** | **2** |

The 2 `curl_interop` failures carried through from M015 remain the only local
failures; neither file is in this milestone's diff, and all four hosted
`verify` jobs prove they are machine-specific.

## Hosted runs

**Qualifying run:** [`37238704257`](https://github.com/eggstack/eggreplay/actions/runs/37238704257)
on `0c48a26` — **all 14 jobs green**.

| Job | Result |
|---|---|
| `verify (ubuntu-latest, stable)` | success |
| `verify (ubuntu-latest, 1.89.0)` — MSRV | success |
| `verify (macos-latest, stable)` | success |
| `verify (windows-latest, stable)` | success |
| `interception (ubuntu-latest)` | success |
| `interception (macos-latest)` | success |
| `interception (windows-latest)` | success |
| `python-bindings (ubuntu-latest, 3.14, stable)` | success |
| `python-bindings (ubuntu-latest, 3.11, 1.89.0)` | success |
| `python-bindings (macos-latest, 3.11, stable)` | success |
| `python-bindings (windows-latest, 3.11, stable)` | success |
| `python-abi3-cross-version` | success |
| `dependency-boundary` | success |
| `protocol-boundary` | success |

`protocol-boundary` matters more than usual here. M017 adds an
`eggfetch_core::Error` match arm and a core `as_str()` API, and the gRPC
replay test exercises a matcher profile — all changes that could plausibly
reach across a feature boundary. That lane compiles every feature profile of
`eggreplay-http` individually; all nine were also checked locally
(`default`, `direct`, `eggserve`, `websocket`, `h2`, `h2-inbound`,
`h2-inbound-tls`, `grpc`, `eggress`).

Prior runs on this branch, for provenance: `37236089130` (research, docs only)
green, `37235015972` on `b5be2d1` green (the M016 qualifying run), and
`37230175365` on `9581748` green (the Stage 11 qualifying run).

## Evidence quality

**Verified by running:** every claim about the recorded category, the stream
events, and the client-visible replay outcome was produced by the tests in this
milestone, including the two harness corrections above.

**Verified by reading source:** the EggFetch/hyper full-duplex finding and the
tonic status mapping, from the local cargo registry.

**Not verified:** whether a non-tonic gRPC client renders the replayed
truncation the same way, and whether replay's asymmetry (downstream termination
applied from an upstream-recorded event) is the behaviour a maintainer wants to
keep. That is the open question M017 records rather than answers.
