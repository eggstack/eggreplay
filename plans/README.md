# EggReplay Planning

This directory is the source of truth for EggReplay implementation handoff.

EggReplay reuses Eggstack transport authorities rather than reimplementing
them:

- **eggfetch** owns outbound HTTP/TLS, pooling, streaming bodies, trailers, and network failures.
- **eggserve** owns inbound HTTP serving/runtime mechanics for replay and mock endpoints.
- **eggress** owns optional listener-free outbound routing/proxy chains.
- **eggreplay** owns the flow model, storage format, normalization, matching, replay semantics, redaction, scenarios, diff/regression logic, and CLI orchestration.

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

v0.1 and C001–C006 are closed. ADR 0005 defines the versioned extension
strategy for future fixture semantics.

The first dependency-ready forward milestone is **M009 — Stateful and Dynamic
Replay**. M010–M014 are planned but blocked in dependency order. See
`registry.md` for the exact handoff state.

Do not start a blocked milestone by duplicating a dependency-owned subsystem.
If current repository evidence invalidates a plan assumption, update the plan
and registry before implementation.
