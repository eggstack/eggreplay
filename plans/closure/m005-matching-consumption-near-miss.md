# M005 closure — Matching, Consumption, and Near-Miss Diagnostics

Status: closed

## Evidence

- Implementation commit: `63ef65773ed24c88df3b5b474515afa2cd13acf6` plus the
  consumption correction in the closure commit.
- `eggreplay-core::matching` is the shared authority used by offline replay
  and available to regression selection. It has explicit strict/practical
  profiles, ordered multi-value normalization, exact/text/semantic-JSON body
  modes, configured JSON-pointer ignores, and bounded diagnostic work.
- Consumption state is replay-session-local and supports once, repeat-last,
  and unlimited modes. Fixture records are immutable.
- Near misses expose stable dimensions and costs, with redaction-safe summaries;
  ranking never upgrades a mismatch to a match.

## Verification

```text
cargo test -p eggreplay-core
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

All passed, including duplicate-header/query matching, semantic JSON ordering
and ignored paths, body mismatch, repeated-call exhaustion, and bounded
near-miss tests.

## Unblocked next plan

M006 is ready. It can invoke the matcher/report contracts without depending on
EggServe runtime types.
