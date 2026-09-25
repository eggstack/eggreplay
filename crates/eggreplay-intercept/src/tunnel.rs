//! Opaque `CONNECT` tunnel relay with finite bounds.
//!
//! Tunnels never parse application bytes: the relay moves bytes between the
//! `EggServe` [`TunnelIo`](eggserve_server::tunnel::TunnelIo) downstream and
//! the `Eggress`-established upstream with direct backpressure
//! (`tokio::io::copy_bidirectional`). Byte, total-duration, and
//! idle/no-progress bounds close runaway relays; half-close in either
//! direction shuts down only the opposite write half. No tunnel content
//! enters logs, fixtures, or errors: [`TunnelEvent`] carries only bounded
//! target/action/error metadata.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use eggfetch_core::DialStream;
use eggserve_server::tunnel::TunnelIo;
use tokio::io::{AsyncRead, AsyncWrite};

/// Default ceiling for total relayed bytes in both directions (256 MiB).
pub const DEFAULT_TUNNEL_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Default ceiling for one tunnel lifetime (5 minutes).
pub const DEFAULT_TUNNEL_MAX_DURATION: Duration = Duration::from_secs(300);
/// Default no-progress ceiling for one tunnel (60 seconds).
pub const DEFAULT_TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Default ceiling for concurrent tunnels admitted by one proxy.
pub const DEFAULT_TUNNEL_MAX_CONCURRENT: usize = 16;
/// Default ceiling for establishing the outbound route (10 seconds).
pub const DEFAULT_TUNNEL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound applied to relay error strings in operational events.
const ERROR_PREVIEW_LEN: usize = 128;
/// Hard ceiling for concurrent tunnels (sanity bound above `EggServe`'s own
/// `max_active_tunnels` admission).
const MAX_CONCURRENT_TUNNELS: usize = 4096;

/// Finite bounds for one opaque tunnel.
#[derive(Debug, Clone, Copy)]
pub struct TunnelLimits {
    /// Total bytes (both directions) before the relay is closed.
    pub max_bytes: u64,
    /// Total tunnel lifetime before the relay is closed.
    pub max_duration: Duration,
    /// No-progress (no bytes either direction) ceiling.
    pub idle_timeout: Duration,
    /// Concurrent tunnels admitted by one proxy service.
    pub max_concurrent: usize,
    /// Ceiling for establishing the outbound route before `200`.
    pub connect_timeout: Duration,
}

impl Default for TunnelLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_TUNNEL_MAX_BYTES,
            max_duration: DEFAULT_TUNNEL_MAX_DURATION,
            idle_timeout: DEFAULT_TUNNEL_IDLE_TIMEOUT,
            max_concurrent: DEFAULT_TUNNEL_MAX_CONCURRENT,
            connect_timeout: DEFAULT_TUNNEL_CONNECT_TIMEOUT,
        }
    }
}

impl TunnelLimits {
    /// Build explicit limits; every bound must be positive and the
    /// concurrency ceiling must fit [`MAX_CONCURRENT_TUNNELS`].
    ///
    /// # Errors
    ///
    /// Returns a message when any bound is zero or out of range.
    pub fn new(
        max_bytes: u64,
        max_duration: Duration,
        idle_timeout: Duration,
        max_concurrent: usize,
        connect_timeout: Duration,
    ) -> Result<Self, String> {
        if max_bytes == 0 {
            return Err("tunnel max_bytes must be positive".to_owned());
        }
        if max_duration.is_zero() {
            return Err("tunnel max_duration must be positive".to_owned());
        }
        if idle_timeout.is_zero() {
            return Err("tunnel idle_timeout must be positive".to_owned());
        }
        if connect_timeout.is_zero() {
            return Err("tunnel connect_timeout must be positive".to_owned());
        }
        if max_concurrent == 0 || max_concurrent > MAX_CONCURRENT_TUNNELS {
            return Err(format!(
                "tunnel max_concurrent must be 1..={MAX_CONCURRENT_TUNNELS}"
            ));
        }
        Ok(Self {
            max_bytes,
            max_duration,
            idle_timeout,
            max_concurrent,
            connect_timeout,
        })
    }
}

/// How a tunnel relay ended. Values are fixed strings; no payload included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelOutcome {
    /// Both directions reached EOF or closed cleanly.
    Completed,
    /// The byte ceiling closed the relay.
    ByteLimit,
    /// The total-duration ceiling closed the relay.
    DurationLimit,
    /// No bytes flowed for the idle ceiling.
    IdleTimeout,
    /// A transport error ended the relay (bounded detail in the event).
    RelayError,
    /// The relay task was cancelled externally (e.g. server shutdown).
    Shutdown,
}

impl TunnelOutcome {
    /// Fixed machine-readable outcome name for operational events.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::ByteLimit => "byte-limit",
            Self::DurationLimit => "duration-limit",
            Self::IdleTimeout => "idle-timeout",
            Self::RelayError => "relay-error",
            Self::Shutdown => "shutdown",
        }
    }
}

/// Summary of one finished relay (no application bytes).
#[derive(Debug, Clone, Copy)]
pub struct TunnelRelaySummary {
    /// How the relay ended.
    pub outcome: TunnelOutcome,
    /// Bytes moved client -> target.
    pub bytes_client_to_target: u64,
    /// Bytes moved target -> client.
    pub bytes_target_to_client: u64,
    /// Wall-clock relay duration.
    pub duration: Duration,
    /// Whether the byte ceiling fired (distinguishes limit errors).
    limit_fired: bool,
}

impl TunnelRelaySummary {
    /// Summary for a relay cancelled before producing counts (e.g. request
    /// lifecycle cancellation during server shutdown).
    #[must_use]
    pub fn shutdown() -> Self {
        Self {
            outcome: TunnelOutcome::Shutdown,
            bytes_client_to_target: 0,
            bytes_target_to_client: 0,
            duration: Duration::ZERO,
            limit_fired: false,
        }
    }

    /// Summary for an observed relay with known directional counts.
    ///
    /// M013D interception outcomes reuse the bounded tunnel event shape;
    /// byte counts are the decrypted-stream counts when known, else zero.
    #[must_use]
    pub fn observed(
        outcome: TunnelOutcome,
        bytes_client_to_target: u64,
        bytes_target_to_client: u64,
        duration: Duration,
    ) -> Self {
        Self {
            outcome,
            bytes_client_to_target,
            bytes_target_to_client,
            duration,
            limit_fired: false,
        }
    }
}

/// Operational event for one tunnel: bounded target/action/error metadata.
///
/// Never carries application bytes, credentials, or key material.
#[derive(Debug, Clone)]
pub struct TunnelEvent {
    /// Normalized target host (policy-checked; never userinfo).
    pub host: String,
    /// Target port.
    pub port: u16,
    /// Action taken; always `"tunnel"` in M013B.
    pub action: &'static str,
    /// [`TunnelOutcome::as_str`] value.
    pub outcome: &'static str,
    /// Bytes moved client -> target.
    pub bytes_client_to_target: u64,
    /// Bytes moved target -> client.
    pub bytes_target_to_client: u64,
    /// Wall-clock relay duration in milliseconds.
    pub duration_ms: u64,
    /// Bounded transport detail for error outcomes only.
    pub error: Option<String>,
}

impl TunnelEvent {
    /// Build an event from a relay summary and its policy-checked target.
    #[must_use]
    pub fn new(host: &str, port: u16, summary: &TunnelRelaySummary, error: Option<String>) -> Self {
        Self::new_with_action(host, port, "tunnel", summary, error)
    }

    /// Build an event with an explicit action label.
    ///
    /// M013B tunnels always use `"tunnel"` (see [`new`](Self::new)); M013D
    /// interception outcomes use `"intercept"`. Labels are fixed strings.
    #[must_use]
    pub fn new_with_action(
        host: &str,
        port: u16,
        action: &'static str,
        summary: &TunnelRelaySummary,
        error: Option<String>,
    ) -> Self {
        let host_preview: String = host.chars().take(253).collect();
        Self {
            host: host_preview,
            port,
            action,
            outcome: summary.outcome.as_str(),
            bytes_client_to_target: summary.bytes_client_to_target,
            bytes_target_to_client: summary.bytes_target_to_client,
            duration_ms: u64::try_from(summary.duration.as_millis()).unwrap_or(u64::MAX),
            error: error.map(|message| message.chars().take(ERROR_PREVIEW_LEN).collect()),
        }
    }
}

/// Bounded in-memory sink for tunnel operational events.
#[derive(Debug, Default)]
pub struct TunnelEventLog {
    inner: std::sync::Mutex<Vec<TunnelEvent>>,
}

impl TunnelEventLog {
    /// Maximum retained events; older events are dropped.
    pub const MAX_EVENTS: usize = 256;

    /// Create an empty log.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Record one event, dropping the oldest when bounded.
    pub fn push(&self, event: TunnelEvent) {
        if let Ok(mut guard) = self.inner.lock() {
            if guard.len() >= Self::MAX_EVENTS {
                guard.remove(0);
            }
            guard.push(event);
        }
    }

    /// Snapshot recorded events.
    #[must_use]
    pub fn snapshot(&self) -> Vec<TunnelEvent> {
        self.inner
            .lock()
            .map_or_else(|_| Vec::new(), |guard| guard.clone())
    }

    /// Number of recorded events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().map_or(0, |guard| guard.len())
    }

    /// Whether any event was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Byte-counting wrapper that fails reads once the shared budget is spent.
struct Metered<S> {
    inner: S,
    /// Bytes read through this direction.
    own: Arc<AtomicU64>,
    /// Bytes read through the opposite direction.
    peer: Arc<AtomicU64>,
    limit_hit: Arc<AtomicBool>,
    limit: u64,
}

impl<S: AsyncRead + Unpin> AsyncRead for Metered<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        match std::pin::Pin::new(&mut self.inner).poll_read(context, buf) {
            std::task::Poll::Ready(Ok(())) => {
                let fresh = buf.filled().len().saturating_sub(before) as u64;
                if fresh > 0 {
                    let own_total = self
                        .own
                        .fetch_add(fresh, Ordering::Relaxed)
                        .saturating_add(fresh);
                    if own_total.saturating_add(self.peer.load(Ordering::Relaxed)) > self.limit {
                        self.limit_hit.store(true, Ordering::Relaxed);
                        return std::task::Poll::Ready(Err(std::io::Error::other(
                            "tunnel byte limit exceeded",
                        )));
                    }
                }
                std::task::Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Metered<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        std::pin::Pin::new(&mut self.inner).poll_write(context, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

/// How the relay/idle/deadline race in [`relay_tunnel`] resolved.
enum RelayEnd {
    /// The relay task finished (completed, errored, or was cancelled).
    Joined(Result<Result<(u64, u64), std::io::Error>, tokio::task::JoinError>),
    /// The total-duration deadline elapsed first.
    Long,
    /// The no-progress ceiling elapsed first.
    Quiet,
}

/// Relay one opaque tunnel with byte, duration, and idle bounds.
///
/// `client` is the `EggServe` downstream; `upstream` is the `Eggress` route
/// established before the `200` handshake. Returns a byte-count summary;
/// the caller attaches the policy-checked target to build a [`TunnelEvent`].
pub async fn relay_tunnel(
    client: TunnelIo,
    upstream: DialStream,
    limits: TunnelLimits,
) -> TunnelRelaySummary {
    let started = Instant::now();
    let client_bytes = Arc::new(AtomicU64::new(0));
    let upstream_bytes = Arc::new(AtomicU64::new(0));
    let limit_hit = Arc::new(AtomicBool::new(false));

    let mut metered_client = Metered {
        inner: client,
        own: client_bytes.clone(),
        peer: upstream_bytes.clone(),
        limit_hit: limit_hit.clone(),
        limit: limits.max_bytes,
    };
    let mut metered_upstream = Metered {
        inner: upstream,
        own: upstream_bytes.clone(),
        peer: client_bytes.clone(),
        limit_hit: limit_hit.clone(),
        limit: limits.max_bytes,
    };
    let mut relay = tokio::spawn(async move {
        tokio::io::copy_bidirectional(&mut metered_client, &mut metered_upstream).await
    });

    // No-progress future: resolves after `idle` elapses without bytes in
    // either direction. Raced against the relay and the total deadline below.
    let idle = limits.idle_timeout;
    let idle_client = client_bytes.clone();
    let idle_upstream = upstream_bytes.clone();
    let idle_watch = async move {
        let check_every = (idle / 4).clamp(Duration::from_millis(10), Duration::from_millis(250));
        let mut last = idle_client
            .load(Ordering::Relaxed)
            .saturating_add(idle_upstream.load(Ordering::Relaxed));
        let mut quiet_since = Instant::now();
        loop {
            tokio::time::sleep(check_every).await;
            let current = idle_client
                .load(Ordering::Relaxed)
                .saturating_add(idle_upstream.load(Ordering::Relaxed));
            if current != last {
                last = current;
                quiet_since = Instant::now();
            } else if quiet_since.elapsed() >= idle {
                return;
            }
        }
    };

    let end = tokio::select! {
        joined = &mut relay => RelayEnd::Joined(joined),
        () = tokio::time::sleep(limits.max_duration) => RelayEnd::Long,
        () = idle_watch => RelayEnd::Quiet,
    };
    // Close a still-pending relay; a no-op when the relay already resolved.
    relay.abort();

    let directional = || {
        (
            client_bytes.load(Ordering::Relaxed),
            upstream_bytes.load(Ordering::Relaxed),
        )
    };
    let (outcome, (up, down)) = match end {
        RelayEnd::Long => (TunnelOutcome::DurationLimit, directional()),
        RelayEnd::Quiet => (TunnelOutcome::IdleTimeout, directional()),
        RelayEnd::Joined(Err(join)) => {
            if join.is_cancelled() && limit_hit.load(Ordering::Relaxed) {
                // Cancelled by the winning bound above; prefer the more
                // specific classification when the byte ceiling fired.
                (TunnelOutcome::ByteLimit, directional())
            } else {
                (TunnelOutcome::Shutdown, directional())
            }
        }
        RelayEnd::Joined(Ok(_)) if limit_hit.load(Ordering::Relaxed) => {
            (TunnelOutcome::ByteLimit, directional())
        }
        RelayEnd::Joined(Ok(Err(_))) => (TunnelOutcome::RelayError, directional()),
        RelayEnd::Joined(Ok(Ok(_))) => (TunnelOutcome::Completed, directional()),
    };
    // Directional counts come from the per-direction meters, so every
    // outcome (including limit/idle cutoffs) reports observed bytes.
    let summary = TunnelRelaySummary {
        outcome,
        bytes_client_to_target: up,
        bytes_target_to_client: down,
        duration: started.elapsed(),
        limit_fired: limit_hit.load(Ordering::Relaxed),
    };
    debug_assert!(!summary.limit_fired || summary.outcome == TunnelOutcome::ByteLimit);
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn test_limits() -> TunnelLimits {
        TunnelLimits::new(
            1024 * 1024,
            Duration::from_secs(10),
            Duration::from_secs(10),
            4,
            Duration::from_secs(5),
        )
        .unwrap()
    }

    #[test]
    fn limits_reject_non_positive_bounds() {
        assert!(
            TunnelLimits::new(
                0,
                Duration::from_secs(1),
                Duration::from_secs(1),
                1,
                Duration::from_secs(1)
            )
            .is_err()
        );
        assert!(
            TunnelLimits::new(
                64,
                Duration::ZERO,
                Duration::from_secs(1),
                1,
                Duration::from_secs(1)
            )
            .is_err()
        );
        assert!(
            TunnelLimits::new(
                64,
                Duration::from_secs(1),
                Duration::from_secs(1),
                0,
                Duration::from_secs(1)
            )
            .is_err()
        );
        assert!(
            TunnelLimits::new(
                64,
                Duration::from_secs(1),
                Duration::from_secs(1),
                MAX_CONCURRENT_TUNNELS + 1,
                Duration::from_secs(1)
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn duplex_relay_moves_bytes_both_directions() {
        let (client_side, proxy_side) = TunnelIo::pair();
        let (upstream_side_a, mut upstream_side_b) = tokio::io::duplex(65_536);
        let upstream: DialStream = Box::new(upstream_side_a);
        let relay = tokio::spawn(relay_tunnel(proxy_side, upstream, test_limits()));
        let mut client = client_side;
        client.write_all(b"hello").await.unwrap();
        let mut buf = [0; 5];
        upstream_side_b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
        upstream_side_b.write_all(b"world").await.unwrap();
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"world");
        drop(client);
        drop(upstream_side_b);
        let summary = relay.await.unwrap();
        assert_eq!(summary.outcome, TunnelOutcome::Completed);
        assert_eq!(summary.bytes_client_to_target, 5);
        assert_eq!(summary.bytes_target_to_client, 5);
    }

    #[tokio::test]
    async fn byte_limit_closes_the_relay() {
        let limits = TunnelLimits::new(
            8,
            Duration::from_secs(10),
            Duration::from_secs(10),
            4,
            Duration::from_secs(5),
        )
        .unwrap();
        let (client_side, proxy_side) = TunnelIo::pair();
        let (upstream_side_a, mut upstream_side_b) = tokio::io::duplex(65_536);
        let upstream: DialStream = Box::new(upstream_side_a);
        let relay = tokio::spawn(relay_tunnel(proxy_side, upstream, limits));
        let mut client = client_side;
        client.write_all(&[b'x'; 64]).await.unwrap();
        let mut sink = Vec::new();
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            upstream_side_b.read_to_end(&mut sink),
        )
        .await;
        drop(client);
        drop(upstream_side_b);
        let summary = tokio::time::timeout(Duration::from_secs(5), relay)
            .await
            .expect("relay must end")
            .unwrap();
        assert_eq!(summary.outcome, TunnelOutcome::ByteLimit);
    }

    #[tokio::test]
    async fn idle_timeout_closes_a_quiet_relay() {
        let limits = TunnelLimits::new(
            1024 * 1024,
            Duration::from_secs(30),
            Duration::from_millis(100),
            4,
            Duration::from_secs(5),
        )
        .unwrap();
        let (_client_side, proxy_side) = TunnelIo::pair();
        let (upstream_side_a, _upstream_side_b) = tokio::io::duplex(65_536);
        let upstream: DialStream = Box::new(upstream_side_a);
        let summary = tokio::time::timeout(
            Duration::from_secs(5),
            relay_tunnel(proxy_side, upstream, limits),
        )
        .await
        .expect("relay must end");
        assert_eq!(summary.outcome, TunnelOutcome::IdleTimeout);
    }
}
