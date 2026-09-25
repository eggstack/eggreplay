//! HTTPS `CONNECT` interception (`intercept` action) for M013D.
//!
//! Ownership:
//!
//! - [`policy`](crate::policy) decides whether a `CONNECT` target is denied,
//!   tunnelled, or intercepted. `Intercept` only fires for explicitly allowed
//!   targets under an intercept default; deny still wins ties and unmatched
//!   targets still deny.
//! - [`leaf`](crate::leaf) mints the exact-host leaf presented to the client.
//!   Leaf private keys stay memory-only and never enter fixtures, logs, or
//!   errors.
//! - `EggServe` owns the decrypted H1 parsing/lifecycle through the
//!   caller-owned [`serve_http1_connection_with_policy`] driver. This module
//!   adds no second HTTP stack.
//! - `EggFetch` owns semantic upstream HTTPS with normal certificate and
//!   hostname verification. Client trust of the `EggReplay` CA has no effect
//!   upstream, and no `danger_accept_invalid_certs` path is enabled by
//!   interception.
//! - `Eggress` (via [`EggressDialer`](eggreplay_http::EggressDialer)) owns
//!   the physical route. A configured route never falls back to direct.
//!
//! # Sequence
//!
//! 1. parse/normalize the `CONNECT` authority (reuse
//!    [`resolve_connect_target`](crate::proxy::resolve_connect_target));
//! 2. evaluate ingress + target + intercept policy;
//! 3. acquire the exact-host leaf **before** acknowledging `CONNECT`;
//! 4. send `200 Connection Established` (via tunnel acceptance);
//! 5. perform the rustls server handshake (ALPN `["http/1.1"]` only);
//! 6. require negotiated ALPN absent or `http/1.1`, check SNI coherence;
//! 7. construct truthful HTTPS [`TlsInfo`] + [`ConnectionContext`];
//! 8. run the decrypted stream through the `EggServe` H1 driver.
//!
//! A failure before step 4 returns an HTTP proxy error and records no flow.
//! A TLS failure after step 4 is a tunnel/TLS failure: the connection closes
//! with a bounded operational event and no HTTP flow is fabricated.
//!
//! # Authority coherence (strict)
//!
//! One `CONNECT` is one origin. For DNS targets the leaf SAN is the CONNECT
//! name, client SNI (when present) must normalize to it, and every decrypted
//! `Host`/request authority must normalize to it with a compatible port. For
//! IP targets the leaf SAN is the IP, SNI may be absent, a present SNI must
//! not redirect elsewhere, and HTTP authority remains the CONNECT IP/port.
//! Cross-origin reuse fails closed: the offending request receives `421` and
//! the connection is poisoned (no flow for that request or any later one).
//!
//! # Record modes
//!
//! Interception records through the same [`RecordingSession`] authority as
//! plain proxy requests, so it is session-agnostic: normal recording
//! sessions and re-record/replacement temporary sessions compose without a
//! second updater. Sessions that cannot own a live listener (sealed or
//! finished sessions) fail at the store boundary; MITM adds no separate
//! append path.
//!
//! # Unsupported traffic
//!
//! HTTP/2-only clients, QUIC/HTTP/3, non-HTTP TLS, WebSocket-over-WSS
//! upgrades, mTLS client certificates, and pinned clients fail explicitly
//! (or belong on an explicit M013B tunnel policy). Nothing is silently
//! downgraded.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eggfetch_core::{Client, TlsConfig};
use eggreplay_core::{PhysicalRoute, RedactionConfig};
use eggreplay_http::record_request_with_session;
use eggreplay_store::RecordingSession;
use eggserve_primitives::Response;
use eggserve_primitives::connection_info::{Scheme, TlsInfo};
use eggserve_primitives::header_block::HeaderBlock;
use eggserve_primitives::request_lifecycle::RequestLifecycle;
use eggserve_primitives::request_target::RequestTargetForm;
use eggserve_server::connection::{
    ConnectionContext, ConnectionOutcome, ConnectionShutdown, serve_http1_connection_with_policy,
};
use eggserve_server::runtime::RuntimeState;
use eggserve_server::tunnel::{TunnelCapability, TunnelIo};
use eggserve_server::{
    H1ConnectionPolicy, Request, RequestBodyPolicy, RuntimeConfig, Service, ServiceError,
    ServiceFuture,
};
use http_body_util::StreamBody;
use thiserror::Error;
use tokio_rustls::TlsAcceptor;

use crate::leaf::{INTERCEPT_ALPN_HTTP1_1, LeafCertificate, LeafError, LeafIssuer};
use crate::policy::{
    ConnectAction, NormalizedHost, NormalizedTarget, TargetPolicy, normalize_host,
};
use crate::proxy::ProxyRoute;
use crate::tunnel::{TunnelEvent, TunnelEventLog, TunnelOutcome, TunnelRelaySummary};

/// Acquisition-mode label recorded on MITM physical routes.
pub const MITM_ROUTE_KIND: &str = "explicit_proxy_mitm";
/// Redaction profile identifier recorded in MITM flow markers.
pub const MITM_PROFILE_ID: &str = "explicit-proxy-mitm-v1";
/// Default ceiling for one client TLS handshake (M013F resource audit).
///
/// Handshake concurrency is bounded by the shared tunnel-concurrency
/// semaphore: `serve_intercepted_connect` acquires a tunnel permit *before*
/// leaf issuance/`200`, and the permit is held for the whole decrypted
/// connection. The handshake future itself is additionally bounded by this
/// timeout so a silent peer cannot park an admitted tunnel forever. Both the
/// timeout value and the effective concurrency ceiling track the tunnel
/// defaults below (`connect_timeout` / `max_concurrent`); the
/// `resource_bounds` integration test pins that correspondence so drift
/// fails.
pub const DEFAULT_TLS_HANDSHAKE_TIMEOUT: Duration = crate::tunnel::DEFAULT_TUNNEL_CONNECT_TIMEOUT;
/// Effective concurrent-TLS-handshake ceiling under default limits.
///
/// This is not a second semaphore: it documents that handshake concurrency
/// cannot exceed the tunnel-concurrency bound because every handshake runs
/// under an already-acquired tunnel permit.
pub const MAX_CONCURRENT_TLS_HANDSHAKES_UNDER_DEFAULT_LIMITS: usize =
    crate::tunnel::DEFAULT_TUNNEL_MAX_CONCURRENT;
/// Bound applied to SNI/authority diagnostics (never credentials).
const DIAGNOSTIC_LEN: usize = 128;

/// Fail-closed MITM errors.
///
/// Messages carry only static descriptions and bounded public facts (host
/// previews, ports, ALPN tokens). They never contain key material, session
/// secrets, request bodies, or credentials.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MitmError {
    /// The target policy is not armed for interception (default action is
    /// not [`ConnectAction::Intercept`]).
    #[error("interception policy is not armed (CONNECT default is not intercept)")]
    PolicyNotArmed,
    /// Exact-host leaf issuance failed.
    #[error("interception leaf unavailable")]
    LeafUnavailable,
    /// The rustls server configuration could not be built.
    #[error("interception TLS configuration failed")]
    ServerConfig,
    /// The caller-owned H1 driver configuration is unusable.
    #[error("interception connection policy unavailable")]
    ConnectionPolicy,
    /// The client TLS handshake failed or timed out.
    #[error("client TLS handshake failed")]
    TlsHandshake,
    /// The negotiated ALPN is not servable (`None` or `http/1.1` required).
    #[error("negotiated ALPN is not http/1.1")]
    AlpnUnsupported,
    /// CONNECT/SNI/HTTP authority coherence failed.
    #[error("CONNECT/SNI/HTTP authority mismatch")]
    AuthorityMismatch,
    /// An upgrade (e.g. WebSocket-over-WSS) was requested inside the tunnel.
    #[error("upgrade inside interception is unsupported")]
    UpgradeUnsupported,
    /// The decrypted request target is not servable origin-form.
    #[error("intercepted request target is not origin-form")]
    NotOriginForm,
    /// The upstream request failed at the session boundary.
    #[error("upstream request failed")]
    Upstream,
    /// The interception holder is missing (policy says intercept but no
    /// issuer was configured).
    #[error("interception is not configured")]
    NotConfigured,
}

/// Truncate a diagnostic string so operational errors stay bounded.
fn bound_diagnostic(message: &str) -> String {
    message.chars().take(DIAGNOSTIC_LEN).collect()
}

/// Caller-facing interception configuration.
///
/// Attach to [`crate::proxy::ExplicitProxyConfig::mitm`] to arm the
/// `intercept` policy action. Without this, an `Intercept` verdict fails
/// closed before `200`.
pub struct MitmConfig {
    /// Issuer bound to the explicitly selected interception CA.
    pub issuer: Arc<LeafIssuer>,
    /// Upstream TLS trust for the recording client. `None` selects secure
    /// defaults (native roots, full verification). Tests inject the hermetic
    /// origin CA here; production operators with private PKI do the same.
    /// There is no verification-bypass setting: upstream certificate and
    /// hostname failures remain failures.
    pub upstream_tls: Option<TlsConfig>,
    /// Redaction profile identifier recorded in flow markers.
    pub profile_id: String,
    /// Redaction policy applied before blob publication.
    pub redaction: RedactionConfig,
    /// Ceiling for structured redaction staging reads.
    pub max_structured_bytes: u64,
    /// Hard request-body ceiling for decrypted requests.
    pub max_request_body_bytes: u64,
}

impl fmt::Debug for MitmConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MitmConfig")
            .field("issuer", &self.issuer)
            .field("upstream_tls_custom", &self.upstream_tls.is_some())
            .field("profile_id", &self.profile_id)
            .field("max_structured_bytes", &self.max_structured_bytes)
            .field("max_request_body_bytes", &self.max_request_body_bytes)
            .finish_non_exhaustive()
    }
}

impl MitmConfig {
    /// Build interception config with secure redaction defaults around one
    /// explicitly selected issuer.
    pub fn new(issuer: Arc<LeafIssuer>) -> Self {
        Self {
            issuer,
            upstream_tls: None,
            profile_id: MITM_PROFILE_ID.to_owned(),
            redaction: RedactionConfig::default_secure(),
            max_structured_bytes: eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
            max_request_body_bytes: crate::proxy::DEFAULT_MAX_PROXY_BODY_BYTES,
        }
    }
}

/// Validated intercept-policy view over a [`TargetPolicy`].
///
/// Construction requires an intercept default
/// ([`ConnectAction::Intercept`]); otherwise interception is not armed and
/// construction fails closed. Rule evaluation (specificity, deny-wins,
/// default-deny) is fully reused: `Intercept` only fires for explicitly
/// allowed targets.
#[derive(Debug, Clone)]
pub struct MitmPolicy {
    target: TargetPolicy,
}

impl MitmPolicy {
    /// Validate that `policy` arms interception.
    ///
    /// # Errors
    ///
    /// Returns [`MitmError::PolicyNotArmed`] when the policy default is not
    /// [`ConnectAction::Intercept`].
    pub fn new(policy: TargetPolicy) -> Result<Self, MitmError> {
        if policy.default_connect_action() != ConnectAction::Intercept {
            return Err(MitmError::PolicyNotArmed);
        }
        Ok(Self { target: policy })
    }

    /// Resolve a normalized target through the armed policy.
    #[must_use]
    pub fn resolve(&self, host: &NormalizedHost, port: u16) -> ConnectAction {
        self.target.resolve_connect(host, port)
    }

    /// Whether this target would be intercepted (allowed under the armed
    /// intercept default).
    #[must_use]
    pub fn allows_intercept(&self, host: &NormalizedHost, port: u16) -> bool {
        self.resolve(host, port) == ConnectAction::Intercept
    }
}

/// Check SNI coherence against the CONNECT target.
///
/// SNI may be absent (plain clients, IP literals). A present SNI must
/// normalize to the CONNECT host; anything else (including a DNS SNI on an
/// IP CONNECT) fails closed.
///
/// # Errors
///
/// Returns [`MitmError::AuthorityMismatch`] for unparseable or divergent SNI.
pub fn check_sni_coherence(connect: &NormalizedTarget, sni: Option<&str>) -> Result<(), MitmError> {
    let Some(sni) = sni else {
        return Ok(());
    };
    let normalized = normalize_host(sni).map_err(|_| MitmError::AuthorityMismatch)?;
    if normalized == connect.host {
        Ok(())
    } else {
        Err(MitmError::AuthorityMismatch)
    }
}

/// Check decrypted HTTP authority coherence against the CONNECT target.
///
/// The `Host` authority must normalize to the CONNECT host with a compatible
/// port (default-port equivalence applies: a bare `Host: name` matches a
/// `:443` CONNECT). `Forwarded`/`X-Forwarded-*` headers are never consulted.
///
/// # Errors
///
/// Returns [`MitmError::AuthorityMismatch`] for missing, malformed, or
/// divergent authorities.
pub fn check_http_authority_coherence(
    connect: &NormalizedTarget,
    http_host: Option<&str>,
) -> Result<NormalizedTarget, MitmError> {
    let Some(http_host) = http_host else {
        return Err(MitmError::AuthorityMismatch);
    };
    let authority = crate::policy::normalize_authority(http_host, connect.port)
        .map_err(|_| MitmError::AuthorityMismatch)?;
    if authority.host == connect.host && authority.port == connect.port {
        Ok(authority)
    } else {
        Err(MitmError::AuthorityMismatch)
    }
}

/// Check full CONNECT/SNI/HTTP authority coherence.
///
/// Combines [`check_sni_coherence`] and [`check_http_authority_coherence`];
/// returns the canonical validated HTTP authority for URI reconstruction.
/// Diagnostics are bounded and credential-free.
///
/// # Errors
///
/// Returns [`MitmError::AuthorityMismatch`] on any mismatch.
pub fn check_authority_coherence(
    connect: &NormalizedTarget,
    sni: Option<&str>,
    http_host: Option<&str>,
) -> Result<NormalizedTarget, MitmError> {
    check_sni_coherence(connect, sni)?;
    check_http_authority_coherence(connect, http_host)
}

/// Build the per-target rustls server configuration from an issued leaf.
///
/// The chain is `[leaf, ca]`; the key is the leaf's memory-only PKCS#8 pair.
/// Safe defaults, no client authentication, and ALPN limited to
/// [`INTERCEPT_ALPN_HTTP1_1`]: `h2` is never advertised.
///
/// # Errors
///
/// Returns [`MitmError::ServerConfig`] when the key or chain is unusable.
pub fn build_intercept_server_config(
    leaf: &LeafCertificate,
    ca_der: &[u8],
) -> Result<Arc<rustls::ServerConfig>, MitmError> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let chain = vec![
        CertificateDer::from(leaf.cert_der().to_vec()),
        CertificateDer::from(ca_der.to_vec()),
    ];
    let key_der = leaf.key_pair().serialize_der();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der));
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| MitmError::ServerConfig)?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|_| MitmError::ServerConfig)?;
    config.alpn_protocols = vec![INTERCEPT_ALPN_HTTP1_1.to_vec()];
    Ok(Arc::new(config))
}

/// Map a rustls protocol version to its conventional display token.
fn protocol_version_token(version: rustls::ProtocolVersion) -> &'static str {
    match version {
        rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2",
        rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3",
        _ => "TLS-unknown",
    }
}

/// Shared interception state for one proxy instance (all secret-free except
/// the memory-only issuer handle, whose `Debug` is redacted).
///
/// Lock discipline (M013F resource audit): no network await ever holds a
/// global lock. Leaf issuance completes *before* tunnel acceptance (under the
/// leaf-cache mutex only, which guards fast local CPU signing with no
/// network or disk I/O). The TLS handshake holds only the per-connection
/// tunnel-concurrency permit plus local state. Session appends serialize only
/// the final bounded metadata write (`record_request_with_session` streams
/// bodies through independent sinks outside the flow-log lock).
pub(crate) struct MitmInner {
    session: RecordingSession,
    issuer: Arc<LeafIssuer>,
    ca_der: Vec<u8>,
    client: Client,
    physical_route: PhysicalRoute,
    profile_id: String,
    redaction: RedactionConfig,
    max_structured_bytes: u64,
    max_body_bytes: u64,
    inner_policy: Arc<H1ConnectionPolicy>,
    inner_state: Arc<RuntimeState>,
    handshake_timeout: Duration,
    events: Arc<TunnelEventLog>,
    stats: Arc<crate::proxy::ProxyStats>,
}

impl fmt::Debug for MitmInner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MitmInner")
            .field("issuer", &self.issuer)
            .field("physical_route", &self.physical_route)
            .field("profile_id", &self.profile_id)
            .field("max_structured_bytes", &self.max_structured_bytes)
            .field("max_body_bytes", &self.max_body_bytes)
            .finish_non_exhaustive()
    }
}

impl MitmInner {
    /// Build shared interception state.
    ///
    /// The upstream recording client routes through `route`'s dialer (no
    /// fallback) with `config.upstream_tls` trust (secure defaults when
    /// unset). The decrypted H1 driver reuses `runtime`'s connection policy
    /// plus a dedicated [`RuntimeState`] admission pool. Flows record through
    /// `session`, the same authority as plain proxy requests.
    ///
    /// # Errors
    ///
    /// Returns [`MitmError`] when the caller-owned driver policy or state
    /// cannot be constructed.
    pub(crate) fn build(
        config: &MitmConfig,
        session: RecordingSession,
        route: &ProxyRoute,
        runtime: &RuntimeConfig,
        handshake_timeout: Duration,
        events: Arc<TunnelEventLog>,
        stats: Arc<crate::proxy::ProxyStats>,
    ) -> Result<Self, MitmError> {
        let tls = config.upstream_tls.clone().unwrap_or_default();
        let client = Client::builder()
            .retry_canceled_requests(false)
            .dialer(route.dialer().clone())
            .tls_config(tls)
            .build();
        let inner_policy = Arc::new(
            runtime
                .h1_connection_policy()
                .map_err(|_| MitmError::ConnectionPolicy)?,
        );
        let inner_state =
            Arc::new(RuntimeState::try_new(runtime).map_err(|_| MitmError::ConnectionPolicy)?);
        let description = if route.is_direct() {
            "direct".to_owned()
        } else {
            route.redacted_spec().to_owned()
        };
        Ok(Self {
            session,
            issuer: Arc::clone(&config.issuer),
            ca_der: config.issuer.ca().cert_der().to_vec(),
            client,
            physical_route: PhysicalRoute {
                kind: MITM_ROUTE_KIND.to_owned(),
                description: Some(description),
            },
            profile_id: config.profile_id.clone(),
            redaction: config.redaction.clone(),
            max_structured_bytes: config.max_structured_bytes,
            max_body_bytes: config.max_request_body_bytes,
            inner_policy,
            inner_state,
            handshake_timeout,
            events,
            stats,
        })
    }
}

/// Per-connection decrypted-origin service.
///
/// One instance serves one intercepted tunnel (`CONNECT` = one origin). Every
/// request revalidates `Host` coherence; the first mismatch poisons the
/// connection (that request receives `421` and every later request fails
/// closed). No poisoned request produces a flow.
#[derive(Clone)]
pub(crate) struct MitmService {
    inner: Arc<MitmInner>,
    connect: NormalizedTarget,
    poisoned: Arc<AtomicBool>,
}

impl fmt::Debug for MitmService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MitmService")
            .field("connect_host", &self.connect.host.as_str())
            .field("connect_port", &self.connect.port)
            .field("poisoned", &self.poisoned.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl MitmService {
    /// Canonical logical HTTPS target for a validated decrypted request.
    ///
    /// Origin-form path/query plus the CONNECT authority; `Forwarded` and
    /// `X-Forwarded-*` headers are never trusted for reconstruction.
    fn upstream_uri(
        connect: &NormalizedTarget,
        path: &str,
        query: Option<&str>,
    ) -> Result<http::Uri, ServiceError> {
        let path = if path.is_empty() { "/" } else { path };
        let query_suffix = query.map_or_else(String::new, |query| format!("?{query}"));
        let host = match &connect.host {
            NormalizedHost::Dns(name) => name.clone(),
            NormalizedHost::Ip(addr) => {
                if addr.is_ipv6() {
                    format!("[{addr}]")
                } else {
                    addr.to_string()
                }
            }
        };
        format!(
            "https://{host}:{port}{path}{query_suffix}",
            port = connect.port
        )
        .parse()
        .map_err(|_| ServiceError::rejected(400, "invalid request target"))
    }
}

impl Service for MitmService {
    fn request_body_policy(
        &self,
        _head: &eggserve_primitives::request_head::RequestHead,
    ) -> RequestBodyPolicy {
        RequestBodyPolicy::Stream {
            max_bytes: self.inner.max_body_bytes,
        }
    }

    fn call(&self, request: Request) -> ServiceFuture<'_> {
        Box::pin(async move {
            use crate::proxy::{
                FAILURE_AUTHORITY_MISMATCH, FAILURE_INVALID_AUTHORITY, FAILURE_UNSUPPORTED,
            };
            let stats = self.inner.stats.clone();
            if self.poisoned.load(Ordering::Acquire) {
                return Err(ServiceError::rejected(
                    400,
                    "connection authority failed; tunnel is closed to new requests",
                ));
            }
            let (head, body, _connection) = request.into_parts();
            // Decrypted requests are origin-form; absolute-form inside a
            // tunnel would smuggle a second authority.
            if head.target().form() != RequestTargetForm::Origin {
                self.poisoned.store(true, Ordering::Release);
                stats.record_failure(FAILURE_INVALID_AUTHORITY);
                return Err(ServiceError::rejected(
                    400,
                    "intercepted request must use origin-form",
                ));
            }
            let host_header = head
                .authority()
                .map(eggserve_primitives::Authority::as_str)
                .map(str::to_owned);
            if check_http_authority_coherence(&self.connect, host_header.as_deref()).is_err() {
                self.poisoned.store(true, Ordering::Release);
                stats.record_failure(FAILURE_AUTHORITY_MISMATCH);
                return Err(ServiceError::rejected(
                    421,
                    "request authority does not match the intercepted origin",
                ));
            }
            let mut upstream_headers = http::HeaderMap::new();
            for field in head.headers().iter() {
                if field.name.as_str().eq_ignore_ascii_case("host") {
                    continue;
                }
                let name = http::header::HeaderName::from_bytes(field.name.as_str().as_bytes())
                    .map_err(|_| ServiceError::rejected(400, "invalid request header"))?;
                let value = http::HeaderValue::from_bytes(field.value.as_bytes())
                    .map_err(|_| ServiceError::rejected(400, "invalid request header"))?;
                upstream_headers.append(name, value);
            }
            let filtered = crate::headers::filter_proxy_headers(upstream_headers);
            if filtered.upgrade_requested {
                // WSS and other upgrades are unsupported inside interception;
                // callers needing them must use an explicit tunnel policy.
                stats.record_failure(FAILURE_UNSUPPORTED);
                return Err(ServiceError::rejected(
                    400,
                    "upgrade inside interception is unsupported",
                ));
            }
            let uri =
                Self::upstream_uri(&self.connect, head.target().path(), head.target().query())?;
            let method = head.method().as_str().to_owned();
            let stream_body = StreamBody::new(crate::proxy::proxy_body_stream(body));
            let mut outbound = http::Request::builder()
                .method(method.as_str())
                .uri(uri)
                .body(stream_body)
                .map_err(|error| ServiceError::internal(error.to_string()))?;
            *outbound.headers_mut() = filtered.forwarded;
            let Ok(flow) = record_request_with_session(
                &self.inner.client,
                &self.inner.session,
                outbound,
                &self.inner.redaction,
                &self.inner.profile_id,
                self.inner.max_structured_bytes,
                Some(self.inner.physical_route.clone()),
            )
            .await
            else {
                self.inner
                    .stats
                    .record_failure(crate::proxy::FAILURE_UPSTREAM_FAILED);
                return Err(ServiceError::rejected(502, "upstream request failed"));
            };
            self.inner.stats.record_flow();
            crate::proxy::flow_to_response(&self.inner.session, flow)
        })
    }
}

/// Serve one intercepted `CONNECT`: leaf issuance (pre-`200`), tunnel
/// acceptance (`200`), TLS termination, and the decrypted H1 driver.
///
/// Leaf issuance happens before acceptance so `200` is sent only when
/// interception can proceed. Everything after acceptance is a tunnel/TLS
/// outcome: failures close the connection with a bounded operational event
/// and never fabricate an HTTP flow.
pub(crate) async fn serve_intercepted_connect(
    capability: TunnelCapability,
    lifecycle: RequestLifecycle,
    target: NormalizedTarget,
    mitm: Arc<MitmInner>,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<Response, ServiceError> {
    let leaf = mitm
        .issuer
        .issue(&target.host)
        .await
        .map_err(|_: LeafError| ServiceError::rejected(502, "interception leaf unavailable"))?;
    let server_config = build_intercept_server_config(&leaf, &mitm.ca_der)
        .map_err(|_| ServiceError::rejected(502, "interception unavailable"))?;
    let acceptor = TlsAcceptor::from(server_config);
    capability
        .accept(HeaderBlock::new(), move |client_io| async move {
            // Hold the tunnel-concurrency permit for the whole decrypted
            // connection; it releases when the tunnel ends.
            let _permit = permit;
            serve_decrypted(client_io, acceptor, target, mitm, lifecycle).await;
        })
        .map_err(|_| ServiceError::internal("tunnel accept failed"))
}

/// Terminated client TLS stream plus its truthful session metadata.
struct DecryptedTls {
    /// The decrypted byte stream for the H1 driver.
    stream: tokio_rustls::server::TlsStream<TunnelIo>,
    /// Truthful HTTPS session description for the connection context.
    info: TlsInfo,
}

/// Perform the bounded client handshake and validate ALPN/SNI coherence.
///
/// Returns the decrypted stream with its [`TlsInfo`] on success; records a
/// bounded operational event and returns `None` for every tunnel/TLS
/// failure (no HTTP flow is ever fabricated here).
async fn accept_decrypted_tls(
    io: TunnelIo,
    acceptor: &TlsAcceptor,
    target: &NormalizedTarget,
    handshake_timeout: Duration,
    events: &Arc<TunnelEventLog>,
    stats: &Arc<crate::proxy::ProxyStats>,
    started: Instant,
) -> Option<DecryptedTls> {
    use crate::proxy::{FAILURE_AUTHORITY_MISMATCH, FAILURE_TLS_FAILED, FAILURE_UNSUPPORTED};
    let host = target.host.as_str();
    let port = target.port;
    let finish = |outcome: TunnelOutcome, error: Option<String>| {
        events.push(TunnelEvent::new_with_action(
            &host,
            port,
            "intercept",
            &TunnelRelaySummary::observed(outcome, 0, 0, started.elapsed()),
            error,
        ));
    };
    // Bound the handshake so a silent peer cannot park the tunnel forever.
    let accepted = tokio::time::timeout(handshake_timeout, acceptor.accept(io)).await;
    let Ok(Ok(tls)) = accepted else {
        stats.record_failure(FAILURE_TLS_FAILED);
        finish(
            TunnelOutcome::RelayError,
            Some("client TLS handshake failed".to_owned()),
        );
        return None;
    };
    let (_, connection) = tls.get_ref();
    // Never advertise `h2`; a client offering only `h2` fails the handshake
    // with a fatal alert instead of serving H2 bytes as H1.
    let alpn: Option<String> = match connection.alpn_protocol() {
        None => None,
        Some(selected) if selected == INTERCEPT_ALPN_HTTP1_1 => Some("http/1.1".to_owned()),
        Some(_) => {
            stats.record_failure(FAILURE_UNSUPPORTED);
            finish(
                TunnelOutcome::RelayError,
                Some("negotiated ALPN is not http/1.1".to_owned()),
            );
            return None;
        }
    };
    let sni = connection.server_name().map(str::to_owned);
    if check_sni_coherence(target, sni.as_deref()).is_err() {
        stats.record_failure(FAILURE_AUTHORITY_MISMATCH);
        finish(
            TunnelOutcome::RelayError,
            Some("CONNECT/SNI authority mismatch".to_owned()),
        );
        return None;
    }
    let version_token = connection
        .protocol_version()
        .map_or("TLS-unknown", protocol_version_token);
    let info = TlsInfo {
        protocol_version: Some(version_token.to_owned()),
        server_name: sni,
        alpn,
        client_authenticated: false,
        peer_certificates_present: false,
        peer_certificate_chain: None,
    };
    Some(DecryptedTls { stream: tls, info })
}

/// Terminate client TLS and drive decrypted HTTP/1.1 to completion.
async fn serve_decrypted(
    io: TunnelIo,
    acceptor: TlsAcceptor,
    target: NormalizedTarget,
    mitm: Arc<MitmInner>,
    lifecycle: RequestLifecycle,
) {
    let started = Instant::now();
    let host = target.host.as_str();
    let port = target.port;
    let finish = |outcome: TunnelOutcome, error: Option<String>| {
        mitm.events.push(TunnelEvent::new_with_action(
            &host,
            port,
            "intercept",
            &TunnelRelaySummary::observed(outcome, 0, 0, started.elapsed()),
            error,
        ));
    };

    let Some(decrypted) = accept_decrypted_tls(
        io,
        &acceptor,
        &target,
        mitm.handshake_timeout,
        &mitm.events,
        &mitm.stats,
        started,
    )
    .await
    else {
        return;
    };
    let context = ConnectionContext::for_non_socket(Scheme::Https, Some(decrypted.info));
    let service = MitmService {
        inner: Arc::clone(&mitm),
        connect: target,
        poisoned: Arc::new(AtomicBool::new(false)),
    };
    let shutdown = ConnectionShutdown::new();
    let signal = shutdown.clone();
    let policy = Arc::clone(&mitm.inner_policy);
    let state = Arc::clone(&mitm.inner_state);
    // The driver future borrows a token owned by its own task; the shared
    // flag lets this task request graceful shutdown from outside.
    let serve = tokio::spawn(async move {
        serve_http1_connection_with_policy(
            decrypted.stream,
            service,
            policy,
            context,
            state,
            &shutdown,
        )
        .await
    });
    // Race the driver against request-lifecycle cancellation so server
    // shutdown (or a vanished peer) ends the decrypted connection instead of
    // serving keep-alive forever.
    loop {
        if lifecycle.is_cancelled() {
            signal.shutdown();
            break;
        }
        if serve.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Give graceful shutdown a brief window, then reclaim the task so
    // proxy shutdown never hangs on an idle decrypted connection.
    let outcome = if serve.is_finished() {
        serve.await.ok()
    } else {
        let graceful = tokio::time::timeout(Duration::from_secs(5), serve).await;
        graceful.ok().and_then(Result::ok)
    };
    match outcome {
        None => finish(
            TunnelOutcome::Shutdown,
            Some("intercepted connection cancelled".to_owned()),
        ),
        Some(outcome) if outcome.is_clean() => finish(TunnelOutcome::Completed, None),
        Some(ConnectionOutcome::Shutdown) => {
            finish(
                TunnelOutcome::Shutdown,
                Some("intercepted connection shut down".to_owned()),
            );
        }
        Some(ended) => finish(
            TunnelOutcome::RelayError,
            Some(bound_diagnostic(&format!(
                "decrypted connection ended: {ended}"
            ))),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ca::{CaAuthority, CaOptions};
    use crate::policy::{
        HostMatch, PortMatch, RequestKind, Rule, RuleAction, TargetPolicy, normalize_authority,
    };

    fn connect_target(authority: &str) -> NormalizedTarget {
        crate::policy::normalize_authority(authority, 443).expect("valid test authority")
    }

    fn armed_policy() -> TargetPolicy {
        TargetPolicy::new(
            vec![Rule::new(
                HostMatch::exact_dns("example.test").unwrap(),
                PortMatch::exact(443).unwrap(),
                RequestKind::Any,
                RuleAction::Allow,
            )],
            ConnectAction::Intercept,
        )
        .unwrap()
    }

    #[test]
    fn intercept_policy_requires_an_armed_default() {
        assert!(MitmPolicy::new(armed_policy()).is_ok());
        let tunnel = TargetPolicy::new(Vec::new(), ConnectAction::Tunnel).unwrap();
        assert_eq!(
            MitmPolicy::new(tunnel).unwrap_err(),
            MitmError::PolicyNotArmed
        );
        let deny = TargetPolicy::new(Vec::new(), ConnectAction::Deny).unwrap();
        assert!(MitmPolicy::new(deny).is_err());
    }

    #[test]
    fn intercept_resolves_only_for_explicitly_allowed_targets() {
        let policy = MitmPolicy::new(armed_policy()).unwrap();
        let allowed = normalize_host("example.test").unwrap();
        assert!(policy.allows_intercept(&allowed, 443));
        assert!(!policy.allows_intercept(&allowed, 8443));
        let other = normalize_host("other.test").unwrap();
        assert!(!policy.allows_intercept(&other, 443));
    }

    #[test]
    fn sni_coherence_accepts_absence_and_normalized_match() {
        let connect = connect_target("Example.TEST.:443");
        assert!(check_sni_coherence(&connect, None).is_ok());
        assert!(check_sni_coherence(&connect, Some("example.test")).is_ok());
        assert!(check_sni_coherence(&connect, Some("Example.TEST.")).is_ok());
        assert!(check_sni_coherence(&connect, Some("other.test")).is_err());
        assert!(check_sni_coherence(&connect, Some("")).is_err());
    }

    #[test]
    fn sni_must_not_redirect_ip_connect_elsewhere() {
        let connect = connect_target("127.0.0.1:443");
        assert!(check_sni_coherence(&connect, None).is_ok());
        assert!(check_sni_coherence(&connect, Some("example.test")).is_err());
    }

    #[test]
    fn http_authority_must_match_connect_with_port_compatibility() {
        let connect = connect_target("example.test:443");
        // Bare Host inherits the CONNECT port.
        assert!(check_http_authority_coherence(&connect, Some("example.test")).is_ok());
        assert!(check_http_authority_coherence(&connect, Some("example.test:443")).is_ok());
        assert!(check_http_authority_coherence(&connect, Some("EXAMPLE.test.")).is_ok());
        assert!(check_http_authority_coherence(&connect, Some("other.test")).is_err());
        assert!(check_http_authority_coherence(&connect, Some("example.test:8443")).is_err());
        assert!(check_http_authority_coherence(&connect, None).is_err());
        assert!(check_http_authority_coherence(&connect, Some("user@example.test")).is_err());
    }

    #[test]
    fn full_coherence_combines_sni_and_http_checks() {
        let connect = connect_target("example.test:443");
        assert!(
            check_authority_coherence(&connect, Some("example.test"), Some("example.test")).is_ok()
        );
        assert!(
            check_authority_coherence(&connect, Some("other.test"), Some("example.test")).is_err()
        );
        assert!(
            check_authority_coherence(&connect, Some("example.test"), Some("other.test")).is_err()
        );
    }

    #[tokio::test]
    async fn server_config_advertises_only_http11() {
        let root = tempfile::TempDir::new().expect("temp root");
        let ca =
            CaAuthority::create_new(&root.path().join("ca"), &CaOptions::default()).expect("CA");
        let issuer = LeafIssuer::new(ca, &crate::leaf::LeafOptions::default()).expect("issuer");
        let leaf = issuer
            .issue(&normalize_host("example.test").expect("host"))
            .await
            .expect("leaf");
        let config =
            build_intercept_server_config(&leaf, issuer.ca().cert_der()).expect("server config");
        assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }

    #[test]
    fn upstream_uri_builds_canonical_https_targets() {
        let dns = connect_target("example.test:8443");
        let uri = MitmService::upstream_uri(&dns, "/a", Some("b=1&b=2")).unwrap();
        assert_eq!(uri, "https://example.test:8443/a?b=1&b=2");
        let bare = MitmService::upstream_uri(&dns, "", None).unwrap();
        assert_eq!(bare, "https://example.test:8443/");
        let ip = normalize_authority("127.0.0.1:443", 443).unwrap();
        let uri = MitmService::upstream_uri(&ip, "/x", None).unwrap();
        assert_eq!(uri, "https://127.0.0.1:443/x");
        let v6 = normalize_authority("[::1]:443", 443).unwrap();
        let uri = MitmService::upstream_uri(&v6, "/", None).unwrap();
        assert_eq!(uri, "https://[::1]:443/");
    }

    #[test]
    fn mitm_config_debug_is_redacted() {
        let root = tempfile::TempDir::new().expect("temp root");
        let ca =
            CaAuthority::create_new(&root.path().join("ca"), &CaOptions::default()).expect("CA");
        let issuer =
            Arc::new(LeafIssuer::new(ca, &crate::leaf::LeafOptions::default()).expect("issuer"));
        let config = MitmConfig::new(issuer);
        let debug = format!("{config:?}");
        assert!(!debug.contains("PRIVATE KEY"));
        assert!(!debug.contains("BEGIN"));
    }

    #[test]
    fn mitm_errors_carry_no_key_material() {
        for error in [
            MitmError::PolicyNotArmed,
            MitmError::LeafUnavailable,
            MitmError::ServerConfig,
            MitmError::ConnectionPolicy,
            MitmError::TlsHandshake,
            MitmError::AlpnUnsupported,
            MitmError::AuthorityMismatch,
            MitmError::UpgradeUnsupported,
            MitmError::NotOriginForm,
            MitmError::Upstream,
            MitmError::NotConfigured,
        ] {
            let text = error.to_string();
            assert!(!text.contains("PRIVATE KEY"), "leak in {text}");
            assert!(!text.contains("BEGIN"), "leak in {text}");
        }
        assert_eq!(bound_diagnostic(&"x".repeat(1024)).len(), DIAGNOSTIC_LEN);
    }
}
