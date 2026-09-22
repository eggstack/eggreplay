# `.eggr` schema 1

An `.eggr` fixture is a directory containing `manifest.json`, `flows.jsonl`,
and content-addressed `blobs/<sha256>`. The manifest is the final publication
marker and records schema/tool versions, flow count, and blob count. Every
flow contains a semantic request and exactly one response or error outcome.

Bodies distinguish `absent`, `empty`, and a blob reference with SHA-256 and
length. Headers, query pairs, and trailers preserve order and duplicates.
Unknown future schemas are rejected until an explicit migration is added.
