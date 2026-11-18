# M021E — Supply-Chain, Dependency, and Security Governance

Status: **implemented** (workflow, license, reporting, and fuzz-lane work
landed; the advisory gate is green with reasoned exceptions; hosted CI and
closure record pending)
Depends on: M020 (implemented, not closed) for a green advisory baseline. This
plan explicitly permits the implemented-but-not-closed dependency for workflow
hardening, license policy, and documentation; it must not mark the advisory
gate green by silently ignoring findings.
Closure record: `plans/closure/m021e-supply-chain-and-security-governance.md`
(not created; requires hosted qualification and the operator checklist)

## Why this plan exists

The audit found no exploited dependency, but the repository has no
CI-enforced answer to "did a new advisory, yanked crate, or non-permissive
license enter the graph?" It also pins GitHub Actions to movable tags and
relies on the repository's default token permissions. The current advisories
are already known and dispositioned in M020, but they are tracked in prose, not
gated.

This subplan makes dependency and workflow risk visible and reviewable. It does
not perform a broad dependency refresh, does not change transport ownership,
and does not migrate `chrono` in this milestone.

## Audit evidence

- `Cargo.lock:2801` (`rsa 0.10.0-rc.18`), `:2820` (`russh 0.62.7`), and
  `:414` (`chrono 0.4.45`).
- `cargo audit` reports RUSTSEC-2023-0071 (rsa, 5.9 medium),
  RUSTSEC-2026-0495 (russh), RUSTSEC-2026-0492 (russh, 7.5 high), and
  RUSTSEC-2026-0543 (chrono, unmaintained). `cargo tree --workspace
  --all-features -i russh`/`-i rsa` shows neither is in the activated graph;
  they are lockfile-only through the optional Eggress SSH path.
- `.github/workflows/ci.yml` has no top-level `permissions:` block and uses
  tag/branch refs (`actions/checkout@v4`, `dtolnay/rust-toolchain@stable`,
  `astral-sh/setup-uv@v6`).
- `.github/workflows/python-wheels.yml:12` already declares
  `permissions: contents: read`; that is the model for the main workflow.
- No `deny.toml`, no `.github/dependabot.yml`, and no `cargo audit`/`cargo
  deny` job.
- `SECURITY.md` asks for private reports without naming a channel.
- No repository-local license allow-list; `cargo deny check licenses` fails by
  default because the allowed set is empty, not because a dependency is
  non-compliant.

## Track A — Workflow least privilege and pinning

### Required behavior

`ci.yml`:

- add a top-level `permissions: contents: read` (and any narrower per-job
  override only if a job genuinely needs it);
- pin every third-party action to a full commit SHA, with a trailing comment
  naming the release (`# vX.Y.Z`), including `actions/checkout`,
  `dtolnay/rust-toolchain`, `actions/setup-python`, `astral-sh/setup-uv`,
  `actions/upload-artifact`, and `actions/download-artifact`;
- keep `on: pull_request` and `on: push`; do not introduce
  `pull_request_target`;
- keep the existing matrix and boundary lanes unchanged. This is a
  permissions/pinning change, not a test-matrix change.

`python-wheels.yml`:

- pin the same action families to SHAs;
- keep `permissions: contents: read`;
- no new secret or write scope.

Add `.github/dependabot.yml`:

- `package-ecosystem: github-actions`, weekly, grouped;
- `package-ecosystem: cargo`, weekly, grouped, with a limit that avoids a
  broad lockfile update in one PR;
- do not auto-merge; a dependency PR still passes the full gate and the
  advisory job.

A tag-pinned action is a supply-chain trust decision. SHA pinning does not
prove the action is safe, but it makes the reviewed revision immutable and
makes an update visible in review.

## Track B — Dependency, license, and advisory gates

### `deny.toml`

Add a checked-in policy covering:

- **Advisories:** fail on new vulnerabilities; yanked crates denied. The three
  known findings (`rsa`, `russh`, `chrono`) may be recorded as explicit,
  reasoned exceptions while M020 is open. Each exception must name the
  advisory ID, the reason (lockfile-only/optional SSH path or unmaintained
  with no fixed release), the owner, and the condition for removal. Do not use
  a blanket severity threshold that hides a new High.
- **Bans:** keep the existing duplicate-crate behavior unless a duplicate is
  a security concern; do not add broad bans that would force a transport bump.
- **Licenses:** allow the permissive licenses actually present after a legal
  review (expected families: MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause,
  ISC, Unlicense, Zlib, Unicode-3.0). Deny unknown or copyleft licenses unless
  an explicit, documented exception is approved. Derive the initial list from
  `cargo metadata`/`cargo deny check licenses` output; do not guess.
- **Sources:** crates.io only. A git or path source requires an explicit
  exception.

### CI job

Add a `dependency-policy` job that runs:

```sh
cargo deny check advisories bans licenses sources
```

- Pin the `cargo-deny` version (or the action SHA) so a tool update is a
  reviewed change.
- Run on pull requests and on a weekly schedule (a scheduled run catches new
  advisories even when the tree is unchanged).
- The job uses `contents: read` only.
- The job output must show the known exceptions and their reasons; do not
  suppress the advisory text entirely.
- Keep the existing `dependency-boundary` and `protocol-boundary` lanes
  separate from this policy job.

### M020 sequencing

M020 is implemented locally but not closed (hosted CI pending). Before M021E
enables a failing advisory gate:

1. record the current advisory baseline in `deny.toml` with reasons;
2. run the job with `continue-on-error` for one PR if a non-blocking baseline
   is needed;
3. flip it to failing once the baseline is accepted and M020's hosted
   qualification is green;
4. when Eggress adopts patched `russh` and `rsa`, remove those exceptions in
   the same change that updates the lockfile;
5. when `chrono` is replaced, remove its exception with the migration plan.

Do not mark the job green by adding a broad `ignore = ["*"]` or by lowering
the advisory severity threshold.

## Track C — `chrono` evaluation (decision only)

`chrono` is unmaintained (RUSTSEC-2026-0543). It is used across core, store,
HAR, HTTP, and interception; `time` is already a dependency in the
interception crate.

Required output: a short decision document (an ADR or a section in the
closure record) comparing:

- migrating timestamp/date handling to `time` (already present) versus `jiff`;
- the affected public types (`OffsetDateTime`, parsing/formatting, CA validity,
  report timestamps);
- MSRV, serialization, and timezone behavior;
- the test and CI cost;
- the compatibility impact on fixture JSON and public Rust APIs.

No migration code in M021E. If the decision is "migrate", create a separate
plan with its own closure record. Until then, keep the documented `chrono`
exception in the advisory policy.

## Track D — Security reporting and fixture-handling governance

### `SECURITY.md`

Expand the policy to include:

- a real private reporting channel (GitHub private vulnerability reporting
  link, or a maintainer security address) instead of "report privately";
- supported versions and the disclosure/response expectations;
- that `.eggr` fixtures and exported HAR files are sensitive, that redaction is
  policy-based, and that opaque bodies are not semantically redacted;
- the Windows ACL caveat and the `0700`/`0600` Unix behavior;
- the `--overwrite` fixture-safety rule from M021B (link the CLI docs).

### Repository hygiene

- Verify `*.har` is present in `.gitignore` (M021A owns the change). HAR
  exports contain recorded bodies and are not protected by the `.eggr`
  directory mode.
- Reference the redaction limits in `docs/non-goals.md` and the interception
  threat model rather than duplicating a second, drifting policy.
- Do not add a separate security guide unless its content is genuinely
  distinct from `SECURITY.md` and `docs/non-goals.md`; a new document that
  repeats the same caveats is a drift risk.

### Operator checklist (outside the tree)

Record in `SECURITY.md` or `docs/release-process.md` that the repository owner
must enable and review:

- GitHub private vulnerability reporting;
- secret scanning and push protection;
- Dependabot alerts and security updates;
- branch protection for `main`, including required status checks for the new
  policy job;
- organization-level access review for the Actions secrets (none are currently
  used by these workflows).

These settings cannot be asserted by a repository file. The closure record
must state which were verified for the qualifying revision and which remain
operator actions.

## Track E — Fuzz smoke CI (coordination with M021C)

M021C adds parser fuzz harnesses. M021E adds the CI lane only if the harnesses
exist and the toolchain is stable:

- a nightly or manual job runs each target for a bounded time (for example
  60 seconds) and uploads the corpus as an artifact;
- the job is not a required PR gate until it has a stable history;
- the job must not use repository secrets or write permissions;
- if M021C records a blocker (nightly-only toolchain or dependency), M021E
  records the same blocker and does not add a failing job.

## Acceptance

- `ci.yml` and `python-wheels.yml` declare least-privilege permissions, and
  every third-party action is SHA-pinned.
- Dependabot is configured for Actions and Cargo, with no auto-merge.
- `deny.toml` exists, is reviewed, and the `dependency-policy` job passes with
  only the documented M020/chrono exceptions.
- The policy job fails on a newly introduced advisory in a local fixture test
  (for example, a temporary test advisory or a deliberate policy check), so
  the gate is proven rather than assumed.
- The license allow-list is derived from the actual graph and documented; no
  dependency is silently exempted.
- The chrono decision document exists; either a follow-up plan is registered or
  the acceptance is recorded with a review date.
- `SECURITY.md` names a private reporting channel and the supported versions.
- `*.har` is ignored; the fixture/HAR sensitivity guidance is updated.
- Full locked workspace gate and the required hosted CI are green.
- The closure record lists which operator-only settings were verified and which
  remain outside the tree.

## Non-goals

- A broad dependency refresh or a transport/Eggress bump.
- Auto-merging Dependabot PRs.
- A full fuzz campaign or a claim that fuzzing proves absence of vulnerabilities.
- Migrating `chrono` in this milestone.
- Changing branch protection from code; that is an operator action.

## Verification

Run the required locked workspace gate from `AGENTS.md`, plus:

```sh
cargo deny check advisories bans licenses sources
```

Confirm the workflow YAML is syntactically valid and every `uses:` entry is a
full commit SHA (review or `actionlint` if available). The provenance of the
policy job — pinned tool version/action and `contents: read` permission — is
part of the review. Record the exact advisory baseline, the job result, and
which operator-only settings were checked in the closure record.

## Implementation record (2026-11-18)

Implemented locally; not closed. `cargo deny check advisories bans licenses
sources` exits `0` with exactly the four documented exceptions; `actionlint
-shellcheck= .github/workflows/*.yml` exits `0`; every `uses:` is a full
40-hex SHA with a release comment.

- **Track A.** `ci.yml` has top-level `permissions: contents: read`, a weekly
  `schedule`, and all 19 `uses:` pinned (checkout v4.4.0, rust-toolchain stable
  tip, setup-python v5.6.0, setup-uv v6.8.0, upload/download-artifact). The
  boundary lanes are otherwise unchanged and `pull_request_target` is absent.
  `python-wheels.yml` pins the same families and keeps `contents: read`.
  `.github/dependabot.yml` configures grouped weekly Actions and Cargo updates
  with no auto-merge.
- **Track B.** `deny.toml` denies vulnerabilities and yanked crates, allows the
  nine permissive license expressions actually present (including
  `CDLA-Permissive-2.0` on `webpki-roots`, documented inline), keeps duplicate
  crates reported rather than banned, and restricts sources to crates.io. The
  `dependency-policy` job installs `cargo-deny 0.20.2` and runs the four
  checks. The four exceptions (`rsa` RUSTSEC-2023-0071, `russh`
  RUSTSEC-2026-0495/0492, `chrono` RUSTSEC-2026-0543) each name an owner and a
  removal condition. A negative control (removing the `chrono` exception)
  fails the gate.
- **Track C.** `docs/chrono-dependency-decision.md` records a `time`
  recommendation: three call sites, no public `chrono` type, `time` already in
  the graph, MSRV 1.88 compatible with the 1.89 workspace. Migration is
  deferred to a separate plan with a 2027-02-01 review date.
- **Track D.** `SECURITY.md` now names GitHub private vulnerability reporting,
  supported versions, response expectations, fixture/HAR sensitivity, the
  symlink residual, Windows ACL and Unix mode behavior, the `--overwrite`
  contract, and listener exposure. `docs/release-process.md` carries the
  operator checklist.
- **Track E.** `.github/workflows/fuzz.yml` runs each of the three M021C targets
  for 60 seconds weekly and on manual dispatch, uploads corpus/crash artifacts,
  uses nightly only in that workflow, and is not a required PR gate.
  `docs/testing.md` points at it.

### Note on inert exceptions

Because cargo-deny checks the resolved graph, the `rsa`/`russh` entries
currently suppress nothing and emit `advisory-not-detected` warnings. They are
kept so the disposition is reviewable and so a future transport change that
makes them reachable must remove them explicitly. The `deny.toml` comment
records that a disappearing warning means an ignored advisory became
reachable.

### Remaining for closure

Commit and push; confirm the hosted `dependency-policy` job and the operator
settings (private reporting, secret scanning, Dependabot alerts, branch
protection) on the qualifying revision; create
`plans/closure/m021e-supply-chain-and-security-governance.md` with the advisory
baseline, job result, and the verified/outstanding operator split. Removing the
`rsa`/`russh` exceptions still depends on M020 and the planned Eggress
transport change; removing `chrono` depends on the separate migration plan.
