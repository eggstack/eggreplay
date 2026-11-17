# M020 — Rust Dependency Advisory Remediation

Status: implemented
Depends on: M019 (closed)

## Objective

Remediate the patched RustSec findings identified in the current Cargo.lock and
remove yanked resolutions where Cargo can select a compatible replacement,
without broad dependency refreshes or changes to EggReplay's transport and
feature boundaries.

## Scope

- Raise the exact `rustls` pin from 0.23.45 to the patched 0.23.46 release.
- Refresh the lockfile to patched compatible releases of `rustls-webpki`,
  `webpki-roots`, `tokio`, `rustix`, `crypto-bigint`, and `ctutils`.
- Replace the yanked `yoke-derive` resolution with a non-yanked compatible
  release.
- Keep all Eggstack transport pins, optional feature selections, product
  source, and the Rust 1.89 MSRV unchanged.
- Re-run the full locked workspace gate and `cargo audit`; record residual
  advisories precisely rather than suppressing or misclassifying them.

## Resolved versions

| Package | Before | Resolved |
|---|---:|---:|
| `rustls` | 0.23.45 | 0.23.46 |
| `rustls-webpki` | 0.103.15 | 0.103.17 |
| `webpki-roots` (1.x) | 1.0.9 | 1.0.12 |
| `tokio` | 1.53.1 | 1.54.1 |
| `rustix` | 1.1.5 | 1.1.6 |
| `crypto-bigint` | 0.7.5 | 0.7.11 |
| `ctutils` | 0.4.2 | 0.4.8 |
| `yoke-derive` | 0.8.3 (yanked) | 0.8.4 |

The lockfile changes were targeted; the broad 93-package dry-run refresh was
not applied. EggFetch, EggServe, Eggress, Tokio-Rustls, and protocol features
remain pinned/configured as before, apart from the direct rustls patch pin.

## Verification result

The full locked workspace gate passed: formatting, all-target/all-feature
`cargo check`, all-target/all-feature Clippy with `-D warnings`, and
all-feature tests with `--no-fail-fast`. The local dependency-boundary queries
confirmed the HTTP direct, default CLI, interception, and Python profiles still
exclude both `eggserve-core` and `eggserve-static`.

After the updates, `cargo audit` reports three vulnerabilities (`rsa 0.10.0-rc.18`
and `russh 0.62.7`, twice) in the inactive optional SSH dependency path, plus
the existing unmaintained `chrono` warning. No yanked crates from the targeted
update remain. Hosted CI has not been run; keep M020 open until hosted
qualification and its closure record are complete.

## Explicit advisory dispositions

- `russh 0.62.7` and its `rsa 0.10.0-rc.18` dependency are in the lockfile but
  not in the activated workspace graph. The current Eggress SSH adapter line
  constrains russh to 0.62.x; the inspected Eggress 1.0.12 adapter still has
  that constraint. Do not locally patch or vendor this transport dependency.
  Keep this finding open for an upstream Eggress release that adopts patched
  russh, or a separately planned transport change.
- `chrono` is reported unmaintained with no fixed release. Do not migrate date
  handling in this dependency-only corrective; evaluate `time`/`jiff` in a
  separately scoped change.
- Do not bump EggServe or Eggress merely to chase newer versions, and do not
  accept a broad all-lockfile update in place of the targeted security fixes.

## Verification

Run the required locked workspace gate:

```sh
cargo fmt --all -- --check \
  && cargo check --workspace --all-targets --all-features --locked \
  && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings \
  && cargo test --workspace --all-features --locked --no-fail-fast
```

Also run `cargo audit` against a refreshed RustSec database, `git diff --check`,
and inspect dependency trees to confirm the default and optional transport
boundaries remain unchanged.

## Acceptance

M020 may be marked implemented when all in-scope findings are on patched or
non-yanked versions, the remaining lockfile-only/unmaintained findings above are
accurately reported, the full locked workspace gate passes, and dependency
boundary checks show no unintended graph expansion. Keep M020 open until its
hosted CI qualification is green and a closure record is added under
`plans/closure/`.
