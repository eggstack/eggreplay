# M019 — Comparison Authority Closure

Status: **implemented** — local gate green; hosted CI outstanding. This
milestone is **not** closed until a hosted run is green and recorded here.

Plan: `plans/implementation/corrective/m019-comparison-authority.md`

## The defect this milestone found

`eggreplay-core::report` exposed a comparison surface that was partly
unreachable from any product. Three of the plan's five items were the same
disease — public API that reads as supported and is not:

| Item | Symptom |
|---|---|
| Track A | `date` is volatile for *matching* but not for *comparison*, so every run pair produced a `Date` finding whose only content was when the runs happened |
| Track B | `TimingAssertion` was honoured inside the authority, but every caller passed `timing: None` and no surface could set one |
| Track C | `compare_stream_events` iterated both directions; both product callers emptied the request side before calling it |

Track C's exclusion was *consistent* — two empty vectors compare equal, so no
false finding — but it was silent, and `DiffKind::StreamEvent` with field
`stream.request.events` was unreachable while the loop arm still read as if it
worked.

## Decisions recorded

### Track C — request direction is out of scope, and the arm was removed

The plan required an explicit written decision. Investigation found the
question was answerable from the code, and the answer was decisive:

1. **Recorded request events are transport-frame boundaries, not semantics.**
   `TeeSessionStream` polls one hyper `Frame<Bytes>` per HTTP/2 DATA frame or
   HTTP/1.1 chunk (`crates/eggreplay-http/src/recording.rs:2160`), so the event
   list describes how the *original client* framed its upload.
2. **The candidate side has no equivalent observation.** Replay synthesizes the
   outbound request as a single `Full<Bytes>` body
   (`crates/eggreplay-http/src/replay.rs:1591`), and `execute_candidate` sends
   the fixture's own recorded request. Its framing is a function of body length
   alone.
3. **Therefore comparing them is a systematic false positive.** A 100-byte body
   recorded as 3 DATA frames and replayed as 1 would produce a finding that says
   nothing about the candidate server — and it would fire on every streamed
   request regardless of behaviour.

So both plan questions resolve the same way: shape-only comparison is not a
weaker version of the real thing, it is a comparison of the wrong quantity, and
"reproduce the fixture's request body" was already happening — tautologically.
The plan's fallback condition ("if the answer is *not now*, the loop arm must be
removed") was therefore taken.

The `request` arm was deleted rather than left in place. Both product callers
previously *constructed* empty request vectors to reach the same result
(`main.rs:1677-1688`, `lifecycle.rs` cleared both sides); that silent narrowing
is gone, because the function itself no longer has a request arm to reach. This
is the difference between a limitation that is documented and one that is
invisible.

`FlowStreamEvents::request` is still recorded and preserved in the fixture. It
is simply not compared.

### Track D — replay reproduces the recorded upstream truncation

Decided against reproducing the live downstream `Unknown`. Recorded in
`docs/grpc-and-faults.md` under "Un-terminated bidi replay semantics". The
decisive reason: a clean downstream termination would produce a client
experience matching the live run while erasing the only durable evidence of the
deadline cut-off (the terminal `Error` stream event). Error-code parity is a
convenience — both codes are non-success, and no gRPC client branches on the
specific value to decide whether the call worked. The live `Unknown` is also a
race artifact, not a recorded property; encoding it would pin a race into a
deterministic replay.

The asymmetry is documented, not smoothed over. The M017 safety property is
unchanged: the call never replays as success.

## What changed

| File | Change |
|---|---|
| `crates/eggreplay-core/src/report.rs` | `ComparisonPolicy::volatile_headers` (manual `Default`, seeded `date`); `SuppressedHeader` + `SuppressionReason`; `RegressionReport::suppressed`; `compare_stream_events` reduced to the response direction with the rationale as its doc contract; `compare_headers` takes the policy and emits suppressions; 5 tests |
| `crates/eggreplay-core/src/lib.rs` | `REPORT_SCHEMA_VERSION` 2 → 3 |
| `crates/eggreplay-cli/src/main.rs` | `--max-elapsed-ms` on `ComparisonArgs`; `timing_assertion()`; both `compare_flows_with_policy` call sites now call `compare_flows_with_timing_and_policy` |
| `crates/eggreplay-python/src/lifecycle.rs` | `regress_flow(max_elapsed_ms=...)`; delegates to `compare_flows_with_timing_and_policy` |
| `crates/eggreplay-python/src/config.rs` | `volatile_headers` getter and `to_dict` key |
| `crates/eggreplay-python/python/eggreplay/__init__.pyi` | stub for both |
| `crates/eggreplay-http/tests/h2_end_to_end.rs` | published-contract test now pins `suppressed` and `schema_version: 3` |
| `docs/testing.md` | `curl_interop` note made reproducible (Track E) |
| `docs/grpc-and-faults.md` | Track D decision recorded |
| `architecture/06-regression-and-reporting.md`, `architecture/02-core-semantic-model.md`, `.skills/fixture-and-store.md` | schema 3, response-only stream scope, timing/suppression invariants |

## The schema bump the plan had ruled out

The plan's non-goals said "no new report schema version for Tracks A or B".
**That non-goal was wrong and was not honoured.** `architecture/06` states
that any change to `DiffKind` or to the `RegressionReport` shape is a
`REPORT_SCHEMA_VERSION` bump with a consumer-compatibility story. Track A's
own non-negotiable — a suppression that is "a distinct, machine-readable
disposition", never an absent finding — cannot be satisfied without a new field
or variant, and either one triggers the rule.

The bump is correct here because the alternative is worse: a suppression
smuggled in without a version bump would let a schema-2 consumer silently
misread a report. `REPORT_SCHEMA_VERSION` is now 3.

**Compatibility story:** `suppressed` is `#[serde(default)]`, so a version-2
report still deserializes and `ReportView::from_json` keeps working; a consumer
checks the counter before reading it. `findings` and `is_success()` are
unchanged, so the JUnit and human projections — which read `findings` only —
are unaffected. The exact key set is pinned by
`regression_report_contracts_are_stable`.

## Scope discipline

- **The matcher's ignore list was not touched.** `DEFAULT_VOLATILE_HEADERS` is
  a separate constant from the matcher's `["date", "user-agent",
  "x-request-id"]`. Matching decides whether a request *is* the recorded
  request; comparison reports on flows that already matched. Coupling them would
  let a change to one silently change the other.
- **No `volatile_headers` mutator was added.** A public setter with no product
  caller is the exact dead-capability disease this milestone exists to remove.
  The field is read on every comparison; it is seeded, narrow, and read-only.
  Making it operator-settable is a separate decision with its own risk of
  over-suppression.
- **No timing threshold was loosened.** `replay::tests::immediate_reproduces_terminal_error_without_delay`
  (`elapsed < 30ms`, load-sensitive) is unchanged and still recorded as a
  flake.
- **`is_volatile` suppresses only when the header is present on both sides.** A
  vanished `Date` is a structural difference and still reports — the suppression
  is about a *value* difference being a clock artifact, not about presence.

## Local gate

- `cargo fmt --all -- --check` — clean
- `cargo check --workspace --all-targets --all-features --locked` — clean
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` — clean
- `cargo test --workspace --all-features --locked --no-fail-fast` — **495 passed,
  2 failed**. Both failures are the pre-existing machine-specific `curl_interop`
  pair documented below; every other suite is green.
- Python: `maturin develop` then `python -m pytest tests` — **38 passed**
- Feature-boundary profiles compiled **individually on both sides** of each
  boundary, per the M015E lesson that `--all-features` structurally cannot
  validate an opt-in:
  - `eggreplay-http`: `--no-default-features`, default (`direct`), `eggserve`,
    `h2`, `h2-inbound` — all compile.
  - `eggreplay-cli`: default, `intercept`, `h2`, `h2-inbound`,
    `h2-inbound-tls` — all compile.
  - `eggreplay-http --no-default-features` emits 6 warnings. Count is **identical
    at `c32aa2b` with these changes stashed**, so they are pre-existing
    feature-gated dead-code warnings in `recording.rs`/`replay.rs` and not
    introduced here.

## Known pre-existing failure, re-verified not a regression (Track E)

`crates/eggreplay-intercept/tests/curl_interop.rs` fails both of its tests on
this machine. Re-verified at `c32aa6b` with the M019 changes stashed —
identical failures — so neither is caused by this milestone:

- `curl_plain_http_proxies_and_records` — `flow_count()` is 0, expected 1;
  deterministic across three consecutive runs.
- `curl_https_connect_mitm_records` — `curl: (60) SSL certificate problem: self
  signed certificate`; the stock macOS `curl` 8.7.1 (SecureTransport, no
  `--proxy-cacert`) will not accept the minted test CA through `--cacert`.

The `docs/testing.md` note stays. It was made reproducible rather than deleted:
a local pass would not have qualified a hosted claim, and a local failure is
not grounds to remove an accurate note.