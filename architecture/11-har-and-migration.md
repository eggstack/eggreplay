> Deep dive for [overview](overview.md).

# HAR interchange and fixture migration

`eggreplay-har` is the only crate that speaks HAR, and the CLI's `migrate`
command is the only surface that rewrites a fixture's session schema. Together
they cover the two "boundary" concerns of the product: getting traffic recorded
by *other* tools into a fixture, and getting old fixtures onto a schema the
current binary understands. Both are explicitly lossy operations, and both
refuse to mutate a fixture in place until a validated replacement exists.

The two live in one file — `crates/eggreplay-har/src/lib.rs`, 2,479 lines,
including its unit tests — because the import path, the export path, and the
migration registry share one vocabulary of loss (`LossAction`) and one set of
extension names.

## Crate contract

The crate is a pure transformation boundary. Its own module doc states the two
negative guarantees first: "this crate performs no network I/O and adds no HTTP
stack" (`crates/eggreplay-har/src/lib.rs:8-9`), and it "converts already-observed
HAR JSON into `eggreplay_core` flows (via `eggreplay_store` writers) and
projects opened sessions back into HAR JSON" (`crates/eggreplay-har/src/lib.rs:9-11`).
`#![forbid(unsafe_code)]` and `#![deny(missing_docs)]` are crate-level
(`crates/eggreplay-har/src/lib.rs:15-16`).

Dependencies are exactly what that sentence implies
(`crates/eggreplay-har/Cargo.toml:10-18`):

| Dependency | Why the contract needs it |
|---|---|
| `eggreplay-core` | `Flow`, `HttpRequest`, `HttpResponse`, `FlowError`, `BodyRef`, `QueryPair`, `HeaderEntry`, `Provenance`, `RedactionConfig` |
| `eggreplay-store` | `Session`, `SessionWriter`, `StoreError` — blob publication, `copy_to`, extension writing |
| `serde` / `serde_json` | HAR wire structs in, HAR document out |
| `base64` | binary body decoding and encoding |
| `url` | URL splitting, query cross-check, form re-encoding |
| `chrono` | RFC 3339 `startedDateTime` parsing and formatting |
| `thiserror` | `HarError`, `MigrationError` |

Nothing transport-shaped appears. There is no `http` dependency, no
`hyper`/`reqwest`/`tokio`, and the only store types imported are
`Session`, `SessionWriter`, and `StoreError` (`crates/eggreplay-har/src/lib.rs:24`).
The CLI depends on it unconditionally, with no feature gate
(`crates/eggreplay-cli/Cargo.toml:18`), so `har import`, `har export`, and
`migrate` are always available.

The public surface is small and symmetric:

| Item | Location | Purpose |
|---|---|---|
| `import_har_to_writer` | `lib.rs:799` | HAR 1.2 document → appended flows + `ImportReport` |
| `export_session_to_har` | `lib.rs:1516` | opened `Session` → `(serde_json::Value, ExportReport)` |
| `migrate_session` | `lib.rs:453` | validated `Session` → new fixture at a target schema |
| `migration_blockers` | `lib.rs:415` | extension descriptors → blocking reasons |
| `registered_migrators` | `lib.rs:405` | the migrator table, as `(name, current_schema)` |
| `InteropProvenance` | `lib.rs:374` | the optional extension payload |
| `LossAction`, `HarLoss`, `ImportReport`, `ExportReport` | `lib.rs:64`, `:79`, `:125`, `:136` | the shared loss vocabulary |

Note the direction of each dependency edge: import takes a *writer* the caller
already created, and export takes a *borrowed* `&Session`. The crate never
opens or creates a fixture root on its own, so fixture creation policy (limits,
metadata, publication) stays with `eggreplay-store` and the CLI.

## The loss model

The central claim is in the module doc and repeated in
`docs/har-interchange.md:3-7`: HAR is never canonical, and "both directions are
lossy by design and never imply round-trip losslessness" (`lib.rs:5-6`).

Three bounds make "lossy" quantitative rather than rhetorical.

**The notice.** `HAR_LOSSY_NOTICE` is a fixed string
(`lib.rs:38-39`): "Lossy HAR export from EggReplay; not round-trip lossless.
See log._eggreplay.losses." It is written into `log.comment` on every export
(`lib.rs:1833`) and repeated as `_eggreplay.export_note` (`lib.rs:1838`), and a
unit test asserts `log.comment` equals the constant verbatim
(`lib.rs:2286-2289`). A downstream HAR consumer therefore cannot mistake an
EggReplay export for a capture tool's lossless record.

**The document bound.** `MAX_HAR_FILE_BYTES` is 32 MiB (`lib.rs:32`), checked
before parsing (`lib.rs:807-809`). This is checked on the *incoming* bytes, so
the rejection happens before any allocation of parsed structure and before a
writer sees a byte.

**The entry bound.** `MAX_HAR_ENTRIES` is 100,000 (`lib.rs:34`), checked against
`log.entries.len()` at `lib.rs:818-820`. The constant is deliberately identical
to the store's flow ceiling — `StoreLimits::max_flows` is `100_000`
(`crates/eggreplay-store/src/lib.rs:41`), enforced when appending at
`store:522` — so an import can never be accepted by HAR bounds and then rejected
by the store's own limit halfway through. The doc comment says as much:
"matches store flow ceiling" (`lib.rs:33`).

A fourth bound is not a constant but a policy: an unsupported entry fails the
*whole* import, so no partially-degraded fixture is ever published
(`lib.rs:787-789`). The same fail-closed posture is why the per-entry rejection
reasons are `HarError::UnsupportedEntry { index, reason }` rather than a skip
counter (`lib.rs:150-156`).

What a reader must never assume after a round trip:

- **Field order and encodings are not preserved verbatim.** The URL is rebuilt
  from semantic query pairs through `form_urlencoded`
  (`lib.rs:1590-1594`), so percent-encoding is normalized on the way out.
- **Identity is not preserved.** Flow ids are regenerated on every import as
  `har-import-{index:06}-{pid-nanos}` (`lib.rs:1252`, `unique_suffix` at
  `lib.rs:1421-1428`). Anything keyed by flow id — `stream-events`,
  `websocket-messages` — cannot survive a HAR round trip.
- **Typed outcomes degrade.** A `FlowOutcome::Error` exports as `status: 0`
  with the category/phase in `statusText` (`lib.rs:1729-1751`), and re-import
  rebuilds it as `ErrorCategory::Other` / `ErrorPhase::Other`
  (`lib.rs:1269-1273`). `status_text` is declared on the wire struct
  (`lib.rs:263`) and never read on import, so the specific category/phase is
  dropped.
- **Timing detail is synthetic.** Export writes
  `timings: {send: 0, wait: duration_ms, receive: 0}` (`lib.rs:1803`) and
  reports that as a global loss (`lib.rs:1811-1815`).
- **Extensions do not travel.** See the export and migration sections.

## Import

`import_har_to_writer(writer, har_bytes, redaction, profile_id,
max_structured_bytes, max_blob_bytes)` (`lib.rs:799-806`) appends one flow per
convertible entry to a *freshly created* writer. Two phases run: a parse phase
that builds flows with `BodyRef::Empty` placeholders and buffers body bytes
in `pending` (`lib.rs:398`, `:1358`), and a publish phase that writes blobs and
appends flows (`lib.rs:1364-1375`). That split is what makes redaction
guaranteed-before-publication, discussed below.

**URL.** `split_url` (`lib.rs:660-701`) parses with the `url` crate and rejects
anything that is not an absolute `http`/`https` URL with a host and an
origin-form path. It returns scheme, authority (host plus explicit port),
path, and the raw string. Empty paths normalize to `/` (`lib.rs:681-688`).
Userinfo is never persisted: a URL with a username or password produces a
`Redacted` loss on `request.url.userinfo` (`lib.rs:866-878`).

**Query.** `queryString` is authoritative *for ordering*
(`lib.rs:880-900`), bounded at 1,024 pairs with name ≤ 1,024 and value ≤ 8,192
bytes (`lib.rs:883-895`). The URL's own query component is then cross-checked
against the decoded array (`lib.rs:901-935`). Two outcomes: an empty
`queryString` with a non-empty URL query adopts the URL pairs and records
`Annotated` (`lib.rs:915-925`); a genuine disagreement keeps the array and
records `Annotated` naming the array as authoritative
(`lib.rs:926-933`).

**Headers and cookies.** The `headers` array wins; `header_entries`
(`lib.rs:640-658`) enforces ≤ 256 entries, name ≤ 256, value ≤ 8,192, and
preserves observed case and duplicates verbatim — the comment at
`lib.rs:651` is explicit that canonicalization is a separate concern. The
`cookies` array is only a consistency check: names absent from the joined
`Cookie`/`Cookie2` headers produce an `Annotated` loss saying the headers are
authoritative (`lib.rs:940-978`). Cookies are never merged into headers.

**postData.** Three shapes, all recorded in the loss report
(`lib.rs:1004-1052`):

- `postData.text` is decoded by `decode_har_body_text`
  (`lib.rs:497-537`): absent encoding means raw UTF-8 bytes; `base64` strips
  whitespace, bounds the *encoded* length at `4/3 * max + 4` before decoding,
  then re-checks the decoded length; any other encoding is `HarError::Invalid`.
- `postData.params` without `text` is re-encoded deterministically through
  `url::form_urlencoded` (`lib.rs:1020-1032`) and reported as `Annotated`. A
  param with a `fileName` is a multipart upload and is rejected outright
  (`lib.rs:1022-1029`).
- `postData` present but empty is an explicit zero-byte body
  (`lib.rs:1039-1043`). Both `params` and `text` present is a duplication, and
  `text` wins (`lib.rs:1044-1051`).

**Response.** `status_code` (`lib.rs:601-638`) accepts a number or numeric
string, maps `0`/empty to "no response", and rejects anything outside 100–599.
A present status becomes `FlowOutcome::Response` with `BodyRef::Empty`; a
status-0 entry becomes `FlowOutcome::Error(ErrorCategory::Other,
ErrorPhase::Other)` with an `Annotated` loss (`lib.rs:1255-1274`).
`response.content` is decoded with the same base64 rules; `compression`
savings, a negative `size`, and "no `text` but nonzero size" are all
`Annotated` with decoded bytes authoritative (`lib.rs:1132-1163`).

**Timing.** `startedDateTime` must be RFC 3339 (`started_ms`, `lib.rs:559-570`).
`entry.time` becomes the total duration, rounded and clamped to
`0..=3_600_000` so the float-to-int cast stays lossless in range
(`total_time_ms`, `lib.rs:572-584`); a missing `time` is 0. A populated
`timings` breakdown is counted — `-1` is HAR's "not applicable" and is skipped
(`timing_number`, `lib.rs:586-599`) — and if any phase is applicable the whole
breakdown is reported as collapsed (`lib.rs:1181-1205`).

**BodyRef production.** Import writes exactly two of the three body states.
The assembled flow starts at `BodyRef::Empty` for both request and response
(`lib.rs:1259`, `:1287`); `publish_body` (`lib.rs:773-781`) then maps zero
length to `BodyRef::Empty` and any non-empty byte run to
`writer.begin_blob()` → `write_all` → `sink.finish()`, which yields a
`BodyRef::Blob`. `BodyRef::Absent` is never produced by import — it appears only
on the read side of export (`lib.rs:1480`). A body state that is non-empty with
no source is treated as corruption (`lib.rs:1068-1072`).

**Store writer usage.** Three store APIs are the only publication points:
`begin_blob`/`finish` for bodies (`lib.rs:777`, `:780`), `append_flow` per flow
(`lib.rs:1373`), and `write_extension` once for provenance
(`lib.rs:1395-1403`). Because the caller owns the writer, a failure anywhere
above returns `Err` with the writer unfinished, and the doc comment states the
caller "must drop it without finishing on error so no partial fixture is
published" (`lib.rs:1361-1363`).

**Redaction precedes publication.** This is the property to check first in any
review. Body redaction runs inside the parse phase — `redact_import_body` at
`lib.rs:1053` (request) and `lib.rs:1165` (response) — while blob writes happen
later at `lib.rs:1366-1371`. `redact_import_body` (`lib.rs:709-771`) mirrors the
recording gateway: JSON bodies transform when the media type is
`application/json` or `*+json` and produce both redacted bytes and markers
(`lib.rs:731-744`); form bodies transform for
`application/x-www-form-urlencoded` (`lib.rs:745-764`); a body over
`max_structured_bytes` fails closed (`lib.rs:723-729`); and structured
selectors against a non-JSON media type also fail closed rather than publishing
unredacted bytes (`lib.rs:765-769`). After structural assembly,
`eggreplay_core::redact_flow` handles headers and query (`lib.rs:1300`), and if
any body transform occurred, `reconcile_headers_after_body_redaction` recomputes
representation headers such as `Content-Length` and drops invalidated
validators (`lib.rs:1327-1351`). Each flow is validated with `flow.validate()`
before it is queued (`lib.rs:1353-1357`).

Provenance on every imported flow is fixed: `mode: "har-import"`,
`observer: "eggreplay-har"` (`lib.rs:1292-1295`). HAR comments survive as
bounded annotations (`har.request.comment`, `har.response.comment` truncated to
512, `har.entry.time_ms`, `har.creator` truncated to 256) at
`lib.rs:1232-1250`, with `truncate_bounded` at `lib.rs:1412-1419`.

## Loss reporting

Loss is never silent: every import returns an `ImportReport { flows, losses,
entries }` (`lib.rs:124-132`) and every export returns
`ExportReport { entries, losses }` (`lib.rs:136-141`).

The unit of report is `HarLoss` (`lib.rs:78-92`): an optional `entry_index`, an
optional `flow_id`, a `field` path (the doc comment's examples are
`request.cookies[2]` and `response.trailers`), a bounded secret-free `reason`,
and an `action`. `HarLoss::global` and `HarLoss::entry` are the two
constructors (`lib.rs:94-121`), so a reader can tell a document-wide loss from
an entry-local one.

`LossAction` (`lib.rs:64-75`) is the classification that makes the loss
actionable rather than decorative:

| Action | Meaning | Where it is used on import |
|---|---|---|
| `Omitted` | HAR cannot represent it; value dropped | `log.browser`, `log.pages`, `serverIPAddress`/`connection`, `cache`, `pageref`, response markers on an error outcome |
| `Annotated` | value preserved, representation changed | queryString/URL divergence, cookie views, `httpVersion` collapse, `timings` collapse, `content.compression`, negative size, `redirectURL` duplication, `params` re-encode, status 0, creator |
| `Redacted` | replaced by policy before publication | `request.url.userinfo`, `request.body.json`, `request.body.form`, header/query field counts |
| `Preserved` | value kept as-is | declared, not constructed by the current code |
| `Rejected` | entry refused entirely | declared, not constructed; refusals surface as `HarError::UnsupportedEntry` instead |

That last row is a deliberate split: a *rejection* is an error return, not a
loss entry, because a rejected entry fails the whole import.

Detected loss classes on import, grouped by the phase that detects them:

- **Document level** — `log.browser` and `log.pages` have no semantic
  equivalent and are `Omitted` (`lib.rs:823-841`); a non-EggReplay
  `log.creator` is `Annotated` because it survives only in
  `interop-provenance` (`lib.rs:842-853`).
- **Representation collapse** — `httpVersion` on either side
  (`lib.rs:979-994`, `:1085-1097`), `timings` breakdown
  (`lib.rs:1181-1205`), `response.content.compression` and negative `size`
  (`lib.rs:1133-1150`), `response.redirectURL` duplicating `Location`
  (`lib.rs:1098-1110`), `request.httpVersion`.
- **Derived views** — request and response `cookies` arrays
  (`lib.rs:940-978`, `:1077-1084`).
- **Divergence** — `queryString` versus the URL query component
  (`lib.rs:901-935`); HAR comments kept as annotations
  (`lib.rs:995-1002`, `:1111-1118`).
- **Omitted physical state** — `serverIPAddress`, `connection`, `cache`,
  `pageref` (`lib.rs:1206-1229`).
- **Policy** — every redaction, whether userinfo, JSON body, form body, or
  header/query count (`lib.rs:872-877`, `:736-742`, `:756-762`,
  `:1302-1312`).
- **Semantics-bearing** — a status-0 entry becoming a typed error
  (`lib.rs:1263-1268`), and redaction markers being dropped when the outcome has
  no response body (`lib.rs:1317-1326`).

Why this matters: `losses` is serialized on both the `ImportReport` and, bounded
to the first 1,024 entries, into the provenance extension
(`lib.rs:1391`). A caller can therefore machine-check "what did this import not
bring in" without diffing two fixtures.

## Export

`export_session_to_har(&Session)` (`lib.rs:1516-1518`) only *reads*: the
session is accessed through `manifest()` (`lib.rs:1520`), `iter_flows()`
(`lib.rs:1581-1585`), and `read_blob` via `read_body_bytes`
(`lib.rs:1478-1483`). It never calls `write_extension`. That is the structural
reason export writes no session extension: provenance and loss live in the HAR
document, in a section foreign tools ignore, rather than in a manifest registry
that would require a schema-2 write and a reader contract.

The document shape (`lib.rs:1828-1842`):

```json
{"log": {
  "version": "1.2",
  "creator": {"name": "eggreplay", "version": "<TOOL_VERSION>"},
  "entries": [ ... ],
  "comment": "<HAR_LOSSY_NOTICE>",
  "_eggreplay": {"tool_version", "session_schema", "session_id", "export_note", "losses"}
}}
```

`session_schema` and `session_id` come straight from the manifest
(`lib.rs:1836-1837`), so a consumer can tell exactly which fixture projection
it is holding. Each entry additionally carries
`_eggreplay: {flow_id, redactions, annotations, provenance}`
(`lib.rs:1791-1796`, attached at `lib.rs:1805`) — this is where flow identity
and redaction-marker provenance survive the projection, since HAR has nowhere
else to put them.

Per-flow projection:

- **URL** is reassembled from scheme, authority, and path, with query pairs
  re-encoded through `form_urlencoded` and appended when non-empty
  (`lib.rs:1589-1605`).
- **Headers** are emitted verbatim as name/value arrays (`har_headers`,
  `lib.rs:1438-1443`); duplicates preserved. `cookies` arrays are *derived* by
  splitting `Cookie`/`Cookie2`/`Set-Cookie` header values on `;` and taking the
  first pair, capped at 256 (`har_cookies_from_headers`, `lib.rs:1445-1469`).
  Cookie attributes are never projected.
- **Bodies**: `BodyRef::Absent` and `BodyRef::Empty` both yield no content;
  a blob is read, and if it is valid UTF-8 it becomes `text` with no encoding,
  otherwise base64 `text` plus `encoding: "base64"` with an `Annotated` loss
  (`lib.rs:1607-1628` for the request, `:1677-1696` for the response). A
  `postData` object is only emitted when there is a body
  (`lib.rs:1629-1635`), with the media type taken from `Content-Type` and
  defaulting to `application/octet-stream`.
- **Trailers** on either side are `Omitted` with a per-flow loss
  (`lib.rs:1637-1645`, `:1697-1705`).
- **Typed errors** become `status: 0` with `statusText` set to
  `Category/Phase` and an explanatory comment (`lib.rs:1729-1751`); the loss
  names both enum values (`lib.rs:1730-1739`).
- **WebSocket** detection is by `Upgrade: websocket` header or a 101 outcome,
  and is reported as handshake-only (`lib.rs:1776-1789`).
- **Timing** is `startedDateTime` from `started_at_ms` RFC 3339 with
  millisecond precision, `time` as the delta, and synthetic per-phase timings
  (`lib.rs:1647-1655`, `:1798-1804`). An out-of-range timestamp falls back to
  the epoch rather than failing (`lib.rs:1648-1652`).

Session-level export losses are emitted before any flow is read
(`lib.rs:1523-1579`): `rules` and `stream-events` and `websocket-messages` are
`Omitted` with per-extension reasons, `interop-provenance` is `Annotated` as
"EggReplay-internal, summarized in `log._eggreplay`", and an *unknown optional*
extension is `Omitted`. Two global losses are appended once at the end for
non-empty sessions: synthetic timings and the `http/1.1` version assumption
(`lib.rs:1809-1821`).

A unit test pins the required set of export loss fields —
`request.trailers`, `response.trailers`, `response`,
`extensions.stream-events`, `redactions`, `physical_route`, `timings`,
`httpVersion` — and asserts at least eight losses reach
`log._eggreplay.losses` (`lib.rs:2271-2290`).

## Interop provenance extension

`INTEROP_PROVENANCE_SCHEMA_VERSION` is `1` (`lib.rs:36`). The payload struct
`InteropProvenance` (`lib.rs:374-391`) records exactly six things:

| Field | Meaning |
|---|---|
| `schema_version` | extension schema (1) |
| `tool_version` | `eggreplay_core::TOOL_VERSION` of the importing binary |
| `source` | fixed `har-import` |
| `har_creator` | upstream `name/version` when declared (`lib.rs:1379-1383`) |
| `entries` / `flows` | HAR entries consumed versus flows published |
| `losses` | the structured loss list, bounded to 1,024 entries (`lib.rs:1391`) |

It is written once per import through the store writer
(`lib.rs:1395-1403`) as `interop-provenance.json`, schema 1, with
`required_for_replay = false`. That flag is the whole point: per ADR 0005, a
required extension forces a reader that does not understand it to reject the
fixture, which would be exactly wrong for advisory metadata. Setting it false
means an older binary ignores the file and replays identically
(`plans/adrs/0005-versioned-session-extensions.md`, "Unknown optional extensions
may be preserved/ignored only when doing so cannot change replay behavior").

So: the extension never changes replay semantics, it is ignored by readers that
do not know it, and the `entries`/`flows` pair plus the bounded loss list is
what lets an operator answer "what did this fixture lose, and where did it come
from" months later without the original HAR file. `docs/eggr-schema.md` states
the same contract in the "Migration and HAR provenance (M014A)" section.

Note the asymmetry that makes this cheap: *import* writes the extension,
*export* does not (`docs/eggr-schema.md`, same section). Provenance is a
one-way note attached at the moment of lossy ingress; export keeps its notes in
the HAR document.

## Known extensions and migrators

`CURRENT_SESSION_SCHEMA` is re-exported from core as
`eggreplay_core::SESSION_SCHEMA_VERSION` (`lib.rs:42`), which is `2`
(`crates/eggreplay-core/src/lib.rs:66`); `SESSION_SCHEMA_V1` is `1`
(`crates/eggreplay-core/src/lib.rs:63`). Flow records stay at
`FLOW_SCHEMA_VERSION = 1` (`crates/eggreplay-core/src/lib.rs:60`) — the session
schema moved, the flow contract did not.

`KNOWN_EXTENSIONS` (`lib.rs:48-59`) is the closed table of names migration
understands, each pinned to its current schema:

| Name | Current schema |
|---|---|
| `rules` | `RULES_SCHEMA_VERSION` |
| `stream-events` | `core::stream::STREAM_EVENTS_SCHEMA_VERSION` |
| `websocket-messages` | `WEBSOCKET_SCHEMA_VERSION` |
| `interop-provenance` | `INTEROP_PROVENANCE_SCHEMA_VERSION` (1) |

`registered_migrators()` (`lib.rs:405-410`) returns that same table. The doc
comment is explicit that only identity migration (current-to-current) is
implemented and that older registered schemas "would list their migrator here"
(`lib.rs:401-404`) — the table is the extension point, not a set of implemented
rewrites.

The blocking rule lives in `migration_blockers` (`lib.rs:415-441`):

- **Unknown name, `required_for_replay = true`** → blocked with a message
  naming the extension and its schema (`lib.rs:422-429`). An unknown
  *optional* extension is silently allowed, which is the same rule ADR 0005
  states for reading.
- **Known name, schema != current** → blocked, because no migrator owns that
  version transition (`lib.rs:430-437`).
- **Known name, current schema** → allowed, identity.

There is no ignore-required-extension switch. `Session::open` already fails
closed on the same conditions, and `migrate_session` re-checks before copying,
so a fixture that opens can still be blocked from migration — the unit test
constructs the descriptors directly for exactly this reason
(`lib.rs:2333-2343`).

`migrate_session` (`lib.rs:453-473`) is then thin and deliberately so: reject
a target outside `SESSION_SCHEMA_V1..=CURRENT_SESSION_SCHEMA` as
`Blocked` (`lib.rs:458-462`), reject non-empty blockers before any copy
(`lib.rs:463-466`), and delegate to `Session::copy_to`, which "streams and
revalidates every blob/flow/extension and publishes transactionally; downgrades
with extensions fail closed inside the store" (`lib.rs:467-472`;
`crates/eggreplay-store/src/lib.rs:1615-1619`). The source session is only ever
read.

## Migration (migrate)

`MigrateArgs` (`crates/eggreplay-cli/src/main.rs:631-649`) is
`--fixture` plus one of `--to <path>` or `--in-place`, optional `--overwrite`,
optional `--target-schema <u16>`, and the standard `OutputArgs`. The flag is
`--to` rather than `--output` for a documented reason: `--output` already
selects the envelope format on every command (`main.rs:634-637`). Dispatch is a
single match arm (`main.rs:732`).

`migrate` (`main.rs:2539-2642`) runs in this order:

1. **Flag validation.** `--to` with `--in-place` is a `configuration` error
   (exit 2); neither is also `configuration` (`main.rs:2540-2551`).
2. **Target validation.** Default is the current schema; a value outside
   `SESSION_SCHEMA_V1..=SESSION_SCHEMA_VERSION` is `configuration`
   (`main.rs:2552-2562`).
3. **Open the source** with `Session::open` and default limits; failures are
   `fixture` (`main.rs:2563-2564`).
4. **Stage.** Compute
   `sibling_transaction_path(&target, "migrate")` and call
   `eggreplay_har::migrate_session` into it. `MigrationError::Blocked` and
   `Store` map to `fixture`; `Runtime` maps to `runtime`
   (`main.rs:2569-2579`, and the identical arm for `--to` at
   `main.rs:2606-2615`).
5. **Read the result** — `flow_count` and `schema_version` come from the
   *migrated* manifest — then `drop` both sessions before renaming
   (`main.rs:2580-2583`, `:2616-2618`).
6. **Publish** with `replace_fixture_transactionally`.

`--in-place` always publishes through `replace_fixture_transactionally`
(`main.rs:2584`). `--to` refuses an existing destination unless `--overwrite`
(`main.rs:2596-2601`); if the destination exists it publishes transactionally
(`main.rs:2619-2621`), otherwise it creates the parent directory and does a
plain `rename` of the staged sibling (`main.rs:2623-2631`).

The two helpers (`main.rs:1264-1329`):

- `sibling_transaction_path(fixture, label)` builds
  `.{name}.{label}-{pid}-{nonce}` in the fixture's own directory
  (`main.rs:1264-1274`). Labels in use are `migrate`, `backup`, `misses`,
  `combined`, and `rerecord`. Same-directory staging is what makes the final
  `rename` atomic rather than a cross-device copy.
- `replace_fixture_transactionally(staged, target)` is a three-step rollback
  dance (`main.rs:1276-1299`): if the target does not exist, rename; otherwise
  rename the target to a `backup` sibling, then rename staged into place, and
  on failure rename the backup back — reporting either "old fixture restored"
  or, if the restore itself failed, the backup path an operator must use by
  hand (`main.rs:1286-1295`). Only after a successful publish is the backup
  removed, and a failure there is reported as "published but backup could not
  be removed" (`main.rs:1296-1298`) rather than being conflated with a
  publication failure.
- `recover_fixture_transactionally(target)` is the crash-recovery counterpart
  (`main.rs:1301-1329`): if the target is missing, it scans for
  `.{name}.backup-*` siblings, sorts them, and restores the newest one that
  actually opens as a valid `Session` (`main.rs:1321-1327`). Validating by
  opening, rather than by name order alone, is what keeps a torn backup from
  being promoted.

A second staging layer lives inside the store: `SessionWriter::create` and the
copy path build `.{name}.incomplete-{suffix}` directories
(`crates/eggreplay-store/src/lib.rs:389`, `:923`) with private permissions
(`store:926-927`). So a migration touches two staging names in sequence — the
CLI's `.{name}.migrate-<pid>-<nonce>`, and inside it the store's
`.incomplete-` directory that becomes a manifest-last-published fixture before
being renamed into the CLI staging path. The contract in
`docs/eggr-schema.md` is that the manifest is the final publication marker,
which is why an interrupted migration leaves a dot-prefixed sibling rather than
a half-valid fixture.

Observed contracts, pinned by `crates/eggreplay-cli/tests/har_migrate.rs`:

| Test | Asserts |
|---|---|
| `migrate_upgrades_schema_one_to_current_transactionally:341` | `target_schema: 2`, `flow_count: 2`, and the checked-in `schema-1-with-flows` source still validates at schema 1 |
| `migrate_current_to_current_is_idempotent:375` | migrating `schema-2-stream` twice yields equal `manifest().metadata`, equal `manifest().extensions`, and equal flow vectors |
| `migrate_in_place_replaces_atomically_and_rejects_conflicting_flags:426` | after in-place migration the fixture validates at schema 2; `--in-place` with `--to` exits 2 |
| `migrate_refuses_to_overwrite_destination_without_flag:478` | exit 2 and a pre-existing marker file inside the destination is preserved |
| `migrate_future_extension_fails_closed_without_mutating_source:502` | a `stream-events` schema bumped to 99 exits 3, no destination directory is created, and the source manifest bytes are unchanged |

Exit-code mapping is the shared CLI contract: `configuration` → 2, `fixture` →
3, `runtime` → 4 (`main.rs:662-667`; `docs/har-interchange.md:101-109`).

## Test corpus

HAR documents live in `crates/eggreplay-har/tests/corpus/` and are read through
`corpus_path` (`lib.rs:2350-2354`). The crate has no integration-test
directory; the corpus is consumed by the in-crate golden test
`golden_har_corpus_imports_with_expected_losses` (`lib.rs:2358`).

| Corpus file | What the test proves (`lib.rs`) |
|---|---|
| `minimal.har` | 2 entries import to exactly 2 flows and re-export to 2 entries (`:2361-2384`); the unit test `import_preserves_method_url_query_headers_status_body_timing:1899` additionally pins scheme/authority/path, query order, header values, status, `b"hello"` body, `started_at_ms`, `provenance.mode == "har-import"`, and the presence of `interop-provenance` |
| `duplicates.har` | 1 flow; a `request.cookies` loss is reported; the two `X-Dup` headers both survive (`:2388-2428`) — duplicates are never collapsed |
| `binary-and-error.har` | 2 flows; losses include `request.url.userinfo` and `request.httpVersion`; the second flow is a `FlowOutcome::Error`; the first flow's body is the blob `[0,1,2,3,4]` (`:2431-2476`) — base64 decode and status-0 handling together |

Session fixtures live in `crates/eggreplay-store/tests/fixtures/`, referenced
by `store_fixture` in the CLI test (`har_migrate.rs:41-44`) and enumerated in
`docs/har-interchange.md:146-151`:

| Fixture | Proves |
|---|---|
| `schema-1-empty` | the empty schema-1 baseline; the cheapest in-place upgrade target (`har_migrate.rs:426-461`) |
| `schema-1-with-flows` | a real schema-1 fixture with ordered queries and duplicate headers; the 1→2 upgrade source, and it must remain readable at schema 1 afterwards (`har_migrate.rs:341-371`) |
| `schema-1-with-flows-migrated-v2` | the checked-in 1→2 result, pinning the upgrade output as a golden rather than a runtime-only assertion |
| `schema-2-rules` | a known extension (`rules`) that must migrate by identity |
| `schema-2-stream` | a `stream-events` fixture; the idempotence and future-schema-blocking source (`har_migrate.rs:375-423`, `:502-549`) |
| `schema-2-websocket` | `websocket-messages`; its `blobs/.gitkeep` exists because the store otherwise ignores empty blob directories (`plans/closure/m014a-har-and-migration.md:18-21`) |
| `schema-2-interop` | the `interop-provenance` extension as stored on disk after a HAR import |

In-crate unit tests cover the areas the corpus cannot: the golden corpus itself
(`lib.rs:2358`), duplicate/cookie views (`:1978`), redaction before publication
(`:2046`), version rejection plus status-0 mapping (`:2120`), the full export
loss matrix (`:2183`), and migration blocking plus idempotence (`:2298`). The
store's own golden test covers all seven fixtures
(`plans/closure/m014a-har-and-migration.md:34-35`).

## Review checklist

For any change to this crate or the `migrate` command:

1. **Redaction before blob publication.** Confirm no `begin_blob` /
   `append_flow` / `write_extension` call can be reached with un-redacted
   bytes. The invariant is structural: redaction is in the parse phase
   (`lib.rs:1053`, `:1165`, `:1300`) and publication is in the second loop
   (`lib.rs:1364-1371`). Moving publication earlier, or buffering
   `request_raw`/`response_raw` past redaction, breaks the guarantee silently.
   Also check the fail-closed paths: structured selectors on a non-JSON media
   type (`lib.rs:765-769`) and oversized bodies
   (`lib.rs:723-729`) must still be errors, not pass-throughs.
2. **Loss-report completeness.** A new silent drop is the default failure mode
   of this design. Every field HAR cannot carry needs a `HarLoss` with a
   `field` path and a secret-free `reason`; prefer `Omitted` for dropped values
   and `Annotated` for representation changes. Verify the loss still reaches
   both `ExportReport`/`ImportReport` and `log._eggreplay.losses`, and that
   `log.comment` still equals `HAR_LOSSY_NOTICE`.
3. **Migration idempotency.** `migrate` at the current schema must be a fixed
   point: manifest metadata, extension descriptors, and the flow vector must be
   byte-equal across repeated runs. The 1→2 upgrade must not touch flow records
   (`FLOW_SCHEMA_VERSION` stays 1), and `schema-1-with-flows-migrated-v2`
   should not need regenerating for an unrelated change.
4. **Transactional publication and rollback.** The source must be untouched
   until the staged copy has validated. Check that both `Session` handles are
   dropped before any rename (`main.rs:2582-2583`, `:2618`), that
   `replace_fixture_transactionally` restores the backup when the publishing
   rename fails (`main.rs:1286-1295`), and that a failed `remove_dir_all` of the
   backup is reported as a *post-publication* problem rather than a migration
   failure (`main.rs:1296-1298`). Confirm that a failure at any earlier step
   leaves no visible destination (`har_migrate.rs:544`).
5. **Extension blocking.** An unknown `required_for_replay` extension and a known
   extension at a non-current schema must both block with a message naming the
   extension, before any copy (`lib.rs:415-441`, `:463-466`). Adding a name to
   `KNOWN_EXTENSIONS` is a semantic act: it declares that current-to-current
   identity migration is correct for that extension, so a new entry needs a
   registered migrator whenever a prior schema exists.
6. **Could a round trip silently change replay semantics?** This is the
   question the whole design exists to answer honestly. The known answers today:

   | Round trip | Effect on replay | Visible in the loss report? |
   |---|---|---|
   | `FlowOutcome::Error` → `status: 0` → `Other/Other` | category and phase collapse; a connect timeout becomes a generic error | yes, `Annotated` on `response` / `response.status` |
   | flow id regenerated (`har-import-{index:06}-…`) | anything keyed by flow id — stream events, WebSocket conversations, rule references — no longer resolves | only indirectly, via the dropped-extension losses |
   | `stream-events` / `websocket-messages` / `rules` omitted from HAR | a partial or mid-body-error stream replays as a plain body; a WebSocket conversation replays as a bare handshake | yes, global `Omitted` |
   | `physical_route` omitted | route-dependent matching has nothing to match against | yes, per-flow `Omitted` |
   | request/response trailers omitted | trailer-sensitive assertions see none | yes, per-flow `Omitted` |
   | query re-encoded through `form_urlencoded` | percent-encoding normalized (`%20` vs `+`); an exact-URL matcher may miss | **no** — no loss is recorded for this |
   | `BodyRef::Absent` re-imports as `BodyRef::Empty` | a matcher distinguishing absent from empty body can change behaviour | **no** — no loss is recorded for this |
   | cookie attributes dropped | attribute-dependent matching sees none | partially, via the `cookies` `Annotated` loss |
   | synthetic timings | cadence is not reproduced | yes, global `Annotated` |

   The last two unflagged rows are the interesting ones: a re-imported HAR is
   still a *usable* fixture, but a matcher built against the pre-export flow may
   fail for reasons the loss report never mentions. That is acceptable only
   because the loss report is advisory and the notice says the export is not
   round-trip lossless — but it is the first thing to check if a user reports
   "HAR import works but matching fails".
