# eggreplay Python bindings

The package is built from the Rust workspace with maturin. Fixture parsing,
validation, reports, configuration checks, and body integrity remain Rust
authorities.

```python
import eggreplay

fixture = eggreplay.Fixture("fixtures/example.eggr")
for flow in fixture.iter_flows():
    print(flow.id, flow.request.method, flow.request.headers)

with fixture.open_body("flow-id", "response") as body:
    for chunk in body:
        consume(chunk)
```

Header and query fields are ordered pair sequences and preserve duplicates.
Body reads are bounded; `read_all(max_bytes=...)` requires an explicit cap
above its 1 MiB default. Report JSON projections use the Rust serde schema.
