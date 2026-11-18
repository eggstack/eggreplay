# M021A — Redaction Completeness and Fixture Data-at-Rest

Status: **implemented** (local gate green; hosted CI and closure record pending)
Depends on: none (independent of M020; no `Cargo.lock` changes planned)
Closure record: `plans/closure/m021a-redaction-completeness.md` (not created;
requires hosted qualification of the committed revision)

## Why this plan exists

The audit found a confidentiality defect in the persisted fixture, plus three
smaller ways that configured redaction can be weaker than an operator expects:

1. **High — stream-event trailers bypass redaction.** Trailer values are copied
   into `stream-events.json` before/independently of `redact_flow`, so a value
   that is `<redacted>` in `flows.jsonl` can remain verbatim in the extension.
2. **Low–Medium — WebSocket JSON-pointer redaction fails open.** A configured
   JSON pointer is applied only when the text payload happens to parse as JSON;
   otherwise the original payload is recorded with no marker.
3. **Medium — defaults and warnings are thin.** `default_secure` covers four
   headers and no query keys. Common API-key/auth-token headers are recorded
   verbatim unless selected, WebSocket payloads are raw unless opted in, and
   the recorder does not warn.
4. **Low — HAR export widens exposure.** Fixtures are `0600` files in `0700`
   directories on Unix; `har export` and `--loss-report` use `std::fs::write`
   and therefore typically create `0644` files containing recorded bodies.

This subplan is a confidentiality boundary. It does not change the matcher,
scenario, replay, or transport authorities, and it does not expand the
redaction feature set (no entropy scanning, DLP, or encryption at rest).

## Audit evidence

- `crates/eggreplay-http/src/recording.rs:259` — response trailers pushed into
  `response_events` with `response_trailers.clone()`.
- `crates/eggreplay-http/src/recording.rs:2266` — request trailers pushed into
  request events with the same raw fields.
- `crates/eggreplay-http/src/recording.rs:369` and `:619` — `redact_flow`
  redacts the flow's headers and trailers only.
- `crates/eggreplay-http/src/recording.rs:388` and `:644` — the unredacted
  event lists are appended to the session.
- `crates/eggreplay-store/src/lib.rs:571` — `stream-events.json` is written as
  an extension with no redaction pass.
- `crates/eggreplay-http/src/recording.rs:1671` — JSON-pointer redaction is
  applied only with `if let Ok(...)`; failure leaves the original bytes.
- `crates/eggreplay-core/src/security.rs:20-34` — the default profile.
- `crates/eggreplay-http/src/recording.rs:668-674` — WebSocket redaction
  defaults to `false`.
- `crates/eggreplay-cli/src/main.rs:2534` and `:2547` — HAR/loss output writes.

Observed on the audit tree: a response trailer `X-Secret` configured with
`--redact-header x-secret` appeared as `<redacted>` in `flows.jsonl` and as
`AUDIT-TRAILER-SENTINEL-7f3c` in `stream-events.json`. A chunked request with
an `Authorization` trailer reproduced the same split.

## Track A — Stream-event trailer values are never persisted

### Required behavior

Stream-event `Trailers` entries preserve **field names, order, and duplicates**
and carry the fixed value `<redacted>` for every field. This applies to both
request and response directions, on every recording path (gateway, explicit
proxy, HTTPS MITM, and `serve --record-mode append-new`).

Why every value, not only configured names:

- `eggreplay-core/src/report.rs:500-510` compares stream-event trailers by
  count and case-insensitive name only; values are not a comparison dimension.
- `eggreplay-http/src/replay.rs:872` consumes a trailer event only to set its
  delay; trailer values come from the flow's redacted `trailers`.
- A policy-specific redaction would still persist values for unconfigured
  names, preserving the same class of leak. Stream-event values are
  non-authoritative duplication, so the safe representation is to remove them.
- The change does not require a schema-version bump: `HeaderEntry` already
  permits `<redacted>` as a value, and names/order remain unchanged.

Add a stable flow annotation when any stream-event trailer value is suppressed,
for example `("redaction", "stream-trailers-values-suppressed")`, so an
inspector can distinguish "no trailers" from "trailers with values withheld".
The annotation carries no values.

### Existing fixtures

The fix is forward-only for new recordings. Existing fixtures may already
contain leaked values, and the tool cannot infer which values were sensitive.
Provide an explicit remediation path:

- `validate` reports a warning when `stream-events.json` contains a trailer
  value other than `<redacted>`. The warning names the flow and field count,
  never the value.
- `migrate` gains `--scrub-stream-trailers` (valid with `--to` and
  `--in-place`). It rewrites trailer values to `<redacted>` idempotently and
  leaves names, order, delays, DATA events, and every other extension
  untouched. A fixture already in the target schema is still scrubbed when the
  flag is present; the flag is not a schema conversion.
- The flag is opt-in. `migrate` without it preserves existing bytes, so a
  user's data is never silently rewritten.
- `docs/eggr-schema.md` states that stream-event trailer values are
  non-authoritative and are redacted; `docs/cli.md` documents the remediation
  flag.

### Tests

- **Whole-fixture sentinel test.** Record a response with a sensitive trailer
  and a chunked request with a sensitive trailer. Walk every file under the
  finalized `.eggr` (manifest, flows, blobs, every extension, staging siblings)
  and assert the sentinel appears nowhere outside a test-only input buffer.
  The test must fail on the pre-fix tree.
- Assert names, order, and duplicate trailers survive in `stream-events.json`.
- Assert the flow annotation is present when values were suppressed.
- Migration test: construct a fixture with raw trailer values; run
  `migrate --scrub-stream-trailers --in-place`; assert values become
  `<redacted>`, names/order/delays unchanged, and a second run is a no-op.
- `validate` warning test: the warning names the flow/count and contains no
  trailer value.
- Cover all recording entry points that share the path: direct gateway, proxy
  plain, MITM, and append-new. A shared helper test may cover the first three;
  at least one end-to-end test must exercise MITM because that is the
  privileged path.

## Track B — WebSocket redaction is fail-safe, not fail-open

### Required behavior

When JSON-pointer redaction is configured for WebSocket text and the payload is
not valid JSON:

- replace the whole message payload with a fixed safe marker (`[REDACTED]`),
  rather than recording the original bytes;
- attach `WebSocketRedaction::WholeMessage` (or a new marker only if the existing
  enum cannot express it without a schema change);
- never emit the original payload bytes or a body blob for that message.

This preserves recording availability while matching the HTTP body policy,
which fails closed when structured redaction cannot be applied. A malformed
JSON text message is common enough that aborting the conversation would be
worse than withholding the payload.

Close reasons are text and are redacted whenever **any** WebSocket redaction is
configured: `redact_text`, `redact_binary`, or a non-empty `json_paths` set.
Close codes remain authoritative and are not redacted.

### Tests

- Text `not-json SENTINEL` with `json_paths` configured records `[REDACTED]`,
  marker `WholeMessage`, no sentinel in any blob/extension, and a replayable
  conversation.
- Valid JSON text still applies the pointer replacement and records
  `JsonPointers` with the original pointer list.
- Binary messages keep the existing `redact_binary` behavior; no regression.
- A close reason containing a sentinel is redacted when only
  `redact_binary` or only `json_paths` is configured.
- A replay/regression test confirms the redacted message is treated as a
  wildcard, not as the literal `[REDACTED]` payload.

## Track C — Expand header defaults; warn for query and WebSocket risk

The audit's Medium finding has two parts. The header-default gap is narrow
enough to fix directly: API-key and auth-token headers are credentials, and a
fixture that records them verbatim is a leak even when the operator forgot a
selector. Query-key defaults and WebSocket payload redaction are different:
their names and shapes vary, and default redaction can break replay of a
request that legitimately needs the value. Those parts get warnings plus a
documented residual-risk acceptance, not silent default expansion.

### Required behavior

Header defaults:

- Extend `RedactionConfig::default_secure` with: `x-api-key`, `api-key`,
  `x-auth-token`, `x-access-token`, `x-amz-security-token`, and
  `x-goog-api-key`.
- Keep `authorization`, `proxy-authorization`, `cookie`, and `set-cookie`.
- Do **not** add `location` or `referer`: a response `Location` is functional
  replay data, and redacting it would break redirects.
- Do **not** add `x-csrf-token` to the defaults; warn instead, because a replay
  of a CSRF-protected request may require the recorded token. Operators can add
  it explicitly.
- The matcher treats the newly redacted headers as wildcards, and
  replay/regression will send `<redacted>` for a recorded request that depends
  on the literal value. This is a deliberate security-over-compatibility
  change: document it in `docs/configuration.md`, note that
  `--unsafe-replace-default-redaction` replaces the default set (including the
  expansion) with the operator's selectors, and update tests that relied on the
  old defaults to pass explicit selectors or the override.
- Do not change `query_keys` defaults.

Warnings:

- Add a value-free warning when a recorded request contains a header whose
  lowercased name matches a curated auth-like set or pattern, and that header
  is not in the effective redaction set. Cover the exact defaults above plus
  `x-csrf-token`, and suffix patterns `*-api-key`, `*-auth-token`,
  `*-access-token`.
- Add a warning when a request query contains a key matching a curated set:
  `token`, `access_token`, `api_key`, `apikey`, `password`, `secret`, `sig`,
  `signature`.
- Add a warning when WebSocket acquisition is enabled and neither
  `--redact-websocket-text`, `--redact-websocket-binary`, nor a configured JSON
  pointer path is present.
- Warnings are emitted once per command/session, to stderr in human mode and in
  the versioned envelope's `warnings` array in JSON mode. They name the header
  or key, never its value, and never the body.
- Warnings are advisory: they do not fail recording. The existing
  `--redact-header`, `--redact-query`, `--redact-json-path`, and
  `--unsafe-replace-default-redaction` behavior is otherwise unchanged.

### Tests

- A request with `X-Api-Key: SENTINEL` and no selector records `<redacted>`,
  contains no sentinel in any fixture file, and the matcher treats the field as
  a wildcard.
- `--unsafe-replace-default-redaction` with no selectors records the sentinel
  and emits a warning, proving the override is explicit but visible.
- A request with `X-Csrf-Token: SENTINEL` and no selector warns but records the
  value (documented residual); with `--redact-header x-csrf-token` it records
  `<redacted>` and does not warn for that header.
- A query `?access_token=SENTINEL` warns without the value.
- `--websockets` without redaction warns; with either redaction flag it does
  not warn for that reason.
- Warning plumbing does not alter exit codes or the existing `warnings` schema.
- Existing suites that used `x-api-key`/`api-key` as ordinary headers are
  updated to use the override or an explicit selector, and the compatibility
  change is called out in the release note.

### Documentation

`docs/configuration.md` and `docs/cli.md` must state:

- the default profile's exact header list and empty query set;
- the header-default expansion and its matching/replay compatibility impact;
- the warning behavior and that it is not a guarantee;
- the recommended selectors for query keys and `x-csrf-token`;
- that `location`/`referer` are deliberately not default-redacted because they
  can be functional replay data;
- that WebSocket payloads remain opt-in redaction and the warning is advisory.

## Track D — HAR export preserves fixture confidentiality

### Required behavior

- On Unix, `har export` creates the HAR and `--loss-report` with `0600` from
  the moment of creation (`OpenOptions::mode(0o600)`), not by chmod after write.
  The existing `--overwrite` refusal/acceptance semantics are unchanged.
- On non-Unix, keep the current creation behavior and document that secrecy
  depends on the destination directory ACLs, matching the existing Windows
  caveat for fixtures.
- Add `*.har` to `.gitignore`. HAR exports are not fixtures and have no
  directory-level protection.
- `docs/har-interchange.md` warns that an exported HAR contains recorded
  bodies and must be treated as sensitive even when redaction markers are
  present.

### Tests

- Unix-only permission test: export a fixture to a fresh path and assert mode
  `0600` for both the HAR and the loss report; assert `--overwrite` still
  replaces an existing file and `--overwrite` absence still refuses.
- A content test confirms redaction markers/values in the exported HAR are the
  same as in the fixture (no new bypass).
- `.gitignore` test is a repository check, not a runtime test.

## Track E — Annotation and provenance disposition

`flow.annotations` can carry `har.request.comment` / `har.response.comment`,
and `interop-provenance.json` preserves upstream HAR creator/comment metadata.
These fields are not covered by header/query/JSON-pointer body redaction. The
audit did not recommend guessing which comments are sensitive.

Required disposition:

- Document in `docs/har-interchange.md` and `docs/eggr-schema.md` that HAR
  comments and provenance are user-supplied metadata and are not redacted by
  the body/header policy.
- Add a test proving that a configured `--redact-header`/`--redact-query`
  selector does not alter these fields, so the documented boundary is pinned.
- Do not add automatic comment redaction in this corrective. If a future
  requirement needs it, it is a separate policy decision with its own
  compatibility analysis.
- The umbrella closure record lists this as an accepted residual with the
  documentation link.

## Acceptance

- Track A's whole-fixture sentinel test passes, and no recording path can write
  a raw trailer value into `stream-events.json`.
- `validate` detects pre-fix fixtures; `migrate --scrub-stream-trailers`
  remediates them idempotently without changing schema or unrelated bytes.
- Track B's non-JSON WebSocket sentinel test passes and no raw payload blob is
  created.
- Track C expands the curated default header set, proves that no sentinel
  persists for those headers, and keeps warnings value-free and documented.
  Query-key and WebSocket defaults remain unchanged and are recorded as
  accepted residuals with warnings.
- Track D permission tests pass on Unix; Windows behavior is documented.
- Track E boundary is documented and pinned by a test.
- Full locked workspace gate and the required hosted CI are green.
- `docs/cli.md`, `docs/configuration.md`, `docs/eggr-schema.md`, and
  `docs/har-interchange.md` are updated.
- A fresh audit of the final tree cannot reproduce the trailer leak, the
  WebSocket fail-open, or the world-readable HAR export.

## Non-goals

- Changing default query-key redaction or default WebSocket payload
  redaction. The curated header set expands; those two remain opt-in with
  warnings.
- Encrypting fixtures at rest, entropy scanning, DLP, or arbitrary JSONPath.
- Redacting opaque binary payloads; `docs/non-goals.md` remains accurate.
- A new redaction schema version. The stream-event trailer change reuses the
  existing string value field.

## Verification

Run the required gate from `AGENTS.md`, plus:

```sh
cargo test -p eggreplay-core
cargo test -p eggreplay-store
cargo test -p eggreplay-http
cargo test -p eggreplay-cli
```

Add the subplan's integration tests to the existing relevant suites rather
than a new test binary unless the fixture needs its own process.

## Implementation record (2026-11-18)

Implemented locally; not closed. The full locked workspace gate passed
(591 tests, 0 failures; core 55, store 32, HAR 9, HTTP 231, CLI 99,
interception 165), all with `--all-features` where the crate has features.

- **Track A.** `suppressed_stream_trailer_fields` in
  `crates/eggreplay-http/src/recording.rs` preserves names, order, and
  duplicates while writing `<redacted>`; it is used at all three fixture
  persistence sites. A flow whose trailer values were suppressed gains
  `("redaction","stream-trailers-values-suppressed")`. `validate` warns on raw
  pre-fix values by flow id and count only. `migrate --scrub-stream-trailers`
  runs inside `Session::copy_to_with_extensions`, so the scrub is transactional
  and idempotent. Evidence:
  `recording::tests::stream_event_trailers_never_persist_values_on_any_file`,
  `mitm_never_persists_stream_event_trailer_values`,
  `migrate_scrub_stream_trailers_is_transactional_and_idempotent`,
  `validate_warns_on_raw_stream_trailers_without_naming_the_value`. A manual
  CLI recording of a chunked response with `X-Secret` persisted the sentinel
  nowhere, recorded the flow trailer as `<redacted>`, and carried the
  annotation.
- **Track B.** Non-JSON text with configured JSON pointers becomes
  `[REDACTED]` with `WholeMessage`; close reasons are withheld when
  `redact_text`, `redact_binary`, or a pointer set is configured. Evidence:
  `websocket_non_json_text_with_json_pointers_is_withheld`,
  `websocket_close_reason_redacted_when_any_redaction_is_configured`, plus the
  replay/regression wildcard tests.
- **Track C.** `DEFAULT_SENSITIVE_HEADERS` is the single core source of truth
  (the original four plus `x-api-key`, `api-key`, `x-auth-token`,
  `x-access-token`, `x-amz-security-token`, `x-goog-api-key`). The CLI scans
  finalized flows for credential-like headers, curated query keys, and
  unredacted WebSocket acquisition; warnings are deduplicated, value-free, and
  do not change exit codes. `x-csrf-token`, `location`, and `referer` remain
  explicit-selector only, as documented.
- **Track D.** HAR and loss-report outputs use `OpenOptions::mode(0o600)` on
  Unix before any bytes are written; `*.har` is ignored; the HAR sensitivity
  warning is in `docs/har-interchange.md`.
- **Track E.** HAR comments and `interop-provenance.json` are documented as
  user-supplied metadata outside the redaction policy, and a test pins that a
  configured header/query selector does not alter them.

### Accepted residuals

- Query-key defaults remain empty and WebSocket payload redaction remains
  opt-in; both now warn and are documented.
- HAR comments and provenance metadata are not redacted.
- Non-Unix HAR secrecy depends on the destination directory ACLs.

### Remaining for closure

Commit and push; run the hosted lanes on the qualifying revision; create
`plans/closure/m021a-redaction-completeness.md`; re-run the trailer sentinel
reproduction on that revision.
