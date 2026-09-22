# M006 closure — Client Replay and Semantic Regression Diff

Status: closed

## Evidence

- Implementation commit: `63ef65773ed24c88df3b5b474515afa2cd13acf6` plus the
  timing assertion addition in the closure commit.
- `execute_candidate` reconstructs baseline method, path/query, headers, and
  body and remaps only the target base URI. Candidate execution uses the same
  EggFetch native body path and typed error mapping.
- `eggreplay-core::report` is a versioned report authority with stable finding
  ordering for status, headers, trailers, body digest/length, outcome, and
  explicit timing assertions. The CLI only renders this report.
- Sequential scheduling is the v0.1 default and is recorded in reports.

## Verification

```text
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

All passed. Candidate network refusal is classified as a typed outcome and
report JSON is versioned and deterministic.

## Unblocked next plan

M007 is ready. It can expose the report and add optional Eggress routing
without duplicating evaluation or HTTP transport.
