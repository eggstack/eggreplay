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

M001–M008 remain closed as historical execution records. A post-closure audit of head `951991e4e4d726a029e703a406d31e4350230153` found acceptance/qualification gaps, so the original v0.1 support claims are **not the current release gate** until the corrective workstream below closes. Do not rewrite the original closure records; C005 will issue a superseding corrective closure.

## Active corrective workstream

| ID | Plan | Status | Depends on | Corrective gate |
|---|---|---|---|---|
| C001 | `implementation/corrective/c001-lazy-replay-streaming.md` | closed | historical M001–M008 baseline | v0.1 requalification |
| C002 | `implementation/corrective/c002-concurrent-recording-session.md` | closed | historical M001–M008 baseline | v0.1 requalification |
| C003 | `implementation/corrective/c003-persistence-redaction-policy.md` | closed | historical M001–M008 baseline | v0.1 requalification |
| C004 | `implementation/corrective/c004-cli-eggress-contracts.md` | closed | historical M001–M008 baseline | v0.1 requalification |
| C005 | `implementation/corrective/c005-v0.1-requalification.md` | closed | C001–C004 | v0.1 corrective closure |

C001–C004 are intentionally parallelizable but may touch shared HTTP/store/CLI seams. Agents must rebase/merge carefully and preserve the ownership decisions in ADRs 0001–0004. C005 is the only plan authorized to reassert the v0.1 qualification gate.

## Audit findings driving C001–C005

- replay fixture load eagerly materializes all stored request/response bodies and selected responses are cloned into byte bodies;
- gateway recording holds a Tokio session-writer mutex across the complete awaited upstream transaction, serializing concurrent requests;
- recording hardcodes the default redactor after body publication, so configured structured-body redaction is not persistence-safe;
- Eggress Dialer exists but is not exposed by CLI execution paths;
- CLI errors all exit 1, JUnit is aggregate-only, and `inspect --bodies` is placeholder behavior;
- initial hosted CI is green on Linux stable/MSRV but the workspace contains only five tests and does not prove the documented qualification matrix.

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

## Deferred feature milestones

These remain deferred. Corrective work must not pull them into v0.1.

| ID | Scope | State |
|---|---|---|
| M009 | record-on-miss/pass-through modes, authored scenarios, deterministic templating | deferred |
| M010 | streaming timing profiles and SSE-aware inspection/replay | deferred |
| M011 | WebSocket message recording/replay over upgrade/tunnel seams | deferred |
| M012 | Python bindings and pytest/VCR-style integration | deferred |
| M013 | optional explicit-proxy/TLS-interception acquisition adapter | deferred |
| M014 | broader H2/H3 qualification, HAR import/export, compatibility polish | deferred |

## Registry rules

A plan moves from **blocked** to **ready** only when every dependency is closed or the dependent plan explicitly permits an implemented-but-not-closed dependency. A plan moves to **closed** only after implementation, required test/evidence commands, documentation updates, and a closure record are present.

Implementation agents must update this registry in the same change that activates, blocks, implements, or closes a corrective milestone. Historical closure records remain immutable audit artifacts.
