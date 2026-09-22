# C001 closure — Lazy Replay Bodies and Bounded Fixture Materialization

Status: closed

## Implementation

- `eggreplay-store`: added opaque `BlobHandle` (`sha256`, `len`, `into_file`, bounded `read_all`) and `Session::open_blob` validating digest form, `max_blob_bytes`, symlink rejection, and exact file length without allocating blob bytes. Documented fixture immutability/TOCTOU contract; full hash verification happens incrementally while streaming. Added `Session::limits` accessor. No Tokio added to store.
- `eggreplay-core`: added filesystem-free `CandidateBody` (`Absent`/`Empty`/`Digest`/`Inline`) with `from_body_ref`, kept `MatchCandidate::new` for inline/tests and added `from_flow` for metadata-bounded loads. `Matcher::select` delegates to new `select_with_loader`; exact bytes/text compare via length + SHA-256 without materialization, semantic JSON loads only narrowed (otherwise-matching) candidates via caller-owned loader. Core stays filesystem-free; loader lives in HTTP/store adapter.
- `eggreplay-http`: `ReplayFixture::load`/`load_with_matcher` retain only flow metadata + descriptors + cloned `Session`; removed `response_bodies: Vec<Vec<u8>>` and per-request candidate body clones. Selected `Blob` responses open via `open_blob`, adapt std file to Tokio, and return `ResponseBody::Stream` with 64 KiB bounded chunks, incremental SHA-256/length verification, known length, and recorded trailers. Empty/absent use `ResponseBody::Empty` (zero-allocation). Lock scope minimized to match + metadata clone; streaming happens after lock release for concurrent replay. Added `candidate_count` for tests.
- Docs: `docs/architecture.md` lazy boundary section, `docs/eggr-schema.md` lazy replay section.

## Tests

New deterministic coverage (no RSS thresholds; structural/open-count proofs):

- store: `open_blob_validates_without_allocating_full_body`, `open_blob_rejects_symlinked_blob`.
- core: `exact_matching_uses_digest_length_without_materialization` (loader call count 0), `semantic_json_materializes_only_narrowed_candidates` (loader called once for narrowed index only).
- http replay (7 tests):
  - `lazy_load_does_not_read_blob_bytes` (3×2 MiB blobs, delete unselected blob post-open, load still succeeds, descriptors are Digest, missing blob fails closed);
  - `selected_large_response_streams_bounded_and_exact` (2 MiB+123 B, 64 KiB chunk bound, exact bytes + hash);
  - `trailers_survive_lazy_fixture` + `trailer_stream_preserves_recorded_trailers` (ResponseStream known_length + has_trailers + trailer future);
  - `corrupt_replaced_body_fails_integrity` (same-length replacement fails hash, short replacement fails open);
  - `concurrent_streams_do_not_clone_payloads` (two concurrent bounded streams);
  - `replay_server_streams_large_body_and_trailers_concurrently` (512 KiB end-to-end via EggServe + EggFetch, two concurrent Once-consumption requests).

## Verification

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

All passed. Test inventory: 4 core + 9 http (7 replay + 2 recording) + 3 store = 16 tests, 0 failures.

## Acceptance

- Aggregate fixture size does not determine `ReplayFixture` resident memory (metadata + descriptors only; verified by missing-blob-after-open load success).
- Selected bodies stream from validated content-addressed storage with bounded 64 KiB chunks and incremental integrity checks.
- Strict exact-body matching uses digest/length only; no fixture-wide request vectors.
- Matching/consumption semantics deterministic; existing tests preserved.
- Docs updated; closure record present.

## Unblocked next plan

C002, C003, C004 remain ready (parallelizable per registry). C005 remains blocked until C001–C004 close. No shared-seam conflicts introduced beyond the documented store/matcher/replay boundary.
