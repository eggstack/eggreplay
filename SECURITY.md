# Security policy

`.eggr` fixtures are sensitive data. EggReplay redacts authorization, proxy
authorization, cookies, and set-cookie headers by default in the semantic
redaction profile; configured query keys and JSON pointer paths are also
supported. Redaction is not a guarantee for opaque binary payloads, so access
to fixture directories must still be restricted.

Fixture readers reject future schemas, malformed or oversized JSONL, invalid
blob names, hash/length mismatches, symlinked blobs, and incomplete manifests.
Diagnostics avoid dumping bodies and route credentials. Report remaining
security issues privately to the maintainers before public disclosure.
