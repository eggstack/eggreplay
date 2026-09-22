# `.eggr` session schema

An `.eggr` fixture is a directory containing `manifest.json`, `flows.jsonl`,
and content-addressed `blobs/<sha256>`. The manifest is the final publication
marker and records the session schema, tool version, flow count, blob count,
and (in schema 2) a bounded extension registry. Flow records remain schema 1
and contain a semantic request and exactly one response or error outcome.
Schema-1 fixtures remain readable.

Bodies distinguish `absent`, `empty`, and a blob reference with SHA-256 and
length. Headers, query pairs, and trailers preserve order and duplicates.
Unknown required extensions and future session schemas are rejected. Schema-2
extensions use confined single-filename paths, reject symlinks, and are bounded
to 16 MiB each and 32 MiB total. Extension payload files are written before
the manifest publication marker. `Session::copy_to` streams and revalidates
all blobs while upgrading/copying a session; it never mutates its source.

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

## Lazy replay (C001)

Replay loading is metadata-bounded and never buffers fixture-wide payloads.
`Session::open_blob` validates digest form, byte bounds, symlink rejection,
and exact file length, returning an opened handle without allocating blob
bytes. Selected responses stream in bounded chunks with incremental
hash/length verification; unselected corrupt blobs never affect load and
selected corrupt replacements fail as integrity errors. `Session::open`
still fully validates at open time; streaming adds TOCTOU protection by
failing on concurrent mutation rather than serving replaced bytes.
