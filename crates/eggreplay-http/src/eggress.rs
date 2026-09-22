//! Optional Eggress listener-free route adapter for EggFetch.

use eggfetch_core::{DialError, DialErrorKind, DialFuture, DialStream, DialTarget, Dialer};
use eggress_outbound::{OutboundConnectErrorKind, OutboundConnector};
use std::sync::Arc;

/// An EggFetch dialer that delegates physical TCP route establishment to Eggress.
#[derive(Clone)]
pub struct EggressDialer {
    connector: Arc<OutboundConnector>,
}

impl EggressDialer {
    /// Build a direct route adapter.
    pub fn direct() -> Self {
        Self {
            connector: Arc::new(OutboundConnector::direct()),
        }
    }

    /// Build an adapter around a caller-owned connector.
    pub fn new(connector: OutboundConnector) -> Self {
        Self {
            connector: Arc::new(connector),
        }
    }
}

impl Dialer for EggressDialer {
    fn dial(&self, target: DialTarget) -> DialFuture<'_> {
        let connector = self.connector.clone();
        Box::pin(async move {
            let stream = connector
                .connect_tcp_detailed(target.host(), target.port())
                .await
                .map_err(map_error)?;
            Ok(Box::new(stream.0) as DialStream)
        })
    }
}

fn map_error(error: eggress_outbound::OutboundConnectError) -> DialError {
    let kind = match error.kind() {
        OutboundConnectErrorKind::Timeout => DialErrorKind::Timeout,
        OutboundConnectErrorKind::Authentication => DialErrorKind::Authentication,
        OutboundConnectErrorKind::Policy => DialErrorKind::Rejected,
        OutboundConnectErrorKind::ConnectionRefused
        | OutboundConnectErrorKind::Dns
        | OutboundConnectErrorKind::NetworkUnreachable
        | OutboundConnectErrorKind::HostUnreachable
        | OutboundConnectErrorKind::Tls
        | OutboundConnectErrorKind::Protocol
        | OutboundConnectErrorKind::Other => DialErrorKind::Connection,
        _ => DialErrorKind::Other,
    };
    DialError::new(kind, format!("Eggress route {}", error.kind()))
}
