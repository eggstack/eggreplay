# Architecture and dependency policy

`eggreplay-core` owns semantic flow types and policies and has no transport,
Tokio, filesystem, or CLI dependency. `eggreplay-store` owns `.eggr` files.
`eggreplay-http` owns adapters and orchestration while EggFetch owns outbound
HTTP/TLS/framing, EggServe owns inbound H1 runtime/framing/lifecycle, and
Eggress owns optional listener-free route establishment. `eggreplay-cli` is a
thin presentation and orchestration layer.

EggFetch 0.2.0 is consumed from crates.io. EggServe's generic runtime and
Eggress's listener-free connector are not yet published as independent
crates, so M001 pins the exact revisions used by v0.1. The removal gate is
that equivalent published crates exist with compatible generic H1, streaming,
trailer, lifecycle, and typed route-failure surfaces; then the Git revisions
must be replaced and the dependency matrix requalified.

No default feature enables Eggress or TLS interception. Direct HTTP is the
default acquisition route.
