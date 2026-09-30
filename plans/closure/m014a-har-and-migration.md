# M014A — HAR Interchange and Fixture Migration Closure

Status: closed

## Qualifying revision and hosted evidence

Implementation commits `ee40365` (HAR crate, CLI, corpus, docs),
`6a69ebb` (hosted rustfmt), and `ec082b0` (tracked empty `blobs` dir for
the websocket golden) qualify on revision `ec082b0` with Actions run
[36736273433](https://github.com/eggstack/eggreplay/actions/runs/36736273433),
which passed all thirteen jobs in the standard matrix:
`verify` (Ubuntu stable, Ubuntu Rust 1.89, macOS stable, Windows stable),
`interception` (Ubuntu, macOS, Windows), `dependency-boundary`, four
`python-bindings` lanes, and `python-abi3-cross-version`. Two earlier runs
failed for non-product reasons and are recorded here for auditability:
[36734044069](https://github.com/eggstack/eggreplay/actions/runs/36734044069)
failed on hosted-stable `rustfmt` layout drift (fixed in `6a69ebb`), and
[36735131915](https://github.com/eggstack/eggreplay/actions/runs/36735131915)
failed on the untracked empty `schema-2-websocket/blobs` directory (fixed in
`ec082b0` with `blobs/.gitkeep`, which the store already ignores like
existing goldens).

Local verification on the qualifying revision is green:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Workspace suite: 334 tests passed, 0 failed (316 pre-M014A + 18 new: 7
`eggreplay-har` lib tests including the golden-corpus round-trip, and 11
`har_migrate` CLI subprocess contracts). The extended store golden test
covers all seven checked-in fixtures.

## Implementation surface

New leaf crate `eggreplay-har` (no network I/O, no HTTP stack; depends only
on `eggreplay-core`, `eggreplay-store`, `serde/serde_json`, `base64`, `url`,
`chrono`, `thiserror`):

- `import_har_to_writer` — HAR 1.2 → flows with a structured `ImportReport`
  (`flows`, `entries`, `losses`). `headers` arrays are authoritative with
  duplicates preserved; `cookies` arrays are consistency-checked views, never
  merged. `queryString` arrays are authoritative for ordering with URL
  cross-checks. Bodies decode `text`/`base64`, multipart/file uploads are
  rejected, status 0 maps to a typed `FlowOutcome::Error`, and timings
  collapse to `started_at_ms`/`completed_at_ms` with breakdown losses.
  Redaction (`RedactionConfig` + profile id) applies before any blob
  publication with gateway-identical JSON/form transforms, header
  reconciliation, and fail-closed structured/media mismatches. Every import
  writes the optional `interop-provenance` extension (schema 1,
  `required_for_replay=false`).
- `export_session_to_har` — session → HAR 1.2 with `log.creator =
  eggreplay/<version>`, `log.comment` carrying the lossy notice, per-entry
  `_eggreplay` (`flow_id`, `redactions`, `annotations`, `provenance`), and
  `log._eggreplay` (`tool_version`, `session_schema`, `session_id`,
  `export_note`, `losses`). Binary bodies become base64, typed errors become
  status 0, and every plan-mandated loss class is reported (trailers, typed
  errors, `rules`, `stream-events`, `websocket-messages`, redaction markers,
  physical routes, version collapse, synthetic timings).
- `migrate_session` + `migration_blockers` + `registered_migrators` —
  schema-1 → current upgrade and current → current idempotence via transactional
  `copy_to` (source never mutated). Known extensions (`rules`,
  `stream-events`, `websocket-messages`, `interop-provenance`, all v1) migrate
  by identity; unknown required or non-current schemas block with explicit
  `fixture` errors. No ignore-required switch.

`eggreplay-cli` adds `har import` (with full `--redact-*` policy,
`--overwrite`, `--loss-report`), `har export` (`--overwrite`,
`--loss-report`), and `migrate --to <fixture>` / `--in-place`
(`--overwrite`, `--target-schema`). `--to` names the destination because
`--output` already selects `human|json|junit`. Exit codes follow `docs/cli.md`
(0/2/3/4/5); JSON envelopes carry `flow_count`, `entries`, and `loss_count`.

## Loss matrices

Full tables are in `docs/har-interchange.md`. Import preserves method,
URL/query ordering, headers (duplicates), status, bodies, and total timing;
it annotates version collapse, timing breakdowns, cookie views, compression,
redirect duplication, creator/comments, and physical/cache/page omission, and
redacts userinfo/secrets. Export preserves method/URL/headers/bodies/totals;
it reports trailers, typed errors (status 0), scenario/stream/WebSocket
omissions, redaction provenance, routes, version assumption, and synthetic
timings. Neither direction claims round-trip losslessness.

## Migration fixtures and CLI contracts

Checked-in goldens (`crates/eggreplay-store/tests/fixtures/`):
`schema-1-empty`, `schema-1-with-flows` (ordered queries, duplicate headers),
`schema-1-with-flows-migrated-v2` (checked-in 1→2 result),
`schema-2-rules`, `schema-2-stream`, `schema-2-websocket` (with
`blobs/.gitkeep`), `schema-2-interop` (HAR provenance). HAR corpus
(`crates/eggreplay-har/tests/corpus/`): `minimal.har`, `duplicates.har`,
`binary-and-error.har` (base64, `h2` collapse, userinfo strip, status 0,
`-1` timings).

CLI contracts are pinned by `crates/eggreplay-cli/tests/har_migrate.rs`
(11 subprocess tests): import/export envelopes and loss sidecars, duplicate
preservation, secret redaction with tree scan, invalid-HAR `configuration`
failure with no partial fixture, export provenance and status-0 projection,
1→2 upgrade with source preservation, current→current idempotence (manifest,
extensions, flows identical), in-place atomic replace with flag-conflict
rejection, overwrite refusal preserving the destination, and future-extension
fail-closed without source/destination mutation.

## Support matrix and handoff

M014A adds no transport support claim. The baseline remains HTTP/1.1
record/replay/regression plus M009–M013 closures; H2/H3/WSS/gRPC remain
evidence-gated. HAR is explicit lossy interchange, never canonical.

M014A is closed. M014B (HTTP/2 qualification) is ready and independent;
M014C and M014D remain blocked on M014B per their declared dependencies.
The M014 umbrella therefore remains open.
