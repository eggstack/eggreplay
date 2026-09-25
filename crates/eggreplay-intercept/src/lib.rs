//! Optional transport authority for explicit HTTP proxying and TLS
//! interception. This crate is deliberately a leaf: ordinary product crates
//! and the Python extension do not depend on it.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::time::Duration;

use eggserve_server::{
    AdmissionOwnership, H1PolicyOwnership, Http1RequestTargetMode, RuntimeConfig,
};

/// Published transport/TLS dependency baseline qualified by M013B0.
pub mod substrate {
    /// `EggServe` caller-owned HTTP/1 serving API version.
    pub const EGGSERVE_SERVER: &str = "0.3.0";
    /// `EggServe` canonical transport-neutral HTTP primitives version.
    pub const EGGSERVE_PRIMITIVES: &str = "0.2.1";
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

/// M013B `EggServe` runtime profile consumed by the explicit proxy listener.
///
/// This helper centralizes the `EggServe` configuration decisions M013B will
/// rely on. Its fields are public so focused tests can assert them: a future
/// `EggServe` upgrade must not silently change default ownership or accept-form
/// semantics.
#[derive(Debug, Clone)]
pub struct InterceptionProfile {
    /// Loopback address supplied by the caller.
    pub bind: SocketAddr,
    /// Hard request-body ceiling shared by the server and service.
    pub max_request_body_bytes: u64,
    /// Hard request-target length ceiling.
    pub max_request_target_bytes: usize,
    /// Maximum concurrent in-flight service executions.
    pub max_in_flight_requests: usize,
    /// Maximum concurrent active tunnels.
    pub max_active_tunnels: usize,
    /// Maximum concurrent connections.
    pub max_connections: usize,
    /// Total connection lifetime ceiling; `Duration::ZERO` disables it.
    pub connection_total_timeout: Duration,
    /// Keep-alive idle timeout.
    pub keep_alive_idle_timeout: Duration,
    /// Response write no-progress timeout.
    pub response_write_timeout: Duration,
}

impl InterceptionProfile {
    /// Construct the canonical M013B interception profile.
    ///
    /// `bind` must be a caller-supplied loopback address; remote listeners
    /// require policy that M013B has not introduced.
    pub const fn loopback(bind: SocketAddr) -> Self {
        Self {
            bind,
            max_request_body_bytes: 8 * 1024 * 1024,
            max_request_target_bytes: 8 * 1024,
            max_in_flight_requests: 64,
            max_active_tunnels: 64,
            max_connections: 64,
            connection_total_timeout: Duration::from_secs(60),
            keep_alive_idle_timeout: Duration::from_secs(60),
            response_write_timeout: Duration::from_secs(30),
        }
    }

    /// Build the validated `EggServe` [`RuntimeConfig`] this profile describes.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile violates an `EggServe` runtime limit.
    pub fn build_runtime_config(&self) -> Result<RuntimeConfig, eggserve_server::ServerError> {
        RuntimeConfig::builder()
            .bind(self.bind)
            .http1_request_target_mode(Http1RequestTargetMode::OriginOrAbsolute)
            .policy_ownership(H1PolicyOwnership::eggserve_owned())
            .admission_ownership(AdmissionOwnership::eggserve_owned())
            .max_request_body_bytes(self.max_request_body_bytes)
            .max_request_target_bytes(self.max_request_target_bytes)
            .max_in_flight_requests(self.max_in_flight_requests)
            .max_active_tunnels(self.max_active_tunnels)
            .max_connections(self.max_connections)
            .connection_total_timeout(self.connection_total_timeout)
            .keep_alive_idle_timeout(self.keep_alive_idle_timeout)
            .response_write_timeout(self.response_write_timeout)
            .build()
    }
}
