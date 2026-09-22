# C003 — Persistence-Time Redaction and Configurable Policy

Status: ready
Depends on: M001–M008 historical baseline
Corrective gate: v0.1 requalification

## Finding

The repository has a useful `RedactionProfile`/`RedactionConfig` vocabulary and `redact_json()`, but the recording path hardcodes `RedactionConfig::default_secure()` after request and response bodies have already been published as blobs.

Configured query/JSON selectors are therefore not wired through the recorder/CLI, and sensitive JSON/body values can be durably stored before any transformation. The current closure text overstates support for configured structured-body redaction.

## Objective

Make the selected redaction policy an explicit recording input and guarantee that any body field configured for redaction is transformed before its authoritative content-addressed blob is published.

## Required design

### Policy plumbing

Unify or explicitly bridge `RedactionProfile` and `RedactionConfig` so one named effective policy is:

- constructible from documented CLI/config inputs;
- passed into gateway/recording APIs rather than hardcoded;
- persisted by identifier in session metadata;
- inspectable without revealing secret values.

At minimum expose repeatable selectors for sensitive headers, query keys, and JSON Pointer body paths. Preserve secure defaults unless explicitly overridden by a clearly named unsafe/replace policy.

### Header/query redaction

Apply header/query transformations before the flow is appended. Keep typed `RedactionMarker` entries.

URL userinfo must never reach persisted logical authority or diagnostics.

### Body redaction before publication

When structured-body redaction is requested, raw sensitive bytes must not first become a finalized blob.

For v0.1 corrective work, bounded buffering of recognized JSON/form bodies is acceptable if it is explicitly limited and fail-closed. Define a `max_structured_redaction_bytes` or reuse a documented body limit. If a configured structured transform cannot be safely applied because the body is too large, malformed, or of an unsupported media type, fail the recording according to policy rather than silently storing unredacted bytes.

Do not claim arbitrary opaque-body secret discovery.

### Representation metadata

If redaction changes body bytes, reconcile headers whose values are invalidated by the transformation. At minimum prevent stale `Content-Length`; explicitly classify `Content-MD5`, digest/signature headers, strong ETags, and similar representation-integrity metadata as removed, recomputed, or preserved-with-warning. Do not replay knowingly inconsistent framing metadata.

### Matching semantics

A redacted request field must not require a future request to literally contain `"<redacted>"`.

For structured request bodies, persisted redaction markers/selectors should cause the matcher to ignore/wildcard those configured paths while comparing the remainder. Query/header redaction follows the same principle where the selected matcher dimension is otherwise active.

### Cookie behavior

The secure default may continue redacting complete Cookie/Set-Cookie values for v0.1. If selective cookie-name redaction is implemented, parse/re-serialize safely; do not perform substring replacement.

## Tests

- Authorization, Proxy-Authorization, Cookie, and Set-Cookie never appear in persisted flow JSON;
- configured query values are absent from fixture bytes and diagnostics;
- configured request and response JSON Pointer values are absent from every finalized blob;
- malformed/oversized structured body with requested redaction fails closed;
- matching succeeds when only a redacted JSON/query/header field differs;
- non-redacted fields still detect mismatch;
- transformed body framing metadata is consistent;
- logs/errors never print the secret test sentinel;
- `inspect` exposes policy/markers without values.

Use unique sentinel strings and scan the entire finalized fixture directory as an acceptance test.

## Non-goals

No entropy-based secret scanner, DLP engine, encryption-at-rest system, arbitrary JSONPath implementation, or opaque binary rewriting.

## Acceptance

A configured redaction sentinel does not occur anywhere in finalized fixture files, machine output, or expected error/log captures; redacted fields behave as intentional wildcards rather than literal placeholders; policy configuration is documented and machine-inspectable.

Closure record: `plans/closure/c003-persistence-redaction-policy.md`.
