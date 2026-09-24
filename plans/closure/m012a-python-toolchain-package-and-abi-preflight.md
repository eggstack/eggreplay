# M012A — Python Toolchain, Package, and ABI Preflight Closure

Status: closed

## Implementation

- Qualifying revision: `4dd280ea72a2e66c22d4677603ebf8f4da800e95`
- Native extension crate: `crates/eggreplay-python`, module
  `eggreplay._native`; package import name `eggreplay`.
- PyO3 `0.29.2`, `pyo3-async-runtimes 0.29.0` with Tokio, and maturin
  `1.14.1`; CI uses uv `0.8.22`, pytest `8.4.2`, and pytest-asyncio `1.2.0`.
- ABI decision: GIL-enabled `abi3-py311`; Python 3.11 through 3.14 are
  qualified. Python 3.15, free-threaded CPython, PyPy, and GraalPy are not
  claimed.
- The extension crate is a `cdylib` and a Rust workspace leaf. Core, store,
  and HTTP product crates have no PyO3 dependency. Artifact excludes prevent
  Python bytecode, virtual environments, fixture material, and build output
  from entering the wheel or source distribution.

## Evidence

Local verification on CPython 3.14 x86_64 passed:

- maturin built and installed the `cp311-abi3-macosx_10_12_x86_64` wheel;
- native import and Rust-to-Python value roundtrip;
- asyncio await and cancellation smoke;
- wheel and source distribution content scan;
- Rust `fmt`, workspace check, clippy with warnings denied, and workspace
  tests (132 passed across 10 suites).

Hosted qualification passed on revision `4dd280ea72a2e66c22d4677603ebf8f4da800e95`:
[GitHub Actions run 35953277189](https://github.com/eggstack/eggreplay/actions/runs/35953277189).
The run includes stable Rust on Ubuntu, macOS, and Windows; Rust 1.89 on
Ubuntu; Python bindings on Ubuntu CPython 3.11/Rust 1.89 and CPython
3.14/stable Rust, macOS CPython 3.11, and Windows CPython 3.11; dependency
boundaries; and same-wheel ABI reuse.

The same wheel,
`eggreplay-0.1.0-cp311-abi3-manylinux_2_34_x86_64.whl`, was built under
CPython 3.11, installed under CPython 3.14, and passed all three Python smoke
tests. The macOS hosted wheel built as
`eggreplay-0.1.0-cp311-abi3-macosx_11_0_arm64.whl`; Windows built as
`eggreplay-0.1.0-cp311-abi3-win_amd64.whl`. Wheel and sdist scans passed with
no `.venv`, bytecode cache, fixture, or build-directory content.

The supported MSRV/platform and Python/ABI gates are green. M012B is ready.
