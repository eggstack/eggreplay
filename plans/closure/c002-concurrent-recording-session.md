# C002 closure — Concurrent Recording Session Ownership

Status: closed

## Implementation

- `eggreplay-core::Flow::new`: collision-resistant IDs `flow-{ms}-{uuid-simple}` via `uuid v4`; chronology stays in timestamps with JSONL append order as tie-breaker, no schema change. Added `uuid` to core deps.
- `eggreplay-store`: new cloneable `RecordingSession`/`RecordingBodyWriter` backed by `Arc<RecordingInner>` with `Mutex<File>` only for final append, `AtomicUsize/AtomicU64/AtomicBool` for counts/totals/shutdown/active. `begin_blob` never holds flow lock; `append_flow(&self)` validates + checks `max_flows`/`max_total_bytes` + writes under short lock; `shutdown` stops admission; `finish` fails with active sinks rather than racing. `BodyWriter` and `RecordingBodyWriter` Drop clean orphan staging files. Fixed `FlowIter` EOF-at-limit bug (allow exactly `max_flows`). `unique_suffix` now includes atomic counter for concurrency-safe staging names. Crash-safe manifest-last unchanged.
- `eggreplay-http`: added `record_request_with_session` (concurrent, no async lock) with session-aware tee streams; `start_recording_gateway` now takes `RecordingSession` clones, no `Arc<AsyncMutex>` spanning `await`. Fixed server-level `max_request_body_bytes` (default 0 rejects all bodies) by building `RuntimeConfig` with caller bound for both gateway and replay servers. Documented shutdown/drain/finish policy.
- `eggreplay-cli record`: uses `RecordingSession`, drains via `shutdown`+`wait`, spins for active==0, then `finish`.
- Docs: `docs/architecture.md` concurrent session section.

## Tests

- core: `flow_ids_unique_with_identical_timestamps` (200 IDs, same ms).
- store (5 new + 3 existing = 8): `concurrent_blobs_overlap_without_flow_lock` (10×32 KiB chunks with sleeps, second start < first end), `concurrent_append_preserves_manifest_count` (20 threads), `session_limits_enforced_under_concurrency` (per-body + flow-count limits), `aborted_body_leaves_no_orphan_and_finish_succeeds`, `shutdown_stops_admission_and_finish_guards_actives`.
- http recording (5 new + 2 existing = 7): `gateway_overlapping_requests_are_concurrent` (delayed upstream 300 ms, second upstream start < first end, 2 flows unique IDs), `gateway_concurrent_large_upload_download` (256 KiB up / 512 KiB down ×2 concurrent), `cancellation_during_upload_leaves_no_flow_or_orphan` (abort mid-transaction, active==0, finish succeeds), `failed_transaction_leaves_no_orphan_reference` (oversized fail-closed, blob_count 0), `shutdown_finalization_policy`.

## Verification

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

All passed. Totals: 5 core + 14 http (7 replay + 7 recording) + 8 store = 27 tests.

## Acceptance

- No network await holds the flow-log lock; overlapping gateway traffic observed via upstream timings.
- Flow IDs unique under identical timestamps.
- Manifest/blob integrity crash-safe; failed/cancelled leaves no referenced incomplete blob (Drop cleanup + append-only-after-publish).
- Direct single-request behavior unchanged (`record_request` preserved).
- Closure record present.

## Unblocked next plan

C003, C004 remain ready. C005 remains blocked until C001–C004 close. Shared HTTP/store seams merged cleanly; no ownership violations.
