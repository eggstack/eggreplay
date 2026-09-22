# M009 closure — Stateful and Dynamic Replay

Status: closed

## Implementation

Qualifying implementation SHA: `fea034b8ec5c722f019c780a53c3034307730ff6`
(`fix(m009): normalize fixture line endings`). The implementation was delivered
across this sequence:

- `bb2a126` — schema extensions, record modes, scenarios, replay coordinator,
  CLI and docs.
- `57809a0` — preserve empty fixture blob directories and exclude the tracked
  zero-byte `.gitkeep` marker from blob validation/copy.
- `fea034b` — enforce LF checkout for golden fixture bytes on Windows.

The implementation adds independent flow/session/report schema constants;
schema-1 read compatibility; schema-2 bounded, path-confined extensions with
unknown-required rejection; typed sealed/once/append-new/re-record policies;
scenario selection, ordered transitions, bounded extraction and deterministic
templates; append-new miss recording with redaction and per-key concurrency
coordination; staged re-record/append-new publication with recovery; route,
matcher and scenario CLI options; and user documentation. Transport ownership
remains EggFetch outbound, EggServe inbound, and Eggress optional routing.

The authored response descriptor is intentionally limited to UTF-8 body
templates up to 16 KiB plus bounded JSON Pointer replacements. It does not yet
provide a blob-reference body source for large authored responses; large bodies
remain available by referencing a recorded response. This is a documented
scope limitation for a later plan, not a schema-1 compatibility issue.

## Verification

Local gates on the qualifying implementation, all passed:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
git diff --check
```

The full workspace test command reported 76 passed across 10 test binaries on
the local Unix host. Coverage includes schema-1 and schema-2 fixtures,
extension path/symlink/size handling, migration and merge, record-mode policy,
same-key append serialization and unrelated-key concurrency, redaction,
scenario transition/state isolation, query/header/path/JSON extraction,
template and transform limits, CLI contracts, and existing HTTP qualification
behavior.

The store package test run after fixture line-ending normalization reported
14 passed. The complete local matrix passed before the final `.gitattributes`
change; the latter only fixes checkout normalization, and the store tests were
rerun successfully after it.

## Hosted qualification

GitHub Actions run `35791869565` on the qualifying SHA:
https://github.com/eggstack/eggreplay/actions/runs/35791869565

All required jobs passed:

- `verify (ubuntu-latest, stable)` — success, 76 tests.
- `verify (ubuntu-latest, 1.89.0)` — success, 76 tests.
- `verify (macos-latest, stable)` — success, 76 tests.
- `verify (windows-latest, stable)` — success, 74 tests; two Unix-only
  symlink-construction tests are excluded because the hosted Windows runner
  does not reliably allow symlink creation. Portable symlink rejection code
  still builds and the Windows test suite passes.
- `dependency-boundary` — success; core/store remain transport-free and the
  direct and Eggress feature boundaries qualify.

Two earlier hosted attempts exposed fixture distribution issues and remain
superseded: run `35790989653` found that Git does not preserve empty blob
directories, and run `35791351478` found CRLF conversion of the byte-exact JSON
fixture on Windows. The tracked directory markers, marker-aware blob handling,
and `.gitattributes` LF rule fixed those failures. The qualifying run above is
green on the final implementation SHA.

## Acceptance and handoff

- Session schema 1 remains readable and schema-2 extension behavior is bounded
  and validated: yes.
- Default replay is sealed/offline; network modes require explicit upstream:
  yes.
- Append-new uses EggFetch, redacts before persistence, and prevents duplicate
  same-key concurrent misses without serializing unrelated misses: yes.
- Re-record and append-new publish validated staged sessions with recovery:
  yes.
- Scenarios are explicitly selected and bounded, deterministic, isolated by
  replay instance, and do not evaluate user code or host resources: yes.
- Local gates and hosted matrix on the qualifying SHA are green: yes.
- Transport ownership boundaries remain intact: yes.

M010 is unblocked and moves to `ready`. M011–M014 remain blocked by their
unclosed dependencies.
