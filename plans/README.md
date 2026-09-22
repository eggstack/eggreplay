# EggReplay Planning

This directory is the source of truth for EggReplay implementation handoff.

EggReplay is a Rust-native semantic HTTP interaction recording, replay, mocking, and network-regression system. Its core must remain useful without TLS interception. The project reuses Eggstack transport authorities rather than reimplementing them:

- **eggfetch** owns outbound HTTP/TLS, pooling, streaming bodies, trailers, and network failures.
- **eggserve** owns inbound HTTP serving/runtime mechanics for replay and mock endpoints.
- **eggress** owns optional listener-free outbound routing/proxy chains.
- **eggreplay** owns the flow model, storage format, normalization, matching, replay semantics, redaction, scenarios, diff/regression logic, and CLI orchestration.

## Planning convention

The planning layout mirrors the conventions used across CodeGG/Eggstack work:

- numbered canonical plans at the root of `plans/`;
- subsystem roadmaps in `plans/subsystems/`;
- bounded, agent-ready implementation plans in `plans/implementation/<workstream>/`;
- architectural decisions in `plans/adrs/`;
- completion evidence in `plans/closure/`;
- historical superseded material in `plans/archive/`;
- `plans/registry.md` is the compact execution/status index.

Plans are retained as audit artifacts after implementation. A plan is not complete merely because code exists: its acceptance criteria and required evidence must be satisfied and the registry updated with the implementation/closure commit.

## Status vocabulary

- **ready** — dependencies satisfied; safe to hand off.
- **blocked** — dependency or decision gate not satisfied.
- **active** — implementation in progress.
- **implemented** — implementation exists but closure evidence is not yet complete.
- **closed** — acceptance criteria and closure evidence are complete.
- **deferred** — intentionally outside the current execution horizon.

## Execution rule

Execute milestones in dependency order. Do not start a blocked milestone by silently duplicating a dependency-owned subsystem. If current repository evidence invalidates a plan assumption, stop that milestone, document the discrepancy, and update the plan/registry before continuing.

The first dependency-ready implementation milestone is **M001**. See `registry.md` and `002-long-term-roadmap.md`.
