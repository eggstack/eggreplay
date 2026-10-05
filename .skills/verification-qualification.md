# Verification and Qualification

Read this **first**, before any task skill. It defines what "done" means here
and what evidence a milestone owes.

## The fast gate

```sh
cargo fmt --all -- --check \
  && cargo check --workspace --all-targets --all-features \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings \
  && cargo test --workspace --all-features
```

CI runs the same four steps with `--locked` added and `--no-fail-fast` on tests
so one failing suite never hides the rest. Note `--all-targets`: the
`protocol-boundary` lane added it after the M015B regression where a `direct`
profile broke partly inside `mod tests` blocks that a lib-only check never
built. An optional feature is only a boundary if the profiles on *both* sides
of it still compile, tests included.

Do not run a subset and call it verified. The four steps are the gate.

## Known-local failure

Two pre-existing `curl_interop` tests fail on some local machines and are
proven machine-specific by four green hosted `verify` jobs across
Ubuntu/macOS/Windows. They gate on `curl` being present and behaving a
particular way. They are not a regression you introduced, and they are not a
green gate either — say so explicitly rather than claiming the suite is clean.

## Python binding loop

Separate from the Rust gate; the native extension is a leaf crate and no Rust
product crate may depend on PyO3.

```sh
cd crates/eggreplay-python
uv sync --extra dev
maturin develop
python -m pytest tests
```

A stale `.so` produces confusing failures — rebuild before blaming the tests.
CI pins the tools explicitly (`maturin==1.14.1`, `pytest==8.4.2`,
`pytest-asyncio==1.2.0`, `pytest-xdist==3.8.0`, `uv 0.8.22`); prefer
`uv run --with <pinned>` to reproduce a CI lane locally.

## The boundary lanes are the real gate

The four single-runner CI jobs assert *architecture*, not behavior. A change
that compiles, passes every test, and violates one of these is a boundary
break, not a feature:

| Lane | Asserts |
|---|---|
| `dependency-boundary` | `core`/`store` pull no transport runtime; the `direct` graph has no WebSocket codec; Eggress enables only `pproxy-compat`; no product crate depends on `eggreplay-intercept` or `rcgen` |
| `protocol-boundary` | ordinary profiles exclude `eggserve-core`/`eggserve-static`/`eggserve-h3`; only `h2-inbound`/`h2-inbound-tls` pull the closure; no QUIC/H3 in any supported graph; interception never adopts multiprotocol serving; the wheel stays multiprotocol-free; **Tonic never enters a product graph**; `inbound.rs` keeps its `#[cfg(feature = "h2-inbound")]` gate |
| `python-bindings` | `core`/`store`/`http` have no PyO3 edge; no Rust product crate depends on `eggreplay-python` |
| `python-abi3-cross-version` | one abi3 wheel built under 3.11 installs and passes under 3.14 |

Reproduce a boundary lane locally before assuming CI will agree:

```sh
cargo tree -p eggreplay-http --no-default-features --features direct --edges normal --prefix none \
  | grep -Ei '(^|[[:space:]])(tungstenite|base64)([[:space:]]|$)' && echo "LEAK" || echo clean
```

## The qualification model

A milestone is **not** closed because the code works. Per `plans/registry.md`,
closing requires all four:

1. implementation,
2. required tests/evidence,
3. documentation updates,
4. a closure record under `plans/closure/`.

Hosted-CI-gated plans stay open until the remote run is green. A plan moves to
`closed` only on evidence, never on the presence of source — "Plans remain audit
artifacts after implementation. Source presence alone never closes a plan."

A closure record must name the qualifying revision (the exact SHA) and the
hosted run ID, and must state the per-suite test counts. If you cannot produce
both, the milestone is `implemented`, not `closed`.

## Recording a closure

- Update the `plans/registry.md` row **and** the "Current execution gate"
  prose below the table — the prose is what a reader actually lands on.
- Add `plans/closure/<milestone>.md` with: the qualifying SHA, the hosted run
  URL, exact commands, per-suite counts, the supported matrix, and known
  limitations.
- Plans become historical after closing. Do not edit a closed plan's step list
  to match what you later built; add a corrective milestone instead.
- Never hardcode a live SHA outside the canonical qualification records. Docs
  and skills reference the ledger.

## Working style

- One logical change per commit. Never commit without an explicit user request.
- Make the workspace green before adding functionality, not after.
- Never add a CI job, matrix, or evidence schema without an explicit request.

## Architecture References

- [`architecture/12-testing-and-qualification.md`](../architecture/12-testing-and-qualification.md)
  — test topology, the CI matrix, the closure record format.
- [`architecture/01-workspace-and-boundaries.md`](../architecture/01-workspace-and-boundaries.md)
  — lint/toolchain policy and the enforcement lanes in detail.
- [`plans/registry.md`](../plans/registry.md) — the live execution gate.
