# M006 — Client Replay and Semantic Regression Diff

Status: closed
Depends on: M002, M003, M005
Release gate: v0.1

## Objective

Replay baseline requests against a candidate target, observe candidate outcomes through the same EggFetch path, and compare them deterministically.

## Work packages

### Request materialization

Reconstruct method, path/query, selected headers, body bytes, and trailers. Support target-base remapping while preserving baseline origin in report metadata.

If a required credential was redacted, configuration must supply a runtime replacement or classify the flow as non-replayable with an explicit reason.

### Replay scheduler

Start with sequential mode and a bounded recorded-start-order mode when capture metadata exists. Sequential is the default v0.1 CLI mode. Reports record scheduler choice.

### Candidate capture

Use M003's observation/error mapping so baseline/candidate are structurally comparable.

### Diff engine

Typed findings for status, selected headers/trailers, exact body, semantic JSON, error category/phase, missing/unexpected behavior, and explicit timing assertions. Finding order must be stable.

### Report authority

Define a versioned Rust report model. M007 renders it; presentation code must not duplicate evaluation logic.

## Tests

Target remap; 200->500; header add/remove/change; JSON ordering equivalence/change; binary hash change; success->network error; deterministic local timing threshold; redacted credential injection requirement; concurrent replay observation isolation.

## Acceptance

Given a baseline and local candidate, the engine returns a stable typed report without requiring an offline replay server.
