# M012A — Python Toolchain, Package, and ABI Preflight

Status: closed
Depends on: M011 closure
Parent milestone: M012
ADR: 0007

## Objective

Establish the Python extension/package substrate before binding product
semantics. Prove the chosen PyO3, async bridge, interpreter baseline, ABI
strategy, and maturin layout work with Rust 1.89 on EggReplay's hosted
platforms.

M012B must not begin until this closes.

Implementation, local verification, artifact inspection, and hosted
qualification are complete. See
`plans/closure/m012a-python-toolchain-package-and-abi-preflight.md`.

## A. Package topology

Add:

```text
crates/eggreplay-python/
  Cargo.toml
  pyproject.toml
  src/lib.rs
  python/eggreplay/__init__.py
  python/eggreplay/py.typed
  tests/
```

The package name is `eggreplay`; the compiled module is
`eggreplay._native`.

Add the Rust crate to the workspace as a `cdylib`. It may depend on
`eggreplay-core`, `eggreplay-store`, and `eggreplay-http`, but those crates
must not gain PyO3/Python dependencies.

Do not publish to PyPI in this plan.

## B. Dependency baseline

Start with exact qualified versions:

- `pyo3 = 0.29.2`;
- `pyo3-async-runtimes = 0.29.0`, Tokio runtime only;
- maturin CLI/build backend 1.14.1 in CI/tooling.

Use PyO3 `extension-module` and prefer `abi3-py311`.

Do not enable experimental/free-threaded features merely because upstream
supports them.

## C. Interpreter/ABI proof

Initial support target is CPython 3.11–3.14, GIL builds.

Prove:

1. Rust 1.89 compiles the extension;
2. `maturin develop` imports `eggreplay._native`;
3. a minimal Rust->Python value roundtrip works;
4. a minimal Rust future exposed through `pyo3-async-runtimes` awaits under
   asyncio;
5. cancellation of that awaitable reaches Rust;
6. the extension imports under CPython 3.11 and 3.14;
7. the `abi3-py311` wheel built once for a platform imports on both tested
   interpreters.

If `abi3-py311` fails because a required async/PyO3 API is unavailable under
the limited API, record the exact blocker and switch to per-interpreter wheels.
Do not use unsafe FFI to bypass PyO3.

Python 3.15 and free-threaded CPython remain unclaimed in M012A.

## D. Runtime authority

Create one small internal module for the shared Tokio/async bridge.

Required invariants:

- no `Runtime::new()` per function/object;
- no detached thread per object;
- no Python callback executed while holding a Rust mutex;
- blocking Python entry points release the GIL while waiting;
- module finalization does not hang on runtime shutdown.

Only lifecycle scaffolding belongs here; real EggReplay server/network
bindings are M012C.

## E. Build/repository boundaries

Add CI checks proving:

- `eggreplay-core`, `eggreplay-store`, and `eggreplay-http` have no PyO3
  dependency;
- direct Rust workspace builds remain Python-free when the extension crate is
  excluded;
- the Python crate cannot become a dependency of any Rust product crate;
- source distributions/wheels do not accidentally vendor fixture secrets,
  test artifacts, or repository-local virtualenv files.

## F. Python development tooling

Use a reproducible local test environment. Prefer `uv` or standard virtualenv
workflow, but do not make the runtime package depend on a particular
environment manager.

Development dependencies should include at least:

- pytest;
- pytest-asyncio;
- maturin.

Pin CI tool versions deliberately. Add commands to AGENTS/developer docs.

## Required tests/evidence

- CPython 3.11 import smoke;
- CPython 3.14 import smoke;
- abi3 cross-version import proof or documented per-interpreter fallback;
- asyncio future/cancellation smoke;
- Rust 1.89 check;
- Linux, macOS, Windows extension build;
- dependency-boundary checks;
- clean `maturin build` and wheel install in a fresh environment.

## Closure

Create `plans/closure/m012a-python-toolchain-package-and-abi-preflight.md`
with exact versions, interpreter results, ABI decision, and hosted run
evidence. Only then move M012B to ready.
