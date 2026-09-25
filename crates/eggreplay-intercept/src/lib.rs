//! Optional transport authority for explicit HTTP proxying and TLS
//! interception. This crate is deliberately a leaf: ordinary product crates
//! and the Python extension do not depend on it.
//!
//! M013B provides the safe explicit HTTP/1.1 forward-proxy path:
//!
//! - [`policy`] owns the transport-neutral proxy target policy;
//! - [`headers`] owns proxy-only/hop-by-hop header filtering;
//! - [`tunnel`] owns opaque `CONNECT` relay bounds and operational events;
//! - [`proxy`] owns the `EggServe` explicit-proxy service and listener.
//!
//! M013C adds the dedicated interception CA lifecycle and bounded leaf
//! issuance (no HTTPS interception yet):
//!
//! - [`ca`] owns CA creation/import/inspection/export/rotation support and
//!   file-permission policy in caller-selected directories (never `.eggr`);
//! - [`leaf`] owns the bounded in-memory leaf issuer/cache. Leaves are exact
//!   SAN only, `serverAuth`, and TLS-server capable; M013D terminates
//!   client TLS advertising only `http/1.1` ALPN.
//!
//! M013D adds opt-in HTTPS interception for policy-approved `CONNECT`
//! targets:
//!
//! - [`mitm`] owns the `intercept` policy view, CONNECT/SNI/HTTP authority
//!   coherence, per-target rustls server configurations, the decrypted
//!   origin service, and the caller-owned H1 driver orchestration. Decrypted
//!   requests record through the existing [`RecordingSession`] authority;
//!   TLS failures after `200` close the tunnel with a bounded operational
//!   event and never fabricate a flow.
//!
//! `EggServe` owns inbound H1 parsing/lifecycle and tunnel handoff, `Eggress`
//! owns raw `CONNECT` route establishment, and `EggFetch` owns semantic
//! upstream HTTP/TLS verification. This crate adds no second `Hyper`
//! client/server, SOCKS/CONNECT stack, TLS verifier, or X.509 implementation:
//! generation/signing is `rcgen`'s job, pairing is `eggnet-tls`'s job, and
//! read-only certificate-property checks belong to the maintained
//! `x509-parser` crate.
//!
//! `EggServe` owns inbound H1 parsing/lifecycle and tunnel handoff, `Eggress`
//! owns raw `CONNECT` route establishment, and `EggFetch` owns semantic
//! upstream HTTP/TLS verification. This crate adds no second `Hyper`
//! client/server, SOCKS/CONNECT stack, TLS verifier, or X.509 implementation.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::time::Duration;

use eggserve_server::{
    AdmissionOwnership, H1PolicyOwnership, Http1RequestTargetMode, RuntimeConfig,
};

pub mod ca;
pub mod headers;
pub mod leaf;
pub mod mitm;
pub mod policy;
pub mod policy_file;
pub mod proxy;
pub mod tunnel;

pub use ca::{
    CA_CERT_FILENAME, CA_CLOCK_TOLERANCE, CA_DIR_MODE, CA_FORMAT_VERSION, CA_KEY_FILENAME,
    CA_KEY_MODE, CA_METADATA_FILENAME, CA_PUBLIC_MODE, CaAuthority, CaError, CaMetadata, CaOptions,
    CaOrigin, DEFAULT_CA_COMMON_NAME, DEFAULT_CA_VALIDITY_DAYS, MAX_CA_SUBJECT_CN_CHARS,
    MAX_CA_VALIDITY_DAYS, MAX_METADATA_BYTES, MAX_PEM_FILE_BYTES, MIN_CA_VALIDITY_DAYS,
    export_ca_cert, inspect_ca, repair_ca_permissions,
};

pub use headers::{ProxyFilterOutcome, filter_proxy_headers};
pub use leaf::{
    DEFAULT_LEAF_VALIDITY_HOURS, INTERCEPT_ALPN_HTTP1_1, LeafCertificate, LeafError, LeafIssuer,
    LeafOptions, MAX_LEAF_CACHE_ENTRIES, MAX_LEAF_CN_CHARS, MAX_LEAF_VALIDITY_HOURS,
    MIN_LEAF_VALIDITY_HOURS,
};
pub use mitm::{
    DEFAULT_TLS_HANDSHAKE_TIMEOUT, MAX_CONCURRENT_TLS_HANDSHAKES_UNDER_DEFAULT_LIMITS,
    MITM_PROFILE_ID, MITM_ROUTE_KIND, MitmConfig, MitmError, MitmPolicy,
    build_intercept_server_config, check_authority_coherence, check_http_authority_coherence,
    check_sni_coherence,
};
pub use policy::{
    ConnectAction, HostMatch, MAX_HOST_PATTERN_LEN, MAX_POLICY_RULES, MAX_PORT_SET_SIZE,
    NormalizedHost, NormalizedTarget, PolicyError, PortMatch, RequestKind, Rule, RuleAction,
    TargetPolicy, normalize_authority, normalize_host,
};
pub use policy_file::{
    FileAction, FilePolicy, FileRule, INTERCEPT_POLICY_VERSION, MAX_POLICY_FILE_BYTES,
    PolicyFileError, parse_default_action, policy_from_flags,
};
pub use proxy::{
    BindError, CanonicalProxyTarget, DEFAULT_MAX_PROXY_BODY_BYTES, ExplicitProxy,
    ExplicitProxyConfig, ExplicitProxyHandle, FAILURE_AUTHORITY_MISMATCH, FAILURE_INTERNAL,
    FAILURE_INVALID_AUTHORITY, FAILURE_LIMIT_REACHED, FAILURE_NOT_CONFIGURED,
    FAILURE_POLICY_DENIED, FAILURE_TLS_FAILED, FAILURE_UNSUPPORTED, FAILURE_UPSTREAM_FAILED,
    PROXY_FAILURE_CATEGORIES, ProxyError, ProxyListenerConfig, ProxyRejection, ProxyRoute,
    ProxyStats, ProxyStatsSnapshot, resolve_absolute_target, resolve_connect_target,
    start_explicit_proxy, validate_bind,
};
pub use tunnel::{
    DEFAULT_TUNNEL_CONNECT_TIMEOUT, DEFAULT_TUNNEL_IDLE_TIMEOUT, DEFAULT_TUNNEL_MAX_BYTES,
    DEFAULT_TUNNEL_MAX_CONCURRENT, DEFAULT_TUNNEL_MAX_DURATION, TunnelEvent, TunnelEventLog,
    TunnelLimits, TunnelOutcome, TunnelRelaySummary, relay_tunnel,
};

/// Published transport/TLS dependency baseline qualified by M013B0.
pub mod substrate {
    /// `EggServe` caller-owned HTTP/1 serving API version.
    pub const EGGSERVE_SERVER: &str = "0.3.0";
    /// `EggServe` canonical transport-neutral HTTP primitives version.
    pub const EGGSERVE_PRIMITIVES: &str = "0.2.1";
    /// `Eggress` outbound connector version; only `pproxy-compat` is enabled.
    pub const EGGRESS_OUTBOUND: &str = "1.0.8";
    /// Neutral TLS helper version.
    pub const EGGNET_TLS: &str = "0.2.0";
    /// Minimum direct rustls version in this crate.
    pub const RUSTLS: &str = "0.23.45";
    /// Compatible Tokio rustls integration.
    pub const TOKIO_RUSTLS: &str = "0.26.2";
    /// Qualified pre-1.0 certificate generator.
    pub const RCGEN: &str = "0.13.2";
    /// Maintained read-only X.509 parser used for CA-property validation.
    pub const X509_PARSER: &str = "0.16.0";
    /// Date/time helper for certificate validity windows.
    pub const TIME: &str = "0.3.55";
}

/// M013B `EggServe` runtime profile consumed by the explicit proxy listener.
///
/// This helper centralizes the `EggServe` configuration decisions M013B will
/// rely on. Its fields are public so focused tests can assert them: a future
/// `EggServe` upgrade must not silently change default ownership or accept-form
/// semantics.
#[derive(Debug, Clone)]
pub struct InterceptionProfile {
    /// Address supplied by the caller (loopback by default; non-loopback
    /// requires the [`ProxyListenerConfig`] opt-in safety gate).
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

    /// Construct the canonical profile for an explicitly validated bind.
    ///
    /// Bounds match [`loopback`](Self::loopback); the caller owns bind
    /// safety via [`validate_bind`] (enforced by [`start_explicit_proxy`]).
    pub const fn with_bind(bind: SocketAddr) -> Self {
        Self::loopback(bind)
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
