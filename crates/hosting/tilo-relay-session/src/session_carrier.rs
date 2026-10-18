//! Internal carrier lifetime boundary for authenticated application sessions.
//!
//! This module does not authenticate peers or construct certified evidence.
//! Callers assemble a client only after the protocol-specific mutual proof.
//! Two carriers: the Iroh transport, and a host's HTTPS routes
//! ([`tilo_relay_client::https::HttpsLink`]) for a host that publishes no
//! iroh address. Both carry the same state envelopes; only Iroh carries the
//! private v2 action channel.

use iroh::endpoint::{Connection, ConnectionStats, Endpoint};
use n0_future::boxed::BoxFuture;
use tokio::sync::mpsc;

use crate::transport::Envelope;
use tilo_delivery_protocol::transport_v2::EnvelopeV2;

/// Already bounded application channels supplied by an authenticated pump.
/// Private frames must originate from the same verified session as state.
pub(crate) struct SessionChannels {
    pub outbound: mpsc::Sender<Envelope>,
    pub inbound: mpsc::Receiver<Envelope>,
    pub action_outbound: Option<mpsc::Sender<EnvelopeV2>>,
    pub action_inbound: mpsc::Receiver<EnvelopeV2>,
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
}

/// Owns the pump between a session's channels and an HTTPS link to its host.
pub(crate) struct HttpsCarrier {
    stop: tokio::sync::watch::Sender<bool>,
}

impl HttpsCarrier {
    /// Pump `link` into fresh state channels until either side ends or the
    /// carrier stops. The link carries state only: the private channel
    /// stays absent.
    pub(crate) fn start(mut link: tilo_relay_client::HttpsLink) -> (Self, SessionChannels) {
        let (outbound, mut outgoing) = mpsc::channel::<Envelope>(256);
        let (incoming, inbound) = mpsc::channel::<Envelope>(256);
        let (_, action_inbound) = mpsc::channel::<EnvelopeV2>(1);
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        n0_future::task::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    // Only `stop` sends, and only `true`.
                    _ = stopped.changed() => break,
                    envelope = outgoing.recv() => {
                        // The client is gone, or the link ended: the host
                        // ends the session once its polls stop.
                        let Some(envelope) = envelope else { break };
                        if link.send(envelope).is_err() {
                            break;
                        }
                    }
                    envelope = link.recv() => {
                        let Some(envelope) = envelope else { break };
                        if incoming.send(envelope).await.is_err() {
                            break;
                        }
                    }
                }
            }
            link.close();
        });
        (
            Self { stop },
            SessionChannels {
                outbound,
                inbound,
                action_outbound: None,
                action_inbound,
            },
        )
    }
}

impl SessionCarrier for HttpsCarrier {
    fn abort(&self, _reason: &'static [u8]) {
        self.stop.send_replace(true);
    }

    fn disconnect(&self) -> BoxFuture<()> {
        self.stop.send_replace(true);
        Box::pin(async {})
    }

    fn close_owned(&self) -> BoxFuture<()> {
        // The host ends the session once its polls stop.
        self.disconnect()
    }
}
