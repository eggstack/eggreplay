# EggReplay Plan Registry

Status source of truth for implementation handoff.

## Current workstream

| ID | Plan | Status | Depends on | Release gate |
|---|---|---|---|---|
| M001 | `implementation/foundation/001-workspace-and-contracts.md` | closed | — | foundation |
| M002 | `implementation/foundation/002-flow-model-and-store.md` | closed | M001 | foundation |
| M003 | `implementation/recording/003-eggfetch-recording-path.md` | closed | M002 | v0.1 |
| M004 | `implementation/replay/004-eggserve-replay-server.md` | closed | M002 | v0.1 |
| M005 | `implementation/matching/005-matching-consumption-near-miss.md` | closed | M002, M004 | v0.1 |
| M006 | `implementation/regression/006-client-replay-and-diff.md` | closed | M002, M003, M005 | v0.1 |
| M007 | `implementation/integration/007-eggress-routing-and-cli.md` | closed | M003, M004, M006 | v0.1 |
| M008 | `implementation/security/008-v0.1-hardening-and-qualification.md` | ready | M001–M007 | v0.1 closure |

Only M001 is dependency-ready at repository bootstrap.

## Canonical planning documents

| Document | Purpose |
|---|---|
| `000-project-charter.md` | Product mission, scope, invariants, and non-goals |
| `001-architecture-and-boundaries.md` | Component topology and Eggstack ownership boundaries |
| `002-long-term-roadmap.md` | Milestones from bootstrap through optional interception |
| `003-qualification-and-release-strategy.md` | Evidence, CI, compatibility, and release gates |
| `004-research-and-compatibility-baseline.md` | External behavior baseline and current Eggstack seams |

## Subsystem roadmaps

- `subsystems/flow-storage-roadmap.md`
- `subsystems/matching-replay-roadmap.md`
- `subsystems/regression-roadmap.md`
- `subsystems/security-roadmap.md`
- `subsystems/protocol-interop-roadmap.md`

## Deferred milestones

These are roadmap commitments, not dependency-ready implementation plans. Detailed executable plans should be written only after v0.1 evidence exists.

| ID | Scope | State |
|---|---|---|
| M009 | record-on-miss/pass-through modes, authored scenarios, deterministic templating | deferred |
| M010 | streaming timing profiles and SSE-aware inspection/replay | deferred |
| M011 | WebSocket message recording/replay over upgrade/tunnel seams | deferred |
| M012 | Python bindings and pytest/VCR-style integration | deferred |
| M013 | optional explicit-proxy/TLS-interception acquisition adapter | deferred |
| M014 | broader H2/H3 qualification, HAR import/export, compatibility polish | deferred |

## Registry rules

A plan moves from **blocked** to **ready** only when every dependency is closed or the dependent plan explicitly permits an implemented-but-not-closed dependency. A plan moves to **closed** only after the implementation commit, required test/evidence commands, documentation updates, and closure record are present.

Implementation agents must update this registry in the same change that activates, blocks, implements, or closes a milestone. Historical closed plans stay in place.
