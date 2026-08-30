//! Client-side dial + handshake helper.
//!
//! A device that wants to sync workspace CRDT deltas through a
//! relay calls [`RelayClient::connect`] with its own identity + the
//! relay's iroh endpoint address. On success the client owns the
//! open bidirectional stream + a receiver for inbound envelopes.
//!
//! The shell network actor uses this client for always-on home and provider
//! hosts. Non-shell workspace tools (for example a backup script or batch job)
//! can use the same primitive.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use iroh::endpoint::{Connection, Endpoint};
use iroh_base::EndpointAddr;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use qos_user_identity::{DeviceCert, RevocationList};

use crate::auth::{self, AuthenticatedPeer, Hello, HelloAck};
use crate::daemon::{read_frame, write_frame};
use crate::transport::{ALPN, Envelope, FrameReader, MAX_FRAME_BYTES};

/// A live authenticated session with a relay. Handles are cheap;
/// dropping the client tears down the connection.
pub struct RelayClient {
    /// The peer view of the relay (its device cert).
    pub relay_peer: AuthenticatedPeer,
    /// Outbound envelope channel — call `send` to enqueue.
    pub outbound: mpsc::Sender<Envelope>,
    /// Inbound envelope channel — call `recv` to consume.
    pub inbound: Arc<AsyncMutex<mpsc::Receiver<Envelope>>>,
    /// Workspaces successfully subscribed on this connection. Drop queues an
    /// explicit unsubscribe for each one before gracefully finishing the
    /// stream, so the host can begin offline delivery without a QUIC timeout.
    subscriptions: Mutex<HashSet<u64>>,
    /// Kept alive so the connection is not dropped.
    _connection: Connection,
    /// Kept alive for the same reason. A `Connection` does **not** keep
    /// its endpoint open: when the last `Endpoint` handle drops, the
    /// socket it owns goes with it and every connection dialled from it
    /// dies mid-flight. `connect` takes the endpoint by value, so
    /// without this field a caller that hands over its only handle gets
    /// a session that completes the handshake (those bytes are already
    /// on the wire) and then loses the connection before the first
    /// envelope — a failure that surfaces as an unexplained silence on
    /// `recv`, not as a dial error.
    _endpoint: Endpoint,
    /// The task driving the send/recv split. Dropping this handle detaches the
    /// task, which owns the endpoint until it can finish the stream cleanly.
    /// A `Connection` alone does not keep its endpoint socket open.
    _pump: tokio::task::JoinHandle<()>,
}

impl Drop for RelayClient {
    fn drop(&mut self) {
        let mut graceful = true;
        if let Ok(subscriptions) = self.subscriptions.lock() {
            for &workspace_id in subscriptions.iter() {
                if self
                    .outbound
                    .try_send(Envelope::Unsubscribe { workspace_id })
                    .is_err()
                {
                    graceful = false;
                    break;
                }
            }
        } else {
            graceful = false;
        }
        if !graceful {
            self._connection
                .close(0u32.into(), b"client cleanup queue saturated");
        }
    }
}

/// Failure modes.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Iroh dial / bi-stream.
    #[error("iroh: {0}")]
    Iroh(String),
    /// Postcard encode/decode.
    #[error("codec: {0}")]
    Codec(String),
    /// Auth check failed.
    #[error("auth: {0}")]
    Auth(#[from] crate::auth::AuthError),
    /// I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl RelayClient {
    /// Dial `relay_addr` on the `qos-relay/1` ALPN, run the auth
    /// handshake with `own_cert` + `own_sk`, and return a
    /// bidirectional session.
    ///
    /// `own_user_public_key` MUST equal `own_cert.user_public_key`;
    /// the mutual-auth step compares the server cert's user pubkey
    /// against this value.
    ///
    /// The returned client owns `endpoint` and holds it open for its own
    /// lifetime, so a caller may pass its only handle.
    pub async fn connect(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        own_user_public_key: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        Self::connect_with_server_trust(
            endpoint,
            relay_addr,
            Some(own_user_public_key),
            own_cert,
            own_sk,
            revocations,
        )
        .await
    }

    /// Connect to a host whose Iroh endpoint address was obtained from a
    /// trusted workspace configuration/invite. The host may be provider-owned
    /// and therefore chain to a different user root.
    pub async fn connect_to_pinned_host(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        Self::connect_with_server_trust(endpoint, relay_addr, None, own_cert, own_sk, revocations)
            .await
    }

    async fn connect_with_server_trust(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: Option<[u8; 32]>,
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        let endpoint_keepalive = endpoint.clone();
        let conn = endpoint
            .connect(relay_addr, ALPN)
            .await
            .map_err(|e| ClientError::Iroh(e.to_string()))?;
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Iroh(e.to_string()))?;

        let mut client_nonce = [0u8; 32];
        {
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut client_nonce);
        }
        let hello = Hello {
            cert: own_cert.clone(),
            client_nonce,
        };
        let hello_bytes =
            postcard::to_allocvec(&hello).map_err(|e| ClientError::Codec(e.to_string()))?;
        write_frame(&mut send, &hello_bytes).await?;

        let ack_bytes = read_frame(&mut recv, MAX_FRAME_BYTES).await?;
        let ack: HelloAck =
            postcard::from_bytes(&ack_bytes).map_err(|e| ClientError::Codec(e.to_string()))?;
        let now_ns = crate::peer::now_ns();
        let relay_peer = match expected_server_user.as_ref() {
            Some(expected) => {
                auth::verify_hello_ack(&ack, expected, &revocations, now_ns, &client_nonce)?
            }
            None => auth::verify_pinned_host_ack(&ack, &revocations, now_ns, &client_nonce)?,
        };

        let ack2 = auth::build_hello_ack2(&own_sk, &ack.server_nonce);
        let ack2_bytes =
            postcard::to_allocvec(&ack2).map_err(|e| ClientError::Codec(e.to_string()))?;
        write_frame(&mut send, &ack2_bytes).await?;

        // Split into pump halves.
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Envelope>(256);
        let (inbound_tx, inbound_rx) = mpsc::channel::<Envelope>(256);
        let mut reader = FrameReader::new(recv);
        let pump = tokio::spawn(async move {
            // Keep the socket alive through the final queued Unsubscribe and
            // FIN even if the caller handed this client its only Endpoint.
            let _endpoint_keepalive = endpoint_keepalive;
            loop {
                tokio::select! {
                    biased;
                    ob = outbound_rx.recv() => {
                        if let Some(env) = ob {
                            let Ok(bytes) = crate::transport::encode(&env) else { break };
                            if send.write_all(&bytes).await.is_err() { break; }
                        } else {
                            let _ = send.finish();
                            // Keep the endpoint alive long enough to flush the final
                            // Unsubscribe/FIN. Bound the wait so a dead network cannot
                            // strand this detached cleanup task indefinitely.
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(2),
                                send.stopped(),
                            )
                            .await;
                            break;
                        }
                    }
                    ib = reader.read_next() => {
                        match ib {
                            Ok(Some(env)) => {
                                if inbound_tx.send(env).await.is_err() { break; }
                            }
                            Ok(None) => break,
                            Err(_) => break,
                        }
                    }
                }
            }
        });

        Ok(Self {
            relay_peer,
            outbound: outbound_tx,
            inbound: Arc::new(AsyncMutex::new(inbound_rx)),
            subscriptions: Mutex::new(HashSet::new()),
            _connection: conn,
            _endpoint: endpoint,
            _pump: pump,
        })
    }

    /// Convenience: send an envelope.
    pub fn send(&self, env: Envelope) -> Result<(), Envelope> {
        let subscription_update = match &env {
            Envelope::Subscribe { workspace_id } => Some((*workspace_id, true)),
            Envelope::Unsubscribe { workspace_id } => Some((*workspace_id, false)),
            _ => None,
        };
        self.outbound
            .try_send(env)
            .map_err(tokio::sync::mpsc::error::TrySendError::into_inner)?;
        if let Some((workspace_id, subscribing)) = subscription_update
            && let Ok(mut subscriptions) = self.subscriptions.lock()
        {
            if subscribing {
                subscriptions.insert(workspace_id);
            } else {
                subscriptions.remove(&workspace_id);
            }
        }
        Ok(())
    }

    /// Convenience: recv the next envelope.
    pub async fn recv(&self) -> Option<Envelope> {
        self.inbound.lock().await.recv().await
    }

    /// Disconnect, waiting until the relay has been told.
    ///
    /// Prefer this to dropping the handle whenever the disconnect is
    /// deliberate. `Connection::close` only queues `CONNECTION_CLOSE` and there
    /// is no per-connection flush; `Endpoint::close` is what waits for the
    /// driver to put it on the wire. Without that wait the endpoint dies with
    /// the handle first, and the relay keeps the subscription until its idle
    /// timeout — fanning deltas out to a connection that is gone rather than
    /// queueing them for store-and-forward.
    ///
    /// This closes the endpoint the client was built on, which
    /// [`RelayClient::connect`] takes by value on the understanding that the
    /// client owns it. A caller that kept a clone and is still using it — the
    /// daemon's federation dial loop passes the relay's own endpoint — must
    /// drop the handle instead.
    pub async fn close(self) {
        self._connection.close(0u32.into(), b"client closed");
        self._connection.closed().await;
        self._endpoint.close().await;
    }
}
