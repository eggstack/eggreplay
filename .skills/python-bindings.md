# Python Bindings

Use when working on `crates/eggreplay-python`, the pytest plugin, or wheel
packaging.

## Leaf status — the rule that breaks the build

`eggreplay-python` is a **leaf workspace crate** in both directions:

- No Rust product crate may depend on PyO3. `eggreplay-core`, `eggreplay-store`,
  and `eggreplay-http` must have no PyO3 edge.
- No Rust product crate (`core`, `store`, `http`, `cli`) may depend on
  `eggreplay-python`.

Both are asserted in the `python-bindings` CI lane and both are load-bearing.
If you find yourself wanting PyO3 in a semantic crate, the design is wrong, not
the lane.

The qualified default wheel intentionally **omits** the M013
interception/CA capability — that lives in the CLI's `--features intercept`
build only. A wheel that gained `rcgen` or `eggreplay-intercept` would be a
boundary break.

The wheel is also multiprotocol-free: it must never pull `eggserve-core`,
`eggserve-static`, or `eggserve-h3`. No inbound HTTP/2 and no gRPC view exist in
the Python surface.

## Development loop

```sh
cd crates/eggreplay-python
uv sync --extra dev
maturin develop
python -m pytest tests
```

Rebuild after **any** Rust change in the crate. A stale `.so` produces failures
that look like test bugs and are not. CI pins the tools — `maturin==1.14.1`,
`pytest==8.4.2`, `pytest-asyncio==1.2.0`, `pytest-xdist==3.8.0`, `uv 0.8.22` —
so reproduce a lane with `uv run --with <pinned>` rather than whatever happens
to be on `PATH`.

## Feature selection

`eggreplay-python` consumes `eggreplay-http` with
`default-features = false, features = ["direct","eggserve","eggress","websocket"]`
— it explicitly drops `default` and re-adds `direct`. No H2, no gRPC. It also
declares `eggfetch-core` and `eggserve-server` directly, so a dependency change
there needs the same scrutiny as a product crate.

## Design constraints

- The binding is an **adapter**. Semantic authority stays in
  `eggreplay-core`/`-store`/`-http`; the binding owns lifecycle, views, and
  exception mapping. Do not reimplement matching, storage, or comparison in
  Python.
- **Two-layer structure**: the Rust module plus a thin `_lifecycle.py` /
  package layer. Keep the split; a public API implemented in the Python layer
  drifts from the Rust one.
- **Error mapping** is a fixed, exhaustive translation from the core taxonomy
  into Python exception types. When you add an `ErrorCategory` or
  `ErrorPhase`, update the mapping in the same change or Python callers get a
  bare internal error. Never invent a new exception type outside this map.
- **Async lifecycle** is managed by the extension: fixtures, replay servers,
  and recording lifecycles have explicit, bounded lifetimes. Dropping a
  connection mid-flight must be safe and must not leak a task.
- **Parallel test safety**: fixtures are directories with atomic publication, so
  concurrent tests never observe a half-written fixture. Do not weaken the
  staging-and-rename discipline to make a test faster.
- The adapter is **not a drop-in VCR.py replacement**. The pytest plugin offers
  fixtures and explicit record modes; it does not aim at cassette compatibility.

## Packaging

- abi3 wheel: one wheel must install and pass across the supported CPython
  range. The `python-abi3-cross-version` lane builds under 3.11 and runs the
  same wheel under 3.14.
- Qualified platforms: Linux x86_64/aarch64, macOS arm64/x86_64, Windows
  x86_64; CPython 3.11–3.14.
- Wheel and sdist contents are asserted. `.venv`, `.pytest_cache`,
  `__pycache__`, `fixtures`, `target`, and `*.pyc`/`*.pyo` must not appear.
- `crates/eggreplay-store/Cargo.toml` excludes `tests/fixtures/**` from the
  published package. Keep it that way; goldens are test data, not product.

## Architecture References

- [`architecture/10-python-bindings.md`](../architecture/10-python-bindings.md)
  — module registration, fixture views, async lifecycle, error mapping, the
  pytest plugin, and packaging.
- [`crates/eggreplay-python/README.md`](../crates/eggreplay-python/README.md)
  — user-facing fixtures, record modes, VCR.py migration notes.
- [`architecture/01-workspace-and-boundaries.md`](../architecture/01-workspace-and-boundaries.md)
  — the Python profile in the feature matrix.
