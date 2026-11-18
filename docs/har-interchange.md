# HAR interchange and fixture migration (M014A)

HAR is an explicit lossy interchange, never canonical. EggReplay flows remain
the authority; HAR import projects external observations into flows with a
structured loss report, and HAR export projects flows into HAR 1.2 with an
explicit `_eggreplay` provenance/loss section. Both directions are lossy by
design and never imply round-trip losslessness.

Transport ownership is unchanged: HAR conversion performs no network I/O and
adds no HTTP stack. Recording still owns EggFetch outbound, EggServe inbound,
and Eggress optional routing.

> **Sensitivity.** An exported HAR contains recorded request and response
> bodies and headers. Treat it as sensitive even when redaction markers such as
> `<redacted>` are present, because opaque bodies, unselected fields, HAR
> comments, and provenance are not covered by the header/query/body policy. On
> Unix `har export` and every `--loss-report` are created with mode `0600`;
> on non-Unix, secrecy depends on the destination directory ACLs. `*.har` is
> git-ignored so an export is never committed by accident.

## Import loss matrix

`eggreplay har import --har input.har --fixture out.eggr` maps each HAR 1.2
entry into one flow. The `headers` array is authoritative; the `cookies`
array is a derived view and never merged. The `queryString` array is
authoritative for ordering; an empty array with a non-empty URL query adopts
the URL pairs with an explicit annotation.

| HAR field | EggReplay mapping | Loss action |
|---|---|---|
| `log.version` (must be `1.2`) | validated, else rejected | reject non-1.2 |
| `request.method`, `request.url` (scheme/authority/path) | `request.method/scheme/authority/path` | preserve; userinfo stripped + `Redacted` |
| `request.queryString` (ordered) | `request.query` ordered pairs | preserve; URL divergence `Annotated` |
| `request.headers` (ordered, duplicates) | `request.headers` verbatim | preserve duplicates, never collapse |
| `request.cookies` | consistency check only | `Annotated` (headers authoritative) |
| `request.postData.text` (+`encoding`) | request body blob (`Empty` when absent) | preserve; base64 decoded; multipart/file rejected |
| `request.postData.params` without `text` | re-encoded `application/x-www-form-urlencoded` | `Annotated` |
| `request.httpVersion` | none (version-agnostic model) | `Annotated` |
| `response.status` (100–599) | `FlowOutcome::Response(status)` | preserve |
| `response.status: 0` | `FlowOutcome::Error(Other/Other)` | `Annotated` |
| `response.headers` / `response.cookies` | same rule as request | preserve headers; cookies `Annotated` |
| `response.content.text` (+`encoding`) | response body blob | preserve; base64 decoded; missing-with-size `Annotated` |
| `response.content.compression/size` | decoded bytes authoritative | `Annotated` |
| `response.redirectURL` | `Location` header authoritative | `Annotated` |
| `startedDateTime` + `time` | `started_at_ms` / `completed_at_ms` | preserve total; breakdown collapsed (`Annotated`) |
| `timings.*` breakdown | total duration only | `Annotated`; `-1` treated as not-applicable |
| `serverIPAddress`, `connection` | none | `Omitted` |
| `cache`, `pageref` | none | `Omitted` |
| `browser`, `pages` | none | `Omitted` (global) |
| `creator` | `har.creator` annotation + `interop-provenance` | `Annotated` |
| `comment` (request/response) | `har.*.comment` annotation (truncated 512) | `Annotated` |

Unsupported entries fail the whole import (no partial fixture):
non-HTTP(S) URLs, missing hosts, empty methods, header/query over limits,
unsupported body encodings, multipart/file uploads, out-of-range statuses,
unparseable dates, or redaction fail-closed (structured selectors with a
non-JSON media type, or bodies exceeding the structured redaction bound).

Imported secrets pass through the selected redaction policy before any blob
is published: header/query via `redact_flow`, JSON/form bodies via bounded
transforms with representation reconciliation (`Content-Length` recomputed,
invalidated validators removed). Non-empty bodies with structured selectors
but unsupported media fail closed.

Every import writes an optional `interop-provenance` extension
(`interop-provenance.json`, schema 1, `required_for_replay=false`) with the
tool version, HAR creator, entry/flow counts, and the bounded loss list.
Older readers ignore it without changing replay semantics.

HAR comments (`har.request.comment`, `har.response.comment`) and
`interop-provenance.json` are **user-supplied metadata, not covered by
header/query/body redaction**. A configured `--redact-header` or
`--redact-query` selector leaves them byte-for-byte intact, and EggReplay does
not guess which comments are sensitive. Review or remove them before sharing a
fixture; automatic comment redaction is a deliberate non-goal.

## Export loss matrix

`eggreplay har export --fixture in.eggr --har out.har` projects each flow
into one HAR entry with `log.creator = eggreplay/<version>`,
`log.comment = "Lossy HAR export … not round-trip lossless"`, and
`log._eggreplay = {tool_version, session_schema, session_id, export_note, losses}`.
Each entry carries `_eggreplay = {flow_id, redactions, annotations, provenance}`.

| EggReplay field | HAR mapping | Loss action |
|---|---|---|
| method, scheme/authority/path/query | `request.method/url/queryString` | preserve (query re-encoded) |
| request/response headers (duplicates) | HAR `headers` arrays | preserve |
| Cookie/Set-Cookie headers | derived `cookies` arrays | `Annotated` (derived view) |
| request/response bodies (UTF-8) | `postData.text` / `content.text` | preserve |
| binary bodies | base64 `text` + `encoding: base64` | `Annotated` |
| `started_at_ms`/`completed_at_ms` | `startedDateTime` + `time` + synthetic `timings {send:0, wait:duration, receive:0}` | `Annotated` (synthetic) |
| HTTP version | assumed `http/1.1` | `Annotated` (model is version-agnostic) |
| request/response trailers | none | `Omitted` per flow |
| `FlowOutcome::Error` (typed) | `status: 0` + `statusText: Category/Phase` + comment | `Annotated` |
| redaction markers | values stay `<redacted>`; markers in `_eggreplay` only | `Annotated` |
| physical routes | none | `Omitted` |
| WebSocket 101 + conversations | handshake only, no messages | `Omitted` (+ global) |
| `rules` extension | none | `Omitted` (global) |
| `stream-events` extension | collapsed to total duration | `Omitted` (global) |
| `websocket-messages` extension | handshake only | `Omitted` (global) |
| `interop-provenance` extension | summarized in `log._eggreplay` | `Annotated` (global) |

A `--loss-report <path>` sidecar (`{command, fixture, har, entries, losses}`)
is available for both import and export. The HAR document itself always
embeds the full loss list; the sidecar is a convenience copy. Both outputs are
created `0600` on Unix.

## CLI contracts

All commands accept `--output human|json|junit` (JUnit projects a single
assertion). JSON is the versioned envelope
(`command`, `schema_version: 1`, `success`, `failure_class`, `warnings`,
`payload`); stdout carries JSON/JUnit, stderr carries
`{failure_class}: {message}`.

Stable exit codes (same contract as `docs/cli.md`):

- `0` success;
- `2` invalid CLI/config/policy (missing HAR, bad version, conflicting
  migrate flags, existing destination without `--overwrite`, or an existing
  destination that is not a fixture or empty directory);
- `3` invalid/corrupt fixture (unreadable source, future extension schema,
  migration blocked);
- `4` runtime (unwritable HAR/loss output, transactional replace failure);
- `5` internal.

Commands:

```sh
eggreplay har import --har input.har --fixture out.eggr [--overwrite] [--loss-report losses.json] [--redact-header ...] [--redact-query ...] [--redact-json-path ...] [--redaction-profile ...]
eggreplay har export --fixture in.eggr --har out.har [--overwrite] [--loss-report losses.json]
eggreplay migrate --fixture old.eggr --to new.eggr [--overwrite] [--target-schema 2] [--scrub-stream-trailers]
eggreplay migrate --fixture old.eggr --in-place [--scrub-stream-trailers]
```

`--to` (not `--output`) names the migration destination because `--output`
already selects the envelope format on every command. `--to` and `--in-place`
are mutually exclusive. For `har import`, `migrate --to`, and `proxy record`,
`--overwrite` replaces an existing fixture or empty directory only; a
malformed HAR, an invalid policy, or any other existing path is refused before
the destination is touched, and the old fixture is preserved byte-for-byte.

## Migration

`migrate` upgrades schema-1 fixtures to the current session schema (2) and
validates current-to-current idempotence. Flow records remain schema 1.
The source is never mutated before the new fixture validates:

- `--to`: validates the destination, migrates into a sibling staging
  directory, then publishes to the destination (transactional replace of an
  existing fixture or empty directory with `--overwrite`; any other existing
  path is refused and left untouched).
- `--in-place`: stages to a sibling, validates the staged copy, then
  atomically replaces the source with rollback on publish failure.

The registered migrator table is `rules v1`, `stream-events v1`,
`websocket-messages v1`, `interop-provenance v1` (identity only). Unknown
required extensions, or known extensions at non-current schemas, block
migration with an explicit `fixture` error naming the extension. There is no
ignore-required-extension switch; `Session::open` already fails closed on
the same conditions, and migration re-checks before copying. `copy_to`
streams and revalidates every blob/flow/extension; downgrades with extensions
fail closed.

`--scrub-stream-trailers` is an explicit, opt-in remediation for fixtures
recorded before stream-event trailer values were suppressed. Inside the same
transactional copy it parses `stream-events`, rewrites every trailer value to
`<redacted>`, and leaves names, order, duplicates, delays, DATA/END events,
and every other extension untouched. A malformed `stream-events` payload fails
the migration closed. The transform is idempotent, and omitting the flag
preserves the existing extension bytes verbatim. `validate` warns (without any
value) when a fixture still carries non-`<redacted>` trailer values.

## Golden corpus

Checked-in fixtures (under `crates/eggreplay-store/tests/fixtures/`):

- `schema-1-empty`, `schema-1-with-flows` (ordered queries, duplicate headers);
- `schema-2-rules`, `schema-2-stream`, `schema-2-websocket`, `schema-2-interop`
  (HAR-import provenance);
- `schema-1-with-flows-migrated-v2` (checked-in 1→2 upgrade result).

HAR corpus (under `crates/eggreplay-har/tests/corpus/`):

- `minimal.har` (GET + JSON POST);
- `duplicates.har` (duplicate headers/query, cookie divergence);
- `binary-and-error.har` (base64 bodies, `h2` collapse, userinfo strip,
  status-0 error, `-1` timings).

Current-to-current migration is validated as idempotent (manifest, extensions,
and flows identical). The `schema-1-with-flows` → `schema-1-with-flows-migrated-v2`
upgrade is pinned by the checked-in result.
