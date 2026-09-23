# M010 closure — Streaming Timing, Mid-Body Events, and SSE (with M010-C1 corrective)

Status: closed

## Implementation

Qualifying implementation SHA: `3bdd1359e00736737dd1610035d7e9f3e49822f1`
(`feat(m010-c1): close stream extension and regression gaps`). The main M010
implementation landed earlier at `cc4e4a9354dd0f6de25e6f905afcb44a732c8c64`
(Actions run `35795730877` green); post-implementation audit found the
required-extension and regression-wiring gaps closed here.

M010-C1 corrects, in dependency order:

- writers (`SessionWriter::finish`, `RecordingSession::finish`) emit
  `stream-events` with `required_for_replay=true`;
- `ReplayFixture::load_inner` recognizes the supported `stream-events`
  extension automatically, validates it, and applies terminal semantics in
  every timing mode; `Immediate` is zero delay, not “ignore stream events”;
  `Recorded`/`Scaled` remain explicit timing opt-ins; unknown required
  extensions still fail closed with no generic ignore switch; missing,
  malformed, unsupported-version, duplicate-flow, and body-length inconsistent
  metadata fail closed;
- `execute_candidate` captures bounded response event metadata (DATA
  offset/length with monotonic `Instant` deltas, trailers, clean `End`,
  terminal `Error` with delivered-byte offset and stable `other`/`body`
  category/phase), retains status/headers and partial bytes on body-frame
  failure, and returns an observation suitable for regression instead of a
  generic runtime failure; existing body-byte limits and M010 event/delay
  limits are enforced;
- shared typed `ComparisonPolicy` for `replay`, `test`, and fixture `diff`
  (`--compare-stream-events`, `--cadence-tolerance-ms <N>` implying stream
  comparison, `--compare-sse`, repeatable `--sse-ignore <field>` for
  `data,event,id,retry,comments` implying SSE comparison); default preserves
  sequential scheduling, immediate timing, and ordinary
  status/header/trailer/raw-body regression with no stream/SSE-only findings;
  live regression loads baseline events by flow id, compares candidate
  response events, reports shape/terminal differences, applies cadence only
  when configured, merges into the existing `RegressionReport` authority, and
  keeps deterministic ordering; fixture `diff` requires valid metadata on both
  sides with explicit errors, not fallback; no second JSON/JUnit evaluator;
- `compare_flows` consumes the typed policy; raw body remains authoritative;
  SSE findings occur only when explicitly enabled with ignored fields passed
  through `compare_sse`; malformed SSE yields a bounded `DiffKind::Sse`
  finding when enabled; `inspect --sse` remains independent;
- Immediate terminal-error flushing fix: cooperative yield when delay is zero
  so back-to-back DATA is flushed before a truncating Error (zero time delay,
  microseconds of scheduler yield; Immediate remains under 30ms for an 80ms
  recorded timeline while reproducing the full 4-byte partial body plus
  terminal error).

Transport ownership remains EggFetch outbound, EggServe inbound, and Eggress
optional routing; no parallel HTTP stack was added.

Explicit response-direction regression scope: M010 closure is
response-stream authoritative. The candidate path materializes the baseline
request into one `Full` body, so candidate request cadence is not meaningful.
Live and fixture stream comparison compare response events only (request
vectors are cleared to empty on both sides before `compare_stream_events`).
Request timing is never fabricated by copying baseline events.

## Verification

Local gates on the qualifying implementation, all passed:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
git diff --check
```

The full workspace test command reported 106 passed across 10 test binaries on
the local Unix host (2 CLI unit + 12 CLI contracts + 30 core + 33 HTTP lib +
12 v0.1 qualification + 17 store). Required-test coverage proves all 17
M010-C1 items using local deterministic origins only:

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

## Hosted qualification

GitHub Actions run `35864247624` on the qualifying SHA:
https://github.com/eggstack/eggreplay/actions/runs/35864247624

All required jobs passed:

- `verify (ubuntu-latest, stable)` — success, 106 tests.
- `verify (ubuntu-latest, 1.89.0)` — success, 106 tests (MSRV).
- `verify (macos-latest, stable)` — success, 106 tests.
- `verify (windows-latest, stable)` — success, 104 tests; two Unix-only
  symlink-construction tests are excluded because the hosted Windows runner
  does not reliably allow symlink creation (`required_extension_rejects_symlinked_payload`,
  `open_blob_rejects_symlinked_blob`). Portable symlink rejection code still
  builds and the Windows suite passes.
- `dependency-boundary` — success; core/store remain transport-free and the
  direct and Eggress feature boundaries qualify.

## Acceptance and handoff

- Extension contract coherent (`required_for_replay=true`, auto-recognized,
  Immediate zero-delay terminal semantics, unknown fails closed, no ignore
  switch): yes.
- Candidate stream observation (bounded response events, Instant cadence,
  partial + Error on frame failure, no generic runtime reduction): yes.
- Opt-in stream/cadence regression wired into CLI/reporting with deterministic
  merging and no second evaluator: yes.
- SSE policy genuinely opt-in with field-specific ignores and bounded
  malformed findings: yes.
- Closure evidence and hosted matrix on the qualifying SHA are green: yes.
- Transport ownership boundaries remain intact: yes.

M010 and M010-C1 are closed. M011 (WebSocket semantic record/replay) is
unblocked and moves to `ready`. M012–M014D remain blocked by their unclosed
dependencies. Historical M009/C006 closure records are unchanged.
