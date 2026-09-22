# C004 closure — CLI, Eggress Routing, Exit Codes, and Output Contracts

Status: closed

## Implementation

- Dependency: `eggreplay-http` `eggress` feature now enables only `eggress-outbound/pproxy-compat`; no extended/SSH/QUIC/listener/server. Verified via `Cargo.lock` (pproxy-compat + deps, no extended).
- Eggress surface (`eggress.rs`): `parse_route` (`direct` → None, else `from_pproxy_uri` with credential-redacted errors, no fallback), `redact_route_credentials` (per-hop `://<redacted>@`), `physical_route_for` (`direct`/`eggress` with redacted description). Unit tests: two-hop construction, malformed credential-redacted failure, redaction helper.
- Recording/regression: `record_request`, `record_request_with_session`, `start_recording_gateway`, and `execute_candidate` take explicit `PhysicalRoute`; recorded/candidate flows carry redaction-safe route metadata. Gateway threads route through clones (9 args, allowed).
- CLI: common `--route` on `record`/`replay`/`test` (default `direct`); `build_client` uses `EggressDialer` via EggFetch Dialer seam for non-direct, fails configuration (exit 2) before network with redacted diagnostics. Stable exit codes 0/1/2/3/4/5 with `failure_class` agreement between stdout JSON and stderr (`regression`/`diff`→1, `configuration`→2, `fixture`→3, `runtime`→4, else 5). `replay` reports with 0; `test`/`diff` enforce with 1. JSON envelope versioned; Human is terminal text (never JSON); JUnit per-flow (`testsuite` with per-`flow-id` testcases, escaped details, correct counts) as projection of report authority. `inspect --bodies` bounded (`--max-body-bytes` 64 KiB, `--bodies-base64` opt-in): UTF-8 text when valid, else length+digest, truncation with counts, stored (redacted) bodies only.
- Docs: `cli.md` (exit codes, route, JUnit, inspect), `eggress-routing.md` (pproxy-compat grammar only), README quickstart routes.

## Tests

- HTTP unit (3): two-hop construction, malformed credential-redacted + no fallback, redaction helper.
- CLI subprocess integration (7 in `cli_contracts.rs`): direct validate JSON exit 0 + separation; malformed route exit 2 redacted + JSON agreement; missing fixture exit 3; invalid target exit 2; per-flow JUnit `tests="2"` with stable names; inspect bodies truncation + redaction; Eggress HTTP forward-proxy replay via `--route http://proxy` exit 0 (local deterministic proxy + target, proving CLI routing without fallback).
- Exit 5 (internal) has no deterministic subprocess trigger by design (fallback for bugs); mapping unit-covered and verified via code search (no production `internal` paths hit). All other categories via subprocess.

## Verification

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

All passed. Totals: 8 core + 21 http + 8 store + 7 CLI integration = 44 tests.

## Acceptance

- Same fixture replays direct and via Eggress route from installed CLI.
- Stable process codes; JSON/JUnit parseable contracts; stderr agrees with `failure_class`.
- `inspect` no longer placeholder; bounded, redaction-safe.
- Closure record present.

## Unblocked next plan

C005 becomes ready (C001–C004 closed). No M009–M014 scope pulled in.
