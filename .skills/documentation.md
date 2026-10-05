# Documentation

Use when updating `README.md`, `docs/`, `architecture/`, `plans/`, or
`AGENTS.md`.

## Layout and roles

| Location | Audience | Rule |
|---|---|---|
| `README.md` | Prospective user | Capability claims and quickstart. Every support claim must be true of a real build. |
| `docs/` | Operator | Task-oriented contracts: CLI, schema, HAR, protocols, security. |
| `architecture/` | Engineer/reviewer | Bird's-eye `overview.md` plus 12 per-component deep dives. `overview.md` is the **index**. |
| `plans/` | Maintainer | Canonical plans, ADRs, closure records, and the live `registry.md`. |
| `.skills/` | Agent | Task workflows, each ending in an Architecture References section. |

## The rule that catches almost every real error

**Verify a claim against source before writing it, and cite what you checked.**
A plausible doc sentence that contradicts the code is worse than a missing one,
because an operator will act on it.

Worked examples of claims that were wrong in this repo and are now corrected —
check these class of thing before you write:

- Flag *value names*. Clap renders kebab-case from variant names. `--outbound-version`
  takes `auto|http1|http2`, **not** `h1|h2|auto`. Documenting a plausible value
  that fails to parse is an operator-facing bug.
- Names that are *recognised but rejected*. `--inbound h2-tls` parses as a known
  name and then deliberately errors, because TLS requires operator identity
  material. Never list it as a usable choice.
- On-disk layout. `.eggr` extensions are root-level single filenames; there is
  no `extensions/` directory.
- `required_for_replay`. It is **not** uniform — `interop-provenance` is
  `false`; the other three are `true`.
- Enum vocabularies. `ErrorPhase` and `ErrorCategory` each have 9 variants, and
  the axes differ: `protocol` is a *category*, not a phase.
- Line counts. Say whether a count is `src` only or `src` + `tests`; they
  differ by ~19k lines, and the diagram labels imply product source.
- Dependency pins. Say *exactly* which are `=`-pinned and which are caret.
  "Pins every artifact exactly" was false for `eggfetch-core`.
- Counts and job names. Recount. CI job counts, subcommand counts, and per-suite
  test counts drift.

## Drift prevention

Some facts are stated in several places and have drifted apart. When you change
one, update all of them in the same change:

| Fact | Also stated in |
|---|---|
| Support matrix / tiers | `README.md`, `docs/http2-support.md`, `architecture/07-…`, `architecture/overview.md` § 4 |
| Interception matrix | `README.md`, `docs/interception-threat-model.md`, `architecture/08-…` |
| CLI flags | `docs/cli.md`, `architecture/09-…`, the `.eggr` fixture flags too |
| Fixture format | `docs/eggr-schema.md`, `architecture/03-…`, `architecture/overview.md` § 6 |
| Milestone status | `plans/registry.md`, `plans/README.md`, `README.md` status section |
| Verification command | `AGENTS.md`, `.skills/verification-qualification.md`, `docs/testing.md`, `architecture/01-…` |

## Plans and closure

- `plans/registry.md` is the **live execution gate**. Its table and its
  "Current execution gate" prose are both part of the contract; update the prose
  too, because that is what a reader lands on.
- A plan closes only with implementation, tests/evidence, documentation, **and**
  a closure record under `plans/closure/`. Hosted-CI-gated plans stay open until
  the remote run is green.
- Closure records are **immutable audit artifacts**. Do not retro-edit a closed
  plan or record to match later reality; add a corrective milestone.
- Never hardcode a live qualifying SHA outside the canonical qualification
  records. Reference the ledger.
- Status vocabulary is fixed: ready, blocked, active, implemented, closed,
  deferred. Do not invent new statuses.

## Writing rules

- Use relative links; make sure every internal link resolves.
- Mark examples as runnable or clearly illustrative.
- Prefer a table to prose for inventories.
- State negative claims as precisely as positive ones. "Not supported" without
  a reason is a future contradiction.
- Never claim a capability that no test covers, and never remove a limitation
  because it is inconvenient — record it.
- When documentation and code disagree, **the code is correct** and the doc is
  a bug. Fix the doc; do not adjust behaviour to match a stale doc without a
  plan and a closure record.

## AGENTS.md and skills

`AGENTS.md` is an **index**, not a manual. It should be short enough to read
first and point at: the architecture overview, the skills, the verification
command, the live registry, and the hard boundaries. Detail belongs in
`architecture/` (for humans) or `.skills/` (for agents). If `AGENTS.md` starts
growing deep sections, that content probably belongs in a deep dive.

Every skill ends with an **Architecture References** section linking the deep
dive it operates on. When you add a skill, add that section; when you add a deep
dive, link it from `architecture/overview.md` § 8 and from the relevant skill.

## Architecture References

- [`architecture/overview.md`](../architecture/overview.md) — the index, the
  capability map, and the review entry points.
- [`architecture/01-workspace-and-boundaries.md`](../architecture/01-workspace-and-boundaries.md)
  — change discipline, layout, gate rules, review checklist.
- [`architecture/12-testing-and-qualification.md`](../architecture/12-testing-and-qualification.md)
  — what a closure record must contain.
- [`plans/README.md`](../plans/README.md) — planning convention and status
  vocabulary.
