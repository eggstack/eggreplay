# M005 — Matching, Consumption, and Near-Miss Diagnostics

Status: ready
Depends on: M002, M004
Release gate: v0.1

## Objective

Create one deterministic matcher shared by offline replay and live regression selection.

## Work packages

### Normalization

Canonicalize scheme/authority/path/query and header names while preserving raw observations for diagnostics. Query and headers stay multi-value.

### Profiles

Implement inspectable `strict` and `practical` profiles from the subsystem roadmap. Hidden heuristic defaults are prohibited.

### Body matching

Implement no-body/presence, exact byte hash, exact UTF-8 text when requested, and semantic JSON equality with explicit ignored paths.

### Candidate indexing

Use cheap route-level keys before expensive body comparisons. Enforce candidate and diagnostic-work limits.

### Consumption

Implement ordered deterministic consumption plus `once`, `repeat-last`, and `unlimited`. State belongs to the replay session, not persisted fixture mutation.

### Near miss

Return a bounded candidate set with explicit mismatch costs/dimensions and redaction-safe differences. Ranking informs diagnostics only; it never silently converts a mismatch into a match.

## Tests

Ambiguous candidates; repeated calls; repeated query/header values; JSON key ordering and arrays; invalid JSON; ignored paths; practical volatile headers; body mismatch; redaction markers; bounded near-miss output.

## Acceptance

Offline replay uses only this matcher authority, and M006 can invoke the same engine without depending on EggServe.
