# M011E — WebSocket Candidate Regression, CLI, and Fixture Diff

Status: implemented
Depends on: M011D
Parent milestone: M011

## Objective

Drive recorded WebSocket client behavior against candidate endpoints, compare
candidate server behavior through the same semantic authority, and expose
stable CLI/JSON/JUnit/inspect surfaces.

## A. Candidate execution

For a flow with a required WebSocket conversation:

1. build the candidate H1 Upgrade through EggFetch;
2. use the configured direct/Eggress route exactly as ordinary regression;
3. require valid 101/Accept/subprotocol semantics;
4. extract the owned upgraded stream;
5. wrap it in client-role codec;
6. execute the recorded transcript deterministically:
   - send recorded client->server messages;
   - read candidate server->client messages;
   - preserve control/close semantics;
7. capture candidate messages into bounded in-memory semantic observations;
8. never persist candidate payloads unless a separate explicit output feature
   later requires it.

A WebSocket fixture must not fall back to ordinary HTTP body regression.

## B. Semantic comparison

Compare:

- handshake status/Upgrade semantics;
- selected subprotocol;
- message count/order/direction;
- message kind;
- text/binary/ping/pong payload;
- close code/reason;
- clean vs abnormal terminal state.

Redaction markers produce the same wildcard behavior used by offline replay.
Do not expose secret payloads in findings; use digest/length/bounded structural
descriptions.

## C. Timing comparison

WebSocket timing is optional. Add a WebSocket-specific cadence tolerance in the
shared comparison policy rather than silently overloading HTTP body cadence.

Suggested CLI:
`--websocket-cadence-tolerance-ms <N>`.

Without the option, timing does not produce findings.

## D. Reports

Extend the single `RegressionReport` authority with WebSocket finding kinds or
field paths. If the machine-readable enum/schema surface changes, bump the
report schema version deliberately and add compatibility/golden tests.

JSON and JUnit must project the same underlying findings. Do not create a
parallel WebSocket report evaluator.

Maintain deterministic finding order.

## E. Fixture-vs-fixture diff

When either selected flow has required WebSocket metadata, `diff` compares the
corresponding conversation semantically.

Requested/required WebSocket semantics with missing/inconsistent metadata are
fixture errors, never implicit ordinary-HTTP fallback.

## F. CLI and inspect

Existing `replay` and `test` automatically execute WebSocket semantics for
flows that carry the required extension.

Add bounded inspection, e.g. `inspect --websockets`, showing:

- flow/conversation id;
- selected subprotocol;
- message sequence/direction/kind;
- payload length/digest;
- timing metadata;
- terminal state;
- redaction markers.

Do not print payload bytes unless the existing explicit body-inspection policy
is intentionally extended with the same byte bounds/redaction rules.

Machine exit codes remain the existing stable contract.

## G. WSS qualification

If M011A proved a caller-owned local trust-root path, qualify candidate
`wss://` directly and through the narrow Eggress route. Otherwise keep WSS
outside the M011 support claim rather than using insecure verification.

## Required tests

- candidate text/binary roundtrip;
- candidate ping/pong/close;
- candidate subprotocol mismatch;
- candidate abnormal termination;
- extra/missing/reordered messages;
- payload digest/length-safe diagnostics;
- redaction wildcard comparison;
- WebSocket timing tolerance pass/fail;
- route/no-fallback behavior;
- fixture diff;
- deterministic JSON/JUnit parity;
- inspect bounds/redaction;
- mixed ordinary HTTP + WebSocket fixture regression.

No public Internet.

## Closure

Create `plans/closure/m011e-websocket-candidate-regression-cli-and-diff.md`.
M011F remains blocked until product surfaces are complete.
