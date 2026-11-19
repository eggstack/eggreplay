# M021C — Untrusted Input Bounds and Panic Safety

Status: **implemented** (local gate green; hosted CI and closure record pending)
Depends on: none (independent of M020; no `Cargo.lock` changes planned beyond
optional fuzz/dev tooling)
Closure record: `plans/closure/m021c-untrusted-input-hardening.md` (not
created; requires hosted qualification of the committed revision)

## Why this plan exists

EggReplay treats `.eggr` fixtures and HAR files as untrusted input. The audit
found three places where a bound is enforced only after the allocation, and
two places where a truncation can panic on a valid UTF-8 string:

1. **Manifest** — `manifest.json` is parsed with `serde_json::from_reader` and
   no size bound. A malicious manifest can allocate until OOM.
2. **JSONL** — `BufReader::read_until` accumulates a full line before the
   `max_line_bytes` check. A single multi-gigabyte line is buffered in full
   before rejection. The same pattern exists in the WebSocket fixture
   validator.
3. **HAR** — the CLI reads the entire HAR with `std::fs::read`; the 32 MiB
   check happens after the allocation.
4. **UTF-8 truncation** — `crates/eggreplay-har/src/lib.rs:1416` slices
   `input[..max]`; `crates/eggreplay-core/src/error.rs:111` calls
   `String::truncate(512)`. Both panic when the boundary falls inside a
   multi-byte character. The HAR case was confirmed with a crafted 600-byte
   `€` comment: the CLI exited `101`.

This is an availability and robustness defect. The fix must preserve the
existing error classes and messages where they are part of the CLI contract.

## Audit evidence

- `crates/eggreplay-store/src/lib.rs:1516` — unbounded manifest reader.
- `crates/eggreplay-store/src/lib.rs:1923` — `read_until` in `FlowIter`.
- `crates/eggreplay-store/src/lib.rs:2197` — `read_until` in
  `validate_websocket_fixture`.
- `crates/eggreplay-cli/src/main.rs:2442` — `std::fs::read(&args.har)`.
- `crates/eggreplay-har/src/lib.rs:807` — size check after the read.
- `crates/eggreplay-har/src/lib.rs:1416` — `input[..max]`.
- `crates/eggreplay-core/src/error.rs:111` — `message.truncate(512)`.

`SECURITY.md` states that malformed or oversized JSONL is rejected. The bound
exists, but it does not protect the allocation, so the claim is currently
stronger than the implementation.

## Track A — Bound every untrusted read before allocation

### Manifest

Add a documented `MAX_MANIFEST_BYTES` (proposed 16 MiB, matching the per-
extension bound) and a bounded reader:

- open the file, check its metadata length where available;
- read at most `MAX_MANIFEST_BYTES + 1` bytes;
- reject with a typed `StoreError::Invalid`/`Unsupported` diagnostic if the
  limit is exceeded;
- parse only the bounded bytes with `serde_json::from_slice`.

Do not add a public `StoreLimits` field unless a testability need outweighs the
API break; a private constant plus a private helper that takes a limit is
sufficient, and the helper can be unit-tested with a 64-byte bound.

The error must not include manifest contents. It may name the path and the
limit.

### JSONL

Add one bounded line reader used by both `FlowIter` and
`validate_websocket_fixture`:

- read at most `max_line_bytes + 1` bytes, using `BufRead::fill_buf`/`consume`
  or `read_until` over a `Take` limited to `max_line_bytes + 1`;
- reject as soon as the limit is exceeded, without retaining the remainder;
- preserve the existing failure message
  `flow JSONL line exceeds configured limit` and the `StoreError` category so
  CLI exit code `3` and the JSON envelope's `failure_class` do not change;
- leave the reader in a state where the iterator returns the error once and
  then terminates, rather than looping on the rest of the oversized line.

Add a unit test for the helper with `max = 16`, a line of 17 bytes, and a line
of exactly 16 bytes. Add an integration test using a small `StoreLimits`
instance so the test does not allocate large buffers.

### HAR

Change the CLI import path to:

1. open the file (not `fs::read`);
2. check `metadata().len()` against `MAX_HAR_FILE_BYTES` for an early,
   allocation-free refusal;
3. read through `take(MAX_HAR_FILE_BYTES + 1)` to close the metadata race;
4. reject if the read exceeded the limit;
5. pass the bounded bytes to `import_har_to_writer`.

Preserve the CLI's `configuration` failure class and the existing
`HAR document exceeds size limit` diagnostic. Factor the bounded-read helper so
it can be tested with a small limit; keep one ignored-by-default integration
test for the real 32 MiB constant if generating that file is too slow for the
normal suite.

Do not change `import_har_to_writer`'s own check; the library must remain safe
for callers that already have bytes.

## Track B — UTF-8-safe truncation

Replace both direct truncations with one shared, tested helper
(`truncate_utf8_boundary` or equivalent):

- if `input.len() <= max`, return the input unchanged;
- otherwise find the largest character boundary `<= max` and slice there;
- append the existing ellipsis character only after a successful slice;
- the result must be valid UTF-8 and must not exceed the caller's intended
  display bound by more than the ellipsis.

Use it in:

- `crates/eggreplay-har/src/lib.rs` `truncate_bounded` (called for HAR request
  comments, response comments, and creator strings);
- `crates/eggreplay-core/src/error.rs` `FlowError::new`.

Tests:

- 2-byte (`é`), 3-byte (`€`), and 4-byte (`😀`) characters at `max`, `max-1`,
  and `max+1`;
- a string whose byte 512 is inside a character;
- a string shorter than the bound is unchanged;
- the result is valid UTF-8 and starts with the expected prefix;
- the exact crafted HAR from the audit exits with a typed configuration/
  fixture error, not `101`.

## Track C — Symlink TOCTOU decision

`Session::open_blob` checks `symlink_metadata` and then opens the path
(`crates/eggreplay-store/src/lib.rs:1852-1866`). The documented immutability
contract says fixtures must not be mutated while open, and streaming hash
verification prevents silent substitution. A local attacker who can race the
fixture directory could still have bytes streamed before the hash check fails.

This track requires an explicit decision, not an unbounded redesign:

- **Preferred:** open blobs with no-follow semantics where the platform
  supports them (`O_NOFOLLOW` on Unix, reparse-point handling on Windows), then
  verify the opened handle is a regular file and has the expected length.
  Add a Unix test that replaces a blob with a symlink between open and stream
  and asserts an integrity error, never the symlink target's bytes.
- **Fallback:** if no portable no-follow open is available without adding a
  new dependency, record the residual in the closure record with the exact
  threat model (local write access to an open fixture), the existing hash
  mitigation, and the fact that this is a documented immutability contract.
  Do not silently leave it unmentioned.

Either outcome must be stated in `SECURITY.md` or the closure record. The
preferred implementation must not add a transport dependency or change the
fixture format.

## Track D — Windows permission honesty

`set_private_permissions` is a no-op on non-Unix
(`crates/eggreplay-store/src/lib.rs:2343-2346`), and CA files have the same
caveat. The interception threat model already states that Unix mode bits do not
imply Windows ACL protection.

Required disposition:

- keep the no-op behavior;
- ensure `SECURITY.md`, `docs/interception-threat-model.md`, and
  `docs/eggr-schema.md` all state the Windows caveat in one consistent place;
- add a test that documents the no-op (or asserts the Unix behavior where
  applicable);
- do not add a Windows ACL dependency in this corrective unless a supported
  API and hosted lane are approved. A future plan may do so.

This is an accepted residual, not a code fix.

## Track E — Parser fuzz harnesses

Add a small `cargo-fuzz` workspace (or `fuzz/` directory) with targets for:

- `har import` (bounded HAR bytes into `import_har_to_writer`);
- `Session::open` (a fixture directory constructed from fuzz bytes for
  manifest, flows, stream-events, and websockets);
- WebSocket transcript validation.

Constraints:

- targets must not touch the network or non-temp paths;
- each run is bounded by a corpus and `-max_total_time`;
- the initial acceptance is a 60-second local smoke per target, not a
  multi-hour campaign;
- CI runs a nightly/manual job, not a required PR gate, to keep the matrix
  stable;
- the fuzz workspace is excluded from the normal workspace build unless the
  targets compile without extra transport features.

If `cargo-fuzz` cannot be made to work with the pinned toolchain without a
nightly-only dependency, record the blocker and keep the harness as a
documented local target. Do not make the required workspace gate depend on
nightly.

## Acceptance

- Manifest, JSONL, and HAR reads reject oversized input without first
  allocating the whole object.
- The JSONL error class, message, and exit code are unchanged.
- The crafted HAR and the crafted `FlowError` boundary no longer panic; both
  return a typed error.
- Track C has an explicit implemented-or-accepted disposition with a test or a
  closure rationale.
- Track D documentation is consistent and tested where applicable.
- Track E targets build and pass a bounded smoke, or the blocker is recorded.
- Full locked workspace gate and the required hosted CI are green.
- `SECURITY.md` and `docs/eggr-schema.md` describe the bounds accurately.

## Non-goals

- A general-purpose sandbox for fixture directories.
- Changing the fixture publication protocol or blob format.
- Making every parser allocation-free; the requirement is that no untrusted
  length is trusted before a bounded read.
- Full fuzzing coverage or a claim that fuzzing proves absence of bugs.

## Verification

Add tests to the existing store/HAR/CLI suites:

```sh
cargo test -p eggreplay-store
cargo test -p eggreplay-har
cargo test -p eggreplay-cli
cargo test -p eggreplay-core
```

For the fuzz smoke, document the exact command and nightly toolchain in the
closure record.

## Implementation record (2026-11-18)

Implemented locally; not closed. The full locked workspace gate passed
(591 tests, 0 failures). At that phase core 55, store 30 + 2 integration, HAR
9, and CLI 89 passed with `--all-features`; later milestones added cases
without changing their outcome.

- **Track A.** `MAX_MANIFEST_BYTES` is 16 MiB with a metadata check plus a
  `take(limit + 1)` read and `serde_json::from_slice`; the error names the path
  and limit, never contents. `read_bounded_line` uses `fill_buf`/`consume`, and
  `FlowIter` yields an oversized line once and then terminates, preserving
  `flow JSONL line exceeds configured limit` and the store error class. The
  CLI HAR read opens, checks metadata against `MAX_HAR_FILE_BYTES` (32 MiB),
  reads through `take(MAX + 1)`, and keeps the exact `HAR document exceeds size
  limit` diagnostic. Evidence: the 64-byte manifest-helper tests, the 16/17-byte
  line tests, `untrusted_input` integration, and the oversized sparse HAR CLI
  test.
- **Track B.** `eggreplay_core::truncate_utf8_boundary` is the single helper;
  HAR's `truncate_bounded` appends `…` only after a boundary-safe slice, and
  `FlowError::new` truncates 512 bytes on a boundary. Tests cover 2-, 3-, and
  4-byte characters at `max±1`, byte 512 inside `€`, and short inputs.
- **Track C.** Accepted residual, documented in `docs/eggr-schema.md` and
  `SECURITY.md`: `symlink_metadata` and `File::open` are separate syscalls, a
  portable no-follow open would add a platform dependency, the length/hash
  check fails closed with an integrity error, and fixtures are contractually
  immutable while open.
- **Track D.** `set_private_permissions` remains a non-Unix no-op; the same
  caveat is stated in `SECURITY.md`, `docs/eggr-schema.md`, and
  `docs/interception-threat-model.md`; Unix `0700`/`0600` and the non-Unix
  no-op are tested.
- **Track E.** `fuzz/` is a standalone `cargo-fuzz` workspace with three
  targets (`har_import`, `session_open`, `websocket_transcript`), temp-only,
  offline, bounded. `cargo +nightly fuzz build` and a 10-second smoke per
  target passed locally. `.github/workflows/fuzz.yml` runs the 60-second weekly
  lane.

### Deliberate deviation

The crafted 600-byte `€` HAR comment now **imports successfully** (exit `0`)
with a valid 513-byte truncation (170 `€` + `…`), rather than returning a typed
error as the plan anticipated. The HAR document is valid and the defect was the
panic; making it an error would introduce a new truncation policy. The tests
assert exit `!= 101`, valid UTF-8, and the expected prefix, and a separate
oversized-HAR test exercises the typed `configuration` error.

### Accepted residuals

- Fixture-wide allocation is bounded by the configured limits, not by an
  allocation counter; the metadata race is closed by `take(limit + 1)`.
- The fuzz smoke is bounded evidence, not a proof of absence; the weekly lane
  is 60 seconds per target.
- Windows ACL protection remains an accepted residual.

### Remaining for closure

Commit and push; dispatch the fuzz workflow manually once and confirm the
hosted lanes; create `plans/closure/m021c-untrusted-input-hardening.md` with
the nightly version, the exact fuzz commands, and the Track C/D disposition.
