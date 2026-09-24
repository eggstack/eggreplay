# M012F — Python Hardening, Hosted Qualification, and M012 Closure

Status: blocked
Depends on: M012E
Parent milestone: M012

## Objective

Audit the completed Python surface for ownership drift, cancellation/resource
bugs, packaging misclaims, and platform gaps; then close M012 with one coherent
hosted evidence set.

## A. Authority audit

Verify that Python contains no second implementation of:

- fixture parsing/validation;
- matcher/scenario transitions;
- redaction;
- HTTP/WebSocket transport;
- record-mode policy;
- regression/diff decisions.

Pure-Python code may orchestrate Rust-backed objects and pytest lifecycle only.

## B. Runtime/GIL audit

Prove:

- no runtime per call/object;
- no leaked/detached lifecycle tasks;
- no GIL held across network waits, sleeps, fixture IO, or shutdown;
- Python cancellation reaches Rust;
- interpreter exit after unclosed-object fallback does not hang;
- explicit close/context-manager paths remain deterministic.

Add stress tests for repeated create/close cycles.

## C. Filesystem/concurrency audit

Prove:

- read-only xdist sharing;
- writer lock refusal;
- lock cleanup on normal completion;
- crash/stale-lock diagnostics are explicit;
- no partial manifest publication;
- Windows file-handle finalization remains clean;
- fixture paths derived by pytest remain confined.

## D. Security/redaction audit

Run Python-level sentinel tests demonstrating secrets do not appear in:

- Python exceptions;
- repr/str of public objects;
- pytest failure output;
- JSON/report helpers;
- persisted blobs after configured redaction.

Do not add Python APIs that expose raw redacted-before-persistence staging data.

## E. Support matrix

The closure records exact support.

Expected initial target if all preceding evidence passes:

- CPython 3.11–3.14 GIL builds;
- Linux x86_64;
- Linux aarch64;
- macOS arm64;
- macOS x86_64;
- Windows x86_64.

Python 3.15/free-threaded may be added only if a final supported interpreter,
ABI tags, wheel build, import, asyncio, pytest, and lifecycle tests all pass.
Otherwise explicitly defer them.

## F. Full verification

Rust:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo audit
git diff --check
```

Python/package:

```text
maturin build --locked
python -m pytest
```

plus the M012E clean-wheel matrix.

Run one qualifying hosted revision containing:

- Ubuntu stable Rust;
- Ubuntu Rust 1.89;
- macOS stable Rust;
- Windows stable Rust;
- dependency-boundary;
- CPython 3.11 binding/pytest;
- CPython 3.14 binding/pytest;
- full claimed wheel smoke matrix.

## G. Documentation and closure

Update:

- README Python quickstart;
- Python API/pytest docs;
- architecture/dependency docs;
- roadmap;
- registry;
- planning README.

Document migration concepts from VCR.py without claiming drop-in compatibility:
semantic `.eggr` directory fixtures, explicit record modes, no arbitrary
callbacks/scripts, Rust-owned matching/redaction.

Create `plans/closure/m012-python-pytest-ecosystem.md` containing:

- implementation revisions;
- PyO3/async/maturin versions;
- ABI decision;
- Python/platform wheel matrix;
- Rust and Python test counts;
- Actions run IDs/URLs;
- known limitations;
- evidence from M012A–M012E closures.

Only after that evidence:

- mark M012 and M012F closed;
- mark M013 ready;
- leave M014 tracks blocked by their declared dependencies.

## Acceptance

M012 closes only when a clean installed Python package can inspect fixtures,
run replay/record/regression lifecycle, use pytest safely in sealed and explicit
update modes, preserve Rust semantic authority, and pass the advertised hosted
platform/interpreter matrix.
