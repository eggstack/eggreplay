# C002 — Concurrent Recording Session Ownership

Status: closed
Depends on: M001–M008 historical baseline
Corrective gate: v0.1 requalification

## Finding

The recording gateway currently wraps `SessionWriter` in `tokio::sync::Mutex` and holds that lock across the entire awaited `record_request()` transaction. Request upload, upstream execution, response download, and store finalization therefore serialize through one writer guard.

This prevents truthful overlapping gateway traffic and invalidates the original concurrency/timeline intent.

Concurrent requests can also start in the same millisecond while `Flow::new()` currently derives identifiers from millisecond time, so the corrective design must prove flow ID uniqueness under concurrency.

## Objective

Allow independent in-flight recordings to stream request/response blobs concurrently while retaining short, serialized authority only where session metadata/JSONL publication actually requires it.

## Required design

### Session recorder ownership

Refactor the store recording API so body sinks are independent of the flow-index lock.

An acceptable shape is a cloneable `RecordingSession`/sink backed by shared state, or an internally synchronized `SessionWriter`, provided:

- `begin_blob()` does not hold the flow-log mutex while body bytes are written;
- `append_flow()` serializes only the final bounded metadata append/count update;
- finalization cannot race active transactions/body sinks;
- crash-safe manifest-last publication remains unchanged;
- aggregate session/body limits remain enforced across concurrent transactions.

Do not replace the current serialization with an unbounded task/channel that can accumulate whole flows or bodies in memory.

### Async gateway

Remove the Tokio mutex that spans `record_request().await`. Gateway requests should be able to execute upstream simultaneously up to EggServe/EggFetch resource limits.

The final store API should make the safe locking scope structurally obvious.

### Flow identity and chronology

Generate collision-resistant flow IDs independent of Unix-millisecond uniqueness. A UUID or recorder-owned monotonic sequence + random/session component is acceptable.

Do not silently change schema meaning. If a new schema-1 optional/defaulted capture-sequence field is judged necessary, document compatibility and add old-fixture tests; otherwise keep chronology in existing start/completion timestamps and define tie behavior.

### Failure/finalization

Ctrl-C/shutdown must stop admission, drain or cancel active recording tasks according to a documented policy, and finalize the fixture only when no transaction can still append/publish a blob. A failed/cancelled transaction must not leave a manifest claiming a referenced incomplete blob.

## Tests

Add local deterministic tests for:

1. two upstream handlers deliberately overlapping and proving the second starts before the first completes;
2. concurrent large uploads and downloads without global request serialization;
3. unique flow IDs for many requests with identical/synthetic start timestamps where practical;
4. safe concurrent JSONL append and correct manifest flow count;
5. session byte/body limits under concurrency;
6. cancellation during upload and during response streaming;
7. shutdown/finalization with active requests;
8. no orphan referenced blobs after a failed transaction.

## Non-goals

No distributed recorder, multiprocess fixture writer, recorded-timing scheduler, or background database.

## Acceptance

- network awaits do not occur while holding the session flow-log lock;
- overlapping gateway requests are observable in tests;
- every committed flow ID is unique;
- manifest/blob integrity remains crash-safe;
- existing direct single-request behavior is unchanged;
- closure record: `plans/closure/c002-concurrent-recording-session.md`.
