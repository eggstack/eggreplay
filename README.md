# EggReplay

EggReplay is a Rust-native semantic HTTP interaction recorder, deterministic
offline replay server, and network-regression tool. It stores versioned
`.eggr` directory fixtures and keeps HTTP/TLS/framing ownership in EggFetch and
EggServe. Optional outbound routing is delegated to Eggress.

## Status

The v0.1 implementation is being delivered through the ordered milestones in
[`plans/registry.md`](plans/registry.md). The core and store remain usable
without a network runtime.

## Development

Rust 1.89 is the minimum supported version. Run the fast verification command
from [`AGENTS.md`](AGENTS.md) before submitting changes. See
[`CONTRIBUTING.md`](CONTRIBUTING.md) for plan closure and evidence rules.
