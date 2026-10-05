# Matcher profiles

`strict` compares scheme, authority, path, ordered multi-value query pairs,
headers, and exact body bytes. `practical` ignores only the explicitly named
volatile headers (`date`, `user-agent`, and `x-request-id` by default); it does
not silently broaden body or route matching.

Body modes are exact bytes, exact UTF-8 text, and semantic JSON with explicit
JSON pointer ignores. Near misses are bounded diagnostics, never a fallback
match.

## Consumption

Consumption state is per replay session and supports `once`, `repeat-last`, and
`unlimited` in `eggreplay-core`. The `practical` profile still requires
`--matcher-profile practical` on the matching side; it does not imply a
consumption policy.

**The CLI currently only offers `once`.** There is no `--consumption` flag on
`serve`, `replay`, or `test`, so a second identical request against a served
fixture returns `409`. `repeat-last` and `unlimited` are reachable from the
Python bindings only. If you need repeat serving from the CLI today, record
duplicate flows or extend the surface — do not assume the core capability is
exposed.
