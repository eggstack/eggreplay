# M014-C1 — Post-M014 Closure and WebSocket Finalization Corrective

Status: closed (qualified via M014-C2 on `cbc9257`)
Implementation: `b188c552`, qualified as amended by M014-C2 `cbc9257`
Hosted qualification: runs `36891564494` (CI) + `36891564581` (wheels) green;
failed run `36881596131` retained as history;
see `closure/m014c1-post-m014-closure-and-websocket-finalization.md` and
`closure/m014c2-websocket-finalization-qualification-and-closure-reconciliation.md`.
The body below remains the historical implementation plan.
Depends on: M014 closure
Corrective gate: post-M014 maintenance / next-roadmap handoff

## Trigger

M014 is closed on `c71ffd7` with standard-matrix Actions run
`36778923619` green. A post-closure audit found two classes of residual debt
that should be closed before defining or executing a new roadmap stage:

1. current-facing planning/support text still contains small contradictions or
   template residue from the M014 transition; and
2. the M014B/M014C qualification sequence exposed a pre-existing intermittent
   M011 WebSocket recording race in
   `recording_gateway_captures_upgrade_and_leading_post_101_messages`.

The WebSocket failure appeared on the first macOS attempt of Actions run
`36773818886` and has also been observed intermittently under local
full-workspace load. The failed lane passed on rerun, and M014 code did not
introduce the defect, so M014 closure remains valid. However, a race between
conversation finalization and session finalization is real maintenance debt and
must not be normalized as acceptable flakiness.

This corrective does not reopen M011 or M014 support decisions. It repairs the
lifecycle invariant and reconciles the post-M014 truth surfaces.

## Objective

Close the repository on a deterministic, internally consistent post-M014
baseline before any Stage 11 feature program begins.

The pass must:

- eliminate the WebSocket conversation/session-finalization race by fixing the
  lifecycle/ownership boundary rather than weakening the test;
- add deterministic regression evidence that session publication cannot outrun
  required WebSocket conversation finalization;
- preserve M011 semantic WebSocket behavior and M013/M014 support boundaries;
- correct stale M014-R1/M014 support text and final closure evidence;
- leave one green cross-platform/MSRV CI run on the qualifying implementation;
- create a closure record and return the registry to no open implementation
  work unless a separately researched next roadmap stage is registered.

## 1. Diagnose the WebSocket finalization race

Start from the M011C atomicity requirement: the initiating HTTP 101 flow and
its WebSocket conversation metadata/blobs must become valid fixture authority
together at session finalization.

Inspect the actual lifecycle across at least:

- `crates/eggreplay-http/src/recording.rs`;
- WebSocket relay/finalization code in `eggreplay-http`;
- `SessionWriter` / concurrent recording finalization in
  `eggreplay-store`;
- the recording-gateway test that captures Upgrade plus leading post-101 data.

Determine the exact ordering that permits the test to observe a published or
finalizing session before the WebSocket conversation finalizer has durably
registered all required metadata/blob references.

Do not assume the cause from the flaky symptom. Pin the race with deterministic
instrumentation/test synchronization before changing production ownership.

Questions the diagnosis must answer:

- What task owns conversation finalization after the 101 handoff?
- What object owns or tracks that task until completion?
- Can session `finish`/publication proceed while the task is still live?
- Is the conversation extension registration ordered before manifest
  validation and atomic rename/publication?
- What happens on cancellation, abnormal EOF, server shutdown, or a finalizer
  error?
- Can a detached task outlive the writer/session authority it needs?
- Are leading post-101 bytes part of the same lifecycle and durability barrier?

Record the resolved ordering in code comments only where the ownership rule is
not self-evident; do not add speculative architecture prose.

## 2. Repair lifecycle ownership, not timing

The fix must establish an explicit completion barrier between accepted
WebSocket conversations and session finalization.

Acceptable designs include a narrow session-owned task/join registry,
structured-concurrency scope, completion future, or equivalent explicit
ownership primitive. The chosen design must satisfy all of these semantics:

- every successful 101 conversation accepted for recording is registered with
  the session before it can become detached from the request path;
- session finalization cannot publish the fixture until every registered
  conversation reaches a terminal durable state or returns an error;
- a finalizer error is surfaced to session finalization and cannot yield a
  manifest that claims a valid conversation with missing metadata/blob refs;
- cancellation/shutdown records truthful abnormal termination when the existing
  M011 contract requires it;
- clean close remains clean only after valid close semantics and durable
  conversation metadata;
- no task holds the session finalization lock across network I/O or an
  unbounded wait;
- limits and shutdown/cancellation bounds remain explicit;
- ordinary HTTP recording and non-WebSocket sessions retain their existing
  behavior and fast path.

Do not fix this with sleeps, widened timeouts, retry loops, test
serialization, platform skips, or by ignoring a missing conversation.

Do not replace EggServe/EggFetch/WebSocket protocol authorities or broaden
supported protocol scope.

## 3. Add deterministic race regression tests

The new evidence must be stronger than repeatedly running the flaky test.

Add a deterministic test harness that can hold the WebSocket conversation at a
known pre-finalization point while session finish is initiated. Prove that the
session cannot publish an incomplete fixture and that completion/error is
propagated according to the lifecycle contract.

At minimum cover:

1. leading post-101 data with the conversation finalizer deliberately delayed;
2. session finish initiated while a registered WebSocket finalizer is pending;
3. successful release of the finalizer followed by valid fixture reopen and
   cross-validation;
4. finalizer failure surfaced through session finish with no falsely valid
   published fixture;
5. cancellation/server-shutdown terminal behavior;
6. clean close remains clean;
7. concurrent conversations do not serialize each other's network relay beyond
   the bounded finalization barrier;
8. ordinary HTTP-only session finish is unaffected.

Keep an additional bounded stress/repetition test if useful, but it is
supplemental. Deterministic synchronization is the acceptance evidence.

The original
`recording_gateway_captures_upgrade_and_leading_post_101_messages` test must
remain enabled on its supported platforms.

## 4. Reconcile post-M014 planning and support truth

Correct the current-facing documentation defects discovered after M014 closure.

### M014-R1 status

Update
`plans/implementation/compatibility/014r1-planning-and-documentation-reconciliation.md`
from `Status: ready` to closed and point to
`plans/closure/m014r1-planning-and-documentation-reconciliation.md`.

Do not rewrite the historical body except where a header/current-status note is
needed for auditability.

### README H2 language

The top-level README currently both records M014B's experimental outbound H2
qualification and later says broadly that H2 remains outside the support
claim.

Make the support wording precise:

- experimental outbound H2 record/regression is a qualified opt-in M014B tier;
- inbound H2 replay/serving remains unsupported;
- H2 MITM remains unsupported/not qualified;
- `h2c` remains unsupported;
- H3 remains unsupported/deferred per ADR 0009;
- WSS interception remains unsupported.

Do not promote experimental outbound H2 to general support.

Update the M013 interception support table row from the stale
`unsupported/deferred M014B` wording to the final post-M014 decision:
H2 MITM is unsupported/not qualified.

### Closure evidence polish

Update current closure records where necessary so final evidence is exact:

- `plans/closure/m014-compatibility-program.md` must pin final umbrella
  closure run `36778923619` on `c71ffd7`;
- remove template residue such as "number filled after green" from M014C/M014D
  closure text now that run numbers are known;
- if the exact M014D workspace test count is recoverable from the qualifying
  run/logs, record the exact count rather than `371+`; otherwise state why
  the closure intentionally reports a lower-bound count;
- preserve the record of earlier failed qualification attempts and their
  diagnosed causes.

Do not alter historical closure facts to make the history look cleaner.

## 5. Registry and roadmap cleanup

The registry's generic decomposed-milestone rule is dependency-driven and
should remain so, but remove the obsolete sentence that still says M014A/B are
blocked until M014-R1 closes.

Register M014-C1 as the sole ready post-M014 corrective handoff while this plan
is open.

Update the canonical roadmap and planning README only enough to state:

- Stages 1-10 remain closed;
- M014-C1 is a post-closure corrective, not a new feature stage;
- no Stage 11 implementation program is currently authorized while M014-C1 is
  open.

Do not invent Stage 11 scope as part of this corrective.

## 6. Preserve support and architecture boundaries

This plan must not change the established support matrix except to make
documentation accurately reflect already-qualified behavior.

In particular:

- no H2 inbound serving or MITM promotion;
- no H3/QUIC implementation;
- no WSS interception expansion;
- no new Python packaging/interception policy;
- no fixture/schema change unless the WebSocket lifecycle bug proves a schema
  invariant is impossible to satisfy without one; if so, stop and write a
  separate architectural plan;
- no dependency version moves unless required to fix a demonstrated upstream
  lifecycle defect; if an upstream change is required, split and register the
  sibling-repo plan rather than hiding it here.

The likely correction belongs to EggReplay task/session ownership. Verify that
assumption from code before implementation.

## Verification

Run the repository-standard locked gates:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked --no-fail-fast
cargo audit
git diff --check
```

Also run targeted WebSocket finalization tests repeatedly on the implementation
platform after the deterministic barrier tests pass. Repetition is useful for
regression confidence but does not substitute for deterministic race coverage.

One qualifying implementation SHA must pass the normal hosted matrix,
including:

- Ubuntu stable;
- Ubuntu Rust 1.89;
- macOS stable;
- Windows stable;
- interception lanes;
- dependency-boundary;
- Python binding lanes;
- Python abi3 cross-version lane.

The macOS lane is specifically required because that is where the recorded
full-suite race surfaced during run `36773818886`.

## Closure procedure

Keep M014-C1 `active` or `implemented` until hosted evidence is green.

After a qualifying run:

1. create
   `plans/closure/m014c1-post-m014-closure-and-websocket-finalization.md`;
2. record the root cause and the lifecycle ownership invariant established by
   the fix;
3. record deterministic race-test evidence and exact targeted/workspace counts;
4. record the qualifying SHA and Actions run;
5. record all documentation/evidence corrections made;
6. mark M014-C1 closed in the registry;
7. update README/planning README/roadmap to show no active implementation plan;
8. retain Stage 11 as undefined until separately researched and planned.

If diagnosis proves the race belongs to EggServe, EggFetch, or another sibling,
keep this plan open, write/register the minimal upstream corrective in that
repository, and make the EggReplay closure depend on the published/adopted
upstream fix.

## Acceptance

M014-C1 closes only when:

- the WebSocket/session race has a demonstrated root cause;
- lifecycle ownership prevents session publication from outrunning registered
  conversation finalization;
- deterministic tests prove pending-success, pending-failure, cancellation,
  clean-close, leading-data, and fixture atomicity behavior;
- the original flaky test remains enabled and passes;
- ordinary HTTP session finalization is not regressed;
- M014-R1's implementation-plan header no longer advertises `ready`;
- README support language accurately distinguishes experimental outbound H2
  from unsupported inbound H2/H2 MITM/`h2c`;
- H3/WSS/interception limitations remain truthful;
- M014 closure records contain final exact run evidence without placeholder
  prose;
- the registry has no stale M014-R1 gate language;
- the standard local gates pass;
- one full hosted matrix is green on the qualifying implementation;
- a closure record exists and the registry returns to no active/ready
  implementation plan.

Until then, do not begin a new Stage 11 feature milestone.
