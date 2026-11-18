# `chrono` dependency decision (M021E Track C)

Status: **decision recorded, no migration in M021E**. `chrono 0.4.45` is
unmaintained (RUSTSEC-2026-0543) and remains an explicit, reasoned exception in
[`deny.toml`](../deny.toml) until a separately planned change removes it.

- Date: 2026-11-17
- Owner: maintainers
- Review date: 2027-02-01 (or earlier on the trigger in § 8)

## 1. What actually depends on `chrono`

Verified against the tree, not against the milestone text.

| Declaration | Uses `chrono`? | Evidence |
|---|---|---|
| `Cargo.toml:39` (`default-features = false`, `features = ["clock", "serde"]`) | — | workspace declaration |
| `crates/eggreplay-core/Cargo.toml:11` | **no source reference** | `grep -rn chrono crates/eggreplay-core/src` finds only the word "chronology" in a doc comment (`src/flow.rs:192`) |
| `crates/eggreplay-http/Cargo.toml:45` | **no source reference** | `grep -rn chrono crates/eggreplay-http/src` is empty |
| `crates/eggreplay-har/Cargo.toml:12` | yes | `src/lib.rs:565`, `:1648` |
| `crates/eggreplay-intercept/Cargo.toml:29` | yes | `src/ca.rs:72`, `:995` |
| `crates/eggreplay-store` | no | no `chrono` entry in its manifest |

The real call sites are three:

| Site | Current code | Purpose |
|---|---|---|
| `crates/eggreplay-har/src/lib.rs:565` | `chrono::DateTime::parse_from_rfc3339` → `timestamp_millis()` | HAR import: `startedDateTime` → `started_at_ms` |
| `crates/eggreplay-har/src/lib.rs:1648` | `DateTime::<Utc>::from_timestamp_millis` + `to_rfc3339_opts(SecondsFormat::Millis, true)` | HAR export: `started_at_ms` → `YYYY-MM-DDTHH:MM:SS.mmmZ` |
| `crates/eggreplay-intercept/src/ca.rs:995` | `chrono::DateTime::from_timestamp` + `to_rfc3339_opts(SecondsFormat::Secs, true)` | CA `created_at_rfc3339` |

The enabled `clock` and `serde` features are **not exercised**: there is no
`Utc::now()`/`Local` call, and no serialized struct has a chrono-typed field
(`grep -rnE "Utc::now|chrono::serde"` is empty). `clock` is what pulls
`iana-time-zone` into the graph, and `chrono` is its only parent
(`cargo tree -i iana-time-zone`), so removing `chrono` removes that crate too.

### Public surface: the milestone's premise is wrong

No public Rust type exposes `chrono`. The plan expected `OffsetDateTime` in
fixture/report timestamps; the code uses:

- `Flow.started_at_ms: u64` and `Flow.completed_at_ms: Option<u64>`
  (`crates/eggreplay-core/src/flow.rs:170`, `:172`) — the `.eggr` fixture and the
  regression report speak integer milliseconds;
- RFC 3339 `String`s at the HAR boundary (`startedDateTime`, serialized as a
  string, `crates/eggreplay-har/src/lib.rs:219`);
- `time::OffsetDateTime` for CA validity — already, today
  (`crates/eggreplay-intercept/src/ca.rs:469`, `src/leaf.rs:47`, `:192`).

So this is a **three-call-site internal migration**, not a public type
migration. That is the single most important input to the decision.

## 2. Option A — migrate the call sites to `time`

`time` is already in the graph at `0.3.55` and is already a **direct**
dependency of `eggreplay-intercept` (`crates/eggreplay-intercept/Cargo.toml:34`),
reached transitively through `x509-parser`/`rcgen`/`yasna`/`asn1-rs`. The
workspace declares it as `= "0.3"`, `default-features = false`,
`features = ["std"]` (`Cargo.toml:54`).

- **MSRV:** `time 0.3.55` declares `rust-version = 1.88.0`; the workspace is
  `1.89` (`Cargo.toml:16`). Compatible today. Because the requirement is a
  caret `0.3`, a future `time` release could raise its MSRV past 1.89; the
  lockfile pins the working version, and the MSRV lane would catch a bump.
- **Features to add:** `parsing` (HAR import), `formatting` (both exports), and
  `macros` (a `format_description!` for fixed-width output). These are
  feature-only changes to an existing dependency — no new crate in the graph.
- **Formatting:** the well-known `Rfc3339` format preserves the subsecond digits
  that are present (its documented example renders `...50.52Z`), so it will
  **not** reproduce chrono's fixed `.000Z`. The HAR export must keep exactly
  three fractional digits, and the CA export exactly zero, using an explicit
  format description. This is the one place a careless migration changes
  fixture bytes.
- **Parsing:** `OffsetDateTime::parse(raw, &Rfc3339)` covers the HAR grammar,
  but chrono's and `time`'s acceptance sets are not identical at the edges
  (lowercase `t`/`z`, offsets without a colon, leap seconds). The migration must
  pin the accepted grammar with tests rather than assume equivalence.
- **Conversion arithmetic:** chrono's `timestamp_millis()` floors; a naive
  `unix_timestamp_nanos() / 1_000_000` truncates toward zero. HAR times are
  modern and the importer already clamps negatives to zero
  (`crates/eggreplay-har/src/lib.rs:568`), but the conversion should preserve
  floor semantics so a pre-1970, sub-millisecond timestamp cannot shift by 1 ms.
- **Timezone:** nothing needs local time. `time`'s `local-offset` feature stays
  off, as it is today.
- **Range:** `time` defaults to years ±9999 (`large-dates` off), which covers
  every realistic HAR timestamp; chrono's range is wider. Out-of-range input
  must map to the existing `HarError::Invalid`, not a panic.

## 3. Option B — migrate to `jiff`

`jiff` is the maintained successor recommended by the chrono advisory. Latest is
`1.0.0` (`rust-version = 1.85`; the 0.2 line declared `1.70`).

- **New dependency:** minimum closure `jiff` + `jiff-core`. With default
  features it also pulls the timezone machinery (`jiff-tzdb-platform`,
  `windows-link`, `portable-atomic`/`portable-atomic-util` on targets without
  atomics). For UTC-only timestamps we would use
  `default-features = false, features = ["std", "serde"]`, so the tzdb is
  avoidable — but the graph still gains a reviewed dependency and
  `deny.toml` gains its license (`Unlicense OR MIT`, both already allowed).
- **API fit:** `jiff::Timestamp` is an instant with nanosecond precision and
  native RFC 3339 parsing, which matches all three call sites. Fixed-width
  output is a printer setting (`jiff::fmt::temporal::DateTimePrinter` with
  `precision`), not the default `Display`, so the same byte-exactness work as
  `time` is required.
- **Timezone:** `Zoned`/tzdb are unnecessary here; disabling default features
  keeps that cost out of the binary and out of the graph.
- **MSRV:** 1.85 is below the workspace's 1.89. No constraint pressure.

## 4. Comparison

| Criterion | `time` | `jiff` |
|---|---|---|
| New crates in graph | none (already present) | `jiff` + `jiff-core` |
| MSRV | 1.88 (workspace 1.89) | 1.85 |
| Already used for CA types | yes (`OffsetDateTime`) | no |
| RFC 3339 parse/format | yes (`parsing`, `formatting`) | yes (built in) |
| Byte-identical `.000Z` | explicit `format_description!` | explicit printer `precision` |
| Timezone / DST machinery | opt-in, stays off | default-on tzdb, must be disabled |
| License impact on `deny.toml` | none (`MIT OR Apache-2.0`) | none (`Unlicense OR MIT`) |
| Consistency with existing code | one timestamp library in the leaf crate | a second one, replacing `time` in the CA |
| Maintenance signal | maintained | actively developed successor |

## 5. Fixture-JSON and public-API compatibility

- **`.eggr` fixtures:** unaffected. Timestamps are `u64` milliseconds
  (`flow.started_at_ms`), not text. Re-recording the golden fixtures is not
  required, and a diff in them would indicate an accidental format change.
- **HAR export:** must stay `YYYY-MM-DDTHH:MM:SS.mmmZ` (see
  `crates/eggreplay-har/src/lib.rs:1873`, `:1981`, `:2049` and
  `crates/eggreplay-cli/tests/har_migrate.rs:150`). This is the compatibility
  contract to test.
- **CA metadata:** `created_at_rfc3339` must stay second-precision `Z`
  (`crates/eggreplay-intercept/src/ca.rs:993-999`).
- **Public Rust API:** no change; no signature mentions `chrono`.
- **HAR import:** charset of accepted inputs must not narrow silently; a
  previously accepted file turning into `HarError::Invalid` is a regression.

## 6. Test and CI cost

Low, and fully inside existing lanes:

- three call sites in two crates;
- unit tests already assert the exact strings (`eggreplay-har` tests above, the
  CA validity tests in `crates/eggreplay-intercept/src/ca.rs`);
- remove `chrono` from the workspace and four manifests, drop `clock`/`serde`,
  add `parsing`/`formatting`/`macros` to `time`;
- remove the RUSTSEC-2026-0543 exception from `deny.toml` in the same change;
- no new CI lane, no matrix change, no feature-profile change. The MSRV lane
  already compiles the affected crates.

## 7. Recommendation

**Migrate to `time`.** It is already in the graph and already owns the CA
timestamp type; the remaining `chrono` surface is three formatting/parsing call
sites that `time` covers with two feature flags; the workspace MSRV is
compatible; and the public API does not change. `jiff` is the better general
date/time library, but adopting it here would add a dependency and a second
timestamp library in the same crate for no capability this project uses.

## 8. Trigger for a follow-up plan

Create a follow-up plan under `plans/implementation/corrective/` (with its own
closure record) when **any** of these holds:

1. the review date 2027-02-01 is reached and this decision has not been
   re-affirmed;
2. a `chrono` advisory is published that is a vulnerability rather than
   "unmaintained", or a fixed release appears that changes the trade-off;
3. a feature need appears that requires `chrono`'s clock/local-time support
   (nothing in the tree needs it today);
4. a transport or CA change touches the three call sites anyway, making the
   migration marginal.

Until then, RUSTSEC-2026-0543 stays in `deny.toml` with this document as its
removal condition. Do not migrate date handling as part of a dependency-only or
supply-chain change.

## References

- [`deny.toml`](../deny.toml) — the advisory exception and its removal
  condition.
- [`crates/eggreplay-har/src/lib.rs`](../crates/eggreplay-har/src/lib.rs) —
  HAR import/export timestamp handling.
- [`crates/eggreplay-intercept/src/ca.rs`](../crates/eggreplay-intercept/src/ca.rs)
  — CA validity and `created_at_rfc3339`.
- [`plans/implementation/corrective/m020-dependency-advisory-remediation.md`](../plans/implementation/corrective/m020-dependency-advisory-remediation.md)
  — the advisory disposition this decision inherits.
