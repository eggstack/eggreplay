# gRPC views and bounded replay faults

The optional gRPC view derives message details from recorded HTTP bodies and
trailers. It does not change the canonical flow or run automatically. gRPC over
HTTP/2 is an experimental opt-in capability; see [HTTP/2 support](http2-support.md).

## Supported gRPC traffic

Unary, server-streaming, client-streaming, and bidirectional calls are
representable. Terminated and un-terminated bidirectional calls are supported
as recordings, replay fixtures, and regression candidates.

For an un-terminated bidirectional call, a client that never half-closes may
produce no terminal `grpc-status`. The fixture records the missing status and a
terminal error event, such as a classified timeout, explaining why the call
stopped. Replay reproduces the recorded upstream truncation, so a live client may
report `Unknown` while a replay client reports `Internal`. Both indicate failure.
This preserves the recorded cause instead of making a truncated call look
complete.

## gRPC views

The `eggreplay_http::grpc` module is behind the `grpc` cargo feature. For flows
with `application/grpc*` content types, it parses the 5-byte length-prefixed
envelope and exposes ordered frames and `grpc-status` or `grpc-message` trailers.

- Compressed frames remain opaque. The view reports the compressed flag and
  does not decompress payloads.
- Protobuf payloads can be decoded to canonical JSON only with a caller-supplied
  `FileDescriptorSet`. Descriptor bytes are bounded to 1 MiB and are never
  fetched from the network.
- Raw body blobs remain authoritative. Views are optional projections, and
  decoded output respects flow redactions.
- Malformed or oversized envelopes return errors rather than being reinterpreted.

## Authored replay faults

`ScenarioResponse.fault` selects a deterministic fault during replay. These
faults use existing EggServe lifecycle controls:

| Fault | Behavior |
|---|---|
| `response_head_delay` | Wait up to 30 seconds before sending response headers. |
| `body_chunk_delay` | Stream the body in bounded chunks with delays between chunks. |
| `close_before_response` | Flush headers, then error the stream and abort the connection without sending scenario body bytes. |
| `close_after_bytes` | Offer a rendered body prefix, then error the stream and abort. The prefix may not reach the client before the abort. |
| `transport_error` | Return the recorded-style `502` response for a semantic upstream error. |

Scenario validation bounds delays to 30 seconds and chunks to 1 through 16 MiB.
Arbitrary packet corruption, TCP flag manipulation, and kernel-level emulation
are not supported.

Cancellation is safe. Dropping a client during a delay leaves the server healthy,
and concurrent requests continue because the state lock is not held during the
sleep.
