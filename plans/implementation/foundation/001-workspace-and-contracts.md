# M001 — Workspace and Contract Skeleton

Status: ready
Depends on: none
Release gate: foundation

## Objective

Create the smallest compilable Rust workspace that fixes ownership boundaries before feature implementation.

## Deliverables

1. Root workspace with MSRV 1.89, resolver 3, shared lints, release profile, license metadata, and CI skeleton.
2. Crates: `eggreplay-core`, `eggreplay-store`, `eggreplay-http`, `eggreplay-cli`.
3. Root README, MIT license, AGENTS.md, and testing/contribution notes.
4. Typed core error categories; no public string-matching contract.
5. Config skeleton for limits, redaction profile, matcher profile, output format.
6. Feature policy: direct HTTP default; Eggress optional; no interception default dependency.
7. Deterministic temporary-fixture/local-network test helpers.
8. CI for stable + MSRV, fmt, clippy, tests, and minimum feature slices.

## Dependency preflight

Before pinning versions, verify current published/supported seams for EggFetch native body/custom Dialer, EggServe generic H1 service/runtime, and Eggress listener-free outbound connector.

If a required seam exists only on sibling main, do not silently make a permanent git-pin architecture. Record the publication gap and choose a temporary explicit pin with removal gate or defer that integration.

## Non-goals

No flow persistence, real network behavior, matcher, Python, MITM, WebSockets, scenarios, or broad configuration DSL. Do not add Tokio/Hyper/Eggstack transport dependencies to `eggreplay-core`.

## Acceptance

Clean checkout passes the documented fast verification command; each crate builds with minimum features; core/store stay transport-free; CLI help/version run; registry marks M001 closed and M002 ready with evidence.

## Closure evidence

Record selected dependency versions/surfaces, commands, CI result, implementation commit, and any topology deviation.
