# EggReplay contribution instructions

Use the plans in `plans/` as the execution source of truth. Work milestones
in dependency order, update `plans/registry.md` and add a closure record when
each milestone closes. Keep transport ownership in EggFetch, EggServe, and
Eggress; do not add a parallel HTTP stack.

The supported local verification command is:

```text
cargo fmt --all -- --check && cargo check --workspace --all-targets --all-features && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features
```
