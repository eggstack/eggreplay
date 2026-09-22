# C006 closure — Windows CI Repair and Hosted v0.1 Qualification

Status: closed

## Implementation

Qualifying implementation SHA: `9b9cc9552c8d1fdee8a64907a666ec2796c2f5d3`
(`fix(c006): close store file handles before staging rename (Stage A2)`).

Prior Stage-A commit on the same plan: `b59dca11012d618909d34580986249a327fd5bfc`
(cfg-hygiene fix; attempt 1, kept open per plan after its hosted run exposed
a real Windows test defect).

Code changes (all in `crates/eggreplay-store/src/lib.rs`, behavior-preserving;
Unix outcomes identical):

- `set_private_permissions`: split into `#[cfg(unix)]` / `#[cfg(not(unix))]`
  definitions; the non-Unix argument is intentionally `_path`. No fake
  filesystem work on Windows.
- `SessionWriter::finish`: flush/sync, then close the flow-log handle and
  scope the manifest handle so both close before the staging-directory
  rename. Windows denies renaming a directory with open files; Unix permits it.
- `RecordingSession::finish`: flow log held as `Mutex<Option<File>>` so
  finalization takes, flushes, syncs, and closes it before the rename;
  manifest handle scoped to close first. (The previous `drop(guard)` only
  released the lock, not the file.)
- `BodyWriter::finish` (`file: Option<File>` now) and
  `RecordingBodyWriter::finish`/Drop: flush/sync, close the staging file
  before any blob remove/rename; abort Drop closes before cleanup.
- Tests: `open_blob_rejects_symlinked_blob` is `#[cfg(unix)]` (symlink
  construction needs privileges unreliable on hosted Windows runners); the
  portable `symlink_metadata` rejection check is unchanged. `drop(handle)`
  before session-directory removal in `open_blob_validates_without_allocating_full_body`
  (Windows denies removing files with open handles).

No schema, fixture format, redaction, replay, recording, matcher, route, CLI,
exit-code, or dependency-boundary change. Transport ownership unchanged
(EggFetch outbound, EggServe inbound H1, Eggress optional listener-free
routing with narrow `pproxy-compat`).

## Verification

Local locked gates on the qualifying tree (all passed):

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Local totals: 8 core + 21 http unit + 12 http qualification + 8 store
(includes Unix symlink test) + 7 CLI integration = 56 tests, 0 failures.
Windows-cfg proxy: `cargo check` and `cargo clippy -D warnings` for
`--target x86_64-pc-windows-gnu` pass workspace-wide; `cargo clippy -p
eggreplay-store --target x86_64-pc-windows-msvc` passes.

## Hosted qualification evidence (Stage A2, green)

Actions run: `35774531684` on the qualifying SHA above.
URL: `https://github.com/eggstack/eggreplay/actions/runs/35774531684`

Every required job on that same SHA concluded successfully:

- `verify (ubuntu-latest, stable)` — success (56 tests: 7 CLI + 8 core + 21
  http + 12 qualification + 8 store).
- `verify (ubuntu-latest, 1.89.0)` — success (same 56-test profile; MSRV lane).
- `verify (macos-latest, stable)` — success (same 56-test profile).
- `verify (windows-latest, stable)` — success; fmt, check, and
  `clippy -D warnings` clean, and the full
  `cargo test --workspace --all-features --locked` step executed and passed:
  0 (binary) + 7 CLI integration + 8 core + 0 (core-version) + 21 http +
  12 qualification + 7 store = 55 tests, 0 failures.
- `dependency-boundary` — success (core/store transport-free, direct build
  without Eggress, narrow `pproxy-compat` only).

Superseded attempt 1: run `35769424825` on `b59dca1` passed
Linux stable/MSRV, macOS stable, and dependency-boundary, with Windows
passing fmt/check/Clippy but failing the test step (6 CLI contract tests,
`SessionWriter::finish` `PermissionDenied` code 5). That real
platform-specific defect was fixed by the qualifying SHA above; C006 was
kept open throughout, per plan.

## Windows count and exclusions

Windows reports 55 tests versus 56 on Linux/macOS. The single delta is the
correctly `cfg(unix)`-excluded `open_blob_rejects_symlinked_blob`
construction test: GitHub-hosted Windows runners cannot reliably create
symlinks without elevated Developer-Mode privileges. The production
`symlink_metadata` rejection path is identical portable code on all
platforms and remains covered for non-symlink inputs by the rest of the
Windows suite; no behavior Windows claims to support lost coverage. No
other test is excluded or weakened.

## Acceptance

- Windows stable is warning-clean under `-D warnings`: yes (hosted step + local
  Windows-target proxy).
- Windows executes and passes the workspace test step: yes (55/55).
- Linux stable passes: yes (56/56).
- Linux Rust 1.89 MSRV passes: yes (56/56).
- macOS stable passes: yes (56/56).
- dependency-boundary passes: yes.
- No behavioral or dependency-boundary regression: yes (store lifecycle
  fix only; boundary job green).
- Hosted green run on the implementation SHA linked above: yes.
- Registry no longer overstates a pending hosted qualification: this record
  flips C006 to `closed` alongside.

## Release-gate state

The v0.1 hosted release qualification gate is now **closed**.

## Unblocked next plan

M009–M014 remain `deferred`: no M009–M014 implementation plan documents exist
yet, so no milestone moves to `ready` in this change. Closing the v0.1 hosted
gate makes M009 (record-on-miss/pass-through, authored scenarios, templating)
eligible for future planning as the next milestone, with M010–M014 following
in roadmap order. No M009–M014 scope was pulled into C006.
