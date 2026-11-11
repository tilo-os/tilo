//! Internal carrier lifetime boundary for authenticated application sessions.
//!
//! This module does not authenticate peers or construct certified evidence.
//! Callers assemble a client only after the protocol-specific mutual proof.
//! Two carriers: the Iroh transport, and any byte stream pair for a host in
//! the same process. Both carry the same state envelopes.

use iroh::endpoint::{Connection, ConnectionStats, Endpoint};
use n0_future::boxed::BoxFuture;
use tokio::sync::mpsc;

use crate::transport::Envelope;

/// Already bounded application channels supplied by an authenticated pump.
pub(crate) struct SessionChannels {
    pub outbound: mpsc::Sender<Envelope>,
    pub inbound: mpsc::Receiver<Envelope>,
}

/// Lifetime operations and transport counters only: no identity, business
/// retry, or durability policy. Normal handle drop must let the pump drain
/// queued unsubscribes and finish.
pub(crate) trait SessionCarrier {
    fn abort(&self, reason: &'static [u8]);
    fn disconnect(&self) -> BoxFuture<()>;
    fn close_owned(&self) -> BoxFuture<()>;
    /// The transport's own packet and frame counters, where it keeps them.
    fn transport_stats(&self) -> Option<ConnectionStats> {
        None
    }
    /// The connection the session rides, where it rides one: other sessions
    /// open streams of their own on it.
    fn connection(&self) -> Option<Connection> {
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) type CarrierHandle = Box<dyn SessionCarrier + Send + Sync>;
#[cfg(target_arch = "wasm32")]
pub(crate) type CarrierHandle = Box<dyn SessionCarrier>;

/// Owns Iroh transport resources without exposing them to the session handle.
pub(crate) struct IrohCarrier {
    connection: Connection,
    // Connection alone does not keep the socket open. Retain this handle for
    // the client's lifetime; the pump also keeps a clone through its final FIN.
    endpoint: Endpoint,
    // Dropping this handle detaches rather than aborting the pump, so normal
    // client drop can still send the queued Unsubscribe and gracefully finish.
    _pump: n0_future::task::JoinHandle<()>,
}

impl IrohCarrier {
    pub(crate) fn new(
        connection: Connection,
        endpoint: Endpoint,
        pump: n0_future::task::JoinHandle<()>,
    ) -> Self {
        Self {
            connection,
            endpoint,
            _pump: pump,
        }
    }
}

impl SessionCarrier for IrohCarrier {
    fn abort(&self, reason: &'static [u8]) {
        self.connection.close(0u32.into(), reason);
    }

    fn disconnect(&self) -> BoxFuture<()> {
        let connection = self.connection.clone();
        Box::pin(async move {
            connection.close(0u32.into(), b"broker lane disconnected");
            connection.closed().await;
        })
    }

    fn close_owned(&self) -> BoxFuture<()> {
        let connection = self.connection.clone();
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            connection.close(0u32.into(), b"client closed");
            connection.closed().await;
            // Endpoint::close flushes the connection close before releasing
            // the socket. Callers sharing an endpoint must use disconnect.
            endpoint.close().await;
        })
    }

    fn transport_stats(&self) -> Option<ConnectionStats> {
        Some(self.connection.stats())
    }

    fn connection(&self) -> Option<Connection> {
        Some(self.connection.clone())
    }
}

/// Owns the pump of a session over any byte stream pair (an in-process
/// duplex to a host in the same process). Stopping it ends the pump, which
/// drops both halves: the host reads the end of the stream.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct StreamCarrier {
    stop: tokio::sync::watch::Sender<bool>,
    ended: tokio::sync::watch::Receiver<bool>,
}

#[cfg(not(target_arch = "wasm32"))]
impl StreamCarrier {
    /// Run `pump` until it ends or the carrier stops.
    pub(crate) fn start(pump: impl std::future::Future<Output = ()> + Send + 'static) -> Self {
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let (ended_tx, ended) = tokio::sync::watch::channel(false);
        n0_future::task::spawn(async move {
            tokio::select! {
                // Only `stop` sends, and only `true`.
                _ = stopped.changed() => {}
                () = pump => {}
            }
            ended_tx.send_replace(true);
        });
        Self { stop, ended }
    }

    fn stopped(&self) -> BoxFuture<()> {
        self.stop.send_replace(true);
        let mut ended = self.ended.clone();
        Box::pin(async move {
            // The pump task always sends before it ends; a closed channel
            // means it is gone too.
            let _ = ended.wait_for(|ended| *ended).await;
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SessionCarrier for StreamCarrier {
    fn abort(&self, _reason: &'static [u8]) {
        self.stop.send_replace(true);
    }

    fn disconnect(&self) -> BoxFuture<()> {
        self.stopped()
    }

    fn close_owned(&self) -> BoxFuture<()> {
        self.stopped()
    }
}
