# Configuration skeleton

The configuration model reserves explicit limits, redaction, matcher, and
output-format profiles. Later milestones add command-specific fields while
preserving the precedence order CLI > explicit file > documented defaults.

## Redaction policy (C003)

`record` accepts repeatable `--redact-header <NAME>`, `--redact-query <KEY>`,
and `--redact-json-path <POINTER>` selectors plus `--redaction-profile <ID>`
(persisted in session metadata) and `--unsafe-replace-default-redaction`
(replace secure defaults instead of extending them). `RedactionProfile`
(`sensitive_headers`, `sensitive_query_keys`, `sensitive_json_paths`) converts
to `RedactionConfig` for recording; `inspect` exposes the profile ID and typed
`RedactionMarker`s without secret values. Structured body redaction buffers
boundedly (1 MiB default) and fails closed on overflow, malformed JSON/form,
or unsupported media types. Opaque binary rewriting, entropy scanners, DLP,
encryption-at-rest, and arbitrary JSONPath are non-goals.
