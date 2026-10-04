//! Inbound serving protocol policy shared by the recording gateway and the
//! offline replay server (M015B, ADR 0010).
//!
//! This module is a **policy and composition** boundary, not a second
//! transport. It answers one question: which protocols may this listener
//! accept, and how is the listener built so that exactly one matcher, store,
//! redaction, scenario, and response-rendering authority serves all of them?
//!
//! # Why an enum and not a flag
//!
//! HTTP/1.1 is served by the direct EggServe runtime (`eggserve-server`).
//! HTTP/2 is served by EggServe Core, the multiprotocol composition layer
//! (M015A). Those are two different crates, but not two different products:
//! `eggserve_core::server::Service` *is* `eggserve_server::service::Service`,
//! and `eggserve_core::server::Request` *is* `eggserve_primitives::Request`.
//! So selecting an HTTP/2 variant hands the existing service implementation
//! to a different runtime. It does not fork the runtime, and it cannot fork
//! any product authority, because there is only one service to fork.
//!
//! The variants are explicit about *which* protocol the operator selected,
//! because cleartext HTTP/2 and TLS+ALPN HTTP/2 are different trust and
//! exposure decisions and should not be a single boolean. [`InboundProtocol::Http1`]
//! is the default everywhere and is what every existing caller gets.
//!
//! # Feature boundary
//!
//! Everything HTTP/2 is behind the opt-in `h2-inbound` feature (and, for TLS,
//! `h2-inbound-tls`). In a build without those features the HTTP/2 variants
//! do not exist, so selecting inbound HTTP/2 is a compile-time error rather
//! than a runtime surprise. See `plans/adrs/0010-inbound-http2-serving-boundary.md`.

use std::net::SocketAddr;
use std::time::Duration;

#[cfg(feature = "eggserve")]
use eggserve_server::Service;

/// Which inbound protocols one listener accepts.
///
/// The default is [`InboundProtocol::Http1`], which is the pre-M015B
/// behaviour in full: the direct EggServe runtime, unchanged. Selecting an
/// HTTP/2 variant is an explicit operator decision that admits EggServe Core
/// behind the opt-in feature boundary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum InboundProtocol {
    /// HTTP/1.1 only, on the direct EggServe runtime. The default.
    #[default]
    Http1,
    /// Cleartext prior-knowledge HTTP/2 (`h2c`) on the same listener.
    ///
    /// EggServe classifies a cleartext stream as HTTP/2 only when the
    /// complete 24-byte HTTP/2 connection preface arrives; a stream that
    /// diverges at any byte is HTTP/1.1 (`eggserve-core`
    /// `connection/driver.rs`, `H2_PREFACE`). A client therefore selects
    /// the protocol by speaking it. This variant cannot be entered by
    /// accidental sniffing, and it is never a silent downgrade: a client
    /// that does not send the preface is served HTTP/1.1 exactly as before.
    ///
    /// Cleartext HTTP/2 is unencrypted, so it exposes recorded traffic in
    /// the clear on that port. It exists because the offline replay server
    /// and the recording gateway are operator-scheduled local listeners;
    /// it is not a default, and it is not for untrusted networks.
    #[cfg(feature = "h2-inbound")]
    Http2Cleartext,
    /// TLS with ALPN `h2` preferred and `http/1.1` also accepted, using
    /// operator-supplied identity material.
    ///
    /// ALPN is explicit, so a client that supports HTTP/1.1 and does not
    /// offer `h2` is served HTTP/1.1 over the same port. The advertised
    /// protocol list is the operator's, derived from the certificate and
    /// key EggReplay is given — never sniffed and never downgraded.
    #[cfg(feature = "h2-inbound-tls")]
    Http2Tls {
        /// PEM certificate chain presented to clients.
        certificate: std::path::PathBuf,
        /// PEM private key for `certificate`.
        private_key: std::path::PathBuf,
    },
}

impl InboundProtocol {
    /// The stable machine-readable token for this policy.
    ///
    /// This is what CLI/JSON status output reports. It never includes key
    /// material, file contents, or identity details beyond the token.
    pub const fn token(&self) -> &'static str {
        match self {
            Self::Http1 => "http1",
            #[cfg(feature = "h2-inbound")]
            Self::Http2Cleartext => "http2-cleartext",
            #[cfg(feature = "h2-inbound-tls")]
            Self::Http2Tls { .. } => "http2-tls",
        }
    }

    /// Whether this policy serves HTTP/2 in this build.
    ///
    /// A build without `h2-inbound` cannot express an HTTP/2 policy, so this
    /// is always `false` there.
    pub const fn serves_http2(&self) -> bool {
        !matches!(self, Self::Http1)
    }

    /// Whether this policy terminates TLS.
    pub const fn terminates_tls(&self) -> bool {
        #[cfg(feature = "h2-inbound-tls")]
        if matches!(self, Self::Http2Tls { .. }) {
            return true;
        }
        false
    }

    /// Whether this policy serves HTTP/2 without TLS.
    pub const fn is_cleartext(&self) -> bool {
        #[cfg(feature = "h2-inbound")]
        {
            matches!(self, Self::Http2Cleartext)
        }
        #[cfg(not(feature = "h2-inbound"))]
        {
            false
        }
    }

    /// A complete, secret-free description of this policy for operator
    /// status output.
    pub fn describe(&self) -> InboundProtocolDescription {
        InboundProtocolDescription {
            protocol: self.token(),
            serves_http2: self.serves_http2(),
            terminates_tls: self.terminates_tls(),
            cleartext: self.is_cleartext(),
        }
    }
}

/// A secret-free, machine-readable description of one serving policy.
///
/// Deliberately holds no certificate path, no key path, and no identity
/// material: operator status output is safe to log, diff, and publish.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InboundProtocolDescription {
    /// Stable policy token: `http1`, `http2-cleartext`, or `http2-tls`.
    pub protocol: &'static str,
    /// Whether HTTP/2 is served on this listener.
    pub serves_http2: bool,
    /// Whether the listener terminates TLS.
    pub terminates_tls: bool,
    /// Whether HTTP/2 is served without TLS.
    pub cleartext: bool,
}

/// EggServe-owned HTTP/2 transport limits an operator may tighten.
///
/// Every field is `Option` and defaults to `None`, which means "keep
/// EggServe's own default". EggServe hands its values to Hyper explicitly
/// so an upstream upgrade cannot silently widen the resource envelope, and
/// EggReplay must not substitute its own guesses for that policy: this type
/// can only *tighten* what EggServe already chose, never loosen it silently.
///
/// The type exists in every build so the serving signature is stable; the
/// limits are only ever read by a build that can serve HTTP/2. In an
/// HTTP/1.1-only build the value is simply not consulted, and starting an
/// HTTP/1 listener is unaffected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct H2Limits {
    /// Maximum concurrent request streams advertised per connection.
    pub max_concurrent_streams: Option<u32>,
    /// Maximum decoded header-list size accepted from peers.
    pub max_header_list_size: Option<u32>,
    /// Maximum HTTP/2 frame size emitted.
    pub max_frame_size: Option<u32>,
    /// Optional H2 keep-alive PING interval.
    pub keep_alive_interval: Option<Duration>,
}

#[cfg(feature = "h2-inbound")]
impl H2Limits {
    fn apply(
        self,
        mut config: eggserve_core::server::Http2Config,
    ) -> eggserve_core::server::Http2Config {
        if let Some(value) = self.max_concurrent_streams {
            config.max_concurrent_streams = value;
        }
        if let Some(value) = self.max_header_list_size {
            config.max_header_list_size = value;
        }
        if let Some(value) = self.max_frame_size {
            config.max_frame_size = value;
        }
        config.keep_alive_interval = self.keep_alive_interval;
        config
    }
}

/// A serving policy EggReplay cannot honour.
///
/// Every variant is a startup-time refusal. A listener never degrades to a
/// weaker protocol than the operator asked for: if the requested policy
/// cannot be built, the server does not start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InboundServingError {
    /// The build cannot express the requested protocol.
    #[error(
        "inbound protocol {0} is not available in this build; \
         rebuild with the opt-in inbound HTTP/2 feature to use it"
    )]
    Unsupported(&'static str),
    /// Operator-supplied TLS identity material could not be loaded.
    #[error("inbound TLS identity material could not be loaded: {0}")]
    TlsIdentity(String),
    /// EggServe rejected the composed runtime configuration.
    #[error("inbound serving configuration is invalid: {0}")]
    Runtime(String),
}

/// A running inbound listener, whatever protocols it was configured for.
///
/// The two variants exist only because the two EggServe runtimes are two
/// types. Every caller-visible operation is protocol-neutral, so product code
/// never branches on which runtime it got. Neither handle is `Debug`, so this
/// enum deliberately is not either: a debugging impl would have to invent a
/// representation for an EggServe handle, and an invented one could leak
/// runtime internals into operator output.
#[non_exhaustive]
pub enum InboundServerHandle {
    /// The direct EggServe HTTP/1.1 runtime.
    Http1(eggserve_server::ServerHandle),
    /// The EggServe Core multiprotocol runtime, with HTTP/2 enabled.
    #[cfg(feature = "h2-inbound")]
    Http2(eggserve_core::server::ServerHandle),
}

impl InboundServerHandle {
    /// The address the listener actually bound.
    ///
    /// Always the resolved address, so a `:0` request is reported back
    /// truthfully rather than echoing the requested port.
    pub fn local_addr(&self) -> SocketAddr {
        match self {
            Self::Http1(handle) => handle.local_addr(),
            #[cfg(feature = "h2-inbound")]
            Self::Http2(handle) => handle.local_addr(),
        }
    }

    /// Stop accepting new connections and begin graceful shutdown.
    ///
    /// On HTTP/2 this is where the server stops advertising new streams, so
    /// in-flight streams finish rather than being reset.
    pub fn shutdown(&self) {
        match self {
            Self::Http1(handle) => handle.shutdown(),
            #[cfg(feature = "h2-inbound")]
            Self::Http2(handle) => handle.shutdown(),
        }
    }

    /// Wait for the accept loop to terminate after [`InboundServerHandle::shutdown`].
    pub async fn wait(self) -> Result<(), String> {
        match self {
            Self::Http1(handle) => {
                handle.wait().await;
                Ok(())
            }
            #[cfg(feature = "h2-inbound")]
            Self::Http2(handle) => handle.wait().await.map(|_| ()).map_err(|e| e.to_string()),
        }
    }
}

/// Build and start an inbound listener for an explicit protocol policy.
///
/// `InboundProtocol::Http1` runs exactly the pre-M015B path: the direct
/// EggServe runtime, with the origin-only request-target mode and
/// eggserve-owned policy/admission that sealed replay and the recording
/// gateway both require.
///
/// `tunnel_bounds` is the operator's WebSocket tunnel bound, or `None` when
/// tunnel admission is disabled. The H1 path maps `Some(max)` to
/// `max_active_tunnels(max)`; the pre-existing replay path used `16` and the
/// recording gateway used its own option, so the bound is passed through
/// rather than re-decided here.
///
/// The `service` value is the caller's *existing* `eggserve_server::Service`
/// implementation. It is passed through to whichever runtime the policy
/// selects; this function never wraps, rewrites, or substitutes it.
#[cfg(feature = "eggserve")]
// In an HTTP/1.1-only build there is no branch that can serve HTTP/2, so the
// limit set and the HTTP/2 variants are structurally unreachable rather than
// accidentally dropped.
#[cfg_attr(not(feature = "h2-inbound"), allow(unused_variables))]
pub async fn start_inbound_server<S>(
    bind: SocketAddr,
    protocol: InboundProtocol,
    limits: H2Limits,
    max_body_bytes: u64,
    tunnel_bounds: Option<usize>,
    service: S,
) -> Result<InboundServerHandle, InboundServingError>
where
    S: Service,
{
    match protocol {
        InboundProtocol::Http1 => start_http1(bind, max_body_bytes, tunnel_bounds, service)
            .await
            .map(InboundServerHandle::Http1),
        #[cfg(feature = "h2-inbound")]
        _ => start_http2(
            bind,
            &protocol,
            limits,
            max_body_bytes,
            tunnel_bounds,
            service,
        )
        .await
        .map(InboundServerHandle::Http2),
    }
}

/// The pre-M015B direct-runtime path, unchanged.
///
/// The request-target mode and both ownership settings are the values
/// EggReplay has always required. Keeping them here — rather than relying on
/// a default — is what makes "the H1 path is untouched" a checkable claim
/// rather than a comment.
#[cfg(feature = "eggserve")]
async fn start_http1<S>(
    bind: SocketAddr,
    max_body_bytes: u64,
    tunnel_bounds: Option<usize>,
    service: S,
) -> Result<eggserve_server::ServerHandle, InboundServingError>
where
    S: Service,
{
    // The server-level body cap defaults to 0 (reject all); align it with
    // the service policy so the effective limit is the caller's bound.
    let builder = eggserve_server::RuntimeConfig::builder()
        .bind(bind)
        .max_request_body_bytes(max_body_bytes)
        .http1_request_target_mode(eggserve_server::Http1RequestTargetMode::OriginOnly)
        .policy_ownership(eggserve_server::H1PolicyOwnership::eggserve_owned())
        .admission_ownership(eggserve_server::AdmissionOwnership::eggserve_owned());
    let builder = match tunnel_bounds {
        Some(max) => builder
            .max_active_tunnels(max)
            .disable_connection_total_timeout(),
        None => builder,
    };
    let runtime = builder
        .build()
        .map_err(|error| InboundServingError::Runtime(error.to_string()))?;
    eggserve_server::Server::builder()
        .runtime(runtime)
        .build()
        .map_err(|error| InboundServingError::Runtime(error.to_string()))?
        .start_with_service(service)
        .await
        .map_err(|error| InboundServingError::Runtime(error.to_string()))
}

/// The opt-in HTTP/2 path, served by EggServe Core.
///
/// The runtime is EggServe's own composition layer. Two of its properties
/// are load-bearing and are relied on rather than reimplemented:
///
/// * Core projects HTTP/1.1 connections onto the direct H1 runtime with
///   `Http1RequestTargetMode::OriginOnly` and eggserve-owned
///   policy/admission, so a client that negotiates HTTP/1.1 over this
///   listener gets the same request-target boundary as the direct path.
/// * Core classifies a cleartext stream as HTTP/2 only on a complete
///   connection preface, so `Http2Cleartext` cannot be entered by accident.
#[cfg(feature = "h2-inbound")]
// Without `h2-inbound-tls` the policy cannot be a TLS one, so the match that
// reads it is compiled out. The HTTP/2 runtime is then cleartext-only.
#[cfg_attr(not(feature = "h2-inbound-tls"), allow(unused_variables))]
async fn start_http2<S>(
    bind: SocketAddr,
    protocol: &InboundProtocol,
    limits: H2Limits,
    max_body_bytes: u64,
    tunnel_bounds: Option<usize>,
    service: S,
) -> Result<eggserve_core::server::ServerHandle, InboundServingError>
where
    S: Service,
{
    let http2 = limits.apply(eggserve_core::server::Http2Config::default());
    let builder = eggserve_core::server::RuntimeConfig::builder()
        .bind(bind)
        .max_request_body_bytes(max_body_bytes)
        .http2(http2);
    // `tls_config` needs `eggserve-core/tls`, which is exactly the
    // `h2-inbound-tls` feature. Without it the HTTP/2 runtime is
    // cleartext-only, and the `Http2Tls` policy cannot even be constructed,
    // so there is no TLS branch to take and no identity to load.
    #[cfg(feature = "h2-inbound-tls")]
    let builder = match protocol {
        InboundProtocol::Http1 | InboundProtocol::Http2Cleartext => builder,
        InboundProtocol::Http2Tls {
            certificate,
            private_key,
        } => {
            let config = eggnet_tls::load_tls_config_with_http2(certificate, private_key, true)
                .map_err(|error| InboundServingError::TlsIdentity(error.to_string()))?;
            builder.tls_config(config)
        }
    };
    let builder = match tunnel_bounds {
        Some(max) => builder
            .max_active_tunnels(max)
            .disable_connection_total_timeout(),
        None => builder,
    };
    let runtime = builder
        .build()
        .map_err(|error| InboundServingError::Runtime(error.to_string()))?;
    let server = eggserve_core::server::Server::builder()
        .runtime(runtime)
        .build()
        .map_err(|error| InboundServingError::Runtime(error.to_string()))?;
    server
        .start_with_service(service)
        .await
        .map_err(|error| InboundServingError::Runtime(error.to_string()))
}

/// Build a serving policy from an operator-facing name.
///
/// This resolves the name-only policies. TLS is deliberately **not**
/// name-resolvable: [`InboundProtocol::Http2Tls`] carries operator
/// certificate and key material, so naming the policy without supplying the
/// identity is a refusal, not a default. Use
/// [`InboundProtocol::Http2Tls`] directly to construct it.
///
/// Returns [`InboundServingError::Unsupported`] for a name this build cannot
/// honour, so a build without `h2-inbound` fails closed on `--inbound
/// http2` instead of quietly serving HTTP/1.1 under an HTTP/2 label.
pub fn parse_protocol(name: &str) -> Result<InboundProtocol, InboundServingError> {
    match name {
        "http1" | "h1" => Ok(InboundProtocol::Http1),
        #[cfg(feature = "h2-inbound")]
        "http2" | "h2" | "h2c" | "http2-cleartext" => Ok(InboundProtocol::Http2Cleartext),
        "http2-tls" | "h2-tls" => Err(InboundServingError::TlsIdentity(
            "TLS serving requires operator certificate and key material; \
             construct the policy with the identity paths instead of naming it"
                .into(),
        )),
        _ => Err(InboundServingError::Unsupported(
            "unrecognised inbound protocol name",
        )),
    }
}

/// Report whether a policy name is understood by this build, for help text
/// and status output that must not expose key material.
pub fn supported_protocol_names() -> Vec<&'static str> {
    #[cfg_attr(not(feature = "h2-inbound"), allow(unused_mut))]
    let mut names = vec!["http1", "h1"];
    #[cfg(feature = "h2-inbound")]
    {
        names.extend_from_slice(&["http2", "h2", "h2c", "http2-cleartext"]);
    }
    #[cfg(feature = "h2-inbound-tls")]
    {
        names.extend_from_slice(&["http2-tls", "h2-tls"]);
    }
    names
}

/// The `ServiceError` re-export used by both product services, so the
/// opt-in path cannot drift onto a second error taxonomy.
#[cfg(feature = "eggserve")]
pub use eggserve_server::ServiceError as InboundServiceError;

#[cfg(test)]
mod tests {
    use super::*;

    /// The default policy is HTTP/1.1 in every build. This is the "defaults
    /// remain H1" claim, and it must hold in a build that *can* serve
    /// HTTP/2, not only in one that cannot.
    #[test]
    fn default_policy_is_http1() {
        let policy = InboundProtocol::default();
        assert_eq!(policy, InboundProtocol::Http1);
        assert!(!policy.serves_http2());
        assert!(!policy.terminates_tls());
        assert!(!policy.is_cleartext());
        assert_eq!(policy.describe().protocol, "http1");
    }

    /// Status output is secret-free: it describes the policy, never the
    /// identity behind it.
    #[test]
    fn description_never_carries_key_material() {
        let described = InboundProtocol::Http1.describe();
        let json = serde_json::to_string(&described).expect("serialize description");
        for forbidden in ["certificate", "private_key", "key", "BEGIN"] {
            assert!(
                !json.contains(forbidden),
                "operator status must not mention {forbidden}: {json}"
            );
        }
    }

    #[test]
    fn http1_names_resolve() {
        for name in ["http1", "h1"] {
            assert_eq!(
                parse_protocol(name).expect("http1 name"),
                InboundProtocol::Http1
            );
        }
    }

    /// An unrecognised name is a refusal, never a silent fallback to HTTP/1.1.
    #[test]
    fn unknown_name_is_refused() {
        let error = parse_protocol("http3").expect_err("unknown name must be refused");
        assert!(matches!(error, InboundServingError::Unsupported(_)));
    }

    /// TLS is not name-resolvable: naming the policy without supplying the
    /// operator's identity must be a refusal, not a default identity.
    #[test]
    fn tls_policy_is_not_name_resolvable() {
        for name in ["http2-tls", "h2-tls"] {
            let error = parse_protocol(name).expect_err("TLS needs identity material");
            assert!(
                matches!(error, InboundServingError::TlsIdentity(_)),
                "got {error:?} for {name}"
            );
        }
    }

    /// Cleartext HTTP/2 is only offered by a build that can serve it, and it
    /// is always labelled as cleartext.
    #[cfg(feature = "h2-inbound")]
    #[test]
    fn cleartext_policy_is_explicitly_labelled() {
        let policy = parse_protocol("h2c").expect("h2c name");
        assert_eq!(policy, InboundProtocol::Http2Cleartext);
        assert!(policy.serves_http2());
        assert!(policy.is_cleartext());
        assert!(!policy.terminates_tls());
        let described = policy.describe();
        assert_eq!(described.protocol, "http2-cleartext");
        assert!(described.serves_http2);
        assert!(described.cleartext);
        assert!(!described.terminates_tls);
    }

    /// A build without `h2-inbound` refuses the HTTP/2 name rather than
    /// serving HTTP/1.1 under an HTTP/2 label.
    #[cfg(not(feature = "h2-inbound"))]
    #[test]
    fn http2_name_is_refused_without_the_feature() {
        let error = parse_protocol("h2c").expect_err("no HTTP/2 in this build");
        assert!(matches!(error, InboundServingError::Unsupported(_)));
        assert!(
            !supported_protocol_names().contains(&"h2c"),
            "this build must not advertise an HTTP/2 name"
        );
    }

    /// Every advertised name either resolves to a startable policy or is a
    /// TLS name that additionally requires operator identity. Nothing is
    /// advertised that this build cannot act on, and nothing resolves that it
    /// does not advertise.
    #[test]
    fn advertised_names_match_what_can_be_started() {
        for name in supported_protocol_names() {
            match parse_protocol(name) {
                Ok(_) => {}
                Err(InboundServingError::TlsIdentity(_)) => {
                    assert!(
                        name.contains("tls"),
                        "only TLS names may need identity material, {name} did not"
                    );
                }
                Err(other) => panic!("advertised name {name} must resolve, got {other}"),
            }
        }
        assert!(supported_protocol_names().contains(&"http1"));
    }

    /// Operator limits are optional and default to EggServe's own values, so
    /// an unset limit never substitutes an EggReplay default.
    #[test]
    fn unset_limits_are_absent() {
        let limits = H2Limits::default();
        assert_eq!(limits.max_concurrent_streams, None);
        assert_eq!(limits.max_header_list_size, None);
        assert_eq!(limits.max_frame_size, None);
        assert_eq!(limits.keep_alive_interval, None);
    }
}
