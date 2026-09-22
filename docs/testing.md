# Testing and qualification

The fast gate is the command in `AGENTS.md` (with `--locked` in CI after the
lockfile update). Tests are local-only and use loopback fixtures; no routine
test depends on public Internet. Network behavior is qualified through the
delegated EggFetch/EggServe/Eggress surfaces; core and store tests do not
require a network runtime.

Supported matrix: direct HTTP/1.1 acquisition and EggServe inbound HTTP/1.1
replay; H2/H3 are deferred and untested. CI covers Linux stable + Rust 1.89
MSRV plus stable macOS/Windows; transport qualification with local loopback
runs uniformly, with any platform-specific limits documented rather than
silently claimed. Redaction is persistence-safe for configured headers, query
keys, and JSON/form bodies; opaque binary secret discovery is explicitly
unsupported.
