# Release process

Releases are manual. There is no publish automation, no release workflow, and
none should be added without an explicit request.

## Pre-release gate

```sh
cargo fmt --all -- --check \
  && cargo check --workspace --all-targets --all-features \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings \
  && cargo test --workspace --all-features
```

Then the hosted `verify`, `dependency-boundary`, `protocol-boundary`,
`dependency-policy`, `python-bindings`, `python-abi3-cross-version`, and
`interception` lanes must be green. The boundary lanes matter as much as the
test lane: a release that carries a protocol or codec into a default profile is
a boundary break even when every test passes.

Run a dependency/advisory review before publication. The hosted
`dependency-policy` job runs `cargo deny check advisories bans licenses sources`
against the resolved graph (policy and exceptions: [`deny.toml`](../deny.toml));
`cargo audit` remains a useful second opinion because it scans the whole
lockfile, including optional dependencies no feature graph activates.

## Repository security settings (operator actions)

These are GitHub repository/organization settings, not files in the tree, so a
review of the checkout cannot assert them. The repository owner must confirm
them for the qualifying revision:

- **Private vulnerability reporting** enabled, so the private disclosure route
  described by `SECURITY.md` actually exists.
- **Secret scanning and push protection** enabled for the repository.
- **Dependabot alerts and Dependabot security updates** enabled.
  `.github/dependabot.yml` schedules version updates only; it never auto-merges,
  and no workflow in this repository does.
- **Branch protection / ruleset for `main`** requiring a pull request and the
  `verify`, `dependency-boundary`, `protocol-boundary`, `dependency-policy`,
  `python-bindings`, `python-abi3-cross-version`, and `interception` checks, and
  blocking force pushes and branch deletion.
- **Organization access review** for Actions secrets and environments. The
  workflows here declare `contents: read` and use no secrets; confirm that is
  still true before granting one.

The closure record states which of these were verified for the qualifying
revision and which remain operator actions.

## Declared dependency revisions

Every Eggstack and TLS artifact is pinned **exactly** in the root `Cargo.toml`,
except `eggfetch-core`, which is a caret range held by `Cargo.lock`:

| Dependency | Pin |
|---|---|
| `eggserve-primitives` | `=0.2.2` |
| `eggserve-server` | `=0.4.0` |
| `eggserve-core` (optional, inbound H2 only) | `=0.4.0` |
| `eggress-outbound` (`pproxy-compat` only) | `=1.0.11` |
| `eggnet-tls` | `=0.2.0` |
| `rcgen` (interception only) | `=0.13.2` |
| `eggfetch-core` | `0.2.2` (caret; resolved by the lockfile) |

A transport version bump is a plan with a closure record, not a `cargo update`.

## Artifacts

- **Primary:** the standalone `eggreplay` binary for Linux, macOS, and Windows
  on stable, plus Linux on the Rust 1.89 MSRV.
- **Python:** wheels and an sdist via `maturin`, built as a separate abi3 lane.
  CI already produces and content-checks these; the qualified default wheel
  intentionally excludes the interception/CA capability and stays
  multiprotocol-free. See
  [`.skills/python-bindings.md`](../.skills/python-bindings.md).
- The checked-in golden fixtures under `crates/eggreplay-store/tests/fixtures/`
  are excluded from the published package.

## Before tagging

Record, in a closure record under `plans/closure/`:

- the exact qualifying revision (full SHA) and the hosted run URLs;
- the exact commands run, and the per-suite test counts;
- the supported matrix, including which capabilities are default, supported,
  experimental, unsupported, or deferred;
- known limitations and any pre-existing machine-specific failures;
- the deferred milestones, if any.

Then update `plans/registry.md` (both the table row and the current-execution
prose), `plans/README.md`, and the status section of `README.md`.

## Claim discipline

A release documents what the build actually does. Re-check the support matrices
against the feature flags in the released binary, and do not promote an
experimental tier to supported without the qualification evidence that
justifies it. When a documented limitation no longer holds, remove it in the
same change that fixes it. Record the milestone that closed it rather than
leaving a stale deferral in place.
