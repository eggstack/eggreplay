# M018 — Candidate-Path Error Attribution and H2 Body Diagnostics Closure

Status: **closed** — local gate green and hosted qualification complete on
qualifying revision `17f40229f76d383a019eae34b312e5c7f1dca595`, hosted run
`37644212037` (all 14 jobs green).

Plan: `plans/implementation/corrective/m018-candidate-error-attribution-and-h2-body-diagnostics.md`

## The defect this milestone found

An audit of `architecture/`, `docs/`, and the source — prompted by the forward
queue being empty with every plan reading `closed` — found that **three**
independent tables mapped the same `eggfetch_core::Error` into the
`ErrorPhase` × `ErrorCategory` taxonomy:

| Site | Introduced | Defect |
|---|---|---|
| `recording.rs` request level | M016 | every `DialErrorKind` collapsed to `Other` |
| `recording.rs` body level | M017 | error discarded, hardcoded `("other", "body")` |
| `regression.rs` candidate path | pre-M016 | **both**, plus a discarded error at the frame site |

The candidate path had no `CustomTransport` arm at all. A dead Eggress route
observed by a candidate collapsed to `(Other, Other)` while the recording path
attributed the same failure correctly, and its mid-body site discarded the
transport error and wrote `("other", "body")` as a literal — so a candidate cut
off by a deadline recorded identically to one killed by a reset.

That mattered because a candidate observation is a semantic artifact: a
regression report could assert a difference that was only an artifact of which
side of the recorder observed the failure.

`architecture/06-regression-and-reporting.md` had already named this
("the M016/M017 defect pattern still present in the candidate path"). It had no
owner, and no plan claimed it.

## What changed

| File | Change |
|---|---|
| `crates/eggreplay-http/src/error_classify.rs` | **new**: the one table. `map_fetch_error`, `classify_dial_error`, `classify_body_error` moved here from `recording.rs`; plus `body_error_event_fields` for stream-event strings, and 3 tests |
| `crates/eggreplay-http/src/lib.rs` | register the module |
| `crates/eggreplay-http/src/recording.rs` | **fix**: delete the local table, import the shared one (−106 lines) |
| `crates/eggreplay-http/src/regression.rs` | **fix**: `map_error` delegates to the shared table; the mid-body frame site classifies instead of discarding; 2 tests |
| `crates/eggreplay-http/src/replay.rs` | **fix**: `declared_body_length` + 413 for a declared oversized body |
| `crates/eggreplay-http/tests/h2_hardening.rs` | **add/correct**: the header-list bound is advertised as the operator's own; declared-oversized 413 (15 → 17) |
| `docs/http2-support.md`, plan, registry | support-matrix and limitation corrections |

### Two candidate body sites stay `other`/`body` on purpose

`Frame::into_data` and `Frame::into_trailers` return the **frame itself**, not
an error value, so there is no transport evidence to classify. That is now a
comment at each site rather than an accident, matching the recording path's
decision to keep `Other` at the request tee where the error is an opaque inbound
EggServe body error.

### The test that was the defect

`candidate_mid_body_error_returns_partial_observation` asserted
`category == "other"`. The assertion *was* the defect — it pinned the
uninformative value and made it look intentional. It now asserts `protocol`
(what the taxonomy actually says) and explicitly rejects a regression to
`other`.

## Track C2: a limitation that was a misread SETTINGS id

M015E recorded that `H2Limits::max_header_list_size` is "enforced inbound but
not advertised: the SETTINGS frame still carries EggServe's own 16384", and
offered it as a hardening item.

Tracing the adopted dependency line suggested that claim was **wrong**:
`inbound.rs:392` applies the operator's limit to EggServe's `Http2Config`,
`eggserve-core`'s driver passes it to hyper's H2 builder, hyper stores it on the
h2 builder, and h2 emits `MaxHeaderListSize` (SETTINGS id `0x5`) into the frame
it sends. On that reading, the limitation was a documentation bug.

**A wire-level test was added to check it, and the first run "confirmed" the
claim — the advertised value really was 16384.** That confirmation was itself
the bug. Per RFC 9113 §6.5.2, and h2's own codec
(`h2::frame::settings::Setting::encode`), id `0x5` is
`SETTINGS_MAX_FRAME_SIZE`; `SETTINGS_MAX_HEADER_LIST_SIZE` is `0x6`. The 16384
is hyper's default max **frame** size — a limit EggReplay never configures and
never claimed to advertise.

Re-probed at the correct id, through both EggServe directly and
`H2Limits::apply`, the operator's exact value is on the wire and tracks the
setting:

```text
configured=49152  advertised 0x5(MaxFrameSize)=16384  0x6(MaxHeaderListSize)=49152
configured=204800 advertised 0x5(MaxFrameSize)=16384  0x6(MaxHeaderListSize)=204800
```

So there is no upstream seam and no wiring fault. `H2Limits::apply` was always
correct; the limitation was a measurement error in the test that claimed to
prove it. M018 retires it in `docs/http2-support.md`, corrects
`an_oversized_header_list_is_refused_and_the_listener_survives` (which asserted
the *wrong* direction — `advertised > 1024`), and replaces the evidence test
with `the_advertised_header_list_bound_is_the_operators_own`, which pins the real
property in both directions: the advertised bound equals the operator's value,
and is not a runtime default.

**The lesson, which is why this is recorded at length:** a "wire-level
confirmation" is only as good as the id it reads. M015E misread `0x5`; M018's
first correction inherited the same wrong id from the test it was auditing,
trusted the matching constant, and then wrote an upstream-seam explanation on top
of it — a plausible story that the measurement never supported. Re-deriving the
expected value from the spec and the codec, rather than from the test's own
constant, is what caught it. The M015E note is corrected in place rather than
left to contradict this record.

## Local gate

```text
cargo fmt --all -- --check                                            OK
cargo check --workspace --all-targets --all-features --locked         OK
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings  OK
cargo test --workspace --all-features --locked --no-fail-fast   492 passed / 0 failed
```

**492 passed, 0 failed**, across 26 suites — 483 at M017 closure plus 9 new
tests. Notably, the two pre-existing `curl_interop` local failures **did not
reproduce** in this run; they remain recorded in `docs/testing.md` as
machine-specific and proven so by four green hosted `verify` jobs. This is a
clean local run, not a claim that those two are fixed.

### Suites

| Suite | Passed |
|---|---|
| `eggreplay-http` unit | 85 (was 80) |
| `h2_hardening` | 17 (was 15) |
| `eggreplay-core` unit | 39 |
| `eggreplay-store` unit | 21 |
| `eggreplay-intercept` unit | 64 |
| `h2_inbound_serving` | 30 |
| `mitm` | 24 |
| `h2_end_to_end` | 23 |
| `proxy_policy` | 23 |
| `grpc_integration` | 16 |
| `h2_qualification` | 16 |
| `v01_qualification` | 16 |
| `h2_inbound_serving` (CLI) | 8 |
| `m013e_operator` | 8 |
| `eggreplay-cli` unit | 12 |
| `cli_contracts` | 12 |
| `substrate` | 12 |
| `ca_leaf` | 11 |
| `har_migrate` | 11 |
| `eggreplay-har` unit | 7 |
| `scenario_faults` | 7 |
| `hardening` | 7 |
| `resource_bounds` | 9 |
| `tls_shutdown_isolation` | 9 |
| `m013e_proxy_stats` | 3 |
| **Total** | **492** |

## Boundary lanes

Reproduced locally, per `.skills/verification-qualification.md`:

- **`dependency-boundary`** — `direct` graph contains no `tungstenite`/`base64`;
  `eggreplay-core` pulls no `hyper`/`tokio`/`h2`/`eggfetch`/`eggserve`.
  Both clean.
- **`protocol-boundary`** — every `eggreplay-http` feature profile compiled
  individually with `--all-targets`, on both sides of each boundary:
  `no-default-features`, `direct`, `eggserve`, `websocket`, `h2`, `eggress`,
  `grpc`, `h2-inbound`, `h2-inbound-tls`. All green.

The per-profile sweep is the M015E lesson applied: `--all-features` structurally
cannot validate an opt-in, and M015B broke the `direct` profile for four commits
because only a per-profile build exposed it. `error_classify` is ungated, so it
must build in every profile — including `direct`, which is why the sweep was run
rather than assumed.

Not reproduced locally: `python-bindings` and `python-abi3-cross-version`. No
Rust change here touches `eggreplay-python`, and its edge is unchanged, but those
lanes were **not** verified locally. Hosted CI is the gate, and it has now run —
see below.

## Hosted qualification

Qualifying revision `17f40229f76d383a019eae34b312e5c7f1dca595`, hosted run
`37644212037`, **all 14 jobs green**:

| Job | Conclusion |
|---|---|
| `verify (ubuntu-latest, stable)` | success |
| `verify (ubuntu-latest, 1.89.0)` | success |
| `verify (macos-latest, stable)` | success |
| `verify (windows-latest, stable)` | success |
| `interception (ubuntu-latest)` | success |
| `interception (macos-latest)` | success |
| `interception (windows-latest)` | success |
| `python-bindings (ubuntu-latest, 3.11, 1.89.0)` | success |
| `python-bindings (ubuntu-latest, 3.14, stable)` | success |
| `python-bindings (macos-latest, 3.11, stable)` | success |
| `python-bindings (windows-latest, 3.11, stable)` | success |
| `python-abi3-cross-version` | success |
| `dependency-boundary` | success |
| `protocol-boundary` | success |

This supplies the evidence the local gate could not: the two Python lanes, all
three Windows and macOS platforms, the MSRV lane, and both boundary lanes as CI
executes them rather than as they were reproduced here. The milestone closes on
this run, not on the presence of source.

## Known limitations

- The advertised header-list limitation is **retired**, not outstanding — see
  Track C2. It was a misread SETTINGS id in M015E, and there is no upstream seam.
- **An undeclared oversized body still ends as a runtime refusal (500).** Only
  a *declared* oversized length is decided by EggReplay, from the headers. A
  malformed `content-length` is deliberately left to the runtime so there is
  exactly one body-limit path.
- **A pre-existing load-sensitive flake remains**:
  `replay::tests::immediate_reproduces_terminal_error_without_delay` asserts
  `elapsed < 30ms` and failed at 35ms under parallel load. It passes in
  isolation both with and without this milestone's changes, and `replay.rs` is
  not in this milestone's diff. Recorded rather than silently loosened. It is a
  distinct third instance of the pattern M016 root-caused for the WebSocket
  case: a wall-clock assertion in a unit test is a load-sensitive test.
- The two `curl_interop` local failures did not reproduce here but remain open
  as machine-specific.

## Deliberately not in this milestone

- **Comparison-level `date` normalization**, the unwired `TimingAssertion`
  authority, and absent request-direction stream comparison. These change what
  a regression report *asserts*, which is a different subject from the
  transport-adapter error attribution fixed here. Request-direction ordering is
  additionally schema-v2 canonical work that M017's closure explicitly ruled
  out of bundling into a corrective. They need their own milestone.
- **Whether replay should reproduce the downstream client experience** of an
  un-terminated bidirectional gRPC call instead of applying the recorded
  upstream truncation. `docs/grpc-and-faults.md` records this as "an open
  question, tracked separately". It is a maintainer decision about correct
  replay semantics, not a defect.

## Invariants this milestone pins

- **The recording path and the candidate path classify the same failure
  identically.** Stated as a property test, so it keeps holding if the shared
  table is ever split again.
- **A route failure is attributed at both layers**, never collapsed to `Other`.
- **A deadline is distinguishable from a reset** at both layers, so an
  un-terminated call is legible in a candidate fixture as well as a recorded one.
- **A stream-event category derives from the table**, not from a literal, so an
  event cannot claim `other` for a failure the taxonomy can name.
- **Only a declared oversized body is refused by EggReplay.** An undeclared or
  malformed length is the runtime's decision.