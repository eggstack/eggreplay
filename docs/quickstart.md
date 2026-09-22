# Quickstart

Create a gateway fixture, inspect and validate it, then serve it offline:

```text
eggreplay record --listen 127.0.0.1:8080 --upstream http://127.0.0.1:9000 --fixture demo.eggr
eggreplay inspect --fixture demo.eggr --output json
eggreplay validate --fixture demo.eggr --output json
eggreplay serve --fixture demo.eggr --listen 127.0.0.1:8080
eggreplay test --fixture demo.eggr --target http://127.0.0.1:9000 --output junit
```

Operational logs are on stderr. JSON and JUnit are result contracts on
stdout. `record` requires explicit `--overwrite` before replacing a fixture.
