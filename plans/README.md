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
- active subsystem roadmaps in `plans/subsystems/`; superseded roadmaps in `plans/archive/subsystems/`;
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

The [registry](registry.md) is the source of truth for milestone status and the
current execution gate. M019 is the latest closed milestone; see
[its closure record](closure/m019-comparison-authority.md). Earlier plans and
closure records remain audit history.

Do not duplicate a dependency-owned subsystem. If repository evidence
invalidates a plan assumption, update the plan and registry before implementation.
