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

M001–M008 remain historical execution records. The original foundation
qualification was superseded by the corrective workstream.

## Corrective workstream

| ID | Plan | Status | Depends on | Corrective gate |
|---|---|---|---|---|
| C001 | `implementation/corrective/c001-lazy-replay-streaming.md` | closed | historical M001–M008 baseline | v0.1 requalification |
| C002 | `implementation/corrective/c002-concurrent-recording-session.md` | closed | historical M001–M008 baseline | v0.1 requalification |
| C003 | `implementation/corrective/c003-persistence-redaction-policy.md` | closed | historical M001–M008 baseline | v0.1 requalification |
| C004 | `implementation/corrective/c004-cli-eggress-contracts.md` | closed | historical M001–M008 baseline | v0.1 requalification |
| C005 | `implementation/corrective/c005-v0.1-requalification.md` | closed (local qualification artifact) | C001–C004 | superseded by hosted gate C006 |
| C006 | `implementation/corrective/c006-windows-hosted-ci-qualification.md` | ready | C001–C005 implementation baseline | **current v0.1 hosted release gate** |

### Current release-gate state

The v0.1 hosted release qualification gate is **open**.

C005 produced the expanded 56-test qualification suite and local green
evidence, but its own closure condition required the newly declared hosted
platform matrix to pass. The first hosted matrix run after C005,
GitHub Actions run `35763470419` on
`be5b3d076531a4ead94999eab2988e4c99e3f880`, failed only in
`verify (windows-latest, stable)`.

Observed Windows Clippy failures:

- `eggreplay-store/src/lib.rs:1058`: Unix-only
  `set_private_permissions(path)` leaves `path` unused on Windows.
- `eggreplay-store/src/lib.rs:1219`: the Unix-gated symlink test leaves
  `session` unused on Windows.

Linux stable, Linux Rust 1.89 MSRV, macOS stable, and
`dependency-boundary` passed. Windows tests did not run because the Clippy
step failed first.

C006 is therefore the only active implementation plan. Do not begin M009–M014
until C006 is closed unless the work is explicitly being done on a separate
future-feature branch.

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

These remain deferred and are not part of C006.

| ID | Scope | State |
|---|---|---|
| M009 | record-on-miss/pass-through modes, authored scenarios, deterministic templating | deferred |
| M010 | streaming timing profiles and SSE-aware inspection/replay | deferred |
| M011 | WebSocket message recording/replay over upgrade/tunnel seams | deferred |
| M012 | Python bindings and pytest/VCR-style integration | deferred |
| M013 | optional explicit-proxy/TLS-interception acquisition adapter | deferred |
| M014 | broader H2/H3 qualification, HAR import/export, compatibility polish | deferred |

## Registry rules

A plan moves from **blocked** to **ready** only when every dependency is
closed or the dependent plan explicitly permits an implemented-but-not-closed
dependency. A plan moves to **closed** only after implementation, required
test/evidence commands, documentation updates, and a closure record are
present.

For hosted-CI-gated plans, an implementation commit must remain open until the
required remote run completes successfully. Do not write a closure record that
assumes a future hosted run will pass.

Implementation agents must update this registry in the same change that
activates, blocks, implements, or closes a milestone. Historical closure
records remain immutable audit artifacts.
