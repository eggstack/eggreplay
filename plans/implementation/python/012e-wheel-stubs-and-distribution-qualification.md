# M012E — Wheels, Typing, and Distribution Qualification

Status: blocked
Depends on: M012D
Parent milestone: M012

## Objective

Produce installable Python artifacts with truthful interpreter/platform tags,
typing metadata, clean-environment smoke tests, and no publication side effects.

## A. Distribution metadata

The distribution/import name is `eggreplay`.

Configure `pyproject.toml` with:

- maturin build backend;
- Python requirement matching the qualified minimum (target 3.11);
- project metadata/license/repository;
- pytest plugin entry point;
- package data for `py.typed` and stubs;
- no runtime dependency on pytest.

Do not publish to PyPI in this plan.

Before eventual publication, verify the package name is available/reserved by
the intended publisher; do not treat search-engine absence as ownership.

## B. ABI/wheel strategy

Use the M012A decision.

Preferred GIL tier if abi3 qualified:

- `abi3-py311`;
- CPython 3.11–3.14 import/runtime smoke;
- one wheel per OS/architecture rather than per interpreter.

Initial required build targets:

- manylinux x86_64;
- manylinux aarch64;
- macOS arm64;
- macOS x86_64;
- Windows x86_64.

If a target cannot be runtime-smoked in hosted infrastructure, mark the wheel
experimental/unqualified rather than claiming support.

Do not claim musllinux, Windows arm64, PyPy, GraalPy, CPython 3.15, or
free-threaded CPython without dedicated evidence.

## C. Linux aarch64

Because Eggstack targets SBCs, aarch64 is a real distribution target.

Build the manylinux aarch64 wheel and perform an import/fixture smoke on native
arm64 CI when available or a documented QEMU/container equivalent. A
cross-compiled wheel without runtime execution is build evidence only, not a
support claim.

## D. Typing surface

Ship `py.typed` and complete public `.pyi` stubs.

Stubs must cover:

- fixture/data wrappers;
- enums/config;
- report/findings;
- async and sync lifecycle APIs actually supported;
- pytest-facing helpers.

Add an API-manifest test comparing exported runtime names to stubbed public
names. Do not duplicate semantic constants by hand where they can be exported
from Rust.

## E. Clean install smokes

For each claimed wheel family:

1. create a clean environment;
2. install only the wheel and its declared Python dependencies;
3. import `eggreplay`;
4. open a tiny fixture;
5. start/stop a local replay server;
6. run one regression;
7. run a tiny pytest plugin smoke.

No source checkout should be required at runtime.

## F. Artifact inspection

Verify:

- wheel tags;
- no unexpected shared libraries;
- no Git paths/build directories;
- no fixture/test secrets;
- no editable-install leakage;
- license/readme metadata;
- deterministic/repeatable build properties where practical;
- sdist contains everything required to build but no local environment files.

## G. CI

Add a dedicated wheel workflow or clearly separated jobs so normal Rust CI
does not require every packaging target on every source-only commit.

PR/main CI should still run at least one Python binding/test lane.

Artifact/release workflow builds the full wheel set on demand/manual release
events, matching the repository's manual-release preference.

## Closure

Create
`plans/closure/m012e-python-wheel-stubs-and-distribution-qualification.md`
with exact artifact filenames/tags, hashes, interpreter/platform smoke results,
and unsupported targets. M012F becomes ready only after artifact qualification.
