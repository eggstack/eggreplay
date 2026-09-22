# v0.1 limitations and non-goals

TLS interception, MITM certificate management, WebSockets, authored scenarios,
record-on-miss/pass-through, streaming timing profiles, Python bindings, HAR
interchange, and broad H2/H3 qualification are outside v0.1. Opaque binary
payloads cannot be semantically redacted; fixtures remain sensitive. Support
claims are limited to the tested HTTP/1.1 direct path and the optional
Eggress TCP dialer.
