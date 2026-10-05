# Fixtures, Store, and Redaction

Use when working on `.eggr` persistence, the session schema, extensions,
redaction, or HAR/migration.

## On-disk layout

An `.eggr` is a **directory**, not a file:

```
demo.eggr/
├── manifest.json            # written last, then an atomic directory rename
├── flows.jsonl              # one JSON flow record per line
├── blobs/<sha256>           # content-addressed, no file extension
├── stream-events.json       # extension
├── websockets.jsonl         # extension
├── rules.json               # extension
└── interop-provenance.json  # extension
```

There is **no `extensions/` subdirectory**. `validate_extension_path` rejects
any path with more than one component, so every extension is a confined
root-level single filename — a nested layout is structurally impossible. If you
write a doc that shows an `extensions/` directory, it is wrong.

## Schema versions

| Constant | Value |
|---|---|
| `FLOW_SCHEMA_VERSION` | 1 |
| `SESSION_SCHEMA_VERSION` | 2 (reader accepts 1..=2) |
| `REPORT_SCHEMA_VERSION` | 2 |
| extension schemas | all currently 1 |

Flow records stay at schema 1 forever; only the session schema moves. There is
no schema-2→3 migration. Unknown *required* extensions and future session
schemas are **rejected** — there is deliberately no
ignore-required-extension switch.

## `required_for_replay` is not uniform

| Extension | File | Required | Enforced how |
|---|---|---|---|
| `rules` | `rules.json` | `true` | Caller-supplied; schema + payload validated |
| `stream-events` | `stream-events.json` | `true` | Hardcoded literal at both write sites |
| `websocket-messages` | `websockets.jsonl` | `true` | Forced; `false` is a hard error, and the path must be exactly `websockets.jsonl` |
| `interop-provenance` | `interop-provenance.json` | **`false`** | Hardcoded literal in `eggreplay-har` |

`required_for_replay` means *the reader must understand and apply this
extension* — not that the user opted into timing. Three places enforce it:
store validation rejects an unknown required extension, replay fails closed
when the build cannot honour a required extension, and migration blocks on a
required extension with no registered migrator.

## Bounds

Three extension limits, all enforced at write **and** at open:

| Bound | Value |
|---|---|
| `MAX_EXTENSIONS` | 64 |
| `MAX_EXTENSION_BYTES` | 16 MiB each |
| `MAX_TOTAL_EXTENSION_BYTES` | 32 MiB aggregate |

Fixture-level validation envelope (`StoreLimits::default()`): 4 MiB max JSONL
line, 64 MiB max blob, 100,000 flows, 512 MiB total.

## Publication ordering

`manifest.json` is the **final publication marker**. In order: flush and fsync
flows → write extension payloads → build the manifest → create `manifest.json`
with `create_new(true)` and fsync → close the flow handle (so the rename works
on Windows) → atomically rename the staging directory into place → re-validate
the published directory by opening it.

Blobs are streamed into staging long before any of this. A crash therefore
leaves a `.incomplete-*` staging directory, never a published fixture that
looks complete.

## Redaction ordering (C003)

**Redaction happens before any finalized blob exists.** This is the invariant,
not an implementation detail.

- The effective policy is an explicit recording input, persisted by identifier.
- Header/query redaction applies before the flow append.
- Structured JSON/form bodies transform before publication; malformed,
  oversized, or unsupported requested transformations **fail closed**.
- **Redacted request fields become matcher wildcards**, not literal
  placeholders. A recorded `<redacted>` must still match whatever the live
  request sends in that position.
- Bounded structured redaction buffers at 1 MiB by default.
- Non-goals: opaque binary rewriting, entropy scanners, DLP, encryption at
  rest, arbitrary JSONPath.

## Lazy replay (C001)

`ReplayFixture::load` is **metadata-bounded**: it keeps flow records and
`CandidateBody` descriptors (`absent`/`empty`/digest) and reads no blob bytes.
`Session::open_blob` returns an opaque validated `BlobHandle` (digest form,
byte bound, symlink rejection, exact length) with an already-opened file; the
HTTP adapter streams selected responses in 64 KiB chunks with incremental
SHA-256 verification.

Fixtures are **immutable while a session or handle is open**. Concurrent blob
replacement fails as an integrity error rather than serving stale or partial
bytes — that is the TOCTOU protection, and it is load-bearing.

Empty/absent bodies use `ResponseBody::Empty` with no allocation.

## Concurrent recording (C002)

`RecordingSession` is the cloneable concurrent owner. `begin_blob` never holds
the flow-log mutex while body bytes stream to independent staging files;
`append_flow` serializes only the final bounded JSONL write plus accounting. No
Tokio mutex spans an awaited upstream transaction, no unbounded channel buffers
whole flows, and an aborted body writer cleans its staging file via `Drop`.
Shutdown stops admission, drains in-flight tasks, then fails closed if a sink
is still active.

## Migration

`migrate` stages into a sibling directory, validates the staged copy, then
publishes transactionally (`--to` replaces only with `--overwrite`;
`--in-place` atomically replaces the source with rollback). It never mutates the
source before the new fixture validates.

Registered migrators: `rules v1`, `stream-events v1`, `websocket-messages v1`,
`interop-provenance v1` (identity only). Any other required extension, or any
known extension at a non-current schema, **blocks** migration. `copy_to`
streams and revalidates every blob/flow/extension.

## Architecture References

- [`architecture/03-store-persistence.md`](../architecture/03-store-persistence.md)
  — on-disk layout, `StoreLimits`, read/write paths, extension validation,
  integrity model.
- [`architecture/02-core-semantic-model.md`](../architecture/02-core-semantic-model.md)
  — redaction policy and matcher wildcard semantics.
- [`architecture/11-har-and-migration.md`](../architecture/11-har-and-migration.md)
  — HAR loss model, provenance extension, migrator table, golden corpus.
- [`docs/eggr-schema.md`](../docs/eggr-schema.md) — the format reference.
- [`docs/har-interchange.md`](../docs/har-interchange.md) — loss matrices.
