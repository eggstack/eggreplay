# C003 closure — Persistence-Time Redaction and Configurable Policy

Status: closed

## Implementation

- Core `security`: unified `RedactionConfig::from_profile`, `wants_body_redaction`, `DEFAULT_MAX_STRUCTURED_REDACTION_BYTES` (1 MiB), body JSON/form markers (`request.body.json:`, `response.body.json:`, `request.body.form:`), `apply_json_redaction`/`apply_form_redaction` (fail-closed malformed), `reconcile_headers_after_body_redaction` (Content-Length recomputed, MD5/Digest/Signature + strong ETags removed with markers, weak ETags preserved with warning), `push_body_markers`, request trailer redaction. Exported via `lib.rs`.
- Core `matching`: redaction-aware wildcards. Per-candidate `request.headers.*`/`request.query.*` ignored; `request.body.json:*`/`form:*` trigger semantic remainder comparison (even in ExactBytes) loading only narrowed candidates via loader. Opaque redacted bodies never match literally.
- Core `flow`: no schema change; markers carry field paths + profile only.
- Store: `BodyWriter::len` + `read_staging_bounded` and `RecordingBodyWriter::len` + `read_staging_bounded` for pre-publication transforms; raw staging never becomes finalized blob (Drop cleans on fail-closed/abort).
- HTTP `recording`: explicit `RedactionConfig`/`profile_id`/`max_structured_bytes` inputs for `record_request`, `record_request_with_session`, and `start_recording_gateway` (8 args, clippy-allowed). Staging-readback transform before publication for JSON/form with bounded buffering and fail-closed unsupported/oversized/malformed. Header/query via `redact_flow` with explicit policy. Userinfo stripped from persisted authority. Representation reconciliation attached with markers/annotations.
- CLI `record`: `--redact-header/--redact-query/--redact-json-path` (repeatable), `--redaction-profile` (persisted), `--unsafe-replace-default-redaction` (replace vs extend defaults). `inspect` exposes `redaction_profile` + `redactions` markers without values.
- Docs: `architecture.md` redaction section, `configuration.md` policy inputs.

## Tests

- Core: `redacted_fields_are_wildcards_not_literals` (header/query/JSON wildcards, non-redacted mismatch), `framing_metadata_reconciled_after_redaction`, `json_and_form_redaction_replaces_sentinels`.
- HTTP recording (4 new): `default_sensitive_headers_never_persisted`, `configured_query_and_json_redacted_before_publication` (query + request/response JSON sentinels absent from all fixture files + flow JSON + markers, Content-Length consistent), `malformed_and_oversized_structured_body_fails_closed` (malformed + oversized with sentinel, Err without secrets in Display, 0 flows/blobs, dir scan clean), `userinfo_never_reaches_persisted_authority`.
- Sentinel scans walk entire fixture directories; logs/errors asserted secret-free.

## Verification

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

All passed. Totals: 8 core + 18 http (7 replay + 11 recording) + 8 store = 34 tests.

## Acceptance

- Configured sentinels absent from all finalized fixture files, flow JSON, markers, and error/log captures.
- Redacted fields wildcard in matching, not literal placeholders; non-redacted still mismatches.
- Framing consistent (Content-Length recomputed, integrity headers classified).
- Policy documented, machine-inspectable without values; secure defaults preserved unless unsafe-replace.
- Closure record present.

## Unblocked next plan

C004 remains ready. C005 remains blocked until C001–C004 close. No M009–M014 scope pulled in.
