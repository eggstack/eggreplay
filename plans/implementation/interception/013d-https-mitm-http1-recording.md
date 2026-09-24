# M013D — HTTPS MITM HTTP/1.1 Recording

Status: blocked
Depends on: M013C
Parent milestone: M013

## Objective

Add opt-in HTTPS interception for policy-approved CONNECT targets and feed
decrypted HTTP/1.1 requests into the existing EggReplay recording authority.

## A. CONNECT transition

Extend the M013B CONNECT action with `intercept`.

Sequence:

1. parse/normalize CONNECT authority;
2. evaluate ingress + target + intercept policy;
3. acquire/generate the exact-host leaf before acknowledging CONNECT;
4. send 200 Connection Established;
5. perform rustls server handshake with the client;
6. require negotiated ALPN to be absent or `http/1.1`; never advertise h2;
7. construct truthful HTTPS `TlsInfo` and `ConnectionContext`;
8. run EggServe `serve_http1_connection` over the decrypted stream.

A failure before step 4 returns an HTTP proxy error. A TLS failure after 200 is
a tunnel/TLS failure and must not fabricate an HTTP flow.

## B. CONNECT/SNI/HTTP authority coherence

M013 is deliberately strict.

For DNS CONNECT:

- leaf SAN is CONNECT DNS name;
- client SNI, when present, must normalize to the CONNECT name;
- decrypted Host/request authority must normalize to the CONNECT name;
- port semantics must be compatible with the CONNECT authority.

For IP CONNECT:

- leaf SAN is the IP address;
- SNI may be absent;
- if SNI is present it must not redirect authority elsewhere;
- HTTP authority remains the CONNECT IP/port.

Reject cross-origin reuse inside one intercepted tunnel. Do not turn one
CONNECT into a general forward proxy for arbitrary decrypted Host values.

Record bounded mismatch diagnostics without echoing credentials.

## C. HTTP request reconstruction

Decrypted HTTP/1.1 requests are origin-form.

Construct the canonical logical HTTPS target from:

- CONNECT authority;
- request target path/query;
- validated Host authority.

Do not trust `Forwarded`/X-Forwarded headers for origin reconstruction.

Preserve ordered duplicate headers/query values, bodies, trailers, streaming
semantics, and M010 event capture through the existing recorder.

## D. Upstream execution

Execute through EggFetch using normal TLS verification and the existing
Eggress route adapter if configured.

Required invariants:

- client trust of the EggReplay CA has no effect on upstream trust;
- upstream certificate/hostname failures remain failures;
- no `danger_accept_invalid_certs` path is enabled by interception;
- logical origin/SNI remains the real target even through a physical proxy
  route;
- route failure never falls back to direct.

## E. Response/recording behavior

Return upstream response semantics through EggServe's H1 driver and persist the
same canonical flow model as non-intercepted recording.

Existing C002/C003 behavior remains authoritative:

- streaming body staging;
- concurrency;
- redaction before durable publication;
- atomic session finalization;
- semantic error classification.

The CA, leaf private keys, client TLS session secrets, and CONNECT operational
state are not fixture data.

A minimal public annotation may record acquisition mode
`explicit_proxy_mitm` and logical route metadata if it contains no secret/key
information.

## F. Unsupported traffic

Fail explicitly or require an explicit M013B tunnel policy for:

- HTTP/2-only clients;
- QUIC/HTTP/3;
- non-HTTP TLS protocols;
- WebSocket-over-WSS upgrades;
- client-certificate/mTLS applications;
- certificate-pinned clients.

Do not silently downgrade traffic and then claim semantic capture.

## G. Record modes/session ownership

Qualify interception with the recording modes that can truthfully own a live
proxy listener.

At minimum support a normal recording session and re-record/replacement
workflow. Add once/append-new only if the existing mode semantics compose
without tunnel-specific duplication.

If append-new cannot safely acquire arbitrary intercepted misses through the
existing authority, reject that combination and document it rather than
inventing a second updater.

## Required tests

Hermetic local CA + TLS origin:

- successful HTTPS GET/POST;
- streaming request/response;
- trailers;
- duplicate headers/query;
- concurrent intercepted clients;
- SNI match;
- SNI mismatch;
- Host mismatch;
- IP SAN interception;
- upstream untrusted cert failure;
- upstream hostname failure;
- Eggress-routed upstream success;
- routed failure/no fallback;
- malformed TLS after CONNECT;
- H2 ALPN not advertised/accepted;
- non-HTTP TLS fails explicitly;
- WebSocket Upgrade rejected as unsupported;
- redaction sentinel does not appear durably;
- CA/private leaf key absent from fixture/report/log;
- cancellation/shutdown with active decrypted connection;
- session finalization remains valid after TLS/protocol failures.

Use at least one independent local HTTP client capable of proxy CONNECT in
addition to an EggReplay-owned scripted client.

## Closure

Create `plans/closure/m013d-https-mitm-http1-recording.md`.
M013E becomes ready only after MITM recording is stable.
