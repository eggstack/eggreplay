# ADR 0004 — Versioned .eggr Directory Format

Status: accepted for initial implementation

## Decision

The canonical fixture is a directory:

```
name.eggr/
  manifest.json
  flows.jsonl
  blobs/
    <sha256>
```

All non-empty authoritative body bytes are content-addressed blobs. Flow records reference SHA-256 + length and may carry semantic metadata. Schema versioning begins at 1.

## Required properties

- atomic blob publication;
- incomplete-session detection;
- deterministic validation;
- hash/length integrity;
- unsupported versions fail closed;
- fixture-controlled paths cannot escape the fixture root;
- explicit migration API before schema 2.

A packed single-file transport form is deferred.
