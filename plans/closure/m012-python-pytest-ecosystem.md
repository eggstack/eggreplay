# M012 — Python Bindings and Pytest/VCR-Style Integration Closure

Status: closed

## Implementation

- Qualifying revision: `d7d64de3249ab2e7aab639262be68051454fcdaa`.
- The native package is `eggreplay`, built by maturin `1.14.1`. It uses
  PyO3 `0.29.2`, `pyo3-async-runtimes 0.29.0`, the Tokio bridge, and the
  `abi3-py311` stable ABI. CPython 3.11–3.14 GIL builds are the supported
  interpreter range.
- Python remains a leaf adapter. Rust owns fixture parsing and validation,
  matching/scenario state, record policy, redaction, HTTP/WebSocket transport,
  persisted fixture publication, and regression decisions. The Python layer
  supplies data ergonomics, typed/report projections, and pytest lifecycle
  management. Transport continues through EggFetch, EggServe, and Eggress.
- Pytest is sealed/read-only by default. Explicit `once`, `append-new`, and
  `re-record` modes control writes. Relative fixture paths are confined to the
  pytest root, read-only xdist workers may share fixtures, and an atomic
  sibling lock refuses concurrent writers with stale-owner recovery guidance.
  Recordings publish only after close/finalization.
- The plugin uses the process-wide Rust runtime bridge. Its Rust cleanup task
  remains observable through `wait()` after Python waiter cancellation; explicit
  close and context-manager paths are deterministic. A child-process test
  confirms that interpreter exit with an unclosed replay server does not hang.
- Regression assertion output omits report values. Sentinel coverage checks
  exceptions, public repr/str, pytest terminal output, and persisted HTTP
  authorization/cookie headers after Rust redaction. There is no API for
  reading pre-persistence staging data.
- VCR.py migration is conceptual rather than compatible: EggReplay uses
  semantic `.eggr` directory fixtures, explicit record modes, and Rust-owned
  behavior. It does not support arbitrary callbacks/scripts or patch Python
  HTTP clients.

## Verification

Local CPython 3.14 x86_64 qualification passed:

- maturin built and installed `eggreplay-0.1.0-cp311-abi3-macosx_10_12_x86_64`;
- Python suite: 37 passed;
- `cargo fmt --all -- --check`;
- `cargo check --workspace --all-targets --all-features --locked`;
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`;
- `cargo test --workspace --all-features --locked`: 132 passed across 10 suites;
- `cargo audit` and `git diff --check` passed.

The qualifying hosted standard CI run passed on the same revision:
[GitHub Actions run 35970168269](https://github.com/eggstack/eggreplay/actions/runs/35970168269).
It includes Ubuntu stable and Rust 1.89, macOS stable, Windows stable,
dependency-boundary checks, CPython 3.11 binding/pytest on Ubuntu/macOS/Windows,
CPython 3.14 binding/pytest on Ubuntu, and same-wheel CPython 3.11-to-3.14 ABI
reuse. All four Python lanes passed 37 tests; Rust lanes passed the 132-test
workspace suite.

Full clean wheel and sdist qualification passed on the same revision:
[GitHub Actions run 35970168143](https://github.com/eggstack/eggreplay/actions/runs/35970168143).
Native wheel builds, tag/content inspection, clean installation and replay /
regression / pytest smoke passed on Linux x86_64, Linux aarch64, macOS arm64,
macOS x86_64, and Windows x86_64. The source distribution inspection and
wheel-from-sdist build passed. One Linux x86_64 `abi3` wheel passed the clean
smoke under CPython 3.11, 3.12, 3.13, and 3.14.

An earlier full CI run on `d9ae7e7` had one macOS Rust-suite failure in the
existing WebSocket gateway test
`recording_gateway_captures_upgrade_and_leading_post_101_messages`, which
reported missing conversation metadata at fixture finish. The same full suite
passed on the qualifying revision `d7d64de` without Rust source changes; the
failure did not reproduce.

## M012A–M012E evidence

The preceding closure records remain the detailed evidence for each plan:

- [M012A toolchain/package/ABI preflight](m012a-python-toolchain-package-and-abi-preflight.md)
- [M012B fixture/report/data bindings](m012b-python-fixture-report-and-data-bindings.md)
  and its [lifetime/symlink errata](m012b-python-fixture-report-and-data-bindings-errata.md)
- [M012C async lifecycle/network bindings](m012c-python-async-lifecycle-and-network-bindings.md)
- [M012D pytest/VCR and parallel safety](m012d-pytest-vcr-and-parallel-safety.md)
- [M012E wheels, stubs, and distribution qualification](m012e-python-wheel-stubs-and-distribution-qualification.md)

Together these records document the complete implementation and hosted
qualification sequence. No package was published to PyPI.

## Support and limits

Qualified: CPython 3.11–3.14 with the GIL on Linux x86_64, Linux aarch64,
macOS arm64, macOS x86_64, and Windows x86_64 using `abi3-py311`.

Not claimed: CPython 3.15, free-threaded CPython, PyPy, GraalPy, musllinux,
Windows arm64, and other architectures. Recording still requires explicit
close to publish. WebSocket `append-new` remains unsupported. Parallel writers
must use distinct fixture paths; stale locks require explicit operator
recovery. The Python API is not drop-in VCR.py compatibility.

M012 and M012F are closed. M013 is ready; M014 and its tracks remain blocked on
their declared M013 dependencies.
