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
M013F are all closed. M014-R1, M014A, M014B, M014C, and M014D are closed,
and the M014 compatibility-program umbrella is closed with the support
matrix in `closure/m014-compatibility-program.md`.

See `registry.md` for the exact handoff state.

Do not start a blocked milestone by duplicating a dependency-owned subsystem.
If repository evidence invalidates a plan assumption, update the plan and
registry before implementation.
