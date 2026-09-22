# C006 — Windows CI Repair and Hosted v0.1 Qualification

Status: ready
Depends on: C001–C005 implementation baseline
Corrective gate: v0.1 hosted release qualification

## Trigger

The first hosted run of the expanded C005 matrix, GitHub Actions run
`35763470419` on commit
`be5b3d076531a4ead94999eab2988e4c99e3f880`, did not satisfy the C005
closure condition.

Linux stable, Linux Rust 1.89 MSRV, macOS stable, and the dependency-boundary
job passed. Windows stable reached the workspace successfully but failed
`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
before tests ran.

Observed Windows failures:

- `crates/eggreplay-store/src/lib.rs:1058`:
  `set_private_permissions(path: &Path)` leaves `path` unused when the
  Unix-only body is cfg-elided.
- `crates/eggreplay-store/src/lib.rs:1219`:
  the symlink-rejection test creates a `session` value that is only consumed
  by Unix-gated assertions, so the Windows build sees it as unused.

These are portability/cfg hygiene defects, not evidence that the core
architecture failed, but Windows remains unqualified because the test step was
skipped after Clippy failed.

## Objective

Make the existing v0.1 implementation warning-clean on Windows, execute the
full Windows test lane, and close the hosted qualification gate only after a
green declared-matrix GitHub Actions run exists.

Do not broaden this plan into M009–M014 feature work.

## Required implementation

### 1. Fix platform-specific code structurally

Prefer cfg-correct definitions and scopes rather than blanket
`#[allow(unused_variables)]`.

For `set_private_permissions`, use one of these equivalent shapes:

- separate `#[cfg(unix)]` and `#[cfg(not(unix))]` function definitions, with
  the non-Unix argument intentionally named `_path`; or
- another equally explicit structure where each platform compiles only the
  code and parameters it uses.

Do not add fake filesystem work on Windows merely to consume the variable.

For the symlink test:

- scope Unix-only setup/assertions under `#[cfg(unix)]` so Windows does not
  construct values used only by Unix code; or
- add a genuinely meaningful Windows symlink test only if it can run reliably
  on standard GitHub-hosted Windows runners without requiring elevated
  developer-mode privileges.

Do not weaken the production symlink rejection check.

### 2. Audit the surrounding store module for the same class

Before pushing, inspect `eggreplay-store` and the rest of the workspace for
other variables/imports/functions whose use disappears under
`cfg(unix)`/`cfg(windows)`.

The pass must remain narrow: only fix compile/lint portability debt actually
exposed by the supported matrix.

### 3. Preserve behavior

No schema, fixture format, redaction, replay, recording, matcher, route, CLI,
or exit-code behavior may change as part of this plan unless a Windows test
proves a platform-specific defect.

The dependency ownership rules remain unchanged:

- EggReplay core/store stay transport-free;
- EggFetch owns outbound HTTP/TLS;
- EggServe owns inbound H1;
- Eggress remains optional listener-free routing with narrow
  `pproxy-compat`.

## Verification before push

Run the ordinary locked workspace gates on the implementation platform:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

If a native Windows environment is available, run the same four commands
there. A local non-Windows run does not substitute for the hosted Windows
lane.

## Hosted qualification procedure

This milestone intentionally requires two stages.

### Stage A — implementation commit

Push the portability fix with C006 still **active/implemented**, not closed.
Let the resulting GitHub Actions run complete.

Required jobs:

- `verify (ubuntu-latest, stable)`
- `verify (ubuntu-latest, 1.89.0)`
- `verify (macos-latest, stable)`
- `verify (windows-latest, stable)`
- `dependency-boundary`

Every required job must conclude successfully.

The Windows job must reach and pass
`cargo test --workspace --all-features --locked`; merely passing Clippy is
not sufficient.

### Stage B — evidence closure

Only after Stage A is green:

1. record the qualifying implementation SHA and Actions run URL/ID;
2. record Windows test counts/results and any platform-specific exclusions;
3. confirm Linux stable/MSRV, macOS stable, Windows stable, and
   dependency-boundary all passed on that same implementation SHA;
4. create `plans/closure/c006-windows-hosted-ci-qualification.md`;
5. update `plans/registry.md` to C006 `closed`;
6. state that the v0.1 hosted release qualification gate is now closed.

The Stage-B commit should be documentation/registry-only. Its code tree is
therefore identical to the already qualified Stage-A implementation tree.

If the Stage-A run exposes any real Windows test failure, keep C006 open,
document the failure in the implementation plan/registry as needed, fix only
that defect, and repeat Stage A.

## Expected Windows evidence

At present the portable suites total 56 tests on Linux/macOS. Windows may have
a different exact count if a Unix-only symlink-construction test is correctly
cfg-excluded. The closure record must report the actual Windows count instead
of assuming 56.

Any excluded test must have a platform-specific reason and must not remove
coverage of a behavior that Windows claims to support.

## CI hygiene

The current run also emits the GitHub-hosted warning that
`actions/checkout@v4` targets deprecated Node.js 20. This is not the cause of
the failure. Updating to the current compatible checkout action may be included
only if it is a trivial workflow-only change and does not obscure the Windows
qualification diff; otherwise defer it to normal maintenance.

## Acceptance

C006 closes only when all of the following are true:

- Windows stable is warning-clean under `-D warnings`;
- Windows executes and passes the workspace test step;
- Linux stable passes;
- Linux Rust 1.89 MSRV passes;
- macOS stable passes;
- dependency-boundary passes;
- no behavioral or dependency-boundary regression is introduced;
- a hosted green run on the implementation SHA is linked in the closure
  record;
- the registry no longer overstates a pending hosted qualification.

Until then, the current implementation may be described as locally qualified
and Linux/macOS hosted-green, but not as fully closed across the declared
v0.1 platform matrix.

Closure record:
`plans/closure/c006-windows-hosted-ci-qualification.md`.
