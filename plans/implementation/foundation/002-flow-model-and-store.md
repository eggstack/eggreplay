# M002 — Canonical Flow Model and .eggr Store

Status: ready
Depends on: M001
Release gate: foundation

## Objective

Implement schema-1 semantic flow/session types and durable streaming fixture storage without any live network dependency.

## Work packages

### A. Canonical types

Implement stable IDs and the HTTP model described by ADR 0001. Preserve ordered multi-value headers and query keys. Model response and error as mutually exclusive outcomes.

Include protocol version, logical origin, optional physical-route metadata, timestamps, provenance, annotations, and redaction markers without coupling persisted types to EggFetch/EggServe.

### B. Body references

Represent absent, empty, and blob-backed bodies distinctly. Blob metadata includes SHA-256 and byte length. Optional body-event metadata is schema-capable but may remain absent in v0.1 recordings.

### C. Store writer

Stream bytes to same-filesystem temporary files while hashing. Publish finalized blobs atomically. Append finalized flow records to a temporary JSONL log. Finalize validated `flows.jsonl` plus `manifest.json`.

A crash before final manifest publication must leave an identifiable incomplete session, never a fixture that validates as complete.

### D. Reader/validator

Bound line sizes, field counts, JSON depth/work, blob lengths, and total resource use. Verify hash/length integrity. Reject unsupported schema versions with typed errors.

### E. Migration seam

Define migration API shape and prove schema-1 round trips. Do not invent schema 2.

## Tests

Golden round trip; repeated headers/query values; absent vs empty bodies; binary and UTF-8 bodies; multi-megabyte streamed blob; duplicate blob dedup; corrupt hash/length; malformed/oversized JSONL; missing manifest; path traversal attempts; deterministic iteration; property tests for serialization invariants.

## Acceptance

A fixture can be created from in-memory semantic flows, validated, reopened, iterated without loading every body, and reproduced byte-for-byte at the body layer. Core/store still require no network runtime.
