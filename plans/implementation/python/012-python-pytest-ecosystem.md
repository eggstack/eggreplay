# M012 — Python Bindings and Pytest/VCR-Style Integration

Status: ready (decomposed; execute M012A first)
Depends on: M011 closure
Roadmap stage: 8
Architecture: ADR 0007

## Objective

Expose EggReplay's closed Rust authorities to Python and pytest without
creating a second matcher, fixture parser, recorder, transport, scenario
engine, or regression evaluator.

The distribution package is `eggreplay`; the native extension is
`eggreplay._native`. Pure-Python code is limited to ergonomics, pytest
integration, typing, and presentation.

## Execution decomposition

M012 is split into dependency-ordered plans:

| ID | Plan | Result |
|---|---|---|
| M012A | `012a-toolchain-package-and-abi-preflight.md` | PyO3/maturin/runtime/ABI/package substrate |
| M012B | `012b-fixture-report-and-data-bindings.md` | read-only fixture/data/report bindings |
| M012C | `012c-async-lifecycle-and-network-bindings.md` | replay/record/regression lifecycle |
| M012D | `012d-pytest-vcr-and-parallel-safety.md` | pytest/VCR ergonomics + explicit update safety |
| M012E | `012e-wheel-stubs-and-distribution-qualification.md` | wheels, typing, clean install qualification |
| M012F | `012f-hardening-hosted-qualification-and-closure.md` | security/runtime/platform hardening + closure |

Only M012A is ready initially. Do not skip subplan dependency order.

## Initial toolchain/support target

M012A starts from the currently compatible line:

- PyO3 0.29.2;
- pyo3-async-runtimes 0.29.0 with Tokio;
- maturin 1.14.1.

The initial product target is CPython 3.11–3.14 on GIL-enabled builds.
`abi3-py311` is preferred if EggReplay's own async/import tests qualify it.
Python 3.15, free-threaded CPython, PyPy, and GraalPy remain unclaimed until
dedicated evidence exists.

## Authority rules

ADR 0007 is binding:

- Rust owns fixture/schema validation, matching, redaction, scenario state,
  transport, WebSocket semantics, record modes, and regression findings;
- Python preserves ordered duplicate headers/query values;
- large bodies remain lazy/bounded;
- one process-wide Tokio/async bridge is used;
- sealed/offline is the pytest default;
- mutation is explicit;
- parallel writers to one fixture must fail rather than interleave.

## Closure

M012 closes only through M012F after all subplan closure records and one
coherent hosted Rust/Python/wheel evidence set exist.

Final closure:
`plans/closure/m012-python-pytest-ecosystem.md`.

M013 remains blocked until M012 closes.
