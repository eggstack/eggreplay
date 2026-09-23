//! Network adapters and semantic HTTP orchestration.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Direct HTTP is the default feature; routing and server adapters are opt-in.
pub const DEFAULT_MODE: &str = "direct";

#[cfg(feature = "eggress")]
pub mod eggress;
pub mod recording;
pub mod regression;
pub mod replay;
#[cfg(feature = "websocket")]
pub mod websocket;

#[cfg(feature = "eggress")]
pub use eggress::{EggressDialer, parse_route, physical_route_for, redact_route_credentials};
pub use recording::{HttpError, RecordedRequest, record_request, record_request_with_session};
#[cfg(feature = "websocket")]
pub use regression::compare_websocket_candidate;
pub use regression::{CandidateObservation, RegressionError, execute_candidate};
pub use replay::{ReplayError, ReplayFixture};
