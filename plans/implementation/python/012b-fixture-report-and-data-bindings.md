# M012B — Fixture, Report, and Data Bindings

Status: implemented (hosted qualification pending)
Depends on: M012A
Parent milestone: M012

## Objective

Expose read-only/static EggReplay authorities to Python before adding
long-lived network lifecycle. Keep every decision in Rust and keep large data
lazy/bounded.

## A. Exception hierarchy

Add stable Python exceptions mapped from Rust categories, for example:

- `EggReplayError`;
- `FixtureError`;
- `MatchError`;
- `NetworkError`;
- `ConfigurationError`;
- `RegressionError`.

Where Rust provides structured category/phase/provenance, expose those as
attributes. Redact messages exactly as Rust does.

## B. Fixture/session wrapper

Expose a `Fixture`/read-only session object backed by
`eggreplay_store::Session`.

Required operations:

- open/validate;
- manifest/schema/redaction profile;
- ordered flow iteration;
- flow lookup by id;
- extension presence/metadata;
- bounded WebSocket/scenario/stream metadata inspection.

Do not deserialize the fixture into independent Python dict authority and then
evaluate it there.

## C. Python value shapes

Expose immutable/read-only wrappers or generated value objects for:

- flow/request/response/error;
- body references;
- ordered header entries;
- ordered query pairs;
- route/protocol metadata;
- redaction markers;
- WebSocket conversation/message summaries;
- scenario metadata;
- stream/timing metadata.

Ordered duplicate headers/query values are returned as ordered pair sequences.
A convenience dict may be offered only when explicitly documented as lossy and
must not be used by matcher/regression APIs.

## D. Large body access

Provide a bounded `BodyReader` over validated store blob handles.

Minimum API:

- `length`;
- `sha256`;
- `read(size)`;
- chunk iteration;
- close/context manager.

Do not implement implicit `__bytes__` over arbitrary blobs. Small explicit
`read_all(max_bytes=...)` may be provided with a caller-supplied/default cap.

Symlink/path/digest validation remains owned by `eggreplay-store`.

## E. Configuration bindings

Expose typed Rust-backed configuration builders/enums needed by later plans:

- matcher profile/mode;
- consumption mode;
- record mode;
- redaction config;
- stream timing mode;
- comparison policy;
- route specification;
- WebSocket recording/comparison controls.

Reject invalid combinations in Rust. Python must not recreate validation.

## F. Regression report binding

Expose `RegressionReport` and findings as typed Python wrappers.

Required projections:

- stable attributes;
- `to_dict()`;
- `to_json()`;
- success/finding counts.

Serialization must use the same Rust serde/report schema used by CLI JSON.
No Python re-evaluation.

## G. Identity/lifetime

A Python child view must keep its owning fixture/session alive or copy only
small immutable metadata. No borrowed pointer may outlive Rust ownership.

## Required tests

- open valid/corrupt fixtures;
- schema/required-extension failure mapping;
- duplicate header/query preservation;
- redaction-safe exceptions;
- body reader chunking/EOF/close;
- digest/symlink failure propagation;
- report JSON parity with Rust serialization;
- WebSocket/stream/scenario summary access;
- object lifetime/GC stress;
- no unbounded body materialization.

## Closure

Create `plans/closure/m012b-python-fixture-report-and-data-bindings.md`.
M012C becomes ready only after the static binding contract is stable.

Local implementation is in `crates/eggreplay-python/src/{errors,fixture,report,config}.rs`.
The CPython 3.14 suite passes 13 tests covering valid/corrupt fixtures,
ordered duplicate fields, typed error/report projections, bounded/chunked body
reads and digest/symlink failures, extension summaries, Rust-backed config
validation, object lifetime, and redaction-safe diagnostics. Hosted
qualification is pending; this plan is not closed until the Python CI matrix
and workspace verification pass and the closure record is written.
