# M021B — Destructive Path Safety and Transactional Replacement

Status: **implemented** (local gate green; hosted CI and closure record pending)
Depends on: none (independent of M020; no `Cargo.lock` changes planned)
Closure record: `plans/closure/m021b-destructive-path-safety.md` (not created;
requires hosted qualification of the committed revision and the Windows lane)

## Why this plan exists

Several commands interpret `--overwrite` as "recursively delete this path".
None of them checks that the path is an EggReplay fixture. Deletion also
happens before the operation can validate its inputs, so a typo or a malformed
HAR destroys the old data before the new data is known to be usable.

The audit confirmed this empirically in `/tmp`: a directory containing
`keep.txt` was passed as `--fixture` to `har import --overwrite` with a
malformed HAR. The command exited `2` with `missing field 'log'`, and
`keep.txt` was already gone.

This is a data-loss/integrity defect, not a confidentiality defect. It is High
because the operation is recursive and irreversible, and because `--overwrite`
is a routine flag rather than a separate explicit "delete this directory"
command.

## Audit evidence

- `crates/eggreplay-cli/src/main.rs:830` — `record` calls
  `remove_dir_all(&args.fixture)`.
- `crates/eggreplay-cli/src/main.rs:2450` — `har import` does the same.
- `crates/eggreplay-cli/src/intercept.rs:528` — `proxy record` does the same.
- `crates/eggreplay-cli/src/main.rs:1300-1321` — `replace_fixture_transactionally`
  renames an existing target to a backup, publishes the new fixture, then
  recursively deletes the backup.
- `crates/eggreplay-cli/src/main.rs:962` — `serve --record-mode re-record`
  reaches the replacement path without first validating that the target is a
  fixture.
- `crates/eggreplay-cli/src/main.rs:2652` — `migrate --to --overwrite` uses the
  same replacement path.

Ordering evidence:

- `record` deletes at `:830` before building the outbound client at `:837`.
- `har import` deletes at `:2450` after reading the HAR but before parsing it.
- `proxy record` deletes at `:528` before loading its policy at `:532`.

## Required behavior

### 1. One replacement-target authority

Add a single CLI-side validation helper used by every destructive command. It
classifies a target as one of:

- **Absent** — safe to create.
- **Empty directory** — safe to replace; deleting an empty directory loses
  nothing.
- **Valid fixture** — `Session::open(path, StoreLimits::default())` succeeds,
  including supported older schemas. Safe to replace.
- **Anything else** — refuse with a `configuration` error that names the path
  and the reason.

The helper must:

- reject `.` and `..` lexically, before any filesystem call;
- reject a filesystem root and any path with no final component;
- use `symlink_metadata` and reject a symlink target rather than following it;
- reject a non-directory;
- reject a non-empty directory that is not a valid fixture;
- reject a fixture that cannot be opened (incomplete manifest, unsupported
  future schema, malformed flows) rather than treating it as disposable.

Calling the helper twice in one operation is acceptable; it is cheap relative
to the recording. It must also be called inside
`replace_fixture_transactionally` as defense in depth, so a future caller
cannot bypass the check.

### 2. Validate before delete, stage before swap

Every replacement follows the same order:

1. Validate all configuration and inputs (route, policy file, CA directory,
   HAR bytes, target schema).
2. Validate the replacement target.
3. Create the new fixture at a sibling staging path, never at the target.
4. Finalize and validate the staged fixture (`Session::open` succeeds and the
   manifest is complete).
5. Replace the target atomically.
6. Delete the backup only after the new fixture is published.

A failure in steps 1–4 leaves the target byte-identical. A failure in step 5
restores the backup as today. A failure in step 6 reports that the new fixture
is published and the backup could not be removed; it never restores the old
fixture over a published new one.

Apply this to:

- `record` — currently creates its recording session directly at the target.
  Build the client/redaction policy first, then record into a sibling staging
  destination, then swap.
- `har import` — parse and validate the HAR into a staging `SessionWriter`
  first; swap only after `finish`.
- `proxy record` — load and validate the policy file/CA/route first; record
  into staging; swap after shutdown/finalization.
- `serve --record-mode re-record` — validate the target before starting the
  listener; it already stages via `sibling_transaction_path` and swaps at
  shutdown. Keep that, but add the target check and ensure a failed recording
  does not reach `replace_fixture_transactionally`.
- `migrate --to` — validate the destination before migration; it already
  stages and swaps.
- `migrate --in-place` — the source is opened and validated by definition; keep
  the existing transactional behavior, but make sure the backup name cannot
  collide with a user file.

`--overwrite` remains the explicit opt-in to replace a **fixture or empty
directory**. It is no longer permission to delete an arbitrary path.

### 3. Cleanup on failure

A failed staged operation must not leave the target damaged. Best-effort
cleanup of the staging directory is required on the error path. Because a
process crash cannot run cleanup, stale `.incomplete-*`/`.pending-*`
directories remain a documented residual:

- `SessionWriter` gets a `Drop` that removes its staging directory if it was
  never finished or discarded.
- The CLI calls an explicit cleanup on every error return from a staged
  operation.
- Do not add an automatic sweep of all `.incomplete-*` directories on startup;
  a future, explicitly confirmed recovery command may do that.
- `docs/quickstart.md:34` already documents staging residue; update it to say
  that normal error paths clean up and only a crash or `SIGKILL` leaves residue.

## Tests

The helper is unit-tested directly, and each command gets at least one
end-to-end refusal test.

### Helper tests

- absent path → `Absent`;
- empty directory → `EmptyDir`;
- valid schema-1 and schema-2 fixture → `ValidFixture`;
- non-empty directory with an unrelated file → refused;
- regular file → refused;
- symlink to a directory → refused, link and target unchanged;
- `.`, `..`, `/`, and a path with a parent component → refused;
- incomplete fixture (manifest missing) → refused;
- future-schema fixture → refused;
- a fixture with an extra unrelated file → still accepted as a fixture
  (fixtures may carry extension files; the check is `Session::open`, not
  directory emptiness).

### Command tests

- `record --fixture victim --overwrite` with a non-fixture `victim` → exit `2`,
  `victim` and its file byte-identical. The pre-fix tree deletes it.
- `record` with a valid target and an invalid `--route` → target unchanged
  because config validation precedes staging.
- `har import` with a malformed HAR and an existing valid fixture →
  exit `2`, old fixture opens and its manifest is unchanged.
- `proxy record` with an invalid policy file and an existing fixture → exit
  `2`, fixture unchanged.
- `serve --record-mode re-record` with a non-fixture target → exit `2` before
  any listener bind.
- `migrate --to` with a non-empty non-fixture destination and `--overwrite` →
  exit `2`, destination unchanged.
- Empty directory is replaced successfully by every command that supports it.
- Valid fixture replacement is atomic: after a successful replacement,
  `Session::open` sees the new flow count, and the backup is gone.
- Simulated stage/finalize failure leaves the old fixture intact and leaves no
  backup that `recover_fixture_transactionally` could mistake for the target.

## Documentation

- `docs/cli.md`: define `--overwrite` as "replace an existing fixture or empty
  directory", name the refusal conditions, and state that a failed operation
  preserves the old fixture.
- `docs/quickstart.md`: update the staging-residue paragraph; remove the
  implication that any existing path can be replaced.
- `docs/har-interchange.md`: update the `--overwrite` line and the
  destination-replacement description.
- CLI help text for `record`, `har import`, `proxy record`, `migrate`, and
  `serve`: use the same wording.
- `docs/eggr-schema.md`: no format change, but note that replacement is
  fixture-aware.

## Acceptance

- No command can recursively delete a non-empty directory that is not a valid
  fixture.
- Every command validates configuration and stages the new fixture before
  touching the target.
- Failed operations preserve the target byte-for-byte.
- `--overwrite` still replaces a valid fixture and still refuses an existing
  target when omitted.
- Unix and Windows tests pass; Windows rename/open semantics are exercised by
  the existing hosted matrix.
- The full locked workspace gate and the required hosted CI are green.
- A closure record records the empirical pre-fix deletion and the post-fix
  refusal, including the exact commands and exit classes.

## Non-goals

- A general filesystem sandbox or a new backup/restore product.
- Automatic deletion of stale staging directories.
- Changing the fixture format or publication protocol.
- Making `--overwrite` removable or adding a confirmation prompt; the flag is
  already explicit and scriptable.

## Verification

Run the required locked workspace gate from `AGENTS.md`, plus:

```sh
cargo test -p eggreplay-cli
cargo test -p eggreplay-har
cargo test -p eggreplay-store
```

The hosted Windows job covers rename/open behavior that a Unix-only local run
cannot. The closure record must include the pre-fix deletion reproduction and
the post-fix refusal with exact commands and exit classes.

## Implementation record (2026-11-18)

Implemented locally; not closed. The full locked workspace gate passed
(591 tests, 0 failures). At that phase the CLI suite reported 68
default-feature tests and 74 with `--features intercept`, and the store suite
24, all green; later milestones added cases without changing their outcome.

- **One authority.** `classify_replacement_target` classifies `Absent`,
  `EmptyDir`, or `ValidFixture` (`Session::open` with default limits), and
  refuses roots, `.`/`..`, any `..` component, symlinks (never followed),
  non-directories, non-empty non-fixtures, and fixtures that fail to open.
  `check_replacement_target` applies `--overwrite` in one place. It is called
  from `record`, `serve --record-mode re-record`, `har import`, `migrate --to`,
  `proxy record`, and again inside `replace_fixture_transactionally`.
- **Validate, stage, swap.** `record`, `har import`, and `proxy record` now
  create the fixture at a sibling staging destination, finalize and validate
  it, then swap; nothing is deleted up front. `record` resolves inbound,
  upstream, redaction, and the outbound client before the target check.
- **Cleanup.** `StagingGuard` removes the staged destination on every early
  return, and `SessionWriter::drop` removes its internal `.incomplete-*`
  directory unless finished or discarded. `sibling_transaction_path` never
  reuses an existing name.
- **Evidence.** The command suite covers non-fixture refusal, malformed-HAR
  preservation, empty-directory replacement, valid-fixture atomic replacement,
  no-backup-after-publish, and `serve re-record` refusal before bind. The
  pre-fix tree deleted an unrelated `keep.txt`; the post-fix manual
  reproduction is exact: `har import --overwrite` on a directory containing
  `keep.txt` exits `2` with `failure_class: "configuration"` and leaves
  `keep.txt` byte-identical.
- **Deviation.** The classifier rejects `..` and roots but accepts a leading
  `./`, which resolves to the same directory and carries no deletion risk.
  Pinned by
  `transaction_tests::current_dir_prefix_is_an_ordinary_spelling`.
- **Residual.** A crash or `SIGKILL` cannot run `Drop`; stale
  `.incomplete-*`/staging directories remain documented and are never swept
  automatically. A loss report written before the swap into the old fixture
  tree would be replaced with it; the reviewer note records that edge.

### Remaining for closure

Commit and push; run the full hosted matrix (including Windows renames) on the
qualifying revision; create `plans/closure/m021b-destructive-path-safety.md`
with the pre-fix/post-fix command evidence.
