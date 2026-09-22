//! Network adapters and semantic HTTP orchestration.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Direct HTTP is the default feature; routing and server adapters are opt-in.
pub const DEFAULT_MODE: &str = "direct";
