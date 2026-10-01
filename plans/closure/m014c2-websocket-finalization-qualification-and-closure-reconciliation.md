# M014-C2 — WebSocket Finalization Qualification and Closure Reconciliation Closure

Status: closed

## Qualifying lineage

Implementation is `cbc9257` (on top of M014-C1 `b188c552`).

Hosted qualification on `cbc9257`:

- CI run
  [36891564494](https://github.com/eggstack/eggreplay/actions/runs/36891564494):
  verify Ubuntu stable, verify Ubuntu Rust 1.89.0, verify macOS stable,
  verify Windows stable, interception Ubuntu/macOS/Windows,
  dependency-boundary, Python bindings (Ubuntu 3.14 stable, Ubuntu 3.11
  1.89.0, macOS 3.11 stable, Windows 3.11 stable), Python abi3
  cross-version — all green.
- Wheel run
  [36891564581](https://github.com/eggstack/eggreplay/actions/runs/36891564581):
  source-distribution plus manylinux/macos/windows wheels and abi3
  interpreter smoke (3.11–3.14) — all green.

Failed run
[36881596131](https://github.com/eggstack/eggreplay/actions/runs/36881596131)
on `b188c552` is retained as history: stable Clippy (1.99.0)
`clippy::assert_is_empty` rejected the pre-existing M014A HAR assertion
before the verify matrix completed. The earlier green M014 umbrella run
`36778923619` on `c71ffd7` predates C1 and is not C1/C2 evidence.

Local verification on `cbc9257` is green:

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo +1.89.0 clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked --no-fail-fast
cargo audit
git diff --check
```

Workspace suite: 379 tests passed, 0 failed. Targeted WebSocket lifecycle
tests (six M014-C1 deterministics + original leading-post-101 gateway +
two new M014-C2 current-thread tests) green, including repeated runs.

## 1. Stable-Clippy blocker (semantics-preserving)

`crates/eggreplay-har/src/lib.rs:2098`
`assert!(!flows[0].redactions.is_empty())` became
`assert_ne!(flows[0].redactions, [] as [RedactionMarker; 0], ...)` per the
1.99 Clippy suggestion, preserving the at-least-one-redaction proof while
printing the value on failure. Two further `assert_is_empty` sites surfaced
under 1.99 stable after the first fix and were corrected the same way
without weakening coverage:

- `crates/eggreplay-cli/tests/m013e_operator.rs` audit file list;
- `crates/eggreplay-intercept/tests/curl_interop.rs` CA DER non-empty.

Stable (1.99.0) and MSRV (1.89.0) Clippy both pass with `-D warnings`.

## 2. Blocking finalization boundary (audit outcome)

`RecordingSession::finish` blocks on a std `Condvar` while a finalizer is
pending. EggServe owns the lifecycle: accepted tunnels are tracked tasks,
`ServerHandle::wait` drains them within the bounded post-shutdown budget
(`min(graceful_shutdown_timeout, 5s)`) and aborts remainders; the gateway
`ConversationCompleter` signals on both `complete()` and `Drop`. Hence
after `shutdown + wait`, `drive()` returns promptly.

To avoid assuming a multi-thread runtime, every runtime-facing async call
site now uses blocking isolation via the new
`eggreplay_http::recording::finish_recording_session` (spawn_blocking) plus
`drain_active_blobs`, documented as already-drained + blocking isolation:

- CLI `record`;
- CLI `serve --record-mode once`;
- CLI append-new recording;
- CLI re-record;
- CLI `proxy-record` (intercept);
- Python `lifecycle.rs::finish_session` (all once/re-record/append modes).

No library helper finalizes a session from async besides these owners and
tests. `RecordingSession::finish` docs now state the async contract
explicitly. The store crate remains tokio-free.

## 3. Bounded shutdown/finalization

Terminal signalling is structurally guaranteed before runtime-facing
finalization given the ordering above plus Drop safety, so the store
primitive stays unbounded by design:

- normal close / abnormal EOF / tunnel cancellation / EggServe
  shutdown-drain timeout / panic-drop / Python waiter cancellation all
  reach `complete()` or `Drop`, releasing `drive()`;
- `drive_with_deadline` remains available for bounded observation;
- `finish` fails closed (101 without transcript) instead of publishing an
  incomplete fixture; no sleeps/polling were added as correctness
  mechanisms.

Single-thread safety is proved by the new current-thread tests: the
executor stays free to run the delayed `complete()` while `finish` waits
on the blocking pool.

## 4. Qualification tests

Kept: all six M014-C1 deterministics plus
`recording_gateway_captures_upgrade_and_leading_post_101_messages`
(updated to the shutdown → wait → drain → blocking-finish ordering).

Added (M014-C2):

- `session_finish_via_blocking_pool_progresses_on_current_thread`
  (current-thread, delayed completer, blocking finish succeeds with
  transcript);
- `session_finish_via_blocking_pool_fails_closed_on_cancelled_conversation`
  (current-thread, Drop without transcript, fail-closed, no hang).

Also covered: CLI ordering via the updated gateway test, Python-pattern
cancellation via the fail-closed test, and HTTP-only fast path via the
unchanged `session_finish_is_unaffected_when_no_websocket_finalizers_are_registered`.

## 5. Support wording

Top-level README baseline no longer says generic `H2` is outside the claim.
It now states: cleartext RFC 6455 WebSocket is the qualified M011 baseline;
WSS interception remains unsupported; outbound H2 record/regression is
experimental opt-in under M014B; inbound H2 serving, H2 MITM, and `h2c` are
unsupported/not qualified; H3 remains unsupported/deferred per ADR 0009;
negotiated WebSocket extensions and wire-frame fidelity remain outside the
claim. The M014B/M014C matrix remains the detailed authority.

## 6. C1 status/evidence correction

The premature C1 closure is amended in
`closure/m014c1-post-m014-closure-and-websocket-finalization.md`: status
closed (qualified via M014-C2), old `c71ffd7` / `36778923619` claim marked
invalid for C1, failed `36881596131` retained, qualifying lineage
`cbc9257` + runs `36891564494` / `36891564581` recorded.

M014 itself remains closed; C2 is a post-closure corrective. Stage 11
remains undefined pending separate research/planning. No future plan is
unblocked by this closure; the registry returns to no open implementation
plan.
