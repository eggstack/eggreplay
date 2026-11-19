# M021 — Security Audit Remediation Program

Status: **implemented** (M021A–M021E implemented on the local tree; hosted CI
and the closure records are pending)
Depends on: none for opening; M021A–M021E for closure. M021E additionally
coordinates with M020 (implemented, not closed).
Closure record: `plans/closure/m021-security-audit-remediation.md` (not
created: closure requires hosted qualification of the committed revision).

## Why this program exists

A read-only security audit on 2026-11-17 reviewed the seven product crates,
the locked dependency graph, CI workflows, fixture parsing, recording and
replay paths, and the interception stack. It found two High, three Medium, six
Low, and a set of residual/process risks.

The findings are not one subsystem defect. They fall into five independent
security properties:

1. **Confidentiality at rest** — what a fixture persists after redaction.
2. **Integrity of user data** — what a destructive command is allowed to
   delete.
3. **Availability under hostile input** — how trusted a fixture, HAR, or
   manifest is before allocation.
4. **Network trust boundaries** — which listeners and forwarded headers an
   operator can expose.
5. **Supply-chain and governance** — how dependency risk and workflow
   privilege are gated.

Bundling them under one milestone gives the audit a single disposition
ledger. It does **not** make them one commit or one release. Each subplan is
independently executable, independently testable, and independently
shippable.

## Finding-to-subplan map

| Audit finding | Severity | Subplan | Track |
|---|---|---|---|
| `stream-events.json` persists request/response trailer values without redaction | High | M021A | A |
| `--overwrite` / `re-record` / `migrate --to` can delete an arbitrary directory; deletion precedes validation | High | M021B | A |
| Unbounded manifest, JSONL-line, and HAR reads permit memory-exhaustion DoS | Medium | M021C | A |
| UTF-8 truncation panics in HAR import; latent same-class panic in `FlowError` | Medium | M021C | B |
| Default redaction misses common secret headers/query keys; WebSocket payloads raw by default | Medium | M021A | C (header defaults fixed; query/WebSocket accepted with warnings) |
| WebSocket JSON-pointer redaction fails open on non-JSON text | Low–Medium | M021A | B |
| HAR export and loss report are created world-readable | Low | M021A | D |
| HAR comments and interop provenance are outside the redaction policy | Info | M021A | E |
| `serve`/`record` accept non-loopback listeners without the proxy's opt-in gate | Low | M021D | A |
| Regression forwards fixture-supplied `Host`/hop-by-hop headers | Low | M021D | B |
| CI supply-chain hardening gaps; no dependency-audit/deny lane | Low | M021E | A |
| Known lockfile advisories remain, ungated; `chrono` unmaintained | Low | M021E | C |
| Staging residue after failed operations | Info | M021B | C |
| Symlink TOCTOU in `open_blob` | Info | M021C | C |
| Windows permission claims | Info | M021C | D |
| Route credential redaction edge case | Info | M021D | C |
| No private security-reporting channel; no license policy | Info | M021E | B/D |
| Parser fuzzing absent | Info | M021C | E |

## Program rules

- **No broad dependency refresh.** Transport ownership and feature boundaries
  stay as they are. A transport bump remains a separate plan with a closure
  record.
- **One logical change per commit.** Do not mix redaction, deletion safety,
  parser bounds, listener policy, and CI in one commit.
- **Behavior changes are documented.** Several fixes deliberately change what
  a command accepts or persists. Each subplan names the user-visible change,
  its compatibility impact, and the documentation it must update.
- **No silent fixture rewriting.** Existing fixtures are immutable while open.
  Remediation of already-recorded data is an explicit, opt-in operation, never
  a side effect of `Session::open`.
- **A regression test must fail on the pre-fix tree and pass on the post-fix
  tree.** A test that only asserts the new behavior does not prove the defect.
- **Every finding gets a disposition.** The umbrella closure record must list
  each row above as `fixed`, `accepted with rationale`, or `deferred with
  owner and trigger`. Silence is not a disposition.
- **No closure on source presence.** Each subplan closes on its tests, the
  full locked workspace gate, the required hosted CI evidence, and a closure
  record under `plans/closure/`.

## Sequencing

1. **M021B** (data loss) and **M021A** (confidentiality) first. Either may land
   first; they touch disjoint code.
2. **M021C** next. The parser bounds are independent of the first two, but the
   truncation fixes are best reviewed together with the bound work.
3. **M021D** next. It depends on the M021A warning plumbing only where a
   listener warning needs the same CLI warnings channel; it does not require
   M021A to close first.
4. **M021E** can start workflow hardening immediately. Its advisory gate is
   sequenced after M020's hosted qualification, because M020 already carries
   the accepted `rsa`/`russh`/`chrono` baseline. The plan explicitly permits
   the implemented-but-not-closed M020 dependency for workflow and license
   work; the failing advisory gate is enabled only when the baseline is
   recorded.

M021 itself remains open until all five subplans close or are explicitly
deferred. It does not authorize skipping a subplan's own dependencies.

## Program acceptance

- M021A–M021E are closed, or a deferred subplan has a recorded owner and
  trigger.
- No High finding remains unfixed. Every Medium, Low, and Info finding is
  either fixed or carries an accepted-risk rationale in the umbrella closure
  record.
- The full locked workspace gate passes on the final tree:
  `cargo fmt --all -- --check`,
  `cargo check --workspace --all-targets --all-features --locked`,
  `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`,
  and `cargo test --workspace --all-features --locked --no-fail-fast`.
- The four CI boundary lanes remain green: `dependency-boundary`,
  `protocol-boundary`, `python-bindings`, and `python-abi3-cross-version`.
- Hosted CI is green on the qualifying revision for each subplan.
- A fresh audit of the final tree confirms the two High and three Medium
  findings are no longer reproducible; the accepted residuals are listed in
  the closure record.
- `plans/registry.md` and its current-execution prose are updated.

## Out of scope

- New product capability, protocol tiers, or transport changes.
- The `chrono` replacement implementation. M021E evaluates and schedules it;
  the migration itself needs its own plan and qualification.
- Repository settings that are not expressible in the tree: branch
  protection, secret scanning, private vulnerability reporting enablement, and
  organization-level policy. M021E records the operator checklist.
- A full penetration test or protocol fuzzing campaign. M021C adds fuzz
  harnesses and a bounded smoke lane; it does not claim exhaustive coverage.

## Closure evidence required

- `plans/closure/m021a-redaction-completeness.md`
- `plans/closure/m021b-destructive-path-safety.md`
- `plans/closure/m021c-untrusted-input-hardening.md`
- `plans/closure/m021d-listener-and-request-boundaries.md`
- `plans/closure/m021e-supply-chain-and-security-governance.md`
- `plans/closure/m021-security-audit-remediation.md` — the disposition ledger,
  the final gate results, and the residual-risk register.

## Local implementation record (2026-11-18)

All five subplans are implemented on the worktree. Hosted qualification is
pending, and no closure record exists yet. Each subplan remains `implemented`,
never `closed`, until the committed revision passes the hosted lanes above.

### Local gate

The exact required gate ran on the final worktree:

```sh
cargo fmt --all -- --check \
  && cargo check --workspace --all-targets --all-features --locked \
  && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings \
  && cargo test --workspace --all-features --locked --no-fail-fast
```

Result: exit `0`, **591 passed, 0 failed**. `cargo deny check advisories bans
licenses sources` passes with the four documented exceptions, and
`actionlint -shellcheck= .github/workflows/*.yml` is clean.

### Finding dispositions

| Finding | Disposition | Evidence |
|---|---|---|
| `stream-events.json` persists trailer values | **fixed** | `recording::tests::stream_event_trailers_never_persist_values_on_any_file`, `mitm_never_persists_stream_event_trailer_values`; manual CLI recording persisted no sentinel and annotated `("redaction","stream-trailers-values-suppressed")` |
| `--overwrite` can delete an arbitrary directory | **fixed** | `m021b_destructive_paths` suite; manual `har import --overwrite` on a non-fixture directory exited `2` with `keep.txt` intact |
| Unbounded manifest/JSONL/HAR reads | **fixed** | `read_bounded_bytes` (16 MiB manifest), bounded line reader, CLI `take(MAX+1)` HAR read; unit and integration tests |
| UTF-8 truncation panics | **fixed** | shared `truncate_utf8_boundary`; crafted 600-byte `€` comment imports as valid UTF-8 (513-byte truncated annotation), never exit `101` |
| Thin default header redaction | **fixed for headers** | ten-header `DEFAULT_SENSITIVE_HEADERS`; `X-Api-Key` sentinel test and matcher-wildcard test |
| Query-key and WebSocket raw defaults | **accepted residual** | value-free warnings only; documented in `docs/configuration.md` |
| WebSocket JSON-pointer fail-open | **fixed** | non-JSON text becomes `[REDACTED]`/`WholeMessage`; close reasons withheld under any WS policy |
| World-readable HAR export | **fixed** | Unix `0600` at creation for HAR and loss reports; permission test |
| HAR comments/provenance outside redaction | **accepted residual** | documented in `docs/har-interchange.md` / `docs/eggr-schema.md`; boundary pinned by test |
| Non-loopback `serve`/`record`/Python listeners | **fixed** | shared always-compiled `check_listener_bind`; CLI and pytest suites; manual exit `2` |
| Regression forwards fixture `Host`/hop-by-hop | **fixed** | shared `is_driver_controlled_header`; `Host` derived from target in both candidate paths |
| CI supply-chain gaps; no dependency lane | **fixed** | least-privilege `permissions`, 19+13 SHA-pinned actions, Dependabot, `deny.toml`, `dependency-policy` job |
| Ungated advisories; unmaintained `chrono` | **gated, migration deferred** | four reasoned `deny.toml` exceptions; `docs/chrono-dependency-decision.md` recommends `time` and schedules a separate plan |
| Staging residue after failures | **fixed on error paths** | `SessionWriter::drop` + `StagingGuard`; crash/`SIGKILL` residue accepted and documented |
| Symlink TOCTOU in `open_blob` | **accepted residual** | `docs/eggr-schema.md` and `SECURITY.md` state the race, the hash fail-closed behavior, and the immutability contract |
| Windows permission claims | **accepted residual** | no-op documented consistently in `SECURITY.md`, `docs/eggr-schema.md`, `docs/interception-threat-model.md`; hosted Windows lane still owed |
| Route credential redaction edge case | **fixed** | span scanner; 13 tests including `__` and multiple `@` cases |
| No private reporting channel; no license policy | **fixed** | `SECURITY.md` names GitHub private reporting; `deny.toml` license allow-list |
| Parser fuzzing absent | **fixed (bounded)** | three `cargo-fuzz` targets; 10-second local smoke each; weekly 60-second lane `.github/workflows/fuzz.yml` |

### Deliberate deviations

1. **M021A crafted HAR.** The plan predicted a typed error for the crafted
   600-byte `€` comment. The post-fix behavior is a successful import with a
   valid UTF-8 truncation (170 `€` + `…`, 513 bytes). The document is valid and
   the defect was the panic; forcing an error would be a new truncation policy,
   not a safety fix. The test asserts "not `101`" and valid UTF-8 instead.
2. **M021B `./name`.** The classifier rejects `..` components and roots, but
   accepts a leading `./` because it resolves to the same directory and carries
   no deletion risk. Pinned by
   `transaction_tests::current_dir_prefix_is_an_ordinary_spelling`.
3. **M021E weekly schedule.** The `schedule` trigger runs the existing `ci.yml`
   jobs, not a separate policy-only workflow. Accepted for a public repository;
   the fuzz lane is a separate workflow because it needs nightly.

### Remaining before closure

- Commit and push the work, then run hosted CI on the qualifying revision:
  `verify`, `dependency-boundary`, `protocol-boundary`, `dependency-policy`,
  `python-bindings`, `python-abi3-cross-version`, `interception`, and a manual
  `fuzz` dispatch.
- Create `plans/closure/m021a-…` through `plans/closure/m021e-…` and this
  umbrella's closure record with the hosted evidence and the residual register.
- Re-run the two High reproductions against the committed revision.
- Operator-only settings in `docs/release-process.md` remain to be confirmed.
- M020's hosted qualification remains separate and is a prerequisite for
  removing the `rsa`/`russh`/`chrono` exceptions, not for this worktree.
