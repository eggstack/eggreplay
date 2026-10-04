# M016 — Post-M015 Corrective: Bounded Outbound Time, Route Error Attribution, and the WebSocket Shutdown Race

Status: closed (qualifying hosted run 37235015972 on b5be2d1, all 14 jobs green)

## Scope

| File | Change |
|---|---|
| `crates/eggreplay-cli/src/main.rs` | **feature**: `--timeout-secs` on `record`, `replay`, `test`, `serve`; `outbound_timeout` in the JSON status payloads; `required_upstream` helper |
| `crates/eggreplay-http/src/recording.rs` | **fix**: `FetchError::CustomTransport` arm in `map_fetch_error`; `drive_with_deadline` early-timeout contract; `await_websocket_conversations`; 101 test waits before shutdown; new `shutdown_during_a_live_websocket_fails_closed`; new `caller_supplied_transport_failures_are_attributed`; strengthened `conversation_completer_drive_with_deadline_observations` |
| `crates/eggreplay-http/tests/h2_hardening.rs` | assertion updated: a dead route is now `Unreachable`/`Connect` |
| `docs/cli.md`, `plans/closure/m015e-…`, `plans/closure/m015-…` | documentation and supersession annotations |

Three defects from the M015E and M015 closures, plus one found during hosted
qualification. The first two turned out, on investigation, to be larger than the
finding stated.

## 1. The CLI had no request timeout at all

M015E recorded the narrower form: `Timeout::from_secs` omits `total`, so a
stall *before* the response starts is unbounded. Investigating that surfaced
the real shape of the problem — `crates/eggreplay-cli/src/main.rs` never
called `ClientBuilder::timeout` at all, and `eggfetch_core::Timeout::default()`
has every field `None`. **Every network-capable CLI command was unbounded.**

`--timeout-secs <N>` now populates `pool`, `connect`, `write`, `read`, **and**
`total`. The `total` field is the one that matters, and the doc comment says so:
the per-phase `read` budget is "time between response body chunks", so it only
begins once the response has. An origin that accepts a connection and says
nothing is bounded only by `total`.

Verified end to end against a stalling origin that accepts and never replies:

```text
$ curl http://127.0.0.1:18717/stall     # --timeout-secs 3
502 Bad Gateway
=== elapsed: 3s ===
```

and the recorded flow carries the outcome:

```json
{"path": "/stall", "outcome": {"error": "Timeout"}}
```

**Unset is the default, deliberately.** A new deadline would change the
behaviour of every existing invocation, which is the same rule
`--outbound-version auto` follows. Verified: with no flag, the status payload
reports `{"bounded": false, "total_secs": null}`.

**A correction to the M016 plan.** The plan's original acceptance criterion
said a timeout should terminate with "exit code 4". That is wrong, and the
implementation does not do it. Exit 4 is the `runtime` failure class; a
timed-out *transaction* is recorded as a flow outcome, and the command itself
succeeds, because a gateway that records "the origin timed out" did its job.
The recorded outcome is more useful to a caller than an exit code. The plan
was corrected before closure rather than the code bent to match it.

`--timeout-secs` is rejected at the parse boundary for `0` and negatives
(`clap::value_parser!(u64).range(1..)`), so `TimeoutArgs::resolve` is
infallible and has no error case to model.

## 2. Every route failure was categorised `Other`

`map_fetch_error` had **no `CustomTransport` arm at all** — and
`CustomTransport` is exactly what the Eggress dialer returns. All five
`DialErrorKind` values fell through to `(Other, Other)`. The consequence is
worth stating plainly: **`ErrorCategory::ConnectionRefused` was unreachable
anywhere in the product.** It was declared in `eggreplay-core` and produced by
no code path.

| `DialErrorKind` | `ErrorCategory` | `ErrorPhase` |
|---|---|---|
| `Connection` | `Unreachable` | `Connect` |
| `Timeout` | `Timeout` | `Timeout` |
| `Authentication` | `Policy` | `Connect` |
| `Rejected` | `Policy` | `Policy` |
| `Other` | `Other` | `Other` |

`Unreachable` rather than `ConnectionRefused`: EggFetch's typed evidence does
not distinguish refused from unreachable, and inferring a distinction the
transport did not make would be worse than an honest general category. Making
`ConnectionRefused` reachable would require inventing evidence.

The narrower `transport_failure_kind()` accessor was rejected — it collapses
route failures to `Connect` regardless of cause, which is the same loss of
information the finding was about.

`caller_supplied_transport_failures_are_attributed` covers all five kinds,
including the two that map to `Policy`, so a future change cannot quietly
collapse them back to `Other`. The M015E hardening assertion for a dead route
was updated from `Other` to `Unreachable`/`Connect` in the same change, and the
M015E closure is annotated as superseded.

## 3. The WebSocket "flake" was a test racing correct runtime behaviour

`recording_gateway_captures_upgrade_and_leading_post_101_messages` failed about
one run in six under load with `Invalid("WebSocket 101 flow is missing required
conversation metadata")`.

It is not a flaky test. `eggserve-server 0.4.0`'s `tunnel.rs` handles shutdown
by **aborting** the handler task:

```rust
tokio::select! { _ = &mut handler_join => {}, _ = cancel => { handler_join.abort(); ... } }
```

The test completed a clean Close handshake and called `server.shutdown()`
immediately. If the abort lands before the relay has observed the close and
appended its transcript, `ConversationCompleter::drop` signals completion
**without** a transcript, and `RecordingSession::finish` refuses to publish the
fixture.

**The product is right, and it is unchanged.** The relay's own comment says
cancellation "signals via Drop with no recorded transcript, which then fails
closed in `RecordingSession::finish`". Publishing a 101 flow whose transcript
is missing would break replay, so failing closed is correct.

Two changes, both in tests:

1. The 101 test now waits for the conversation to be durably staged —
   `RecordingSession::websocket_conversation_count()`, an existing public
   accessor — via the new bounded `await_websocket_conversations` helper, so
   shutdown order is explicit rather than a race. The wait is bounded and
   fails with a clear message instead of spinning forever.
2. `shutdown_during_a_live_websocket_fails_closed` **pins** the contract
   deliberately: it shuts the server down while a conversation is provably
   live and asserts `finish_recording_session` refuses to publish, matching on
   the validator's message. The behaviour that produced a flake is now a
   guarantee.

### Evidence honesty

The pre-fix race was **not reproduced** under synthetic load in this
milestone. Reverting only the new wait and running the test 30 times with 8
`yes` processes saturating the CPU passed 30/30 both before *and* after the
change. The real trigger is evidently subtler than CPU contention — most
likely the full parallel workspace suite, where the 30s `mitm` and
`h2_qualification` suites run concurrently.

So the causal claim rests on the code path, not on a reproduction. The
ordering is readable in `tunnel.rs` and in the relay's documented `Drop`
behaviour, and the original failure message points at exactly that. The fix
removes the test's dependence on who wins the race rather than trying to lose
it less often.

## 4. A Windows-only flake found during hosted qualification

Not part of the original three, but found while watching this milestone's CI:
the Stage 11 commit `97f0589` (docs-only) failed hosted `verify (windows-latest,
stable)` on `recording::tests::conversation_completer_drive_with_deadline_observations`.

```text
wait must observe the deadline (elapsed=49.8944ms, deadline=50ms)
```

The test asserts `elapsed >= deadline` and observed 49.89ms — 0.1ms short. This
is a real contract violation in the product, not a bad assertion.

`ConversationCompletion::drive_with_deadline` did:

```rust
if timed_out.timed_out() {
    return Err(start.elapsed());
}
```

`Condvar::wait_timeout` may report a timeout *slightly before* the deadline
actually elapses — Windows' coarse system timer makes a 50ms wait come back
flagged as timed out at 49.9ms. Returning `Err(elapsed)` on that signal hands
back an elapsed time below the deadline, so a caller checking
`elapsed >= deadline` — which is the natural reading of this function's
contract — sees a spurious early timeout.

The fix treats `timed_out()` as a hint rather than proof and re-reads the clock
at the top of the loop, which is the only place that decides the deadline was
reached:

```rust
if timed_out.timed_out() {
    continue;   // top of loop re-checks `elapsed >= deadline`
}
```

No busy-wait risk: each iteration calls `wait_timeout(remaining)` with a
monotonically shrinking `remaining`, so it always blocks. Verified with an
injected-early-signal harness (0/1/5/20 synthetic early timeouts) — the
contract holds in every case.

Worth noting: this is the *second* time the shutdown/deadline interaction in
this area has turned out to be subtler than it looked. That is the argument for
pinning the contract with a test rather than trusting it.

## Local gate

```text
cargo fmt --all -- --check                                    OK
cargo check --workspace --all-targets --all-features --locked OK
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings  OK
cargo test --workspace --all-features --locked --no-fail-fast  479 passed / 2 failed
```

479 = Stage 11's 477 + the two new M016 tests. The 2 failures are
`eggreplay-intercept/tests/curl_interop.rs`
(`curl_plain_http_proxies_and_records`, `curl_https_connect_mitm_records`) —
the same pre-existing, machine-specific pair carried through from M015. They
reproduce at the Stage 11 commit with all M016 changes stashed, neither file is
in this milestone's diff, and they are green on all four hosted `verify` jobs.
They are recorded, not waived as flakiness.

> Counting note: `cargo test` halts on the first failing test binary by
> default, which truncates the run at 371. `--no-fail-fast` is required to see
> the true workspace total. An earlier M015 note on test counts was corrected
> for the same reason.

Per-suite, from the run's own output:

| Suite | Passed | Failed |
|---|---|---|
| `eggreplay-cli` unittests | 12 | 0 |
| `cli_contracts.rs` | 12 | 0 |
| `har_migrate.rs` | 11 | 0 |
| `m013e_operator.rs` | 8 | 0 |
| `m015b_inbound_serving.rs` | 8 | 0 |
| `eggreplay-core` unittests | 37 | 0 |
| `eggreplay-har` unittests | 7 | 0 |
| `eggreplay-http` unittests | 78 | 0 |
| `grpc_integration.rs` | 16 | 0 |
| `h2_end_to_end.rs` | 23 | 0 |
| `h2_hardening.rs` | 15 | 0 |
| `h2_inbound_serving.rs` | 30 | 0 |
| `h2_qualification.rs` | 16 | 0 |
| `scenario_faults.rs` | 7 | 0 |
| `v01_qualification.rs` | 16 | 0 |
| `eggreplay-store` unittests | 64 | 0 |
| `ca_leaf.rs` | 11 | 0 |
| `curl_interop.rs` | 0 | **2** |
| `hardening.rs` | 7 | 0 |
| `m013e_proxy_stats.rs` | 3 | 0 |
| `mitm.rs` | 24 | 0 |
| `proxy_policy.rs` | 23 | 0 |
| `resource_bounds.rs` | 9 | 0 |
| `substrate.rs` | 12 | 0 |
| `tls_shutdown_isolation.rs` | 9 | 0 |
| `eggreplay-intercept` unittests | 21 | 0 |
| **Total** | **479** | **2** |

## Hosted runs

**Qualifying run:** [`37235015972`](https://github.com/eggstack/eggreplay/actions/runs/37235015972)
on `b5be2d1` — **all 14 jobs green**, including `verify (windows-latest, stable)`,
which is the job that proves the section 4 fix.

- `37234665655` on `6c6f395` — superseded before completion when the Windows fix
  landed; its non-green jobs were cancellations, not failures.
- `37230696837` on `97f0589` (Stage 11 docs) — **failed** `verify (windows-latest,
  stable)`. This is the run that surfaced section 4, and it is retained as
  evidence: it is why `97f0589` is not the Stage 11 qualifying revision.
- `37230175365` on `9581748` — the Stage 11 qualifying run, all 14 jobs green.

| Job | Result |
|---|---|
| `verify (ubuntu-latest, stable)` | success |
| `verify (ubuntu-latest, 1.89.0)` — MSRV | success |
| `verify (macos-latest, stable)` | success |
| `verify (windows-latest, stable)` | success |
| `interception (ubuntu-latest)` | success |
| `interception (macos-latest)` | success |
| `interception (windows-latest)` | success |
| `python-bindings (ubuntu-latest, 3.14, stable)` | success |
| `python-bindings (ubuntu-latest, 3.11, 1.89.0)` | success |
| `python-bindings (macos-latest, 3.11, stable)` | success |
| `python-bindings (windows-latest, 3.11, stable)` | success |
| `python-abi3-cross-version` | success |
| `dependency-boundary` | success |
| `protocol-boundary` | success |

The `dependency-boundary` and `protocol-boundary` lanes matter here beyond their
usual role: M016 adds a CLI flag and a store-side `DialError` import, and those
are exactly the lanes that would catch a new edge into a forbidden graph.



## Non-goals, held

- No default timeout — unset preserves existing behaviour.
- No retry or backoff change.
- No change to `ErrorCategory` variants or their serialized values.
- No change to the WebSocket relay, the completion barrier, or the store
  validator. The product behaviour is correct as written and is now pinned.
- The un-terminated bidirectional gRPC deferral from M015D was deliberately left
  alone here, on the view that it needs a terminal-status story of its own.
  **That deferral is closed by M017**, which found no new canonical semantics
  were required; see `closure/m017-unterminated-bidi-grpc.md`.
