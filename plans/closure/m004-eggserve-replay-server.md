# M004 closure — EggServe Offline Replay Server

Status: closed

## Evidence

- Implementation commit: `63ef65773ed24c88df3b5b474515afa2cd13acf6`.
- `ReplayFixture` validates and loads a complete session before starting,
  materializes only the loaded response stream, and serves it through the
  pinned EggServe H1 runtime.
- Replay state is held per fixture/server instance. Matching, consumption, and
  no-match/exhausted responses are deterministic and do not mutate the stored
  fixture. EggServe remains authoritative for HEAD and body-forbidden status
  normalization.
- The offline path has no EggFetch dependency at runtime; the HTTP crate uses
  EggServe-only code for `ReplayFixture::start`.

## Verification

```text
cargo check -p eggreplay-http --all-features
cargo test --workspace --all-features
```

All passed. Fixture loading rejects incomplete/corrupt sessions through the
store validator, and the replay server uses bounded request-body policy.

## Unblocked next plan

M005 is ready. Its matcher is now the sole authority replacing the temporary
strict selector used to prove this milestone.
