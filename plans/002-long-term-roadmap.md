# 002 — Long-Term Roadmap

Status: canonical

## Stage 1 — Foundation (M001–M002)

Workspace/contracts and the canonical flow/session + `.eggr` store.

## Stage 2 — Record and replay (M003–M005)

EggFetch recording, EggServe offline replay, deterministic matching,
consumption, and near-miss diagnostics.

## Stage 3 — Regression product (M006–M007)

Candidate replay/diff plus optional Eggress routing and CLI machine contracts.

## Stage 4 — v0.1 hardening (M008 + C001–C006)

Security/redaction, streaming/concurrency corrections, CLI/routing correction,
expanded qualification, and hosted Linux/macOS/Windows/MSRV closure.

Outcome: v0.1 hosted release qualification closed.

## Stage 5 — Stateful/dynamic replay (M009)

Record modes, explicit pass-through/record-on-miss, authored state-machine
scenarios, extraction, deterministic templates, and bounded transforms.

Status: closed.

## Stage 6 — Streaming semantics (M010)

Stream-event timing, mid-body failures, SSE-aware views/diffing, and concurrent
timeline replay.

Status: closed with M010-C1 corrective qualification.

## Stage 7 — WebSockets (M011)

Semantic RFC 6455 conversations attached to initiating HTTP Upgrade flows using
EggFetch upgraded streams and EggServe tunnel handoff.

Status: closed through M011A–M011F under ADR 0006.

## Stage 8 — Python/test ecosystem (M012)

Thin PyO3 bindings over Rust authorities plus pytest/VCR-style fixture
ergonomics. No second matcher/store/network implementation.

ADR: `adrs/0007-python-binding-authority-and-runtime.md`.

Execution is decomposed:

- M012A — Python toolchain, package, ABI/runtime preflight;
- M012B — fixture/report/data bindings;
- M012C — async lifecycle/network bindings;
- M012D — pytest/VCR ergonomics and parallel mutation safety;
- M012E — wheels, typing, distribution qualification;
- M012F — hardening, hosted qualification, milestone closure.

See `implementation/python/`. M012A–M012F and the M012 umbrella are closed
with local and hosted Rust/Python/wheel qualification. See
`closure/m012-python-pytest-ecosystem.md`.

## Stage 9 — Explicit proxy and optional interception (M013)

Separate explicit-forward-proxy acquisition and opt-in HTTPS MITM with a
dedicated CA/key boundary. Interception stays outside default library/Python
dependency graphs.

ADR: `adrs/0008-interception-security-and-transport-boundary.md`.

Execution is decomposed:

- M013A — substrate/dependency/threat preflight (closed);
- M013B0 — EggServe 0.3 adoption + absolute-form qualification (closed);
- M013B — explicit HTTP proxy + CONNECT deny/tunnel (closed);
- M013C — CA lifecycle + bounded exact-host leaf issuance (closed);
- M013D — HTTPS MITM HTTP/1.1 recording (closed);
- M013E — CLI/policy/operator UX (closed);
- M013F — hardening, hosted qualification, closure (closed).

Status: closed. M013 and M013A–M013F are closed on qualifying implementation
`5efc6f9` with hosted Actions runs
[36211265347](https://github.com/eggstack/eggreplay/actions/runs/36211265347)
and
[36456906216](https://github.com/eggstack/eggreplay/actions/runs/36456906216).
See `closure/m013-explicit-proxy-and-optional-mitm.md` for the full matrix,
commands, and support/limitation table; the roadmap records no separate
qualification evidence.

## Stage 10 — broader compatibility (M014)

Status: closed decomposed compatibility stage.

Umbrella program split into bounded tracks plus a planning reconciliation
gate:

- M014-R1 planning/documentation reconciliation (closed);
- M014A HAR import/export + fixture migration (closed);
- M014B HTTP/2 qualification (closed; experimental outbound H2);
- M014C HTTP/3 feasibility/qualification (closed; H3 deferred per ADR 0009);
- M014D gRPC-aware views + bounded fault-model polish (closed).

Status: closed. See `implementation/compatibility/` and
`closure/m014-compatibility-program.md` for the consolidated support
matrix.

### Post-Stage-10 corrective gate (closed)

M014-C1 fixed the WebSocket conversation/session-finalization race with a
deterministic session-owned completion barrier. M014-C2 isolated synchronous
finalization from async executors, cleared the stable Clippy blocker, corrected
support wording, and supplied fresh hosted qualification on `cbc9257` (CI run
`36891564494` plus wheel run `36891564581`; closure commit `88daa3ed` green on
`36894700433`). The failed `36881596131` attempt on `b188c552` and the invalid
pre-C1 `c71ffd7` / `36778923619` claim are retained as audit history in the
C1/C2 closures and are not qualification evidence. M014-C1 and M014-C2 are both
closed; M014 remains closed. Stage 11 executed as M015 and is closed.

## Stage 11 — Bidirectional HTTP/2 and transport baseline (M015)

Status: closed (M015A–M015E).

Stage 11 refreshed EggReplay onto the current published Eggstack transport
line and extended M014B's outbound-only HTTP/2 experiment into a coherent,
opt-in bidirectional H2 path without changing the supported H1 default.

Execution decomposed as:

- M015A — published dependency refresh + optional EggServe-Core H2 boundary
  (ADR 0010);
- M015B — inbound H2 recording gateway + offline replay;
- M015C — end-to-end H2 semantic/regression/routing qualification;
- M015D — real gRPC-over-H2 integration qualification using M014D views;
- M015E — hardening, hosted qualification, support-matrix reconciliation,
  and Stage 11 closure.

The successful tier is **experimental bidirectional H2**, as expected, because
EggServe 0.4 still classifies H2 as opt-in/experimental. H1 remains the
supported default. gRPC over H2 is a qualified experimental tier, with
un-terminated bidirectional calls deferred for want of a terminal-status story.
H2 MITM, H3/QUIC, WSS, WebSocket extended CONNECT, and a generic reverse proxy
are outside M015 and remain unqualified.

M015 deliberately keeps H3 for a later stage: EggFetch and EggServe now have
experimental H3 endpoints/adapters, but Eggress 1.0.11 still does not expose a
generic caller-owned QUIC connection seam analogous to its TCP connector.

## Roadmap rule

Later stages may not inflate earlier dependency closure. Every support claim
requires EggReplay repository evidence; sibling-project capability or roadmap
language alone is not support evidence.
