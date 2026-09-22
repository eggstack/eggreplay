# M003 closure — EggFetch Recording Path

Status: closed

## Evidence

- Implementation commit: `63ef65773ed24c88df3b5b474515afa2cd13acf6`.
- `record_request` uses EggFetch `execute_http_body` with native frame bodies;
  request DATA/trailers are tee-written under backpressure and response DATA/
  trailers are streamed into content-addressed sinks.
- The EggServe gateway uses the pinned generic service/runtime, streams inbound
  request chunks to EggFetch, and streams finalized response blobs back through
  EggServe response streams without re-buffering large bodies.
- Native client construction disables canceled-idle-request retries. Failure
  categories map to stable EggReplay error categories without display-string
  parsing, and persisted records use explicit redaction markers.

## Verification

```text
cargo test -p eggreplay-http
cargo check -p eggreplay-http --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

All passed. Local refusal and conversion tests cover request streaming setup,
repeated headers/query pairs, error mapping, and fail-closed fixture behavior.

## Unblocked next plan

M004 is ready and its EggServe-only offline path can proceed without EggFetch.
