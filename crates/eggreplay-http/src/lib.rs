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

#[cfg(feature = "eggress")]
pub use eggress::EggressDialer;
pub use recording::{HttpError, RecordedRequest, record_request};
pub use regression::{CandidateObservation, RegressionError, execute_candidate};
pub use replay::{ReplayError, ReplayFixture};
