# M011E — WebSocket Candidate Regression, CLI, and Diff Closure

Status: closed

## Implementation

- Implementation commit: `ec5a6ff678f7278608ca8447cb6b75c2ee574a0a`
- Candidate upgrades use EggFetch and the configured direct/Eggress route.
  Candidate WebSocket flows execute the recorded strict message script and
  never fall back to HTTP body comparison.
- Handshake, protocol, message, payload, timing, and terminal findings use the
  shared deterministic `RegressionReport` authority. Schema version 2 adds the
  WebSocket finding kind. Diagnostics expose bounded length/digest summaries,
  not payload values.
- Cadence tolerance is WebSocket-specific. Fixture diff compares transcript
  metadata and payload refs, with redaction-aware wildcard semantics.
- `inspect --websockets` reports bounded metadata and payload digests without
  printing payload bytes. Existing JSON/JUnit projections and exit codes are
  retained.

## Verification

The local workspace gates passed with 132 tests; a deterministic local
candidate server test executes the recorded upgrade and message script. Hosted
stable Linux/macOS/Windows, Linux Rust 1.89, and dependency-boundary evidence
passed in run 35880303725 on `63d9e6c`.

M011F completed hardening and qualification; evidence is in the umbrella
closure.
