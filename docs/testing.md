# Testing and qualification

The fast gate is the command in `AGENTS.md`. Tests are local-only and use
loopback fixtures. Network behavior is qualified through the delegated
EggFetch/EggServe/Eggress surfaces; core and store tests do not require a
network runtime.
