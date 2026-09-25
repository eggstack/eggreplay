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
- M013B0 — EggServe 0.3 adoption + absolute-form qualification
  (implemented locally; hosted closure pending);
- M013B — explicit HTTP proxy + CONNECT deny/tunnel;
- M013C — CA lifecycle + bounded exact-host leaf issuance;
- M013D — HTTPS MITM HTTP/1.1 recording;
- M013E — CLI/policy/operator UX;
- M013F — hardening, hosted qualification, closure.

EggServe's upstream absolute-form blocker is published under Plan 286. M013B0
has adopted the 0.3 line and is awaiting hosted qualification and closure;
M013B and later work remain blocked until that closure.

## Stage 10 — broader compatibility (M014)

Umbrella program split into bounded tracks:

- M014A HAR import/export + fixture migration;
- M014B HTTP/2 qualification;
- M014C HTTP/3 feasibility/qualification;
- M014D gRPC-aware views + bounded fault-model polish.

See `implementation/compatibility/`. M014/M014A/M014B remain blocked until
M013 closes; later tracks keep their declared dependencies.

## Roadmap rule

Later stages may not inflate earlier dependency closure. Every support claim
requires EggReplay repository evidence; sibling-project capability or roadmap
language alone is not support evidence.
