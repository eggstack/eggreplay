> Deep dive for [overview](overview.md).

# 03 — `eggreplay-store`: `.eggr` format, sessions, publication

`eggreplay-store` is one 3,428-line file (`crates/eggreplay-store/src/lib.rs`)
that owns exactly two things: the bytes of an `.eggr` fixture directory, and
the rules for when those bytes may be trusted. It has no sockets and no
runtime, so it cannot hold a lock across an upstream transaction; the HTTP
adapter supplies whatever async primitive it needs.

---

## Crate contract

The dependency set is deliberately tiny
(`crates/eggreplay-store/Cargo.toml:11-17`): `eggreplay-core`, `serde`,
`serde_json`, `sha2`, `tempfile`, `thiserror`. No HTTP client, no server, no
async runtime, no base64. Crate attributes are `#![forbid(unsafe_code)]` and
`#![deny(missing_docs)]` (`crates/eggreplay-store/src/lib.rs:3-4`), so no raw
pointer IO is possible. Every concurrency primitive is `std::sync`
(`crates/eggreplay-store/src/lib.rs:12-15`) and every I/O path returns
`std::fs::File` or `std::io::Error`.

**The CI purity lane.** `.github/workflows/ci.yml:134-135` greps the resolved
normal-edge tree of `eggreplay-store` (and `eggreplay-core`) for `eggfetch`,
`eggserve`, `eggress`, `tokio`, `hyper`, `tungstenite`, `base64` and fails on
any hit; `.github/workflows/ci.yml:66-75` asserts the store stays Python-free
and that no Rust product crate depends on the PyO3 extension. That is the
mechanical form of "transport ownership stays in EggFetch/EggServe/Eggress".

**Re-export surface: there are none.** `BlobRef`, `Flow`, `FlowError`, and
`SessionMetadata` are imported from `eggreplay-core`
(`crates/eggreplay-store/src/lib.rs:6`) and named by path in public signatures
(`Session::open_blob(&BlobRef)` at `:1852`, `BodyWriter::finish() -> BodyRef` at
`:202`). One `From` bridges the crates' errors: `From<FlowError> for
StoreError`, folding core flow-validation failures into `StoreError::Invalid`
(`:89-93`).

| Public item | Line | Role |
|---|---|---|
| `StoreLimits` / `StoreError` | `25` / `49` | bounds; typed failure categories |
| `Migration`, `SchemaOneMigration` | `68` / `75` | declared migration seam |
| `Manifest`, `ExtensionDescriptor` | `97` / `114` | on-disk JSON shapes |
| `BodyWriter`, `SessionWriter` | `126` / `252` | single-owner writer pair |
| `RecordingSession`, `RecordingBodyWriter` | `756` / `765` | cloneable concurrent writer |
| `WebSocketConversationFinalizer` | `715` | host-supplied drain hook |
| `BlobHandle`, `Session`, `FlowIter` | `1441` / `1505` / `1912` | read side |

`RecordingInner` (`:647`) is private: the concurrency state machine is reachable
only through `RecordingSession`.

---

## On-disk layout

Per ADR 0004 (`plans/adrs/0004-eggr-storage-format.md:7-15`):

```text
name.eggr/
  manifest.json          # final publication marker
  flows.jsonl            # one schema-1 Flow per line
  blobs/<sha256>         # content-addressed non-empty body bytes
  <extension>.json(l)    # schema-2 payloads, fixture root only
```

`Manifest` flattens `SessionMetadata` and adds `complete: bool`, `flow_count`,
`blob_count`, and the optional `extensions` registry
(`crates/eggreplay-store/src/lib.rs:96-110`). `complete` is the atomic-finalize
marker: `Session::open` rejects `complete: false` outright (`:1524-1526`).

### Publication ordering

The ordering *is* the crash-safety argument:

1. `create` builds a staging sibling `.{name}.incomplete-{suffix}` plus `blobs/`
   (`:389-393`, `:923-927`). `unique_suffix` is nanos + a process-global
   `AtomicU64` + pid (`:1947-1954`), so same-process sessions cannot collide.
2. Bodies stream to `blobs/.staging-{suffix}` and are published by rename into
   `blobs/<sha256>` *inside staging* (`:220-231`, `:861-872`).
3. Flows are appended only after their bodies publish — a flow never references
   an unpublished blob.
4. Extension payloads are written and `sync_all()`ed (`:2047-2053`) and
   validated (`:2090`) **before any manifest exists**.
5. `manifest.json` is created with `create_new(true)`, written, `sync_all()`ed
   (`:611-619`, `:1418-1426`).
6. Only then is staging renamed onto the destination, and the result reopened as
   a validated `Session` (`:636-637`, `:1427-1428`).

The manifest is the final publication marker because its presence is the only
assertion that everything it references is complete. A crash before step 5
leaves a `.{name}.incomplete-*` directory with no `manifest.json`, which
`Session::open` can never accept. The directory rename is a single filesystem
operation, so a reader sees either staging-without-manifest or the published
fixture. The extra care about closing handles is Windows-driven: it refuses to
rename a directory containing open files, so the flow log and manifest handle are
flushed, synced, and dropped first (`:562-564`, `:1311-1324`, `:1417-1426`); on
Unix this is merely conservative.

### Schema 1 vs schema 2

`SESSION_SCHEMA_V1 = 1` and `SESSION_SCHEMA_VERSION = 2` live in
`eggreplay-core` (`crates/eggreplay-core/src/lib.rs:63-66`); flow records have
their own `FLOW_SCHEMA_VERSION = 1` and are never bumped
(`crates/eggreplay-core/src/lib.rs:60`). Every read/write path accepts exactly
the inclusive range `SESSION_SCHEMA_V1..=SESSION_SCHEMA_VERSION` — `Session::open`
(`:1517-1523`), `SessionWriter::create` (`:378-382`),
`RecordingSession::create` (`:912-916`), `copy_to`/`merge_to` (`:1624-1628`,
`:1696-1700`), `SchemaOneMigration::migrate` (`:79-85`). Anything outside is
`UnsupportedSchema`.

The backward-compatibility rule follows: **flow records never change shape; new
session-level data is added only as schema-2 extensions.** Writing an extension
is what promotes a fixture — `SessionWriter::write_extension` stamps schema 2
directly (`:434`) and the concurrent path derives it from registry emptiness
(`:1392-1405`). An extension-free session stays schema 1 and remains readable.
`count_blobs` ignores a `.gitkeep` marker (`:2356-2364`) because Git cannot
preserve an empty directory; the checked-in empty fixtures rely on that
(`:1649-1651`, `:1888-1890`).

---

## StoreLimits and validation

`StoreLimits` (`:25-34`) is `Copy`, passed to `create`/`open`, and retrievable
from an opened `Session` via `limits()` (`:1874`).

| Field | Default | Guards | Enforced at |
|---|---|---|---|
| `max_line_bytes` | 4 MiB (`:39`) | one JSONL record | `:530-534`, `:1196-1200`, `:1932-1936`, `:2201-2205` |
| `max_blob_bytes` | 64 MiB (`:40`) | one blob | `:143-148`, `:776-785`, `:1854-1856`, `:2292-2294` |
| `max_flows` | 100 000 (`:41`) | flows per session | `:522-526`, `:1209-1213`, `:1926-1931` |
| `max_total_bytes` | 512 MiB (`:42`) | aggregate body bytes | `:535-543`, `:1214-1222`, `:1559-1566` |

The pattern is to check a limit at the earliest representation that carries the
value: declared blob lengths are rejected before any file is opened
(`validate_body_refs` `:1966-1985`), and `BodyWriter::write` refuses over-limit
chunks before touching the file (`:139-148`). `max_flows` is off-by-one-correct
rather than conservative: `FlowIter` compares `seen >= max_flows` *before*
incrementing, so exactly `max_flows` lines are accepted and the next errors
(`:1926-1931`).

Extension bounds are **not** configurable; they are module constants
(`:18-20`): `MAX_EXTENSIONS = 64` (`:2019-2023`, `:2091-2095`),
`MAX_EXTENSION_BYTES = 16 MiB` (`:2024-2028`, `:2117-2121`, `:1785-1797`),
`MAX_TOTAL_EXTENSION_BYTES = 32 MiB` (`:2037-2045`, `:2122-2129`). The
aggregate check re-stats already-registered payloads on disk rather than trusting
an in-memory counter (`:2037-2040`), so a rolled-back write cannot leave the
accounting optimistic. A third layer guards the aggregate stream-event and
WebSocket payloads by comparing encoded size before writing (`:334-338`,
`:357-361`).

### Error categories

| Variant | Category | Examples |
|---|---|---|
| `Io` (`:52`) | filesystem / `std::io` | missing `manifest.json` `:1516`; `File::open` of a blob `:1861`; any `io::Error` from a `Write` impl |
| `Json` (`:55`) | malformed JSON | bad `flows.jsonl` line `:1939`; unparsable extension payload `:2148` |
| `Invalid` (`:58`) | semantically unacceptable | over-limit, `complete: false`, path escape, unknown required extension, duplicate flow id — **and** operator refusals like `"session is shutting down"` `:1190` |
| `UnsupportedSchema` (`:61`) | terminal version refusal | `:1520`, `:381`, `:915`, `:1627` |
| `Integrity` (`:64`) | hash/length verification | content mismatch `:2316-2317`; staging read-back length `:193`, `:833`; `open_blob` length `:1864` |

Two boundaries are easy to get wrong in caller code. First, **a per-body limit
violation is `Io`, not `Invalid`**: the `Write` impls signal it as
`io::ErrorKind::InvalidInput` (`:143-148`, `:780-785`), so an oversized body
looks like a filesystem error. Second, `Invalid` is overloaded across "the
writer asked too much" and "the fixture is untrustworthy"; distinguishing
operator error from hostile input requires matching on the message. Lock
poisoning is also normalized to `Invalid` (`:972`, `:1050`, `:1205`) so
`PoisonError` never leaks into the public error surface.

---

## Reading a session

### `Session::open` — full validation, once

`Session::open` (`:1513-1608`) is the trust boundary and is deliberately *not*
metadata-only: it hashes every body referenced by every flow. Order:

1. Parse `manifest.json` (`:1515-1516`); range-check the schema
   (`:1517-1523`); require `complete` (`:1524-1526`).
2. Require schema 2 if a `websocket-messages` extension exists (`:1527-1536`).
3. `validate_extensions` — names, confinement, symlinks, sizes, per-extension
   schema version, payload semantics (`:1542`).
4. Iterate flows: reject duplicate flow ids (`:1552-1557`), re-accumulate total
   body bytes against `max_total_bytes` (`:1559-1566`), and fully verify each
   referenced blob (`:1567` → `verify_blob_file` `:2290-2320`).
5. Counted flows must equal `manifest.flow_count` (`:1569-1571`).
6. `validate_websocket_fixture` — 101/transcript correspondence, payload
   confinement, text UTF-8 (`:1572-1577`).
7. If `stream-events` exists, cross-check summed DATA lengths against the
   recorded response body length (`:1578-1603`).
8. `count_blobs(blobs/) == manifest.blob_count` (`:1604-1606`).

The resulting `Session` holds a `PathBuf`, a `Manifest`, and a `StoreLimits`
(`:1505-1509`) — no payload bytes.

### Flow records and the body descriptor model

`iter_flows` returns a `FlowIter` over a `BufReader<File>` (`:1802-1808`); `next`
reads with `read_until(b'\n')` (`:1923`), enforces flow-count and line bounds,
then `serde_json::from_slice::<Flow>` + `flow.validate()` (`:1937-1940`). It is
lazy, so `open`'s pass and any later pass are independent.

`eggreplay_core::BodyRef` (`crates/eggreplay-core/src/flow.rs:36-59`) is the
three-way on-disk model:

- `Absent` — no body present/permitted; `len()` is `None`.
- `Empty` — a body existed with zero bytes; `len()` is `Some(0)`. The store never
  materializes a blob for it: both `finish` impls delete the staging file and
  return `BodyRef::Empty` (`:216-219`, `:857-860`).
- `Blob(BlobRef { sha256, length })` — bytes under `blobs/<sha256>` with an exact
  declared length (`flow.rs:63-68`).

The matching side converts this to `CandidateBody`
(`crates/eggreplay-core/src/matching.rs:54-68`) — the `absent`/`empty`/`digest`
descriptor plus an explicit `Inline` variant that fixture-wide loads must not
use; `from_body_ref` never reads blob bytes (`matching.rs:75-84`). The store's
contribution to that model is the guarantee that a `Digest` names a file that
exists, matches its declared length, and hashes to its digest — which is what
lets exact matching compare length + SHA-256 without materializing.

### `Session::open_blob` → `BlobHandle`

`open_blob` (`:1852-1871`) is the C001 lazy primitive
(`plans/closure/c001-lazy-replay-streaming.md:7`):

| Check | Line | Failure |
|---|---|---|
| digest form: 64 lowercase ASCII hex | `validate_digest` `:1956-1964` via `:1853` | `Invalid` |
| declared length ≤ `max_blob_bytes` | `:1854-1856` | `Invalid` |
| not a symlink (`fs::symlink_metadata`) | `:1858-1860` | `Invalid` |
| on-disk length == declared length | `:1862-1865` | `Integrity` |

It returns a `BlobHandle` (`:1441-1445`) holding an **already-opened**
`std::fs::File` plus validated digest and length. Guarantees: no blob bytes are
allocated (stream via `into_file()` `:1468`; `eggreplay-http` wraps it with
`tokio::fs::File::from_std` and 64 KiB chunks at
`crates/eggreplay-http/src/replay.rs:801-802`); and the full content hash is
**deliberately not** computed here, because a fixture-wide load must stay
metadata-bounded — the streaming caller must hash incrementally and treat
mismatch as `Integrity` (`:1839-1851`; done at
`crates/eggreplay-http/src/replay.rs:944-953`). `read_all` (`:1477-1500`) is the
bounded convenience path — re-seek, hash in 64 KiB chunks, refuse to exceed
`length` (`:1491-1493`), require exact length *and* digest (`:1496-1498`) —
documented for tests and explicit-bounds CLI inspection only (`:1472-1476`).
`Session::read_blob` (`:1811-1837`) is the older allocating equivalent.

### Fixture immutability and TOCTOU

The contract (`:1846-1851`): treat a fixture as immutable while any `Session` or
`BlobHandle` is open; replacement must fail, never silently serve.

- **Different length** — caught at `open_blob` by the exact comparison →
  `Integrity` (`:1863-1865`).
- **Same length** — *not* caught at open; caught by the incremental hash during
  streaming, so it surfaces mid-response. This is the residual risk, and it is
  why the downstream hash is not optional.
- **Deleted unselected blob** — irrelevant, unselected blobs are never opened
  (`:1851`); a selected deleted blob fails `open_blob` with `Io`.
- **Symlink swap** — rejected at open (`:1858-1860`).

Residual race for reviewers: the symlink check uses `fs::symlink_metadata` and
*then* separately `File::open` (`:1858-1861`, same pattern at `:2296-2299`). A
hostile multi-tenant filesystem could swap the entry between those calls.
Closing that needs `O_NOFOLLOW`, i.e. `unsafe` or a platform dependency — and
`unsafe` is forbidden (`:3`). The rejection test is `#[cfg(unix)]` for
construction-privilege reasons (`:3158-3164`).

---

## Writing a session

Two writers, two concurrency models. `SessionWriter` is single-owner and takes
`&mut self` for every mutation (`:252-266`, `:521-549`). `RecordingSession` is
cloneable over `Arc<RecordingInner>` (`:756-758`, `:647-677`).

`SessionWriter::create` refuses an existing destination (`:373-377`),
range-checks the schema, creates staging plus `blobs/`, and sets `0700` on
directories / `0600` on files on Unix (`:383-398`, `:2334-2346`).
`begin_blob` opens `blobs/.staging-{suffix}` with `create_new(true)` (`:479-495`).
`append_flow` validates, validates body-reference forms, encodes, checks the
line bound, accumulates `flow_body_bytes` with a checked add, and only then
writes JSON + `\n` (`:521-549`). `finish` flushes/syncs the log, writes and
validates aggregate `stream-events` / `websocket-messages` extensions, runs
`validate_websocket_fixture`, writes the manifest, destructures `self` to drop
the flow handle before the rename, and reopens the published fixture
(`:565-638`).

`BodyWriter` is the streaming sink: over-limit chunks refused before the write
(`:139-148`); `read_staging_bounded` (`:179-196`) is the C003 pre-publication
transform hook the gateway uses so structured redaction happens before any blob
is finalized (`crates/eggreplay-http/src/recording.rs:310`, `563`, `2047`,
`2101`); `finish` (`:202-235`) flushes, `sync_all()`s, closes, then renames into
`blobs/<digest>` — or just deletes the staging file when the destination exists,
which is how identical bodies deduplicate (`:227-231`).

### `RecordingSession`: lock and atomic discipline

`RecordingInner` (`:647-677`) uses four atomics and six mutexes, and the split is
the design:

| Field | Kind | Guards |
|---|---|---|
| `flows: Mutex<Option<File>>` | mutex | the JSONL append only |
| `flow_count: AtomicUsize` | atomic | flow count (exact under the flow lock) |
| `total_bytes: AtomicU64` | atomic | aggregate body bytes |
| `active_blobs: AtomicUsize` | atomic | open sinks; gates `finish` |
| `shutdown: AtomicBool` | atomic | admission gate |
| `extensions`, `stream_events`, `stream_event_ids`, `websocket`, `websocket_finalizers` | mutexes | per-registry metadata, own critical sections |

The load-bearing rule is the type's own comment (`:641-646`): *`flows` is the
only serialized authority… Body bytes never hold this lock; they stream to
independent staging files.* Concretely:

- **`begin_blob` (`:1141-1163`) takes no store lock at all.** It checks
  `shutdown`, creates a private staging file, and `active_blobs.fetch_add(1,
  SeqCst)`; later `write`/`finish` touch only that file.
  `concurrent_blobs_overlap_without_flow_lock` (`:3214-3281`) proves overlap by
  asserting writer two starts before writer one ends.
- **`append_flow` (`:1188-1230`) validates outside the lock and accounts inside
  it.** `flow.validate()`, `validate_body_refs`, encoding, and the line bound are
  pure CPU on local data (`:1192-1200`). Then the flow mutex is taken
  (`:1201-1208`) and the `max_flows` check, `total_bytes` checked add,
  `max_total_bytes` check, the `write_all` of JSON + `\n`, and both counter
  updates all happen in that one short critical section (`:1209-1228`). Because
  check and update share the mutex the limit is exact, not racy:
  `session_limits_enforced_under_concurrency` (`:3313-3352`) asserts exactly 4 of
  8 racing appends succeed with `max_flows: 4`, and
  `concurrent_append_preserves_manifest_count` (`:3283-3311`) asserts 20 threads
  yield 20 flows and a matching manifest count.
- **Shutdown is re-checked on both sides of every lock acquisition** for
  extensions (`:965`, `:973`), stream events (`:1074`, `:1082`), WebSocket
  conversations (`:1238`, `:1246`), and finalizer registration (`:1041`,
  `:1051`) — closing the window where shutdown lands while a task blocks on a
  mutex. `append_flow` checks before acquisition only (`:1189-1191`).
- **All ordering is `SeqCst`** except the `unique_suffix` counter's `Relaxed`
  (`:1952`), where uniqueness is all that matters.
- **`std::mem::take` detaches the finalizer registry** before draining
  (`:1295-1298`) so a re-entrant finalizer cannot observe a recursive drain.

### Staging model and Drop cleanup

Both writers stage through `.<name>.staging-<suffix>`, a name that can never
become a blob name, and both clean up on abort. `BodyWriter::Drop` removes the
staging file unless the path is the `__finished__` sentinel installed by
`finish` (`:238-249`, sentinel at `:206`). `RecordingBodyWriter::Drop` takes the
path, drops the handle, removes the file, and decrements `active_blobs`
(`:885-897`); because `finish` leaves all three `Option`s as `None` (`:843-845`),
Drop is a no-op after a successful publish and the explicit `fetch_sub` at `:880`
is the single accounting point — no double decrement, no leak. `finish` also
removes staging on the sync-failure path (`:849-854`) and on any
post-publish-check failure (`:877-879`). `aborted_body_leaves_no_orphan_and_finish_succeeds`
(`:3354-3389`) asserts an empty blob directory after a dropped writer and
`blob_count == 0` in the published manifest.

### Shutdown sequence

Stop admission → drain in-flight → fail closed if a sink remains
(`docs/architecture.md:27-35`):

1. `shutdown()` sets the `AtomicBool` (`:992-994`); `begin_blob`, `append_flow`,
   `write_extension`, `append_stream_events`,
   `append_websocket_conversation`, and finalizer registration then fail with
   `Invalid("session is shutting down")`.
2. The host drains: `drain_active_blobs` spins on `active_blobs() == 0` for up
   to 1000 yields (`crates/eggreplay-http/src/recording.rs:937-943`), after
   server `shutdown` + `wait` drains tunnel tasks
   (`plans/closure/c002-concurrent-recording-session.md:10`).
3. `finish` (`:1283-1429`) re-sets `shutdown` defensively, then fails with
   `Invalid("cannot finalize with active transactions")` if `active_blobs != 0`
   (`:1285-1289`) — it refuses to race rather than finalize around a live writer.
   `Arc` strong count is explicitly *not* the signal; see the comment at
   `:1304-1307`. `RecordingBodyWriter` must also be dropped first, because an
   open staging handle blocks the Windows directory rename.

---

## Session extensions

Schema 2 adds a bounded manifest registry. Each `ExtensionDescriptor` (`:114-123`)
is `{name, schema_version, path, required_for_replay}`.

**`required_for_replay` means the reader must understand and apply the extension
to serve the fixture faithfully** — not that the user opted into a behaviour. ADR
0005 gives the case: `stream-events` is required because it can encode a
terminal mid-body failure, and a reader that ignored it would turn a partially
failed interaction into a successful one
(`plans/adrs/0005-versioned-session-extensions.md:37-48`). Timing policy stays
orthogonal — `immediate` is the default with zero added delay, `recorded`/
`scaled` are explicit. Both writer paths mark `stream-events` and
`websocket-messages` required (`:571-580`, `:1345-1353`, `:585-593`,
`:1369-1377`), pinned by `writer_emits_stream_events_as_required`
(`:2687-2755`).

**Confinement and symlinks.** `validate_extension_path` (`:2075-2088`) enforces a
confined single filename: non-empty, ≤128 bytes, exactly one `Path` component,
`file_name()` equal to the whole string, not dot-prefixed — so `../escape.json`
and `sub/dir.json` both fail. `validate_extension_name` (`:2063-2073`) allows
1–64 chars of `[a-z0-9-]`. Symlinks are rejected via `fs::symlink_metadata` in
`validate_extensions` (`:2111-2116`) and again in `read_extension`
(`:1779-1784`); `required_extension_rejects_symlinked_payload` (`:2883-2914`)
proves a symlinked `rules.json` makes `open` fail. Files are created
`create_new(true)` and `sync_all()`ed before the descriptor is registered
(`:2047-2053`); duplicate names *or* paths are refused (`:2029-2036`).

**Unknown required extensions and future schemas are hard rejects.**
`validate_extensions` (`:2130-2140`) enumerates the four known names and fails
with `unknown required extension {name}` for any required entry outside
`rules | stream-events | websocket-messages | interop-provenance`. There is no
ignore switch in the crate — the check is unconditional, and the only theoretical
opt-out is a falsified `required_for_replay`, which migration tooling separately
blocks. `unknown_required_extension_is_rejected` (`:2916-2946`) edits the
manifest name to `future-required` and asserts `Invalid`. Future *session*
schemas fail as `UnsupportedSchema` at the range check (`:1517-1523`), matching
ADR 0005's requirement that older binaries reject schema 2 rather than silently
ignoring extensions.

| Extension | Path | Schema constant | Payload validation |
|---|---|---|---|
| `rules` | caller-supplied | `eggreplay_core::RULES_SCHEMA_VERSION` `:2142-2147` | `ScenarioRules::validate` `:2148-2149` |
| `stream-events` | `stream-events.json` `:575`, `:1349` | `eggreplay_core::stream::STREAM_EVENTS_SCHEMA_VERSION` `:2152-2157` | `StreamEvents::validate` `:2158-2159` + body-length cross-check `:1578-1603` |
| `websocket-messages` | `websockets.jsonl` `:589`, `:1373` | `eggreplay_core::WEBSOCKET_SCHEMA_VERSION` `:2162-2167` | must be required and at `websockets.jsonl` `:2168-2172`; `WebSocketTranscript::validate` `:2173-2177` |
| `interop-provenance` | caller-supplied | `eggreplay-har`'s `INTEROP_PROVENANCE_SCHEMA_VERSION` (`crates/eggreplay-har/src/lib.rs:36`) | none in the store; identity-only |

Two deliberate deviations from the ADR 0005 name list: the store writes
`stream-events.json` (one JSON `StreamEvents` document, `:575`) and
`websockets.jsonl` (one JSON transcript, `:1363`) — the `.jsonl` names are
historical. `read_extension` (`:1769-1799`) returns `Option<Vec<u8>>` by name
(`Ok(None)` when absent) after re-checking symlink and bound, and reads with
`take(MAX_EXTENSION_BYTES + 1)` so a file grown after the stat is still bounded
(`:1790-1797`).

---

## Copy, migration support, and finalization

**`Session::copy_to` (`:1619-1685`)** is the migration/duplication primitive.
It rejects an out-of-range target schema (`:1624-1628`) and refuses a
**downgrade** of a session that has extensions (`:1629-1633`). It creates a
fresh `SessionWriter` with the source's limits and the target schema stamped in
(`:1634-1636`), so `flow_count`/`blob_count`/`extensions` are re-derived and
revalidated rather than trusted. It copies the **entire blob namespace**, not
just flow-referenced blobs, because WebSocket payloads may reference blobs
(`:1638-1641`); the loop rejects symlinks and non-regular files (`:1643-1648`),
skips an empty `.gitkeep` (`:1649-1651`), validates each filename as a digest
(`:1652-1656`), revalidates the source through `open_blob` (`:1660`), then
`io::copy` into a new `BodyWriter` whose recomputed digest must equal the
filename or the copy fails `Integrity` (`:1664-1667`) — a source blob whose
content does not match its name cannot be laundered into the new publication.
Flows are re-appended (`:1669-1671`) and each extension payload is re-written
through `write_extension` so the new registry is re-bounded and re-confined;
a vanished extension fails as `"extension disappeared during copy"` (`:1675`).
`finish()` then publishes transactionally and reopens (`:1684`). It is `&self`
throughout: the source is never mutated and every digest is rechecked.
`merge_to` (`:1690-1766`) intersects both limit sets field-wise with `min`
(`:1701-1715`), merges stream-event payloads when descriptors match
(`:1732-1744`), and fails closed on conflicting extensions (`:1745-1750`).

**The `Migration` seam.** `Migration` (`:68-71`) and `SchemaOneMigration`
(`:74-87`) are the explicit migration API ADR 0004 requires before schema 2
(`plans/adrs/0004-eggr-storage-format.md:27`). `SchemaOneMigration` is a pure
range check returning the input version, with a doc comment stating no schema-2
migration is implemented (`:67`). `Session::open` does not use it — it inlines
the same check (`:1517-1523`) — and no other crate currently consumes the trait,
so it is a declaration rather than a live path.

**`WebSocketConversationFinalizer` (`:715-733`)** is the store's only hook into
a host runtime, and it is std-only by necessity. Every accepted HTTP/1 101
upgrade for recording must register exactly one finalizer before the request
path may detach (`:1021-1036`). `drive(self: Box<Self>)` blocks to a terminal
state and must not hold a store lock across the wait (`:716-720`);
`drive_with_deadline` defaults to delegating to an unbounded `drive`
(`:728-732`). Drop safety is a hard requirement — a finalizer dropped without
`drive` completing (panic/abort) must still release any waiter (`:711-714`).
`finish` drives every registered finalizer before the manifest is written
(`:1290-1303`), so a manifest can never reference a 101 flow whose transcript is
in flight. `eggreplay-http` supplies `ConversationCompletion`, signalling on
both `complete()` and `Drop` (`crates/eggreplay-http/src/recording.rs:1410-1417`).

**Drain/finalize entry points used by the recording gateway:**
`finish_recording_session` wraps `RecordingSession::finish` in `spawn_blocking`
so an async executor thread never blocks on a pending finalizer
(`crates/eggreplay-http/src/recording.rs:925-931`); `drain_active_blobs` is the
cooperative spin above; `RecordingSession::shutdown` (`:992-994`) is the
admission stop. `finish` itself stays unbounded by design (`:1280-1282`): rather
than time out, it fails closed on a 101 flow with no transcript.

---

## Integrity model

### Guarantees a reader can rely on

| # | Guarantee | Grounding |
|---|---|---|
| G1 | The fixture was fully finalized (`complete: true`) | `:1524-1526` |
| G2 | The session schema is understood (inclusive range) | `:1517-1523` |
| G3 | Flow records are valid; count matches the manifest; ids are unique | `:1938`, `:1569-1571`, `:1552-1557` |
| G4 | Every referenced blob exists, is a regular non-symlink file, and hashes to its declared digest at its exact declared length | `:2290-2320` |
| G5 | Blob count on disk matches `manifest.blob_count` | `:1604-1606` |
| G6 | Every extension descriptor is name-valid, path-confined, non-symlink, bounded, uniquely named and uniquely pathed, and its payload parses and validates | `:2090-2181` |
| G7 | No required-but-unknown extension exists | `:2130-2140` |
| G8 | Every WebSocket 101 flow has one validating conversation and every conversation references a real 101 flow; text payloads are valid UTF-8 of the declared length | `:2183-2288` |
| G9 | Stream-event DATA lengths agree with the recorded response body length and reference known flows | `:1578-1603` |
| G10 | A 101 recorded without a transcript cannot be published (both `finish` paths validate before writing the manifest) | `:596-601`, `:1386-1391` |
| G11 | `open_blob` returns an already-opened, length-verified handle with no blob allocation; streaming callers can detect replacement by incremental hash | `:1852-1871`, `:1839-1851` |
| G12 | Aggregate body bytes never exceed `max_total_bytes`, on both read and write | `:1559-1566`, `:535-543`, `:1214-1222` |
| G13 | Fixture files are created private on Unix (`0700`/`0600`) | `:2334-2341` |
| G14 | Concurrent writers never interleave into one file; aborted bodies leave no orphan blob | `:1141-1163`, `:885-897` |

### The axes of rejection

- **Path-based** → `Invalid`: extension name or path failing the confinement
  rules (`:2063-2088`); symlinked blob (`:1858-1860`, `:2296-2298`); symlinked or
  non-regular extension payload (`:2111-2116`); non-regular or symlinked entry in
  `blobs/` during copy/merge (`:1643-1648`, `:1882-1887`); non-UTF-8 blob filename
  (`:1652-1656`).
- **Digest-based** → `Invalid` for wrong *form*, `Integrity` for wrong
  *content*: non-64-char/non-lowercase digest (`:1956-1964`); content hashing to
  a different digest (`:2316-2317`); staging read-back length mismatch (`:193`,
  `:833`); copy whose recomputed digest differs from the filename (`:1664-1667`).
- **Length-based** → `Invalid` for declared values over bound, `Integrity` for
  observed mismatch: blob length over `max_blob_bytes` (`:1854-1856`,
  `:2292-2294`); JSONL line over `max_line_bytes` (`:1932-1936`, `:2201-2205`);
  flows over `max_flows` (`:1926-1931`); extension over 16 MiB (`:2024-2028`) or
  aggregate over 32 MiB (`:2041-2045`); on-disk length ≠ declared length
  (`:1863-1865` → `Integrity`).
- **Manifest-based**: `complete: false`; `flow_count` mismatch; `blob_count`
  mismatch; `websocket-messages` at schema 1 (`:1527-1536`).
- **Content-semantic**: unknown/duplicate flow id (`:1552-1557`, `:1095-1097`);
  duplicate extension name or path (`:2029-2036`); `schema_version == 0`
  (`:2014-2018`); known extension at a non-current schema (`:2142-2167`);
  transcript/flow disagreement in either direction (`:2228-2233`, `:2254-2258`,
  `:2282-2286`); invalid UTF-8 text payload (`:2272-2277`).
- **Schema-based**: `UnsupportedSchema` from the range checks and
  `SchemaOneMigration`.
- **I/O**: missing files, unreadable paths, rename failures → `Io`.

The axes matter because they imply different remediation: *path* and
*digest-form* rejections mean the fixture is hostile or malformed and must not
be served; *length* and *content* rejections mean truncation, corruption, or a
policy refusal. All fail closed — the store never repairs, truncates, or
substitutes bytes.

---

## Review checklist

**Lock discipline (`RecordingSession`)**
1. Does the new code hold the `flows` mutex across anything unbounded — I/O,
   hashing, allocation, a caller callback? Body bytes must never touch it
   (`:641-646`).
2. Is every exact counter (`flow_count`, `total_bytes`) checked and updated in
   the same critical section? Splitting check from `fetch_add` reintroduces the
   over-limit race the concurrency test guards (`:1209-1228`).
3. Does a new registry get its own mutex, and is `shutdown` re-checked *after*
   acquisition, not only before?
4. Does new accounting state need a `Drop` release so `active_blobs` cannot leak
   and permanently wedge `finish`?
5. Are new atomics `SeqCst`, or is a weaker ordering justified in a comment (as
   `unique_suffix`'s `Relaxed` is, `:1952`)?

**Bounds enforcement**
6. Which bound covers this input, and is it enforced at the earliest point the
   input exists — before the write, not after?
7. Does a new aggregate counter use a checked add against a limit, accumulated
   from on-disk metadata (as the extension total is at `:2037-2040`) rather
   than an in-memory counter a failed write could desynchronize?
8. Does a new extension get a name rule, path rule, size cap, aggregate cap, and
   a per-extension schema constant check?

**TOCTOU on paths**
9. Does the new read path check with `symlink_metadata` *and* open — or open a
   fixture-controlled path with no preceding check? Remember the unavoidable
   residual window between the two.
10. Is a new reader bounded (`take(MAX + 1)` as `read_extension` is, `:1792`),
    and is every fixture-supplied path still a confined filename?

**Publication ordering**
11. Is the new artifact written and `sync_all()`ed *before* `manifest.json` is
    created, and is the manifest still created last in staging?
12. Are open handles dropped before the staging rename (Windows)? `flows` and the
    manifest handle are both scoped deliberately (`:610-619`, `:1311-1324`,
    `:1417-1426`).
13. Does any failure path still leave staging without a manifest, with staging
    files removed so they cannot be counted as blobs?
14. Does `finish` still fail closed rather than race — active sinks non-zero, 101
    without transcript, extension that disappeared?

**Error-category mapping**
15. Is a *policy* refusal `Invalid` and a *verification* failure `Integrity`?
    Reversing them makes corruption look like user error.
16. Is a per-body limit violation (surfacing as `Io`, `:143-148`) acceptable
    here, or should it be fixed deliberately?
17. Does a new lock access map `PoisonError` to `StoreError::Invalid` rather than
    leaking it?
18. Is any new failure path that produces no `StoreError` at all — a panic, an
    `expect`, an ignored `let _ = fs::remove_file` whose loss matters?

### Load-bearing invariants I believe are under-tested

1. **Same-length blob replacement** is detected only downstream: `open_blob`
   checks length, not content (`:1862-1865`), so the guarantee lives in
   `eggreplay-http`'s streaming hash and is covered there, not in a store test.
2. **The `symlink_metadata`-then-open window** has no test and cannot have a
   deterministic one.
3. **`FlowIter`'s exact-`max_flows` boundary** — the EOF-at-limit behaviour C002
   explicitly fixed has no dedicated store test with `max_flows` set to exactly
   the record count.
4. **Finalizer drain ordering and re-entrancy** (`std::mem::take` at
   `:1295-1298`, drop safety at `:711-714`) are contract statements with no
   in-crate test.
5. **Staging reclamation after a failed `finish`** is asserted only as
   "destination does not exist" (`:2501`, `:2643`); the tests then ignore or
   remove the `.{name}.incomplete-*` sibling (`:2779`, `:2801`, `:2878`).
6. **`proptest` is declared** (`Cargo.toml:20`) but never used; the test module
   (`:2366-3428`) is hand-written and filesystem-based.
7. **The schema bump has two implementations** — `SessionWriter::write_extension`
   mutates metadata directly (`:434`) while `RecordingSession` derives it from
   registry emptiness (`:1392-1405`). One rule, two code paths.
