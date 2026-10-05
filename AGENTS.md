# Agent Guide

EggReplay is a Rust-native semantic HTTP interaction recorder, deterministic
offline replay server, and network-regression tool. It owns **semantics** —
flows, matching, scenarios, redaction, comparison, reporting — and delegates
**transport** — HTTP/TLS/framing, inbound serving, route establishment — to
EggFetch, EggServe, and Eggress.

## Start here

1. [`architecture/overview.md`](architecture/overview.md) — the bird's-eye map:
   crate ownership, transport delegation, capability tiers, data flow, and a
   review-entry-point list. It indexes 12 per-component deep dives.
2. A **task skill** in [`.skills/`](.skills/) — the workflow and invariants for
   the kind of work you are doing:

| Skill | Use when |
|---|---|
| [`verification-qualification`](.skills/verification-qualification.md) | **Read first.** The gate, the boundary CI lanes, and what a milestone owes to close. |
| [`rust-development`](.skills/rust-development.md) | Writing or reviewing Rust anywhere in the workspace. |
| [`fixture-and-store`](.skills/fixture-and-store.md) | `.eggr` persistence, session schema, extensions, redaction, HAR/migration. |
| [`protocol-and-routing`](.skills/protocol-and-routing.md) | HTTP/2, gRPC views, WebSocket codec, Eggress routing, the H3 deferral. |
| [`cli-development`](.skills/cli-development.md) | Commands, flags, envelopes, exit codes. |
| [`interception`](.skills/interception.md) | The opt-in proxy, CONNECT policy, CA, and HTTPS MITM. |
| [`python-bindings`](.skills/python-bindings.md) | PyO3 bindings, the pytest plugin, wheel packaging. |
| [`documentation`](.skills/documentation.md) | Updating any doc, plan, or this file. |

3. [`plans/registry.md`](plans/registry.md) for the live execution gate, and
   [`plans/README.md`](plans/README.md) for the planning convention.

## Boundaries

These are not style preferences. CI asserts them, and breaking one is a defect
even if every test passes.

- **EggReplay never owns an HTTP or TLS stack.** Do not add a parallel HTTP
  implementation. A missing seam is a research task with a plan and a closure
  record, not a local workaround.
- **`eggreplay-core` and `eggreplay-store`** carry no transport runtime — no
  EggFetch, EggServe, Eggress, Tokio, Hyper, or tungstenite edge.
- **`eggreplay-intercept` is a leaf.** No product crate and no Python build
  depends on it, and it never adopts the multiprotocol serving layer, so
  interception can never silently gain HTTP/2.
- **`eggreplay-python` is a leaf in both directions.** No Rust product crate may
  depend on PyO3 or on `eggreplay-python`.
- **No HTTP/2 capability is default in any profile**, including the Python wheel
  and the interception graph. `h2-inbound` is the only feature that may admit
  `eggserve-core`, and it is never default. `h2` (outbound) and `h2-inbound`
  are independent.
- **`tonic` is a dev-dependency of `eggreplay-http` only.** An independent gRPC
  implementation qualifies the view; no product graph gains a gRPC stack.
- **Fail closed, never downgrade.** An unbuildable protocol policy, an unknown
  required extension, a missing operator identity, or a dead route is an
  explicit error. No silent fallback to a weaker protocol, and no
  ignore-required-extension switch.
- **Redaction precedes blob finalization**, and redacted request fields become
  matcher wildcards rather than literal placeholders.
- **Fixtures are immutable while a session or handle is open**; concurrent blob
  replacement must fail as an integrity error.
- **A match that is found but cannot be consumed is a near-miss**, never a
  silent repeat or skip. Offline replay never falls back to the network on a
  miss.

## Verification

The supported command, required before submitting changes:

```sh
cargo fmt --all -- --check \
  && cargo check --workspace --all-targets --all-features \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings \
  && cargo test --workspace --all-features
```

CI adds `--locked` and `--no-fail-fast`. Note that the four CI
boundary lanes (`dependency-boundary`, `protocol-boundary`, `python-bindings`,
`python-abi3-cross-version`) assert architecture rather than behavior — a change
that compiles and passes every test can still be a boundary break. Details and
local reproductions are in
[`.skills/verification-qualification.md`](.skills/verification-qualification.md).

Python bindings are a separate loop: an isolated environment in
`crates/eggreplay-python`, pinned tools, then `maturin develop` and
`python -m pytest tests`. Rebuild after any Rust change there — a stale `.so`
produces failures that look like test bugs.

## Working style

- Use `plans/` as the execution source of truth: work milestones in dependency
  order, update `plans/registry.md` **and** its current-execution prose, and add
  a closure record under `plans/closure/` before marking anything closed. A
  milestone closes on evidence, never on the presence of source.
- One logical change per commit. Never commit without an explicit user request.
- Make the workspace green before adding functionality, not after.
- Keep machine-readable command results on stdout and operational diagnostics on
  stderr.
- Prefer preserving a documented limitation over quietly removing it. When
  documentation and code disagree, the code is correct and the doc is the bug.
