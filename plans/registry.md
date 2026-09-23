# EggReplay Plan Registry

Status source of truth for implementation handoff.

## Historical v0.1 workstream

| ID | Plan | Status | Depends on | Release gate |
|---|---|---|---|---|
| M001 | `implementation/foundation/001-workspace-and-contracts.md` | closed | — | foundation |
| M002 | `implementation/foundation/002-flow-model-and-store.md` | closed | M001 | foundation |
| M003 | `implementation/recording/003-eggfetch-recording-path.md` | closed | M002 | v0.1 |
| M004 | `implementation/replay/004-eggserve-replay-server.md` | closed | M002 | v0.1 |
| M005 | `implementation/matching/005-matching-consumption-near-miss.md` | closed | M002, M004 | v0.1 |
| M006 | `implementation/regression/006-client-replay-and-diff.md` | closed | M002, M003, M005 | v0.1 |
| M007 | `implementation/integration/007-eggress-routing-and-cli.md` | closed | M003, M004, M006 | v0.1 |
| M008 | `implementation/security/008-v0.1-hardening-and-qualification.md` | closed (historical) | M001–M007 | v0.1 foundation closure |

## Corrective v0.1 workstream

| ID | Plan | Status | Depends on | Gate |
|---|---|---|---|---|
| C001 | `implementation/corrective/c001-lazy-replay-streaming.md` | closed | M001–M008 baseline | v0.1 requalification |
| C002 | `implementation/corrective/c002-concurrent-recording-session.md` | closed | M001–M008 baseline | v0.1 requalification |
| C003 | `implementation/corrective/c003-persistence-redaction-policy.md` | closed | M001–M008 baseline | v0.1 requalification |
| C004 | `implementation/corrective/c004-cli-eggress-contracts.md` | closed | M001–M008 baseline | v0.1 requalification |
| C005 | `implementation/corrective/c005-v0.1-requalification.md` | closed | C001–C004 | local qualification |
| C006 | `implementation/corrective/c006-windows-hosted-ci-qualification.md` | closed | C001–C005 | v0.1 hosted qualification |

v0.1 hosted qualification is closed. Qualifying implementation:
`9b9cc9552c8d1fdee8a64907a666ec2796c2f5d3`, Actions run
`35774531684`.

## Forward implementation queue

ADR 0005 owns versioned session extensions. ADR 0006 owns WebSocket semantic
conversation storage/matching.

| ID | Plan | Status | Depends on | Primary result |
|---|---|---|---|---|
| M009 | `implementation/stateful/009-stateful-dynamic-replay.md` | closed | C006 | record modes, scenarios, deterministic templates |
| M010 | `implementation/streaming/010-streaming-timing-and-sse.md` | closed | M009 | stream events, timing, SSE |
| M010-C1 | `implementation/streaming/010c-stream-extension-and-regression-corrective.md` | closed | M010 implementation | extension contract + candidate stream regression closure |
| M011 | `implementation/websocket/011-websocket-semantic-record-replay.md` | ready (decomposed) | M010 + M010-C1 | WebSocket milestone umbrella |
| M011A | `implementation/websocket/011a-transport-dependency-and-upgrade-preflight.md` | **ready** | M010 + M010-C1 | dependency/upgrade substrate qualification |
| M011B | `implementation/websocket/011b-semantic-model-store-and-codec.md` | blocked | M011A | semantic/store/codec authority |
| M011C | `implementation/websocket/011c-recording-gateway.md` | blocked | M011B | recording gateway |
| M011D | `implementation/websocket/011d-offline-replay.md` | blocked | M011C | deterministic offline replay |
| M011E | `implementation/websocket/011e-candidate-regression-cli-and-diff.md` | blocked | M011D | regression/report/CLI/diff |
| M011F | `implementation/websocket/011f-hardening-qualification-and-closure.md` | blocked | M011E | hardening + M011 closure |
| M012 | `implementation/python/012-python-pytest-ecosystem.md` | blocked | M011 closure | PyO3 + pytest integration |
| M013 | `implementation/interception/013-explicit-proxy-and-optional-mitm.md` | blocked | M012 | explicit proxy + opt-in HTTP/1.1 MITM |
| M014 | `implementation/compatibility/014-compatibility-program.md` | blocked | M013 | umbrella compatibility stage |
| M014A | `implementation/compatibility/014a-har-and-migration.md` | blocked | M013 | HAR + migration |
| M014B | `implementation/compatibility/014b-http2-qualification.md` | blocked | M013, M010 | H2 qualification |
| M014C | `implementation/compatibility/014c-http3-feasibility-and-qualification.md` | blocked | M014B | H3 architecture/support decision |
| M014D | `implementation/compatibility/014d-grpc-and-fault-polish.md` | blocked | M014B, M010 | gRPC view + bounded faults |

### Current execution gate

M009 and M010/M010-C1 are closed. M010's qualifying corrective implementation
is `3bdd1359e00736737dd1610035d7e9f3e49822f1`, Actions run
`35864247624`.

M011 is decomposed. **M011A is the only dependency-ready implementation
task.** It must first migrate/qualify the now-published EggServe direct-server
crates, prove EggFetch 0.2.0 direct and Eggress-routed 101 upgrade ownership,
and avoid adopting Eggress 1.0.9 while its upstream route-isolation/metadata
release blockers remain open.

M011B–M011F and M012–M014D remain blocked by dependency order.

## Canonical planning documents

| Document | Purpose |
|---|---|
| `000-project-charter.md` | Product mission, scope, invariants, and non-goals |
| `001-architecture-and-boundaries.md` | Component topology and Eggstack ownership boundaries |
| `002-long-term-roadmap.md` | Milestones from bootstrap through compatibility |
| `003-qualification-and-release-strategy.md` | Evidence, CI, compatibility, and release gates |
| `004-research-and-compatibility-baseline.md` | External behavior baseline and Eggstack seams |

## Registry rules

A plan moves from **blocked** to **ready** only when every dependency is
closed or the plan explicitly permits an implemented-but-not-closed
dependency. A plan moves to **closed** only after implementation, required
tests/evidence, documentation updates, and a closure record are present.

For decomposed milestones, only the first unblocked subplan is executable; the
umbrella's readiness is not permission to skip subplan dependencies.

Hosted-CI-gated plans remain open until their required remote evidence is
green. Historical closure records remain immutable audit artifacts.
