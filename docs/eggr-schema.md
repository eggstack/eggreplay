# `.eggr` session schema

An `.eggr` fixture is a directory containing `manifest.json`, `flows.jsonl`,
and content-addressed `blobs/<sha256>`. The manifest is the final publication
marker and records the session schema, tool version, flow count, blob count,
and (in schema 2) a bounded extension registry. Flow records remain schema 1
and contain a semantic request and exactly one response or error outcome.
Schema-1 fixtures remain readable.

Bodies distinguish `absent`, `empty`, and a blob reference with SHA-256 and
length. Headers, query pairs, and trailers preserve order and duplicates.
`required_for_replay` means the reader must understand and apply the extension,
not that the user opted into timing delays. Unknown required extensions and
future session schemas are rejected; there is no generic
ignore-required-extension switch. Schema-2 extensions use confined
single-filename paths, reject symlinks, and are bounded to 16 MiB each and
32 MiB total. Extension payload files are written before the manifest
publication marker. `Session::copy_to` streams and revalidates all blobs while
upgrading/copying a session; it never mutates its source.

## Authored scenarios (M009)

The optional required-when-used `rules` extension contains named finite state
machines. A scenario must be selected explicitly with `serve --scenario`.
Transitions run in fixture order, request predicates are exact, state is
isolated per replay-server instance, and extraction/template evaluation is
bounded and has no access to process or host state. Missing extraction can
abort or skip the transition. Template substitutions insert UTF-8 values
literally; callers must author escaping appropriate to the response content
type. JSON Pointer replacements operate on parsed JSON values. The rules
extension does not replace ordinary flow matching unless an enabled scenario
transition matches.

## Stream events and SSE views (M010)

The required-when-used `stream-events` extension stores bounded JSON metadata
keyed by flow id. It records relative monotonic DATA boundaries, trailers,
clean EOF, or a typed mid-body error and byte offset. It never duplicates DATA
bytes; body blobs remain authoritative. Event capture is limited to 2,048
entries per direction, 4,096 per flow, and 16 MiB serialized metadata per
session. Fixture opening validates offsets, order, schema, and delays; missing,
malformed, unsupported-version, duplicate-flow, or body-length inconsistent
stream metadata fails closed.

Replay is immediate by default. Immediate applies semantic event and terminal
mid-body error behavior with zero added delay; `serve --timing-mode recorded`
reproduces relative delays and `scaled:<factor>` scales them within the
documented bounds. These modes reproduce semantic body event cadence rather
than TCP packet timing. `inspect --sse` remains an independent explicit
inspection option. `replay`, `test`, and fixture `diff` compare ordered
response stream events only when `--compare-stream-events` is given,
cadence only when `--cadence-tolerance-ms <N>` is given (which implies stream
comparison), and derived SSE semantics only when `--compare-sse` or
`--sse-ignore <field>` is given (supported fields:
`data,event,id,retry,comments`). Raw body comparison remains authoritative;
SSE findings never suppress raw-body findings. Candidate response stream
observation is authoritative; candidate request cadence is not recorded because
the candidate path materializes the baseline request into one body.

## Lazy replay (C001)

Replay loading is metadata-bounded and never buffers fixture-wide payloads.
`Session::open_blob` validates digest form, byte bounds, symlink rejection,
and exact file length, returning an opened handle without allocating blob
bytes. Selected responses stream in bounded chunks with incremental
hash/length verification; unselected corrupt blobs never affect load and
selected corrupt replacements fail as integrity errors. `Session::open`
still fully validates at open time; streaming adds TOCTOU protection by
failing on concurrent mutation rather than serving replaced bytes.
