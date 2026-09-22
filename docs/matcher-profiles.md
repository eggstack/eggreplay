# Matcher profiles

`strict` compares scheme, authority, path, ordered multi-value query pairs,
headers, and exact body bytes. `practical` ignores only the explicitly named
volatile headers (`date`, `user-agent`, and `x-request-id` by default); it does
not silently broaden body or route matching.

Body modes are exact bytes, exact UTF-8 text, and semantic JSON with explicit
JSON pointer ignores. Near misses are bounded diagnostics, never a fallback
match. Consumption state is per replay session and supports `once`,
`repeat-last`, and `unlimited`.
