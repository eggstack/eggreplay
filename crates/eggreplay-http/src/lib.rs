//! Network adapters and semantic HTTP orchestration.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Direct HTTP is the default feature; routing and server adapters are opt-in.
pub const DEFAULT_MODE: &str = "direct";

#[cfg(feature = "eggress")]
pub mod eggress;
#[cfg(feature = "grpc")]
pub mod grpc;
#[cfg(feature = "h2")]
pub mod h2;
// The inbound serving policy is a server concern: it composes an EggServe
// runtime, so it exists in exactly the profiles that have one. The `eggserve`
// feature carries `eggserve-server`; `h2-inbound` and `h2-inbound-tls` are
// built on top of it. Leaving this module ungated was a real M015B regression:
// the `direct` profile pulls no EggServe at all and stopped compiling. The
// `direct`, `eggress`, `websocket`, `h2`, `grpc`, and `eggress`-only profiles
// must all keep building without it.
mod error_classify;
#[cfg(feature = "eggserve")]
pub mod inbound;
pub mod recording;
pub mod regression;
pub mod replay;
#[cfg(feature = "websocket")]
pub mod websocket;

#[cfg(feature = "eggress")]
pub use eggress::{EggressDialer, parse_route, physical_route_for, redact_route_credentials};
#[cfg(feature = "eggserve")]
pub use inbound::{H2Limits, InboundProtocol, InboundProtocolDescription, InboundServingError};
pub use recording::{HttpError, RecordedRequest, record_request, record_request_with_session};
#[cfg(feature = "websocket")]
pub use regression::compare_websocket_candidate;
pub use regression::{CandidateObservation, RegressionError, execute_candidate};
pub use replay::{ReplayError, ReplayFixture};
