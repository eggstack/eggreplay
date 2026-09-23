# M010-C1 — Stream Extension and Regression Corrective Closure

Status: closed
Depends on: M010 implementation at `cc4e4a9354dd0f6de25e6f905afcb44a732c8c64`
Corrective gate: M010 closure / M011 unblock
Closure: `plans/closure/m010-streaming-timing-and-sse.md`
Qualifying implementation: `3bdd1359e00736737dd1610035d7e9f3e49822f1`, Actions run `35864247624`

## Trigger

M010 is substantially implemented and Actions run `35795730877` is green
across Linux stable, Linux Rust 1.89, macOS, Windows, and
`dependency-boundary`. Post-implementation audit found several acceptance
gaps. This plan is deliberately narrow; do not begin M011 until it closes.

## 1. Make the stream-events extension contract coherent

ADR 0005 says behavior-changing extensions are required for replay, and the
M010 plan calls `stream-events` required when used. Current store finalization
writes it with `required_for_replay=false`. Meanwhile
`ReplayFixture::load_inner` rejects every required extension except an
enabled `rules` extension before decoding `stream-events`.

This matters because `stream-events` may encode a terminal mid-body error.
Ignoring it can turn a partial failed response into a successful complete
response.

Required correction:

- writers that emit `stream-events` register it with
  `required_for_replay=true`;
- replay recognizes the supported `stream-events` extension automatically,
  validates it, and applies its terminal semantics;
- `Immediate` means zero delay, not “ignore stream events”;
- `Recorded` and `Scaled` remain explicit timing opt-ins;
- unknown required extensions still fail closed;
- missing, malformed, unsupported-version, duplicate-flow, or body-length
  inconsistent stream metadata fails closed;
- no generic ignore-required-extension switch.

Update ADR 0005 and `docs/eggr-schema.md` to state that
`required_for_replay` means the reader must understand/apply the extension,
not that the user must opt into timing delays.

## 2. Capture candidate response stream events

`execute_candidate` currently observes response DATA/trailers but does not
return `FlowStreamEvents`. A body-frame failure aborts candidate execution
rather than becoming a comparable terminal event.

Extend `CandidateObservation` (or an equivalent narrow type) with bounded
response event metadata:

- DATA offset/length + monotonic delta;
- trailers;
- clean End;
- terminal Error with delivered-byte offset and stable category/phase.

Use `Instant`; do not use wall-clock time for cadence.

On a candidate body-frame failure, retain status/headers and partial bytes,
append the Error event, and return an observation suitable for regression.
Do not reduce this observable condition to a generic CLI runtime failure.

Keep existing body-byte limits and enforce M010 event/delay limits.

### Request-direction scope

The current candidate path materializes the baseline request into one
`Full` body, so candidate request cadence is not meaningful. M010 closure is
response-stream authoritative. Do not fabricate request timing by copying
baseline events. Document this limitation in the closure.

## 3. Wire stream regression into CLI/reporting

M010 requires stream findings to be opt-in. Preserve the old default regression
contract.

Add a shared typed policy for `replay`, `test`, and fixture `diff`:

- `--compare-stream-events` enables ordered response event comparison;
- `--cadence-tolerance-ms <N>` enables cadence comparison and implies stream
  event comparison;
- `--compare-sse` enables derived SSE semantic comparison;
- repeatable `--sse-ignore <field>` supports only
  `data,event,id,retry,comments` and implies SSE comparison.

Equivalent value-enum spelling is acceptable if it matches established CLI
style, but these semantics are required.

Default remains sequential scheduling, immediate replay timing, and ordinary
status/header/trailer/raw-body regression with no stream/SSE-only findings.

For live candidate regression:

1. load baseline stream events by flow id when requested;
2. compare candidate response events from the observation;
3. report event-shape/terminal differences;
4. apply cadence tolerance only when configured;
5. merge findings into the existing `RegressionReport` authority;
6. keep deterministic fixture/report ordering.

For fixture `diff`, requested stream comparison requires valid metadata on
both sides. Missing requested metadata is an explicit fixture/configuration
error, not fallback.

Do not create a second JSON/JUnit evaluator.

## 4. Make SSE policy genuinely opt-in

`compare_sse` already supports ignored fields, but current
`compare_flows` invokes SSE comparison automatically with no ignore policy.

Correct this so:

- raw body comparison remains authoritative;
- SSE semantic findings occur only when explicitly enabled;
- ignored fields are passed through the existing `compare_sse` authority;
- malformed SSE produces a bounded `DiffKind::Sse` finding when enabled;
- raw-body findings are not suppressed merely because SSE semantics match;
- `inspect --sse` remains an independent explicit inspection option.

Refactor `compare_flows` only enough to consume a typed comparison policy;
do not fork flow-diff logic.

## 5. Closure/status cleanup

There is no M010 closure record yet. During corrective implementation keep
M010 marked implemented, not closed, and M011 blocked.

After all acceptance criteria and hosted evidence pass:

- create `plans/closure/m010-streaming-timing-and-sse.md`;
- mark M010 and M010-C1 closed;
- move M011 to ready;
- update README/planning status accordingly.

Do not rewrite M009/C006 historical closure records.

## Required tests

At minimum prove:

1. writer emits stream-events as required;
2. immediate replay accepts the known required extension;
3. immediate replay reproduces terminal mid-body error with no delay;
4. unknown required extension still fails closed;
5. malformed/unsupported stream-events fails closed;
6. candidate DATA offsets/deltas are valid and bounded;
7. candidate trailers and clean End are captured;
8. candidate mid-body error returns partial observation + Error event;
9. default regression emits no stream/SSE-only findings;
10. stream comparison detects event-shape/terminal differences;
11. cadence tolerance passes/fails deterministic thresholds;
12. requested comparison with missing metadata fails explicitly;
13. SSE comparison detects ordered semantic differences;
14. each supported SSE ignore field only ignores that field;
15. malformed SSE yields a bounded semantic finding when enabled;
16. fixture-vs-fixture stream comparison is deterministic;
17. JSON and JUnit project the same new findings.

Use local deterministic origins only.

## Verification and closure gate

Run:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
git diff --check
```

One qualifying implementation SHA must then pass:

- Ubuntu stable;
- Ubuntu Rust 1.89 MSRV;
- macOS stable;
- Windows stable;
- dependency-boundary.

The closure record must include the qualifying SHA, Actions run ID/URL, actual
platform test counts, cfg-specific exclusions, and the explicit
response-direction regression scope.

M010-C1 closes only when the extension contract, candidate stream observation,
opt-in stream/cadence regression, opt-in SSE policy, closure evidence, and
hosted matrix are all complete. M011 is unblocked only in that same closure
transition.
