# EggReplay Planning

This directory is the source of truth for EggReplay implementation handoff.

EggReplay reuses Eggstack transport authorities rather than reimplementing
them:

- **eggfetch** owns outbound HTTP/TLS, pooling, streaming bodies, trailers, and upgraded connection IO.
- **eggserve** owns inbound HTTP serving/runtime mechanics and generic tunnel handoff.
- **eggress** owns optional listener-free outbound routing/proxy chains.
- **eggreplay** owns the semantic models, storage, matching, replay, redaction, scenarios, regression, and orchestration.
- **Python bindings** are adapters over those Rust authorities, not a second implementation.

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

v0.1/C001–C006, M009, M010/M010-C1, and M011/M011A–M011F are closed.

M012 is decomposed under ADR 0007 into M012A–M012F. M012A is closed; M012B
— Fixture, Report, and Data Bindings — is implemented and awaiting hosted
qualification. M012C–M012F and M013–M014 remain blocked in dependency order.

See `registry.md` for the exact handoff state.

Do not start a blocked milestone by duplicating a dependency-owned subsystem.
If repository evidence invalidates a plan assumption, update the plan and
registry before implementation.
