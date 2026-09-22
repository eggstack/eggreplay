# M007 closure — Eggress Routing and Primary CLI

Status: closed

## Evidence

- Implementation commit: `63ef65773ed24c88df3b5b474515afa2cd13acf6` plus the
  report/timing correction in the closure commit.
- `EggressDialer` adapts `OutboundConnector::connect_tcp_detailed` to the
  EggFetch `Dialer` contract. Direct mode remains explicit/default, typed route
  facts are mapped without string parsing, and EggFetch retains Host/SNI/TLS.
- CLI commands exist for record, serve, replay, test, diff, inspect, and
  validate. Result-producing commands support human/JSON; test emits JUnit.
  JSON envelopes include command, schema version, success, failure class,
  warnings, and payload. Operational logs use stderr.
- Record and serve use explicit listen/target/fixture arguments and fixture
  overwrite protection. Environment/config precedence remains reserved for the
  documented configuration layer; secrets are not accepted through semantic
  matcher defaults.

## Verification

```text
cargo run -p eggreplay-cli -- --help
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

All passed. The CLI help contract lists all seven commands and the optional
Eggress feature compiles through the pinned sibling revision.

## Unblocked next plan

M008 is ready. All implementation dependencies are closed and the release
hardening/qualification closure can proceed.
