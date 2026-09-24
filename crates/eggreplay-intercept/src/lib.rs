//! Optional transport authority for explicit HTTP proxying and TLS
//! interception. This crate is deliberately a leaf: ordinary product crates
//! and the Python extension do not depend on it.

#![forbid(unsafe_code)]

/// Published transport/TLS dependency baseline qualified by M013A.
pub mod substrate {
    /// `EggServe` caller-owned HTTP/1 serving API version.
    pub const EGGSERVE_SERVER: &str = "0.2.1";
    /// Eggress outbound connector version; only `pproxy-compat` is enabled.
    pub const EGGRESS_OUTBOUND: &str = "1.0.8";
    /// Neutral TLS helper version.
    pub const EGGNET_TLS: &str = "0.2.0";
    /// Minimum direct rustls version in this crate.
    pub const RUSTLS: &str = "0.23.45";
    /// Compatible Tokio rustls integration.
    pub const TOKIO_RUSTLS: &str = "0.26.2";
    /// Qualified pre-1.0 certificate generator.
    pub const RCGEN: &str = "0.13.2";
}
