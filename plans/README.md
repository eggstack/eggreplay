# EggReplay Planning

This directory is the source of truth for EggReplay implementation handoff.

EggReplay reuses Eggstack transport authorities rather than reimplementing
them:

- **eggfetch** owns outbound HTTP/TLS, pooling, streaming bodies, trailers, and upgraded connection IO.
- **eggserve** owns inbound HTTP serving/runtime mechanics and generic tunnel handoff.
- **eggress** owns optional listener-free outbound routing/proxy chains.
- **eggreplay** owns the semantic models, storage, matching, replay, redaction, scenarios, regression, and orchestration.
- **eggreplay-intercept** is the optional privileged acquisition leaf for M013.
- **Python bindings** remain adapters over non-interception Rust authorities in M013.

## Planning convention

- canonical roadmap/qualification documents at the root of `plans/`;
- subsystem roadmaps in `plans/subsystems/`;
- bounded implementation plans in `plans/implementation/<workstream>/`;
- architectural decisions in `plans/adrs/`;
- completion evidence in `plans/closure/`;
- historical superseded material in `plans/archive/`;
- `plans/registry.md` is the execution/status index.

Plans remain audit artifacts after implementation. Source presence alone never
closes a plan.

## Status vocabulary

- **ready** — dependencies satisfied; safe to hand off.
- **blocked** — dependency or decision gate not satisfied.
- **active** — implementation in progress.
- **implemented** — implementation exists but closure evidence is incomplete.
- **closed** — acceptance criteria and closure evidence are complete.
- **deferred** — intentionally outside the execution horizon.

## Current execution

v0.1/C001–C006, M009, M010/M010-C1, M011/M011A–M011F, and M012/M012A–M012F
are closed.

M013 is closed under ADR 0008 on qualifying revision `5efc6f9` (Actions
runs [36211265347](https://github.com/eggstack/eggreplay/actions/runs/36211265347)
and [36456906216](https://github.com/eggstack/eggreplay/actions/runs/36456906216)
on the closure commit); M013A, M013B0, M013B, M013C, M013D, M013E, and
M013F are all closed. M014-R1, M014A, M014B, M014C, M014D, and the
M014 compatibility-program umbrella are closed; the consolidated
support matrix is in `closure/m014-compatibility-program.md`.
M014-C1 WebSocket conversation-finalization repair and M014-C2
qualification/closure reconciliation are closed, qualified through `cbc9257`
(hosted CI `36891564494` and wheel run `36891564581`; closure/documentation
commit `88daa3ed` green on `36894700433`); see
`closure/m014c1-post-m014-closure-and-websocket-finalization.md` and
`closure/m014c2-websocket-finalization-qualification-and-closure-reconciliation.md`.
M014, M014-R1, M014A–M014D, M014-C1, and M014-C2 are closed. M014-R2
post-C2 documentation reconciliation is closed; see
`closure/m014r2-post-c2-documentation-reconciliation.md`.

Stage 11 executed as M015 — bidirectional HTTP/2 and transport baseline — and
is closed. M015A (published dependency refresh + H2 serving-boundary preflight),
M015B (inbound H2 gateway and replay), M015C (end-to-end H2 semantic and
regression qualification), M015D (gRPC over HTTP/2), and M015E (hardening and
hosted qualification) are all closed.

Two post-Stage-11 correctives are also closed. **M016** closed the three items
Stage 11 left open, each of which proved larger than first recorded: the CLI set
no request timeout at all (now `--timeout-secs`, with `total` populated and the
flag unset by default), every Eggress route failure was categorised `Other`
because `map_fetch_error` had no `CustomTransport` arm, and the WebSocket
"flake" was a test racing documented abort-on-shutdown behaviour. A fourth
defect surfaced during hosted qualification: `drive_with_deadline` could report
a deadline as reached ~0.1 ms early on Windows. **M017** closed the one
deferral Stage 11 left standing — un-terminated bidirectional gRPC. The
investigation disproved the stated blocker (the gateway is already full-duplex
and the cut-off is already recorded); the real defect was one layer down, where
body-stream errors were hardcoded and the underlying error discarded. That
support row moved from deferred to supported (experimental), and the live-vs-
replay status-code asymmetry is recorded rather than smoothed over. See
`registry.md` for exact closure state, qualifying revisions, and hosted run
IDs.

Do not start a blocked milestone by duplicating a dependency-owned subsystem.
If repository evidence invalidates a plan assumption, update the plan and
registry before implementation.
