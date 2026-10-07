# M018 — Post-M017 Corrective: Candidate-Path Error Attribution and H2 Body Diagnostics

Status: **implemented** (local gate green; hosted qualification outstanding)

## Why this milestone exists

After M017 closed, every plan in `plans/registry.md` read `closed` and the
forward queue was empty — but a fresh audit of `architecture/`, `docs/`, and
the source found residual defects that **no plan owned**. M015's closure named
some of them as "candidate fixes for a future milestone"; the M015 next-stage
handoff was itself partly stale, because items 2 and 5 were subsequently closed
by M017 and M016 respectively.

This milestone takes the two tracks that are evidence-backed, dependency-free,
and inside EggReplay's ownership. It deliberately does **not** take the
comparison-authority work, which is a separate milestone (see
"Not in this milestone").

## Track A — candidate-path error attribution

### The defect

Three independent tables mapped the same `eggfetch_core::Error` into the
`ErrorPhase` × `ErrorCategory` taxonomy:

| Site | Introduced | Defect |
|---|---|---|
| `recording.rs` request level | M016 | every `DialErrorKind` collapsed to `Other` |
| `recording.rs` body level | M017 | error discarded, hardcoded `("other", "body")` |
| `regression.rs` candidate path | pre-M016 | **both**, plus a discarded error at the frame site |

The candidate path was the worst of the three. Its `map_error` had **no
`CustomTransport` arm at all**, so a dead Eggress route observed by a candidate
collapsed to `(Other, Other)` even though the recording path attributed it
correctly — and its mid-body frame site discarded the transport error and wrote
`("other", "body")` as a literal, so a candidate cut off by a deadline recorded
identically to one killed by a reset.

This matters more than a cosmetic inconsistency: a candidate observation *is* a
semantic artifact, and a regression report could therefore assert a difference
that was an artifact of which side of the recorder observed the failure.

The defect was self-declared in `architecture/06-regression-and-reporting.md`
("the M016/M017 defect pattern still present in the candidate path") but had no
owner.

### The fix

A new `eggreplay-http` module, `error_classify`, holds the one table.
`recording.rs` and `regression.rs` both call it. `classify_dial_error` and
`classify_body_error` moved there from `recording.rs` rather than being
duplicated, so a fourth table cannot appear without deliberately reimplementing
a shared function.

Stream-event category/phase strings are now derived from the same table via
`body_error_event_fields`, instead of being written as literals.

Sites 2 and 3 in the candidate body loop were left on `other`/`body`
**deliberately**: `Frame::into_data` and `Frame::into_trailers` return the frame
itself, not an error value, so there is no transport evidence to classify. That
is now a comment at the site rather than an accident, matching the recording
path's decision to keep `Other` at the request tee.

### The test that was wrong

`candidate_mid_body_error_returns_partial_observation` asserted
`category == "other"`. The assertion *was* the defect: it pinned the
uninformative value and made it look intentional. It now asserts `protocol` —
what the taxonomy actually says — and explicitly rejects a regression to
`other`.

## Track C — HTTP/2 body diagnostics

### C1 — declared oversized body returns 413

EggServe surfaces an oversized body as **500**. That remains true for a body of
**undeclared** length, where the overrun is only discoverable mid-stream and no
handler can know in advance.

A peer that *states* its own size is decidable from the headers alone, and
answering 500 for it reports a server fault for what is a client fault. The
replay handler now refuses a parseable `content-length` exceeding the operator's
ceiling with 413 before consuming a byte. An absent or malformed
`content-length` is deliberately **not** guessed at and is left to the runtime,
so there is only one body-limit path and the two cannot disagree.

### C2 — the header-list limitation was a misread SETTINGS id; it is retired

M015E recorded that `H2Limits::max_header_list_size` is "enforced inbound but
not advertised: the SETTINGS frame still carries EggServe's own 16384", and
listed it as a hardening item.

Implementation research suggested the opposite — that `inbound.rs:392` applies
the operator's limit, EggServe forwards it to hyper's H2 builder
(`server/connection/driver.rs`), and h2 emits `MaxHeaderListSize` into the
SETTINGS frame — which would make this a documentation defect.

**Both readings were wrong, and the wire settles it.** M015E, and the first M018
pass, read SETTINGS id `0x5` as `SETTINGS_MAX_HEADER_LIST_SIZE`. Per RFC 9113
§6.5.2 — and h2's own codec, `h2::frame::settings::Setting::encode` — `0x5` is
`SETTINGS_MAX_FRAME_SIZE`; the header-list bound is `0x6`. The 16384 that looked
like a stubborn default is hyper's default max **frame** size, a limit EggReplay
never configures and never claimed to.

Read at the correct id, EggReplay advertises the operator's exact value, and it
tracks `H2Limits` rather than sitting at a constant. So there is no upstream seam
and no wiring fault: `H2Limits::apply` was always correct, and the limitation was
a measurement error in the test that claimed to prove it.

M018 retires the limitation, corrects the test that encoded it (it asserted the
*wrong* direction — `advertised > 1024`), and replaces it with one that pins the
real property in both directions. The lesson is recorded in the closure record:
a "confirmed" limitation deserves a check on what the confirming code was
actually reading.

## Non-goals, held

- **No change to the taxonomy.** `eggreplay-core::error` is untouched; this
  milestone only stops the layers from disagreeing about how to use it.
- **No synthesized statuses.** A declared oversized body is refused; an
  undeclared one is still the runtime's call.
- **No change to `H2Limits::apply`**, which is correct and is where the
  operator's value already flows.
- **No comparison-authority work.** See below.
- **No change to the `immediate_reproduces_terminal_error_without_delay`
  test**, which asserts `elapsed < 30ms` and fails under parallel load. It is
  load-sensitive and pre-existing; it passes in isolation both with and without
  this milestone. Recorded rather than silently loosened.

## Not in this milestone

Two audited items are **not** taken here, on purpose:

1. **Comparison-level `date` normalization**, the unwired `TimingAssertion`
   authority, and absent request-direction stream comparison. These are the
   comparison authority's gaps, not the transport-adapter gaps this milestone
   addresses, and they change what a regression report asserts. Bundling them
   would put a semantic change to the diff authority inside a corrective whose
   subject is error attribution — the same thing M017's closure warned against
   when it deferred cross-direction ordering as "a genuine canonical milestone
   — schema v2, additive field, cross-cutting consumers". They warrant their
   own milestone.
2. **Whether replay should reproduce the downstream client experience** for an
   un-terminated bidirectional gRPC call, rather than applying the recorded
   upstream truncation. `docs/grpc-and-faults.md` records this as "an open
   question, tracked separately". It is a maintainer decision about correct
   replay semantics, not a defect, and M018 does not decide it.

## Definition of done

- Implementation, tests, documentation, and a closure record under
  `plans/closure/`.
- Full local gate green: `fmt`, `check --all-targets --all-features`,
  `clippy -D warnings`, `cargo test --workspace --all-features`.
- Feature-boundary profiles re-checked on both sides of each boundary, per the
  M015E lesson that `--all-features` cannot validate an opt-in.
- Hosted CI green across all fourteen jobs. **Until then this milestone is
  `implemented`, not `closed`.**