# Regression Roadmap

Status: active roadmap
Owners: M006, M007, M010

## Regression unit

Materialize a baseline request, send it to a candidate, observe the candidate through the same semantic recorder, then compare typed fields.

## v0.1 comparison classes

Status; selected/ignored headers and trailers; exact body/hash; semantic JSON; error category/phase; missing/unexpected behavior; response-head and total-duration assertions; replay transport failure.

Later work adds stream cadence, SSE, WebSocket messages, and scenario transitions.

## Target remapping

`--target` can replace the baseline origin while preserving path/query. Reports keep baseline logical origin, candidate target, and physical route separate.

## Timing

Never compare exact timestamps. Assertions are explicit absolute ceilings, relative ratios, or disabled, and reports retain observed values plus threshold.

## Determinism

Finding ordering and exit categories are stable for the same inputs/config. JSON reports are versioned. JUnit is a presentation of the same report authority, not a second evaluator.
