# M014-C2 — WebSocket Finalization Qualification and Closure Reconciliation

Status: closed
Implementation: `cbc9257`
Hosted qualification: runs `36891564494` (CI) + `36891564581` (wheels) green;
see `closure/m014c2-websocket-finalization-qualification-and-closure-reconciliation.md`.
Depends on: M014-C1 implementation on `b188c552`
Corrective gate: M014-C1 closure / post-M014 clean baseline

## Trigger

M014-C1 implementation landed on `b188c552`, but its closure was recorded
before the implementation had valid hosted qualification.

Actions run `36881596131` on `b188c552` completed with failure. The
WebSocket barrier itself was not the reported failure: stable Clippy rejected
an existing M014A HAR test in `crates/eggreplay-har/src/lib.rs`:

```text
assert!(!flows[0].redactions.is_empty());
^ clippy::assert_is_empty under -D warnings
```

As a result, Ubuntu/macOS stable verify lanes failed or were cancelled and the
required hosted matrix never qualified the new lifecycle code. The prior green
M014 umbrella run `36778923619` on `c71ffd7` predates the WebSocket repair
and cannot serve as M014-C1 qualification evidence.

A post-failure audit also found:

- the README still broadly says `H2/H3` are outside the claim despite M014B's
  explicit experimental outbound-H2 tier;
- asynchronous CLI recording paths call the now-potentially-blocking
  `RecordingSession::finish()` directly;
- Python `finish_session` is async but also calls `RecordingSession::finish()`
  directly;
- M014-C1's closure record and current status surfaces incorrectly describe the
  corrective as closed.

This plan owns only qualification/reconciliation of the already-landed repair.
It must not become a new feature milestone.

## Objective

Return EggReplay to a truthful post-M014 state with the M014-C1 lifecycle
repair actually qualified on its implementation lineage.

M014-C2 must:

1. clear the unrelated stable-Clippy blocker without changing HAR semantics;
2. validate and, if needed, correct the blocking behavior of the new WebSocket
   finalization barrier at every runtime-facing async call site;
3. preserve the deterministic M014-C1 race fix and add any missing
   single-thread/runtime-starvation evidence;
4. correct remaining H2 support wording;
5. run one complete green hosted matrix on the final corrective SHA;
6. amend M014-C1 closure evidence to point to the actual qualified
   implementation lineage;
7. close C1 and C2 only after that evidence exists.

## 1. Clear the stable-Clippy blocker

Fix the M014A HAR assertion at
`crates/eggreplay-har/src/lib.rs:2098` in the smallest semantics-preserving
way accepted by the current stable Clippy.

The test must continue proving that the imported flow contains at least one
redaction marker. Do not weaken or suppress the assertion globally.

Prefer an idiomatic assertion that reports useful failure context. Do not add
a broad `#[allow]` unless current stable and MSRV behavior make the idiomatic
form impossible.

Run Clippy on both current stable and Rust 1.89.0 because the repository
supports both.

## 2. Audit the blocking finalization boundary

M014-C1 changed `RecordingSession::finish()` from a purely local synchronous
finalization path into one that may wait on registered WebSocket conversation
finalizers through a `Condvar`.

Audit all runtime-facing uses, including at minimum:

- CLI `record`;
- CLI `serve --record-mode once`;
- CLI append-new recording;
- CLI re-record;
- Python `lifecycle.rs::finish_session`;
- any library/network helper that finalizes a `RecordingSession` from an async
  context.

Current evidence shows the CLI and Python paths call `.finish()` directly
from async functions.

For each call site establish one of these contracts explicitly:

- **already drained**: the owning server/runtime guarantees every conversation
  task has reached terminal state before `finish()`, so the call cannot wait
  on runtime progress; or
- **blocking isolation**: execute synchronous finalization through an
  appropriate blocking boundary (for example `tokio::task::spawn_blocking`)
  so the async executor remains available to drive conversation completion.

Do not assume a multi-thread Tokio runtime makes direct blocking safe. Python
and embedders may use different runtime shapes.

## 3. Bound shutdown/finalization behavior

The store-level trait exposes `drive_with_deadline`, but M014-C1's ordinary
`finish()` path uses unbounded `drive()`.

Determine whether supported network/runtime lifecycles can leave a registered
finalizer permanently unsignalled. The proof must cover:

- normal close;
- abnormal EOF;
- tunnel task cancellation;
- EggServe shutdown/drain timeout;
- panic/drop of the conversation task;
- caller cancellation of Python close;
- single-thread Tokio execution.

If terminal signalling is structurally guaranteed before runtime-facing
finalization, document that invariant in tests and keep the store primitive
simple.

If an indefinite wait remains possible, add a bounded finalization policy at
the correct ownership layer. Do not silently time out and publish an incomplete
fixture. Timeout must fail closed with an actionable error and leave no valid
manifest claiming a complete WebSocket conversation.

Do not add arbitrary sleeps or polling as correctness mechanisms.

## 4. Qualification tests for async/runtime safety

Keep all six deterministic M014-C1 regression tests and the original
`recording_gateway_captures_upgrade_and_leading_post_101_messages` test.

Add only the missing evidence required by the call-site audit. At minimum
qualify:

- finalization from a current-thread/single-thread Tokio runtime without
  starving a pending conversation finalizer;
- CLI shutdown -> wait -> finalization ordering for WebSocket-enabled recording;
- Python async close/finalization with a pending or cancelling conversation;
- cancellation/drop still releases the barrier and fails closed if transcript
  authority is absent;
- HTTP-only finalization remains on the fast path.

If the correct design is to use `spawn_blocking`, tests must prove that the
executor can continue driving the conversation while finalization waits.

## 5. Reconcile support wording

Fix the remaining top-level README contradiction.

The baseline prose must no longer say generic `H2` is outside the claim.
State instead that:

- cleartext RFC 6455 WebSocket support remains the qualified M011 baseline;
- WSS interception remains unsupported;
- outbound H2 record/regression is experimental and opt-in under M014B;
- inbound H2 serving, H2 MITM, and `h2c` are unsupported/not qualified;
- H3 remains unsupported/deferred under ADR 0009;
- negotiated WebSocket extensions and wire-frame fidelity remain outside the
  claim.

Keep the existing M014B/M014C matrix as the detailed authority.

## 6. Correct M014-C1 status/evidence

Treat the existing C1 closure as a premature closure record, not valid
qualification evidence.

During C2 implementation:

- M014-C1 remains `implemented (qualification pending M014-C2)`;
- M014-C2 is the sole active/ready corrective gate;
- the C1 closure record must retain the historical explanation but clearly mark
  the old `c71ffd7` / `36778923619` qualification claim as invalid for C1
  because it predates `b188c552`;
- do not delete the failed `36881596131` evidence;
- after a final green run, amend the C1 closure record with the actual
  qualifying SHA/run and the C2 reconciliation note;
- create
  `plans/closure/m014c2-websocket-finalization-qualification-and-closure-reconciliation.md`;
- only then mark both M014-C1 and M014-C2 closed.

M014 itself remains closed throughout; C2 is a post-closure corrective.

## Verification

Before pushing the qualifying implementation:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo +1.89.0 clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked --no-fail-fast
cargo audit
git diff --check
```

Also run the targeted M014-C1/M014-C2 WebSocket lifecycle tests repeatedly
after deterministic tests pass. Repetition is supplementary evidence only.

The qualifying SHA must pass every normal hosted job, not merely the lanes
unrelated to the failure:

- verify Ubuntu stable;
- verify Ubuntu Rust 1.89.0;
- verify macOS stable;
- verify Windows stable;
- interception Ubuntu/macOS/Windows;
- dependency-boundary;
- Python binding lanes;
- Python abi3 cross-version.

A rerun of a failed job is acceptable only if the failure is demonstrated to be
infrastructure-only. A deterministic code/lint failure requires a new commit
and a new qualifying run.

## Acceptance

M014-C2 closes only when:

- the stable-Clippy HAR failure is fixed without weakening redaction coverage;
- stable and Rust 1.89 Clippy both pass;
- every async `RecordingSession::finish()` call site has a documented safe
  ownership/blocking contract;
- no supported runtime can deadlock by blocking the executor thread needed to
  release a WebSocket finalizer;
- shutdown/cancellation cannot create an indefinite finalization wait without a
  bounded fail-closed outcome;
- deterministic WebSocket finalization tests remain green;
- the original leading-post-101 regression remains enabled and green;
- README support prose no longer contradicts the experimental outbound-H2 tier;
- one complete hosted matrix is green on the final C2 implementation SHA;
- M014-C1 closure evidence cites that actual qualified lineage rather than
  pre-C1 `c71ffd7`;
- C1 and C2 closure records are internally consistent;
- the registry returns to no open implementation plan;
- Stage 11 remains undefined pending separate research/planning.

Until these criteria are met, do not mark M014-C1 closed and do not begin a
Stage 11 feature milestone.
