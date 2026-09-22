# Security and Privacy Roadmap

Status: active roadmap
Owners: M002, M003, M008, M013

## Threat model

Fixtures can contain credentials, tokens, cookies, personal/proprietary payloads, and internal topology. Treat `.eggr` as sensitive even after redaction.

Threats include log leakage, persistence before filtering, path traversal, malicious fixture resource exhaustion, unsafe template evaluation, and future interception-key compromise.

## v0.1 requirements

- redact common credential headers before persistence/diagnostics;
- parse cookies/Set-Cookie sufficiently for configured value redaction;
- never persist URL userinfo raw;
- configurable query-key redaction;
- configurable JSON-path/form-field redaction for recognized structured bodies;
- typed redaction markers rather than literal "***" matcher values;
- apply configured body redaction before blob publication;
- bound metadata counts, sizes, JSON work, candidate diagnostics, and concurrency;
- fixture path confinement;
- credential-safe errors;
- restrictive local permissions where supported.

Do not claim automatic discovery of arbitrary secrets in opaque bodies.

## M009/M013

Templates must be deterministic and sandboxed by default. Interception gets a separate CA/private-key lifecycle and threat model; those keys never share fixture storage.
