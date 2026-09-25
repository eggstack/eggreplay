//! Explicit HTTP/1.1 forward-proxy service over `EggServe`.
//!
//! Ownership:
//!
//! - `EggServe` owns inbound H1 parsing, lifecycle, and tunnel handoff. This
//!   service only sees canonical requests and one-shot tunnel capabilities.
//! - `Eggress` (via [`eggreplay_http::EggressDialer`]) owns raw outbound route
//!   establishment for `CONNECT` tunnels. A configured route never falls
//!   back to direct: dial failures fail closed.
//! - `EggFetch` (via [`record_request_with_session`](eggreplay_http::record_request_with_session))
//!   owns semantic upstream HTTP/TLS verification for plain proxy requests.
//! - This crate owns listener safety, target policy, hop-by-hop filtering,
//!   recording integration, and tunnel bounds.
//!
//! Plain absolute-form `http://` requests are recorded as semantic flows.
//! `CONNECT` tunnels are opaque byte relays and never produce flows.
//!
//! # Open-proxy warning
//!
//! The listener binds loopback by default. A non-loopback bind requires an
//! explicit opt-in ([`ProxyListenerConfig::with_remote_opt_in`]) plus an
//! operator-managed ingress allow policy, because M013 provides no proxy
//! authentication: remote exposure without one creates an open forward proxy.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use eggfetch_core::{Client, DialTarget, Dialer};
use eggreplay_core::{DEFAULT_MAX_STRUCTURED_REDACTION_BYTES, PhysicalRoute, RedactionConfig};
use eggreplay_http::{EggressDialer, record_request_with_session, redact_route_credentials};
use eggreplay_store::RecordingSession;
use eggserve_primitives::{
    HeaderBlock, RequestBodyPolicy, Response, ResponseBody, ResponseStream, ResponseStreamError,
    StatusCode, Trailers, request_target::RequestTargetForm,
};
use eggserve_server::{
    Request, Server, ServerHandle, Service, ServiceError, ServiceFuture, tunnel::TunnelCapability,
};
use futures_util::Stream;
use http_body::Frame;
use http_body_util::StreamBody;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::InterceptionProfile;
use crate::headers::filter_proxy_headers;
use crate::policy::{
    ConnectAction, NormalizedHost, NormalizedTarget, TargetPolicy, normalize_authority,
};
use crate::tunnel::{
    TunnelEvent, TunnelEventLog, TunnelLimits, TunnelOutcome, TunnelRelaySummary, relay_tunnel,
};

/// Default ceiling for proxied request bodies (8 MiB, aligned with the
/// interception profile).
pub const DEFAULT_MAX_PROXY_BODY_BYTES: u64 = 8 * 1024 * 1024;
/// Bound applied to dial/route diagnostics (never credentials).
const DIAGNOSTIC_LEN: usize = 128;

/// Failure category: the target policy denied the request.
pub const FAILURE_POLICY_DENIED: &str = "policy_denied";
/// Failure category: the request/CONNECT authority was malformed.
pub const FAILURE_INVALID_AUTHORITY: &str = "invalid_authority";
/// Failure category: the upstream request or dial failed.
pub const FAILURE_UPSTREAM_FAILED: &str = "upstream_failed";
/// Failure category: TLS leaf issuance, configuration, or handshake failed.
pub const FAILURE_TLS_FAILED: &str = "tls_failed";
/// Failure category: CONNECT/SNI/HTTP authority coherence failed.
pub const FAILURE_AUTHORITY_MISMATCH: &str = "authority_mismatch";
/// Failure category: a concurrency or bound limit was reached.
pub const FAILURE_LIMIT_REACHED: &str = "limit_reached";
/// Failure category: interception was requested but no issuer is configured.
pub const FAILURE_NOT_CONFIGURED: &str = "not_configured";
/// Failure category: the client requested an unsupported feature (upgrade,
/// non-HTTP ALPN, non-origin-form inside interception).
pub const FAILURE_UNSUPPORTED: &str = "unsupported";
/// Failure category: an internal tunnel-accept or driver failure.
pub const FAILURE_INTERNAL: &str = "internal";

/// Fixed bounded set of failure categories tracked by [`ProxyStats`].
pub const PROXY_FAILURE_CATEGORIES: &[&str] = &[
    FAILURE_POLICY_DENIED,
    FAILURE_INVALID_AUTHORITY,
    FAILURE_UPSTREAM_FAILED,
    FAILURE_TLS_FAILED,
    FAILURE_AUTHORITY_MISMATCH,
    FAILURE_LIMIT_REACHED,
    FAILURE_NOT_CONFIGURED,
    FAILURE_UNSUPPORTED,
    FAILURE_INTERNAL,
];

/// Operational counters for one explicit-proxy instance (M013E).
///
/// `accepted` counts requests admitted by policy (plain allows plus
/// `CONNECT` resolves to tunnel/intercept); `rejected` counts fail-closed
/// denials before admission; `tunneled`/`intercepted` count accepted
/// `CONNECT` tunnels by action; `flows` counts successfully recorded semantic
/// flows (plain and decrypted); `failures` counts bounded categorized
/// failures. All counters are monotonic and process-local.
pub struct ProxyStats {
    accepted: AtomicU64,
    rejected: AtomicU64,
    tunneled: AtomicU64,
    intercepted: AtomicU64,
    flows: AtomicU64,
    failures: [AtomicU64; PROXY_FAILURE_CATEGORIES.len()],
}

impl std::fmt::Debug for ProxyStats {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProxyStats")
            .field("snapshot", &self.snapshot())
            .finish_non_exhaustive()
    }
}

impl Default for ProxyStats {
    fn default() -> Self {
        Self::new()
    }
}

impl ProxyStats {
    /// Create zeroed counters.
    #[must_use]
    pub fn new() -> Self {
        Self {
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            tunneled: AtomicU64::new(0),
            intercepted: AtomicU64::new(0),
            flows: AtomicU64::new(0),
            failures: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Record one policy admission.
    pub fn record_accept(&self) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one fail-closed rejection.
    pub fn record_reject(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one accepted opaque tunnel.
    pub fn record_tunnel(&self) {
        self.tunneled.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one accepted intercepted tunnel.
    pub fn record_intercept(&self) {
        self.intercepted.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one successfully recorded semantic flow.
    pub fn record_flow(&self) {
        self.flows.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one categorized failure; unknown categories are ignored so the
    /// set stays bounded.
    pub fn record_failure(&self, category: &str) {
        if let Some(index) = PROXY_FAILURE_CATEGORIES
            .iter()
            .position(|known| *known == category)
        {
            self.failures[index].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Point-in-time snapshot for machine-readable output.
    #[must_use]
    pub fn snapshot(&self) -> ProxyStatsSnapshot {
        let mut failures = std::collections::BTreeMap::new();
        for (index, category) in PROXY_FAILURE_CATEGORIES.iter().enumerate() {
            failures.insert(
                (*category).to_owned(),
                self.failures[index].load(Ordering::Relaxed),
            );
        }
        ProxyStatsSnapshot {
            accepted: self.accepted.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            tunneled: self.tunneled.load(Ordering::Relaxed),
            intercepted: self.intercepted.load(Ordering::Relaxed),
            flows: self.flows.load(Ordering::Relaxed),
            failures,
        }
    }
}

/// Serializable snapshot of [`ProxyStats`] (contains counts only, never key
/// material, paths, credentials, or payloads).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProxyStatsSnapshot {
    /// Requests admitted by policy.
    pub accepted: u64,
    /// Requests denied fail-closed before admission.
    pub rejected: u64,
    /// Accepted opaque `CONNECT` tunnels.
    pub tunneled: u64,
    /// Accepted intercepted `CONNECT` tunnels.
    pub intercepted: u64,
    /// Successfully recorded semantic flows.
    pub flows: u64,
    /// Bounded categorized failure counts.
    pub failures: std::collections::BTreeMap<String, u64>,
}

/// Listener safety failure: a non-loopback bind without explicit opt-in.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BindError {
    /// A non-loopback bind was requested without remote opt-in.
    #[error(
        "refusing non-loopback explicit-proxy bind on {addr}: remote listeners require an \
         explicit opt-in and an ingress allow policy; remote exposure without proxy \
         authentication creates an open forward proxy"
    )]
    NonLoopback {
        /// The rejected bind address.
        addr: SocketAddr,
    },
}

/// Validate a proxy listener bind address.
///
/// Loopback binds always pass. Any other bind requires `allow_non_loopback`
/// (see the module-level open-proxy warning).
///
/// # Errors
///
/// Returns [`BindError::NonLoopback`] for non-loopback binds without opt-in.
pub fn validate_bind(bind: SocketAddr, allow_non_loopback: bool) -> Result<(), BindError> {
    if bind.ip().is_loopback() || allow_non_loopback {
        Ok(())
    } else {
        Err(BindError::NonLoopback { addr: bind })
    }
}

/// Listener configuration for [`start_explicit_proxy`].
#[derive(Debug, Clone, Copy)]
pub struct ProxyListenerConfig {
    /// Address to bind.
    pub bind: SocketAddr,
    /// Explicit opt-in for non-loopback binds (open-proxy warning applies).
    pub allow_non_loopback: bool,
}

impl ProxyListenerConfig {
    /// Loopback listener; rejects non-loopback binds.
    ///
    /// # Errors
    ///
    /// Returns [`BindError`] when `bind` is not loopback.
    pub fn loopback(bind: SocketAddr) -> Result<Self, BindError> {
        validate_bind(bind, false)?;
        Ok(Self {
            bind,
            allow_non_loopback: false,
        })
    }

    /// Listener with an explicit remote opt-in. The caller accepts the
    /// open-proxy warning and owns the ingress allow policy.
    #[must_use]
    pub fn with_remote_opt_in(bind: SocketAddr) -> Self {
        Self {
            bind,
            allow_non_loopback: true,
        }
    }

    /// Re-check the bind safety gate.
    ///
    /// # Errors
    ///
    /// Returns [`BindError`] for non-loopback binds without opt-in.
    pub fn validate(&self) -> Result<(), BindError> {
        validate_bind(self.bind, self.allow_non_loopback)
    }
}

/// Explicit-proxy startup failures (bounded, credential-free diagnostics).
#[derive(Debug, Error)]
pub enum ProxyError {
    /// The listener bind failed the safety gate.
    #[error("proxy listener: {0}")]
    Bind(#[from] BindError),
    /// The outbound route expression failed closed (credentials redacted).
    #[error("proxy route: {0}")]
    Route(String),
    /// The `EggServe` runtime or service failed to start.
    #[error("proxy runtime: {0}")]
    Runtime(String),
}

/// Outbound route for upstream HTTP and `CONNECT` dials.
///
/// `direct` uses ordinary `EggFetch` routing; any other value is parsed with
/// the narrow `pproxy-compat` grammar. Malformed routes fail closed and never
/// fall back to direct; direct and routed paths share the same target policy.
#[derive(Clone)]
pub struct ProxyRoute {
    dialer: EggressDialer,
    redacted_spec: String,
    direct: bool,
}

impl std::fmt::Debug for ProxyRoute {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProxyRoute")
            .field("spec", &self.redacted_spec)
            .field("direct", &self.direct)
            .finish_non_exhaustive()
    }
}

impl ProxyRoute {
    /// Direct routing (no upstream proxy hop).
    #[must_use]
    pub fn direct() -> Self {
        Self {
            dialer: EggressDialer::direct(),
            redacted_spec: "direct".to_owned(),
            direct: true,
        }
    }

    /// Routed construction via `OutboundConnector::from_pproxy_uri`.
    ///
    /// # Errors
    ///
    /// Returns [`ProxyError::Route`] (credential-redacted) for malformed
    /// expressions. No fallback to direct occurs.
    pub fn from_pproxy_uri(spec: &str) -> Result<Self, ProxyError> {
        eggress_outbound::OutboundConnector::from_pproxy_uri(spec)
            .map(|connector| Self {
                dialer: EggressDialer::new(connector),
                redacted_spec: redact_route_credentials(spec),
                direct: false,
            })
            .map_err(|error| {
                ProxyError::Route(bound_diagnostic(&redact_route_credentials(
                    &error.to_string(),
                )))
            })
    }

    /// Whether this route dials targets directly.
    #[must_use]
    pub fn is_direct(&self) -> bool {
        self.direct
    }

    /// The `Eggress` dialer shared by plain, tunnel, and interception paths.
    pub(crate) fn dialer(&self) -> &EggressDialer {
        &self.dialer
    }

    /// Credential-redacted route expression for operational metadata.
    pub(crate) fn redacted_spec(&self) -> &str {
        &self.redacted_spec
    }

    /// Redaction-safe route description for flow physical routes.
    #[must_use]
    pub fn physical_route(&self) -> PhysicalRoute {
        if self.direct {
            PhysicalRoute {
                kind: "direct".into(),
                description: Some("direct".into()),
            }
        } else {
            PhysicalRoute {
                kind: "eggress".into(),
                description: Some(self.redacted_spec.clone()),
            }
        }
    }
}

/// Configuration for one explicit-proxy service instance.
pub struct ExplicitProxyConfig {
    /// Concurrent recording session for plain-request flows.
    pub session: RecordingSession,
    /// Target policy shared by plain and `CONNECT` paths.
    pub policy: TargetPolicy,
    /// Outbound route shared by plain, tunnel, and interception paths.
    pub route: ProxyRoute,
    /// Redaction policy applied before blob publication.
    pub redaction: RedactionConfig,
    /// Redaction profile identifier recorded in markers.
    pub profile_id: String,
    /// Ceiling for structured redaction staging reads.
    pub max_structured_bytes: u64,
    /// Bounds for opaque `CONNECT` tunnels.
    pub tunnel_limits: TunnelLimits,
    /// Hard request-body ceiling for proxied requests.
    pub max_request_body_bytes: u64,
    /// Concurrent connection ceiling for the listener (`EggServe` admission).
    pub max_connections: usize,
    /// Interception issuer/trust for the `intercept` policy action (M013D).
    /// `None` preserves M013B behavior: an `Intercept` verdict then fails
    /// closed before `200`.
    pub mitm: Option<crate::mitm::MitmConfig>,
}

impl ExplicitProxyConfig {
    /// Build a config with secure redaction defaults.
    pub fn new(session: RecordingSession, policy: TargetPolicy, route: ProxyRoute) -> Self {
        Self {
            session,
            policy,
            route,
            redaction: RedactionConfig::default_secure(),
            profile_id: "explicit-proxy-v1".to_owned(),
            max_structured_bytes: DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            tunnel_limits: TunnelLimits::default(),
            max_request_body_bytes: DEFAULT_MAX_PROXY_BODY_BYTES,
            max_connections: 64,
            mitm: None,
        }
    }
}

/// A fail-closed rejection with an explicit status for proxy validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyRejection {
    status: u16,
    message: &'static str,
}

impl ProxyRejection {
    const fn new(status: u16, message: &'static str) -> Self {
        Self { status, message }
    }
}

impl From<ProxyRejection> for ServiceError {
    fn from(rejection: ProxyRejection) -> Self {
        Self::rejected(rejection.status, rejection.message)
    }
}

/// Canonical absolute-form proxy target derived from `EggServe` metadata.
#[derive(Debug, Clone)]
pub struct CanonicalProxyTarget {
    /// Normalized target host.
    pub host: NormalizedHost,
    /// Explicit target port (default 80 applied).
    pub port: u16,
    /// Upstream `http://` URI with canonical authority.
    pub upstream_uri: http::Uri,
}

/// Validate an absolute-form proxy target without touching the network.
///
/// Requires absolute form, the `http` scheme (HTTPS uses `CONNECT`), a
/// parseable authority, and a `Host` header that normalizes to the same
/// host/port (default-port equivalence applies). `EggServe` already rejects
/// contradictory authorities with 400; this check keeps the proxy
/// fail-closed even if transport validation ever changes.
///
/// # Errors
///
/// Returns [`ProxyRejection`] (fail-closed 400) for non-absolute targets,
/// non-`http` schemes, malformed authorities, missing `Host` headers, and
/// contradictory `Host` authorities.
pub fn resolve_absolute_target(
    form: RequestTargetForm,
    scheme: Option<&str>,
    uri_authority: Option<&str>,
    host_header: Option<&str>,
    path: &str,
    query: Option<&str>,
) -> Result<CanonicalProxyTarget, ProxyRejection> {
    if form != RequestTargetForm::Absolute {
        return Err(ProxyRejection::new(
            400,
            "explicit proxy requires absolute-form request target",
        ));
    }
    if scheme.is_none_or(|scheme| !scheme.eq_ignore_ascii_case("http")) {
        return Err(ProxyRejection::new(
            400,
            "explicit proxy supports http only; https uses CONNECT",
        ));
    }
    let Some(uri_authority) = uri_authority else {
        return Err(ProxyRejection::new(400, "absolute target has no authority"));
    };
    let target = normalize_authority(uri_authority, 80)
        .map_err(|_| ProxyRejection::new(400, "invalid absolute target authority"))?;
    let Some(host_header) = host_header else {
        return Err(ProxyRejection::new(
            400,
            "explicit proxy request has no Host",
        ));
    };
    let host = normalize_authority(host_header, 80)
        .map_err(|_| ProxyRejection::new(400, "invalid Host authority"))?;
    if host.host != target.host || host.port != target.port {
        return Err(ProxyRejection::new(
            400,
            "Host authority contradicts the absolute target",
        ));
    }
    let path = if path.is_empty() { "/" } else { path };
    let query_suffix = query.map_or_else(String::new, |query| format!("?{query}"));
    let uri: http::Uri = format!(
        "http://{}:{}{path}{query_suffix}",
        target.host.as_str(),
        target.port
    )
    .parse()
    .map_err(|_| ProxyRejection::new(400, "invalid request target"))?;
    Ok(CanonicalProxyTarget {
        host: target.host,
        port: target.port,
        upstream_uri: uri,
    })
}

/// Validate a `CONNECT` authority without touching the network.
///
/// Applies the 443 default port and rejects userinfo, empty hosts, port
/// zero, and path/query fragments fail-closed.
///
/// # Errors
///
/// Returns [`ProxyRejection`] (fail-closed 400) for missing or malformed
/// authorities.
pub fn resolve_connect_target(authority: Option<&str>) -> Result<NormalizedTarget, ProxyRejection> {
    let Some(authority) = authority else {
        return Err(ProxyRejection::new(400, "CONNECT requires an authority"));
    };
    normalize_authority(authority, 443)
        .map_err(|_| ProxyRejection::new(400, "invalid CONNECT authority"))
}

/// Truncate a diagnostic string so operational errors stay bounded.
pub(crate) fn bound_diagnostic(message: &str) -> String {
    message.chars().take(DIAGNOSTIC_LEN).collect()
}

struct ProxyShared {
    session: RecordingSession,
    policy: TargetPolicy,
    route: ProxyRoute,
    redaction: RedactionConfig,
    profile_id: String,
    max_structured_bytes: u64,
    tunnel_limits: TunnelLimits,
    max_body_bytes: u64,
    client: Client,
    physical_route: PhysicalRoute,
    tunnels: Arc<Semaphore>,
    events: Arc<TunnelEventLog>,
    stats: Arc<ProxyStats>,
    mitm: Option<Arc<crate::mitm::MitmInner>>,
}

/// Explicit-proxy `EggServe` service: absolute-form recording + CONNECT relay.
#[derive(Clone)]
pub struct ExplicitProxy {
    shared: Arc<ProxyShared>,
}

impl ExplicitProxy {
    /// Build the service and its tunnel operational-event log.
    ///
    /// The `EggFetch` client routes through the configured [`ProxyRoute`]
    /// dialer, so plain upstream requests and `CONNECT` dials share one
    /// route with no fallback. When `config.mitm` is present, shared
    /// interception state (issuer, upstream TLS trust, decrypted H1 driver
    /// policy) is built against `runtime`, the same validated `EggServe`
    /// configuration the listener runs; an `Intercept` verdict without that
    /// state fails closed before `200`.
    #[must_use]
    pub fn new(
        config: ExplicitProxyConfig,
        runtime: &eggserve_server::RuntimeConfig,
    ) -> (Self, Arc<TunnelEventLog>) {
        let client = Client::builder()
            .retry_canceled_requests(false)
            .dialer(config.route.dialer.clone())
            .build();
        let events = Arc::new(TunnelEventLog::new());
        let stats = Arc::new(ProxyStats::new());
        let mitm = config.mitm.as_ref().and_then(|mitm_config| {
            let inner = crate::mitm::MitmInner::build(
                mitm_config,
                config.session.clone(),
                &config.route,
                runtime,
                config.tunnel_limits.connect_timeout,
                events.clone(),
                stats.clone(),
            )
            .ok();
            debug_assert!(
                inner.is_some(),
                "MitmInner must build against the validated listener runtime"
            );
            inner.map(Arc::new)
        });
        let proxy = Self {
            shared: Arc::new(ProxyShared {
                session: config.session,
                policy: config.policy,
                route: config.route.clone(),
                redaction: config.redaction,
                profile_id: config.profile_id,
                max_structured_bytes: config.max_structured_bytes,
                tunnel_limits: config.tunnel_limits,
                max_body_bytes: config.max_request_body_bytes,
                client,
                physical_route: config.route.physical_route(),
                tunnels: Arc::new(Semaphore::new(config.tunnel_limits.max_concurrent)),
                events: events.clone(),
                stats: stats.clone(),
                mitm,
            }),
        };
        (proxy, events)
    }

    /// Operational tunnel-event log (bounded target/action/error metadata).
    #[must_use]
    pub fn event_log(&self) -> Arc<TunnelEventLog> {
        self.shared.events.clone()
    }

    /// Operational counters for machine-readable output.
    #[must_use]
    pub fn stats(&self) -> Arc<ProxyStats> {
        self.shared.stats.clone()
    }

    async fn dispatch(
        self,
        request: Request,
        tunnel: Option<TunnelCapability>,
    ) -> Result<Response, ServiceError> {
        if request.head().method().as_str() == "CONNECT" {
            return self.handle_connect(request, tunnel).await;
        }
        if let Some(capability) = tunnel {
            // Plain proxy never upgrades in M013B: ordinary 403 denial.
            drop(capability);
            return Ok(denied_response());
        }
        self.handle_plain(request).await
    }

    async fn handle_plain(self, request: Request) -> Result<Response, ServiceError> {
        let stats = self.shared.stats.clone();
        let (head, body, _connection) = request.into_parts();
        let canonical = match resolve_absolute_target(
            head.target().form(),
            head.target().scheme(),
            head.target()
                .uri_authority()
                .map(eggserve_primitives::Authority::as_str),
            head.authority().map(eggserve_primitives::Authority::as_str),
            head.target().path(),
            head.target().query(),
        ) {
            Ok(canonical) => canonical,
            Err(rejection) => {
                stats.record_reject();
                stats.record_failure(FAILURE_INVALID_AUTHORITY);
                return Err(rejection.into());
            }
        };
        if !self
            .shared
            .policy
            .allows_plain(&canonical.host, canonical.port)
        {
            stats.record_reject();
            stats.record_failure(FAILURE_POLICY_DENIED);
            return Err(ServiceError::rejected(403, "target denied by proxy policy"));
        }
        stats.record_accept();
        let mut upstream_headers = http::HeaderMap::new();
        for field in head.headers().iter() {
            if field.name.as_str().eq_ignore_ascii_case("host") {
                continue;
            }
            let name = http::header::HeaderName::from_bytes(field.name.as_str().as_bytes())
                .map_err(|_| ServiceError::rejected(400, "invalid proxy header"))?;
            let value = http::HeaderValue::from_bytes(field.value.as_bytes())
                .map_err(|_| ServiceError::rejected(400, "invalid proxy header"))?;
            upstream_headers.append(name, value);
        }
        let filtered = filter_proxy_headers(upstream_headers);
        if filtered.upgrade_requested {
            stats.record_reject();
            stats.record_failure(FAILURE_UNSUPPORTED);
            return Err(ServiceError::rejected(400, "proxy upgrade unsupported"));
        }
        let method = head.method().as_str().to_owned();
        let stream_body = StreamBody::new(proxy_body_stream(body));
        let mut outbound = http::Request::builder()
            .method(method.as_str())
            .uri(canonical.upstream_uri)
            .body(stream_body)
            .map_err(|error| ServiceError::internal(error.to_string()))?;
        *outbound.headers_mut() = filtered.forwarded;
        let Ok(flow) = record_request_with_session(
            &self.shared.client,
            &self.shared.session,
            outbound,
            &self.shared.redaction,
            &self.shared.profile_id,
            self.shared.max_structured_bytes,
            Some(self.shared.physical_route.clone()),
        )
        .await
        else {
            stats.record_failure(FAILURE_UPSTREAM_FAILED);
            return Err(ServiceError::rejected(502, "upstream request failed"));
        };
        stats.record_flow();
        flow_to_response(&self.shared.session, flow)
    }

    async fn handle_connect(
        self,
        request: Request,
        tunnel: Option<TunnelCapability>,
    ) -> Result<Response, ServiceError> {
        let stats = self.shared.stats.clone();
        let Some(capability) = tunnel else {
            stats.record_reject();
            stats.record_failure(FAILURE_INVALID_AUTHORITY);
            return Err(ServiceError::rejected(400, "CONNECT requires a tunnel"));
        };
        let authority = capability
            .request()
            .authority()
            .map(eggserve_primitives::Authority::as_str)
            .map(str::to_owned)
            .or_else(|| {
                request
                    .head()
                    .authority()
                    .map(eggserve_primitives::Authority::as_str)
                    .map(str::to_owned)
            });
        let target = match resolve_connect_target(authority.as_deref()) {
            Ok(target) => target,
            Err(rejection) => {
                drop(capability);
                stats.record_reject();
                stats.record_failure(FAILURE_INVALID_AUTHORITY);
                return Err(rejection.into());
            }
        };
        let action = self
            .shared
            .policy
            .resolve_connect(&target.host, target.port);
        // Admission accounting happens before the action dispatch: denials
        // count as rejections, tunnel/intercept count as admissions with a
        // per-action counter at acceptance time.
        match action {
            ConnectAction::Deny => {
                drop(capability);
                stats.record_reject();
                stats.record_failure(FAILURE_POLICY_DENIED);
                Err(ServiceError::rejected(
                    403,
                    "CONNECT target denied by proxy policy",
                ))
            }
            ConnectAction::Intercept => self.handle_intercept(request, capability, target).await,
            ConnectAction::Tunnel => self.handle_tunnel(request, capability, target).await,
        }
    }

    /// Serve an intercepted `CONNECT` (M013D).
    ///
    /// The exact-host leaf is acquired before tunnel acceptance, so `200`
    /// is sent only when interception can proceed. Without configured
    /// interception state this fails closed before `200`.
    async fn handle_intercept(
        self,
        request: Request,
        capability: TunnelCapability,
        target: NormalizedTarget,
    ) -> Result<Response, ServiceError> {
        let stats = self.shared.stats.clone();
        let Some(mitm) = self.shared.mitm.clone() else {
            drop(capability);
            stats.record_reject();
            stats.record_failure(FAILURE_NOT_CONFIGURED);
            return Err(ServiceError::internal(
                "CONNECT interception is not configured",
            ));
        };
        let permit: OwnedSemaphorePermit =
            if let Ok(permit) = self.shared.tunnels.clone().try_acquire_owned() {
                permit
            } else {
                drop(capability);
                stats.record_reject();
                stats.record_failure(FAILURE_LIMIT_REACHED);
                return Err(ServiceError::rejected(
                    503,
                    "tunnel concurrency limit reached",
                ));
            };
        stats.record_accept();
        stats.record_intercept();
        let lifecycle = request.lifecycle_clone();
        match crate::mitm::serve_intercepted_connect(capability, lifecycle, target, mitm, permit)
            .await
        {
            Ok(response) => Ok(response),
            Err(error) => {
                stats.record_failure(FAILURE_TLS_FAILED);
                Err(error)
            }
        }
    }

    /// Relay an opaque `CONNECT` tunnel (M013B).
    async fn handle_tunnel(
        self,
        request: Request,
        capability: TunnelCapability,
        target: NormalizedTarget,
    ) -> Result<Response, ServiceError> {
        let stats = self.shared.stats.clone();
        let permit: OwnedSemaphorePermit =
            if let Ok(permit) = self.shared.tunnels.clone().try_acquire_owned() {
                permit
            } else {
                drop(capability);
                stats.record_reject();
                stats.record_failure(FAILURE_LIMIT_REACHED);
                return Err(ServiceError::rejected(
                    503,
                    "tunnel concurrency limit reached",
                ));
            };
        // Dial before accepting: the 200 handshake is sent only after the
        // target route is established. No fallback to direct occurs.
        let dialer = self.shared.route.dialer.clone();
        let dial_target = DialTarget::new(target.host.as_str(), target.port);
        let dial = dialer.dial(dial_target);
        let upstream =
            match tokio::time::timeout(self.shared.tunnel_limits.connect_timeout, dial).await {
                Err(_) => {
                    drop(capability);
                    drop(permit);
                    stats.record_failure(FAILURE_UPSTREAM_FAILED);
                    return Err(ServiceError::rejected(504, "target dial timed out"));
                }
                Ok(Err(error)) => {
                    drop(capability);
                    drop(permit);
                    stats.record_failure(FAILURE_UPSTREAM_FAILED);
                    return Err(ServiceError::rejected(
                        502,
                        bound_diagnostic(&error.to_string()),
                    ));
                }
                Ok(Ok(stream)) => stream,
            };
        stats.record_accept();
        stats.record_tunnel();
        let events = self.shared.events.clone();
        let limits = self.shared.tunnel_limits;
        let host = target.host.as_str();
        let port = target.port;
        let lifecycle = request.lifecycle_clone();
        let accept_stats = stats.clone();
        capability
            .accept(HeaderBlock::new(), move |client_io| async move {
                // Race the relay against request-lifecycle cancellation so
                // server shutdown (or a vanished peer) ends the tunnel with
                // a Shutdown outcome instead of relaying forever.
                let relay = tokio::spawn(relay_tunnel(client_io, upstream, limits));
                loop {
                    if lifecycle.is_cancelled() {
                        relay.abort();
                        break;
                    }
                    if relay.is_finished() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                let summary = relay
                    .await
                    .unwrap_or_else(|_| TunnelRelaySummary::shutdown());
                let error = match summary.outcome {
                    TunnelOutcome::RelayError | TunnelOutcome::Shutdown => {
                        Some("relay transport error".to_owned())
                    }
                    TunnelOutcome::Completed
                    | TunnelOutcome::ByteLimit
                    | TunnelOutcome::DurationLimit
                    | TunnelOutcome::IdleTimeout => None,
                };
                events.push(TunnelEvent::new(&host, port, &summary, error));
                drop(permit);
            })
            .map_err(|_| {
                accept_stats.record_failure(FAILURE_INTERNAL);
                ServiceError::internal("tunnel accept failed")
            })
    }
}

impl Service for ExplicitProxy {
    fn request_body_policy(&self, head: &eggserve_primitives::RequestHead) -> RequestBodyPolicy {
        if head.method().as_str() == "CONNECT" {
            RequestBodyPolicy::Reject
        } else {
            RequestBodyPolicy::Stream {
                max_bytes: self.shared.max_body_bytes,
            }
        }
    }

    fn call(&self, request: Request) -> ServiceFuture<'_> {
        Box::pin(self.clone().dispatch(request, None))
    }

    fn call_with_tunnel(
        &self,
        request: Request,
        tunnel: Option<TunnelCapability>,
    ) -> ServiceFuture<'_> {
        Box::pin(self.clone().dispatch(request, tunnel))
    }
}

/// Ordinary 403 denial for unsupported upgrade/tunnel intent on plain paths.
fn denied_response() -> Response {
    Response::builder()
        .status(StatusCode::new(403).expect("valid status"))
        .body(ResponseBody::Empty)
        .expect("valid denial response")
}

/// Convert a recorded flow outcome into an `EggServe` semantic response.
pub(crate) fn flow_to_response(
    session: &RecordingSession,
    flow: eggreplay_core::Flow,
) -> Result<Response, ServiceError> {
    match flow.outcome {
        eggreplay_core::FlowOutcome::Response(response) => {
            let status = StatusCode::new(response.status)
                .map_err(|error| ServiceError::internal(error.to_string()))?;
            let mut builder = Response::builder().status(status);
            for header in &response.headers {
                builder = builder
                    .header(header.name.clone(), header.value.clone())
                    .map_err(|error| ServiceError::internal(error.to_string()))?;
            }
            let bytes = session
                .read_body(&response.body)
                .map_err(|error| ServiceError::internal(error.to_string()))?;
            if response.trailers.is_empty() {
                builder
                    .body(ResponseBody::Bytes(bytes))
                    .map_err(|error| ServiceError::internal(error.to_string()))
            } else {
                // Hyper only emits the terminal trailer block when the
                // response announces it with a `Trailer` header; attach the
                // announcement derived from the recorded trailer fields.
                let announcement = response
                    .trailers
                    .iter()
                    .map(|header| header.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
                builder = builder
                    .header("trailer", announcement)
                    .map_err(|error| ServiceError::internal(error.to_string()))?;
                let stream = response_body_stream(bytes, response.trailers);
                builder
                    .body(ResponseBody::Stream(stream))
                    .map_err(|error| ServiceError::internal(error.to_string()))
            }
        }
        eggreplay_core::FlowOutcome::Error(_) => {
            Err(ServiceError::rejected(502, "upstream request failed"))
        }
    }
}

/// Build an unknown-length response stream carrying terminal trailers.
///
/// Unknown length selects chunked framing, the only H1 framing that can
/// carry a terminal block. Note: `EggServe` 0.3.0 strips the `Trailer`
/// announcement during response normalization, and `Hyper` does not emit the
/// block without it, so terminal trailers are recorded in the flow but not
/// yet re-emitted on H1 egress. Attaching them here keeps the service
/// correct for transports that preserve the announcement.
fn response_body_stream(
    bytes: Vec<u8>,
    trailers: Vec<eggreplay_core::HeaderEntry>,
) -> ResponseStream {
    let byte_stream: Pin<Box<dyn Stream<Item = Result<Bytes, ResponseStreamError>> + Send>> =
        Box::pin(futures_util::stream::once(async move {
            Ok::<_, ResponseStreamError>(Bytes::from(bytes))
        }));
    ResponseStream::with_trailers(byte_stream, async move {
        let mut block = HeaderBlock::new();
        for header in trailers {
            block
                .push_bytes(header.name, header.value.as_bytes())
                .map_err(|error| ResponseStreamError::new(error.to_string()))?;
        }
        Trailers::new(block)
            .map(Some)
            .map_err(|error| ResponseStreamError::new(error.to_string()))
    })
}

/// Stream error for the `EggServe`-to-`EggFetch` body bridge.
#[derive(Debug)]
pub(crate) struct ProxyBodyError(String);

impl std::fmt::Display for ProxyBodyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProxyBodyError {}

/// Bridge an `EggServe` request body (with terminal trailers) into an
/// `EggFetch`-compatible frame stream, preserving duplicates and trailers.
///
/// Shared by the plain proxy path and the M013D decrypted service so both
/// preserve identical body semantics.
pub(crate) fn proxy_body_stream(
    body: eggserve_primitives::RequestBody,
) -> impl Stream<Item = Result<Frame<Bytes>, ProxyBodyError>> + Send {
    futures_util::stream::unfold((body, false), |(mut body, trailers_emitted)| async move {
        if trailers_emitted {
            return None;
        }
        // Reborrow through the owned body each poll; the unfold state owns it.
        let next = body.next_chunk().await;
        match next {
            Ok(Some(bytes)) => Some((Ok(Frame::data(bytes)), (body, false))),
            Ok(None) => {
                let trailers = match body.trailers().await {
                    Ok(Some(trailers)) => {
                        let mut map = http::HeaderMap::new();
                        for field in trailers.iter() {
                            if let (Ok(name), Ok(value)) = (
                                http::header::HeaderName::from_bytes(
                                    field.name.as_str().as_bytes(),
                                ),
                                http::HeaderValue::from_bytes(field.value.as_bytes()),
                            ) {
                                map.append(name, value);
                            }
                        }
                        Some(Frame::trailers(map))
                    }
                    Ok(None) => None,
                    Err(error) => {
                        return Some((Err(ProxyBodyError(error.to_string())), (body, true)));
                    }
                };
                trailers.map(|frame| (Ok(frame), (body, true)))
            }
            Err(error) => Some((Err(ProxyBodyError(error.to_string())), (body, true))),
        }
    })
}

/// Handle for a running explicit proxy: the `EggServe` server plus the tunnel
/// operational-event log.
pub struct ExplicitProxyHandle {
    server: ServerHandle,
    events: Arc<TunnelEventLog>,
    stats: Arc<ProxyStats>,
}

impl ExplicitProxyHandle {
    /// Local address the proxy is bound to.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.server.local_addr()
    }

    /// Stop admission; use [`wait`](Self::wait) to drain.
    pub fn shutdown(&self) {
        self.server.shutdown();
    }

    /// Drain in-flight proxy tasks.
    pub async fn wait(self) {
        self.server.wait().await;
    }

    /// Bounded tunnel operational events (target/action/error metadata only).
    #[must_use]
    pub fn events(&self) -> Arc<TunnelEventLog> {
        self.events.clone()
    }

    /// Operational counters for machine-readable output.
    #[must_use]
    pub fn stats(&self) -> Arc<ProxyStats> {
        self.stats.clone()
    }
}

/// Start an explicit proxy listener on `EggServe`.
///
/// The runtime uses [`InterceptionProfile`] bounds with
/// `OriginOrAbsolute` target mode; the bind address must pass
/// [`validate_bind`]. Plain requests record through `config.session`;
/// `CONNECT` tunnels relay opaquely and never append flows.
///
/// # Errors
///
/// Returns [`ProxyError`] when the bind fails the safety gate or the
/// `EggServe` runtime/service fails to start.
pub async fn start_explicit_proxy(
    listener: ProxyListenerConfig,
    config: ExplicitProxyConfig,
) -> Result<ExplicitProxyHandle, ProxyError> {
    listener.validate()?;
    let mut profile = InterceptionProfile::with_bind(listener.bind);
    profile.max_request_body_bytes = config.max_request_body_bytes;
    profile.max_connections = config.max_connections;
    let runtime = profile
        .build_runtime_config()
        .map_err(|error| ProxyError::Runtime(error.to_string()))?;
    let (service, events) = ExplicitProxy::new(config, &runtime);
    let stats = service.stats();
    let server = Server::builder()
        .runtime(runtime)
        .build()
        .map_err(|error| ProxyError::Runtime(error.to_string()))?
        .start_with_service(service)
        .await
        .map_err(|error| ProxyError::Runtime(error.to_string()))?;
    Ok(ExplicitProxyHandle {
        server,
        events,
        stats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_target_requires_absolute_http_with_coherent_host() {
        let ok = resolve_absolute_target(
            RequestTargetForm::Absolute,
            Some("http"),
            Some("example.test:8080"),
            Some("example.test:8080"),
            "/a",
            Some("b=1"),
        )
        .unwrap();
        assert_eq!(ok.port, 8080);
        assert_eq!(ok.upstream_uri.host(), Some("example.test"));
        // Origin form is rejected on the explicit listener.
        assert!(
            resolve_absolute_target(
                RequestTargetForm::Origin,
                None,
                None,
                Some("example.test"),
                "/a",
                None
            )
            .is_err()
        );
        // Non-http schemes are rejected.
        assert!(
            resolve_absolute_target(
                RequestTargetForm::Absolute,
                Some("https"),
                Some("example.test"),
                Some("example.test"),
                "/a",
                None
            )
            .is_err()
        );
        // Contradictory Host is rejected.
        assert!(
            resolve_absolute_target(
                RequestTargetForm::Absolute,
                Some("http"),
                Some("example.test"),
                Some("other.test"),
                "/a",
                None
            )
            .is_err()
        );
        // Default-port equivalence is accepted.
        assert!(
            resolve_absolute_target(
                RequestTargetForm::Absolute,
                Some("http"),
                Some("example.test:80"),
                Some("example.test"),
                "/a",
                None
            )
            .is_ok()
        );
        // Userinfo is rejected.
        assert!(
            resolve_absolute_target(
                RequestTargetForm::Absolute,
                Some("http"),
                Some("user@example.test"),
                Some("user@example.test"),
                "/a",
                None
            )
            .is_err()
        );
    }

    #[test]
    fn connect_target_validation_applies_default_port() {
        let target = resolve_connect_target(Some("example.test:443")).unwrap();
        assert_eq!(target.port, 443);
        let defaulted = resolve_connect_target(Some("example.test")).unwrap();
        assert_eq!(defaulted.port, 443);
        assert!(resolve_connect_target(None).is_err());
        assert!(resolve_connect_target(Some("user@example.test:443")).is_err());
        assert!(resolve_connect_target(Some("example.test:0")).is_err());
    }

    #[test]
    fn loopback_gate_rejects_remote_without_opt_in() {
        let loopback: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert!(validate_bind(loopback, false).is_ok());
        let remote: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        assert!(validate_bind(remote, false).is_err());
        assert!(validate_bind(remote, true).is_ok());
        assert!(ProxyListenerConfig::loopback(remote).is_err());
    }

    #[test]
    fn proxy_stats_snapshot_counts_and_bounds_failures() {
        let stats = ProxyStats::new();
        let empty = stats.snapshot();
        assert_eq!(empty.accepted, 0);
        assert_eq!(empty.failures.len(), PROXY_FAILURE_CATEGORIES.len());
        stats.record_accept();
        stats.record_accept();
        stats.record_reject();
        stats.record_tunnel();
        stats.record_intercept();
        stats.record_flow();
        stats.record_failure(FAILURE_POLICY_DENIED);
        stats.record_failure(FAILURE_UPSTREAM_FAILED);
        // Unknown categories are ignored so the set stays bounded.
        stats.record_failure("credential-leak-attempt");
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.accepted, 2);
        assert_eq!(snapshot.rejected, 1);
        assert_eq!(snapshot.tunneled, 1);
        assert_eq!(snapshot.intercepted, 1);
        assert_eq!(snapshot.flows, 1);
        assert_eq!(snapshot.failures[FAILURE_POLICY_DENIED], 1);
        assert_eq!(snapshot.failures[FAILURE_UPSTREAM_FAILED], 1);
        assert_eq!(
            snapshot.failures.values().sum::<u64>(),
            2,
            "only known categories are counted"
        );
        // Snapshots serialize to counts only (no key material, paths, or payloads).
        let json = serde_json::to_value(&snapshot).expect("snapshot serializes");
        assert_eq!(json["accepted"], 2);
        let text = json.to_string();
        assert!(!text.contains("PRIVATE KEY"));
    }
}
