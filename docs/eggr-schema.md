# `.eggr` schema 1

An `.eggr` fixture is a directory containing `manifest.json`, `flows.jsonl`,
and content-addressed `blobs/<sha256>`. The manifest is the final publication
marker and records schema/tool versions, flow count, and blob count. Every
flow contains a semantic request and exactly one response or error outcome.

Bodies distinguish `absent`, `empty`, and a blob reference with SHA-256 and
length. Headers, query pairs, and trailers preserve order and duplicates.
Unknown future schemas are rejected until an explicit migration is added.

## Lazy replay (C001)

Replay loading is metadata-bounded and never buffers fixture-wide payloads.
`Session::open_blob` validates digest form, byte bounds, symlink rejection,
and exact file length, returning an opened handle without allocating blob
bytes. Selected responses stream in bounded chunks with incremental
hash/length verification; unselected corrupt blobs never affect load and
selected corrupt replacements fail as integrity errors. `Session::open`
still fully validates at open time; streaming adds TOCTOU protection by
failing on concurrent mutation rather than serving replaced bytes.
