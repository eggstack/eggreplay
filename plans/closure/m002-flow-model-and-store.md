# M002 closure — Canonical Flow Model and `.eggr` Store

Status: closed

## Evidence

- Schema-1 semantic flow/session types are in `eggreplay-core::flow`.
- Ordered duplicate headers, ordered query pairs, trailers, separate absent
  and empty bodies, content-addressed SHA-256 blobs, timestamps, provenance,
  route metadata, annotations, and redaction markers are represented without
  EggFetch/EggServe types.
- `SessionWriter` streams body bytes through a same-filesystem staging file,
  hashes while writing, deduplicates by digest, appends validated JSONL, and
  publishes `manifest.json` only after finalization. Incomplete sibling
  directories are deliberately retained and are not valid sessions.
- `Session::open` bounds JSONL, flow count, body sizes, total referenced bytes,
  validates schema and flow invariants, verifies blob length/hash without
  loading all bodies, and confines blob lookup to validated digest names.
- The store reader iterates JSONL records and loads only requested bodies.

## Verification

```text
cargo fmt --all
cargo test -p eggreplay-core -p eggreplay-store
cargo check --workspace --all-targets --all-features
```

All commands passed. The store unit test proves a streamed binary blob,
content-addressed reopening, and body-layer round trip; the schema uses no
schema-2 reinterpretation.

## Unblocked next plans

M003 and M004 are ready. M005 remains blocked until M004 closes, and later
milestones remain blocked by their declared dependencies.
