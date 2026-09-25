//! M013F resource audit: pin every hard bound so drift fails.
//!
//! Each assertion below references the defining constant; changing a bound
//! without updating this test (and the threat-model resource table) is a
//! compile-visible failure by design. Hermetic and local-only: no sockets,
//! no network, no CA issuance beyond in-process unit constructors.

use std::time::Duration;

use eggreplay_intercept::{
    CA_DIR_MODE, CA_FORMAT_VERSION, CA_KEY_MODE, CA_PUBLIC_MODE, DEFAULT_CA_COMMON_NAME,
    DEFAULT_CA_VALIDITY_DAYS, DEFAULT_LEAF_VALIDITY_HOURS, DEFAULT_MAX_PROXY_BODY_BYTES,
    DEFAULT_TLS_HANDSHAKE_TIMEOUT, DEFAULT_TUNNEL_CONNECT_TIMEOUT, DEFAULT_TUNNEL_IDLE_TIMEOUT,
    DEFAULT_TUNNEL_MAX_BYTES, DEFAULT_TUNNEL_MAX_CONCURRENT, DEFAULT_TUNNEL_MAX_DURATION,
    INTERCEPT_POLICY_VERSION, MAX_CA_SUBJECT_CN_CHARS, MAX_CA_VALIDITY_DAYS,
    MAX_CONCURRENT_TLS_HANDSHAKES_UNDER_DEFAULT_LIMITS, MAX_HOST_PATTERN_LEN,
    MAX_LEAF_CACHE_ENTRIES, MAX_LEAF_CN_CHARS, MAX_LEAF_VALIDITY_HOURS, MAX_METADATA_BYTES,
    MAX_PEM_FILE_BYTES, MAX_POLICY_FILE_BYTES, MAX_POLICY_RULES, MAX_PORT_SET_SIZE,
    MIN_CA_VALIDITY_DAYS, MIN_LEAF_VALIDITY_HOURS, PROXY_FAILURE_CATEGORIES, TunnelEventLog,
    TunnelLimits,
};

#[test]
fn policy_bounds_are_pinned() {
    assert_eq!(MAX_POLICY_RULES, 128, "TargetPolicy rule ceiling");
    assert_eq!(MAX_HOST_PATTERN_LEN, 253, "host pattern ceiling");
    assert_eq!(MAX_PORT_SET_SIZE, 64, "port-set ceiling");
    assert_eq!(MAX_POLICY_FILE_BYTES, 64 * 1024, "policy file ceiling");
    assert_eq!(
        INTERCEPT_POLICY_VERSION, "eggreplay-intercept-policy/v1",
        "policy version must not drift silently"
    );
}

#[test]
fn ca_input_bounds_are_pinned() {
    assert_eq!(MAX_PEM_FILE_BYTES, 64 * 1024, "PEM input ceiling");
    assert_eq!(MAX_METADATA_BYTES, 64 * 1024, "metadata input ceiling");
    assert_eq!(MAX_CA_SUBJECT_CN_CHARS, 128, "CA CN ceiling");
    assert_eq!(DEFAULT_CA_COMMON_NAME, "EggReplay Interception CA");
    assert_eq!(DEFAULT_CA_VALIDITY_DAYS, 365);
    assert_eq!(MIN_CA_VALIDITY_DAYS, 31);
    assert_eq!(MAX_CA_VALIDITY_DAYS, 1825);
    assert_eq!(CA_FORMAT_VERSION, 1);
    assert_eq!(CA_DIR_MODE, 0o700, "Unix CA directory mode");
    assert_eq!(CA_KEY_MODE, 0o600, "Unix CA key mode");
    assert_eq!(CA_PUBLIC_MODE, 0o644, "Unix CA public-file mode");
}

#[test]
fn leaf_bounds_are_pinned() {
    assert_eq!(MAX_LEAF_CACHE_ENTRIES, 128, "leaf cache ceiling");
    assert_eq!(DEFAULT_LEAF_VALIDITY_HOURS, 168, "7-day default");
    assert_eq!(MIN_LEAF_VALIDITY_HOURS, 1);
    assert_eq!(MAX_LEAF_VALIDITY_HOURS, 720, "30-day maximum");
    assert_eq!(MAX_LEAF_CN_CHARS, 64);
}

#[test]
fn tunnel_bounds_are_pinned() {
    assert_eq!(DEFAULT_TUNNEL_MAX_BYTES, 256 * 1024 * 1024);
    assert_eq!(DEFAULT_TUNNEL_MAX_DURATION, Duration::from_secs(300));
    assert_eq!(DEFAULT_TUNNEL_IDLE_TIMEOUT, Duration::from_secs(60));
    assert_eq!(DEFAULT_TUNNEL_MAX_CONCURRENT, 16);
    assert_eq!(DEFAULT_TUNNEL_CONNECT_TIMEOUT, Duration::from_secs(10));
    assert_eq!(TunnelEventLog::MAX_EVENTS, 256, "diagnostic event ceiling");
    let defaults = TunnelLimits::default();
    assert_eq!(defaults.max_bytes, DEFAULT_TUNNEL_MAX_BYTES);
    assert_eq!(defaults.max_duration, DEFAULT_TUNNEL_MAX_DURATION);
    assert_eq!(defaults.idle_timeout, DEFAULT_TUNNEL_IDLE_TIMEOUT);
    assert_eq!(defaults.max_concurrent, DEFAULT_TUNNEL_MAX_CONCURRENT);
    assert_eq!(defaults.connect_timeout, DEFAULT_TUNNEL_CONNECT_TIMEOUT);
}

#[test]
fn tls_handshake_bounds_track_the_tunnel_semaphore() {
    // Handshake concurrency shares the tunnel permit (no second semaphore);
    // the timeout tracks the tunnel connect timeout.
    assert_eq!(
        DEFAULT_TLS_HANDSHAKE_TIMEOUT, DEFAULT_TUNNEL_CONNECT_TIMEOUT,
        "handshake timeout must track the tunnel connect timeout"
    );
    assert_eq!(
        MAX_CONCURRENT_TLS_HANDSHAKES_UNDER_DEFAULT_LIMITS, DEFAULT_TUNNEL_MAX_CONCURRENT,
        "handshake concurrency must track the tunnel concurrency ceiling"
    );
}

#[test]
fn proxy_admission_and_diagnostics_bounds_are_pinned() {
    assert_eq!(
        DEFAULT_MAX_PROXY_BODY_BYTES,
        8 * 1024 * 1024,
        "proxied request-body ceiling"
    );
    assert_eq!(
        PROXY_FAILURE_CATEGORIES.len(),
        9,
        "failure categories stay a fixed bounded set"
    );
    for category in [
        "policy_denied",
        "invalid_authority",
        "upstream_failed",
        "tls_failed",
        "authority_mismatch",
        "limit_reached",
        "not_configured",
        "unsupported",
        "internal",
    ] {
        assert!(
            PROXY_FAILURE_CATEGORIES.contains(&category),
            "missing failure category {category}"
        );
    }
}

#[test]
fn inherited_session_bounds_are_pinned() {
    // Request/body/session sizes are inherited from EggReplay authorities;
    // pin the values the interception paths rely on.
    assert_eq!(
        eggreplay_core::DEFAULT_MAX_STRUCTURED_REDACTION_BYTES,
        1024 * 1024,
        "structured-redaction staging ceiling"
    );
    assert_eq!(
        eggreplay_core::stream::MAX_STREAM_EVENTS_PER_FLOW,
        4096,
        "stream-event ceiling per flow"
    );
    assert_eq!(
        eggreplay_core::stream::MAX_SSE_BODY_BYTES,
        16 * 1024 * 1024,
        "SSE parser cap"
    );
    let store = eggreplay_store::StoreLimits::default();
    assert_eq!(store.max_line_bytes, 4 * 1024 * 1024);
    assert_eq!(store.max_blob_bytes, 64 * 1024 * 1024);
    assert_eq!(store.max_flows, 100_000);
    assert_eq!(store.max_total_bytes, 512 * 1024 * 1024);
}

#[test]
fn listener_profile_bounds_are_pinned() {
    let bind: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let profile = eggreplay_intercept::InterceptionProfile::loopback(bind);
    assert_eq!(profile.max_request_body_bytes, 8 * 1024 * 1024);
    assert_eq!(profile.max_request_target_bytes, 8 * 1024);
    assert_eq!(profile.max_in_flight_requests, 64);
    assert_eq!(profile.max_active_tunnels, 64);
    assert_eq!(profile.max_connections, 64);
    assert_eq!(profile.connection_total_timeout, Duration::from_secs(60));
    assert_eq!(profile.keep_alive_idle_timeout, Duration::from_secs(60));
    assert_eq!(profile.response_write_timeout, Duration::from_secs(30));
}

#[test]
fn substrate_versions_are_pinned() {
    use eggreplay_intercept::substrate;
    assert_eq!(substrate::EGGSERVE_SERVER, "0.3.0");
    assert_eq!(substrate::EGGSERVE_PRIMITIVES, "0.2.1");
    assert_eq!(substrate::EGGRESS_OUTBOUND, "1.0.8");
    assert_eq!(substrate::EGGNET_TLS, "0.2.0");
    assert_eq!(substrate::RUSTLS, "0.23.45");
    assert_eq!(substrate::TOKIO_RUSTLS, "0.26.2");
    assert_eq!(substrate::RCGEN, "0.13.2");
    assert_eq!(substrate::X509_PARSER, "0.16.0");
}
