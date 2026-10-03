//! Internal carrier lifetime boundary for authenticated application sessions.
//!
//! This module does not authenticate peers or construct certified evidence.
//! Callers assemble a client only after the protocol-specific mutual proof.
//! The only production carrier here remains the existing Iroh transport.

use iroh::endpoint::{Connection, Endpoint};
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

/// Lifetime operations only: no identity, business retry, or durability policy.
/// Normal handle drop must let the pump drain queued unsubscribes and finish.
pub(crate) trait SessionCarrier {
    fn abort(&self, reason: &'static [u8]);
    fn disconnect(&self) -> BoxFuture<()>;
    fn close_owned(&self) -> BoxFuture<()>;
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
}
