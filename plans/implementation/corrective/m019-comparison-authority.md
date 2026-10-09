# M019 — Comparison Authority: Volatile-Header Normalization, Timing Assertions, and Request-Direction Stream Comparison

Status: **implemented** — local gate green (495 passed, 2 failed; both the known
machine-specific `curl_interop` pair, re-verified identical at `c32aa6b` with
these changes stashed). Hosted CI outstanding, so this milestone is **not**
closed. Dependencies satisfied (M018 closed on `17f4022`, run `37644212037`).

Closure record: `plans/closure/m019-comparison-authority.md`.

> **Two of this plan's stated positions did not survive implementation**, and
> both are recorded rather than quietly rewritten:
>
> 1. The non-goal "no new report schema version for Tracks A or B" was
>    **wrong**. Track A's non-negotiable — a suppression that is a distinct,
>    machine-readable disposition, never an absent finding — cannot be
>    satisfied without changing the `RegressionReport` shape, and
>    `architecture/06-regression-and-reporting.md` requires a
>    `REPORT_SCHEMA_VERSION` bump for exactly that. `REPORT_SCHEMA_VERSION` is
>    now **3**; `suppressed` is `#[serde(default)]` so a version-2 report
>    still deserializes.
> 2. Track C's "schema v2 scope" framing below is now historical. The plan
>    called itself the schema-v2 milestone; what that bought in practice was
>    the version bump above, and the request-direction question was resolved by
>    removal rather than by new capability.

## Why this milestone exists

M018 audited the forward queue and deliberately left three items on the table
because they change what a regression report *asserts* — a different subject from
the transport-adapter error attribution that milestone fixed. Two further items
were recorded alongside them. All five are open, none is owned by a plan, and
three of them are defects in the same subsystem: the comparison authority in
`eggreplay-core::report`.

The unifying claim: **`eggreplay-core` exposes a comparison surface that is
partly unreachable from any product.** `compare_stream_events` compares request
and response events; no product ever supplies request events. `TimingAssertion`
is honoured inside the authority; no product ever passes one. Both are dead
capability — public API with no caller, which reads as supported and is not.

The items are grouped because they share one blast radius: each changes the
finding set a regression report produces. Bundling is safe here in a way it was
not in M018, because every item here is a *comparison* change and none is a
transport change.

## Track A — `date` normalization at the comparison level

### The defect

`matching.rs:222` seeds the matcher's ignore list with
`["date", "user-agent", "x-request-id"]`, so **`date` is already treated as
volatile for matching**. The comparison authority has no equivalent:
`compare_headers` (`report.rs:428`) compares every header name in the union of
both sides and emits a `DiffKind::Header` finding on any difference, with no
volatile-name concept at all.

This is an internal inconsistency, not a preference: the same header is
correctly ignored when deciding whether a request matches, and correctly
reported as a difference when comparing the flows that matched. Headers are
copied verbatim onto both sides of the comparison (`replay.rs` builds
`HeaderEntry` lists from the recorded block and the candidate block), so a
`Date` differing between two otherwise identical runs yields a finding whose
only content is the wall-clock time the two runs happened.

### Why it is a defect and not a feature

A regression report's purpose is to surface *semantic* differences. A `Date`
difference is an artifact of when the runs executed. Reporting it trains
operators to ignore the report, and it is worse than noise because it looks like
a real signal: a genuine header regression and a clock difference are
indistinguishable in the output.

The fix is to give the comparison authority the same volatile-header concept the
matcher already has, and to use it. It must be **explicit and narrow**:

- a `ComparisonPolicy` field carrying volatile header names, seeded to
  `["date"]` so the default contract is unchanged;
- a `date`-specific reason for the seed, not a general "ignore all volatile"
  escape hatch — the report must still say a `Date` was present on both sides
  and deliberately not compared, so a reader can tell a suppression from a pass;
- matching's ignore list and comparison's volatile list stay **separate
  settings**. Sharing the constant would couple two decisions that legitimately
  differ, and would make a change to one silently change the other.

### Non-negotiable

A suppressed header is **not** a passing header. It must appear in the report as
a distinct, machine-readable disposition that says "present on both sides, not
compared", never as an absent finding. If it were silently dropped, a real
`Date`-shaped regression would be invisible and the suppression would be
undetectable in review.

## Track B — wire `TimingAssertion` to a product caller

### The defect

`TimingAssertion` (`report.rs:61`) holds `max_elapsed_ms`. It is honoured
inside the authority — `compare_flows_with_timing_and_policy` reads it at
`report.rs:310` and emits a `DiffKind::Timing` finding when elapsed exceeds it.

No product ever passes it. Every caller uses `compare_flows_with_policy`
(`main.rs:1652`, `main.rs:1796`, `lifecycle.rs:511`), and that function
hardcodes `timing: None` when delegating (`report.rs:199`). The CLI and Python
surfaces expose no way to set it either.

The result: `DiffKind::Timing` for flow elapsed time is **unreachable from the
product**, and `compare_flows_with_timing` / `compare_flows_with_timing_and_policy`
are public API with no external caller.

### The work

- Expose the bound on the CLI regression surface and in the Python binding,
  following each surface's existing flag/config conventions.
- Thread it to `compare_flows_with_timing_and_policy` at every call site that
  accepts it, rather than adding a second evaluator.
- Note the interaction with `StreamTimingMode`: the CLI already rejects
  non-immediate timing modes when serving (`main.rs:906`). A timing
  *assertion* is a comparison bound and is independent of playback timing, and
  the two must not be conflated in the flag surface or the docs.

### Pre-existing hazard, not introduced here

`replay::tests::immediate_reproduces_terminal_error_without_delay` asserts
`elapsed < 30ms` and fails under parallel load (observed at 35ms; passes in
isolation with and without M018's changes). That is a wall-clock assertion in a
unit test and is load-sensitive. This milestone **must not** raise a threshold to
make it pass. If a timing threshold anywhere needs adjusting, it needs an
argument about the bound's meaning, not a bigger number.

## Track C — request-direction stream comparison

### The defect, stated precisely

`compare_stream_events` (`report.rs:335`) iterates **both** directions:

```rust
for (direction, left, right) in [
    ("request", &baseline.request, &candidate.request),
    ("response", &baseline.response, &candidate.response),
]
```

Both product callers then **discard the request side before calling it**:

- `main.rs:1677` builds `baseline_response_only` with `request: Vec::new()`,
  and the same for the candidate.
- `lifecycle.rs:520-527` clears the baseline's request events with the comment
  "it does not observe candidate request frame cadence. Compare only the
  response stream instead of fabricating request events", then passes an empty
  candidate request vector.

Because both sides are emptied, the comparison is **consistent** — two empty
vectors compare equal, so no false finding is produced. The requests are not
being mismatched; they are being *silently excluded*. `DiffKind::StreamEvent`
with field `stream.request.events` is therefore unreachable, and the
`"request"` arm of that loop is dead code that reads as if it works.

The Python comment is the reason this is not a pure defect: a candidate
execution **materializes the baseline request body and does not observe its own
request frame cadence**. There is no candidate-side request event stream to
compare against, because the thing being compared would have to be fabricated.
That reasoning is correct and must be preserved.

### Why the item is still open

Two things are unresolved, and neither is answerable by reading code:

1. Whether request-direction comparison should compare only *shape* (event
   count and kind sequence), never cadence — since cadence on one side is
   unobservable.
2. Whether the baseline's recorded request events should be compared against a
   **replayed** request body (the fixture can reproduce what it sends), which
   would make the comparison real rather than excluded.

Option 2 changes what a fixture is asked to guarantee and is the substantive
design question. This milestone must resolve it explicitly, in writing, with a
recorded decision — and if the answer is "not now", the loop arm must be removed
or the exclusion made visible in the report rather than left to read as working.

### Resolved: both questions, the same way

**Answered during implementation** — the questions were answerable from the
code, and the answer was decisive:

1. Recorded request events are inbound *transport-frame* boundaries.
   `TeeSessionStream` polls one hyper `Frame<Bytes>` per HTTP/2 DATA frame or
   HTTP/1.1 chunk (`crates/eggreplay-http/src/recording.rs:2160`), so the list
   describes how the *original client* framed its upload.
2. The candidate side has no equivalent observation. Replay synthesizes the
   outbound request as a single `Full<Bytes>` body
   (`crates/eggreplay-http/src/replay.rs:1591`) and `execute_candidate` sends
   the fixture's own recorded request, so its framing is a function of body
   length alone.
3. So shape-only comparison is not a weaker version of the real thing — it is a
   comparison of the wrong quantity. And option 2 was already happening:
   replay *does* reproduce the fixture's request body, which makes it
   tautological.

Comparing them would compare a foreign client's framing against EggReplay's one
write, firing on every streamed request regardless of candidate behaviour: a
systematic false positive with no actionable cause.

The plan's own fallback condition was therefore taken. The `request` arm was
**deleted**, not left in place. Both product callers previously *constructed*
empty request vectors to reach the same result (`main.rs:1677-1688` and the
`lifecycle.rs` clear); that silent narrowing is gone, because the function no
longer has a request arm to reach. The rationale is the function's doc
contract, and `request_direction_stream_events_are_not_compared` pins it from
both sides.

`FlowStreamEvents::request` is still recorded and preserved in the fixture. It
is simply not compared.

### Schema v2 scope

M017's closure ruled cross-direction ordering out of a corrective as "a genuine
canonical milestone — schema v2, additive field, cross-cutting consumers". This
plan is that milestone, not a corrective, and that is why request-direction
comparison is in scope here and was not in M018.

## Track D — un-terminated bidirectional gRPC replay semantics (decision)

`docs/grpc-and-faults.md` records this as an open question: **should replay
reproduce the downstream client experience of an un-terminated call**, or apply
the recorded upstream truncation?

M017 established that the gateway is already full-duplex, that the cut-off is
already recorded, and that live and replayed clients observe different status
codes (`Internal` live, `Unknown` replayed) — which the closure recorded rather
than smoothed over.

This is a **maintainer decision about correct replay semantics, not a defect.**
The work here is bounded and must stay bounded: present the two options with
their consequences, decide, and record the decision and its rationale. Choosing
either answer is a valid outcome; leaving it undecided is not, and inventing a
third behaviour without a decision is worse than both.

No code changes until the decision is recorded.

### Decided: reproduce the recorded upstream truncation

Recorded in `docs/grpc-and-faults.md` under "Un-terminated bidi replay
semantics". The alternative — reproduce the downstream client experience so a
replayed client sees `Unknown` exactly as a live one did — was rejected:

- **A replay that lies about the recorded cause is a worse failure than a
  differing error code.** The fixture records *why* the call stopped: a
  terminal `Error` stream event with category `timeout`, which suppresses the
  clean `End`. Terminating cleanly downstream would match the live client
  experience while erasing the only durable evidence of the deadline cut-off,
  so a later reader could not distinguish a truncated call from a completed one.
- **Error-code parity is a convenience, not a contract.** `Unknown` and
  `Internal` are both non-success. No gRPC client branches on the specific value
  to decide whether the call worked; they branch on success.
- **The live symptom is a race, not a recorded property.** The live path's
  `Unknown` depends on where the gateway ended the downstream response relative
  to the outbound deadline. Encoding it would pin a race into a deterministic
  replay.

The asymmetry stays documented, not smoothed over. The M017 safety property is
unchanged: the call never replays as success. Changing this is a replay-semantics
change, not a corrective, and needs its own milestone.

## Track E — `curl_interop` recorded status (records only)

`curl_interop` passed locally in both M018 runs, after being recorded in
`docs/testing.md` as two machine-specific failures proven so by four green
hosted `verify` jobs.

This is **not** a defect and **not** a code change. The work is to reconcile the
record: either the two failures are fixed and the note is stale, or they remain
machine-specific and the note stays accurate. What must not happen is the note
being quietly deleted because a local run passed — local evidence does not
qualify a hosted claim. If the note is wrong, prove it the way the original
claim was proven: hosted, on all qualifying platforms.

## Ordering and dependencies

Tracks A, B, and C are independent of one another and can be implemented in any
order. Track D gates nothing — no other track depends on it, and its decision
only affects future gRPC work. Track E is records-only and independent.

Implement in the order **C, A, B, D, E**: C carries the design decision and the
schema-v2 work and is the largest; A and B are mechanical once C's decision is
settled and benefit from it; D and E close out.

## Non-goals, held

- **No change to the `ErrorPhase` × `ErrorCategory` taxonomy.** M018 unified it;
  this milestone does not touch it.
- **No change to matcher's ignore list.** Track A adds a comparison-level
  concept; coupling the two is explicitly rejected above.
- **No loosening of a timing threshold to fix a flake.** See Track B.
- **No re-litigating the M017 gRPC deferral.** The asymmetry M017 recorded is a
  fact, not a regression; Track D decides the future, it does not rewrite M017.
- **No new report schema version for Tracks A or B.** A suppressed header and a
  timing finding are dispositions in the existing report; only Track C may
  require schema v2, and only if its decision demands an additive field.

  > **Not honoured.** Track A's non-negotiable cannot be met inside the existing
  > report shape — a suppression must be visible and machine-readable, and
  > neither an absent finding nor a `DiffKind` variant satisfies that. A
  > `DiffKind` variant would additionally make every `Date`-bearing report fail
  > `is_success()`. `architecture/06` mandates a bump for a
  > `RegressionReport` shape change, so `REPORT_SCHEMA_VERSION` is now 3 with a
  > stated compatibility story. This is recorded here rather than deleted,
  > because a plan that quietly drops its own non-goal teaches the reader that
  > non-goals in this repository are advisory.

## Definition of done

- A written, recorded decision for Track C's two questions and for Track D,
  with rationale, in this plan's closure record.
- Implementation, tests, documentation, and a closure record under
  `plans/closure/`.
- Track A: a suppressed volatile header is visible in the report as a distinct
  disposition, and a test proves a genuine difference in another header still
  reports.
- Track B: a timing assertion is reachable from the CLI and from Python, and a
  test proves an exceeded bound produces a finding and a satisfied one does not.
- Track C: request-direction events are either compared for real, or the
  comparison arm is removed and the exclusion is documented — not left reading as
  working.
- Full local gate green: `fmt`, `check --all-targets --all-features`,
  `clippy -D warnings`, `cargo test --workspace --all-features`.
- Feature-boundary profiles re-checked on both sides of each boundary, per the
  M015E lesson that `--all-features` cannot validate an opt-in.
- Hosted CI green across all fourteen jobs. **Until then this milestone is
  `implemented`, not `closed`.**

## Risks

- **Track C could become a second gRPC milestone.** It is the one track with a
  real design fork. If its decision resolves toward "not now", the honest
  outcome is removing the dead loop arm and recording the exclusion — a small,
  complete result — rather than a partial comparison.
- **Track A invites over-suppression.** The default must stay narrow and the
  suppression must be visible, or this trades a false positive for a false
  negative, which is strictly worse.
- **Track B touches two public surfaces.** CLI and Python must agree on the
  bound's meaning and units; a mismatch here is a cross-surface contract defect
  that hosted CI will not catch on its own.