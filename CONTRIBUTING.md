# Contributing

Implement milestones in dependency order. A plan is closed only after its
acceptance criteria, tests, documentation, registry transition, and a closure
record are present. Keep machine-readable command results on stdout and
operational diagnostics on stderr.

Changes must preserve the transport boundaries documented in
[`docs/architecture.md`](docs/architecture.md). Do not replace a missing
Eggstack seam with a second HTTP implementation.
