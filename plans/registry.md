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
`35774531684`. The docs-only C006 closure head also passed the full matrix.

## Forward implementation queue

ADR 0005 (`adrs/0005-versioned-session-extensions.md`) is the architecture
basis for M009–M011.

| ID | Plan | Status | Depends on | Primary result |
|---|---|---|---|---|
| M009 | `implementation/stateful/009-stateful-dynamic-replay.md` | **ready** | C006 | record modes, scenarios, deterministic templates |
| M010 | `implementation/streaming/010-streaming-timing-and-sse.md` | blocked | M009 | stream events, timing, SSE |
| M011 | `implementation/websocket/011-websocket-semantic-record-replay.md` | blocked | M010 | WebSocket semantic capture/replay |
| M012 | `implementation/python/012-python-pytest-ecosystem.md` | blocked | M011 | PyO3 + pytest integration |
| M013 | `implementation/interception/013-explicit-proxy-and-optional-mitm.md` | blocked | M012 | explicit proxy + opt-in HTTP/1.1 MITM |
| M014 | `implementation/compatibility/014-compatibility-program.md` | blocked | M013 | umbrella compatibility stage |
| M014A | `implementation/compatibility/014a-har-and-migration.md` | blocked | M013 | HAR + migration |
| M014B | `implementation/compatibility/014b-http2-qualification.md` | blocked | M013, M010 | H2 qualification |
| M014C | `implementation/compatibility/014c-http3-feasibility-and-qualification.md` | blocked | M014B | H3 architecture/support decision |
| M014D | `implementation/compatibility/014d-grpc-and-fault-polish.md` | blocked | M014B, M010 | gRPC view + bounded faults |

Only M009 is dependency-ready. Later plans are intentionally written now for
handoff clarity but must not be activated early. If implementation evidence
invalidates a later plan assumption, update that plan before changing code.

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

Hosted-CI-gated plans remain open until their required remote evidence is
green. Historical closure records remain immutable audit artifacts.
