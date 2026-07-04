//! Client-side dial + handshake helper.
//!
//! A device that wants to sync workspace CRDT deltas through a
//! relay calls [`RelayClient::connect`] with its own identity + the
//! relay's iroh endpoint address. On success the client owns the
//! open bidirectional stream + a receiver for inbound envelopes.
//!
//! **Note.** This module is where the shell's network actor will
//! plug in when the iroh workspace-CRDT transport lands — the shell
//! doesn't consume it in this crate, but the tests do, and any
//! non-shell workspace tool (e.g. a backup script, a batch job) can
//! use the same primitive.

use std::sync::Arc;

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
    pub outbound: mpsc::UnboundedSender<Envelope>,
    /// Inbound envelope channel — call `recv` to consume.
    pub inbound: Arc<AsyncMutex<mpsc::UnboundedReceiver<Envelope>>>,
    /// Kept alive so the connection is not dropped.
    _connection: Connection,
    /// The task driving the send/recv split. Aborted on drop.
    _pump: JoinHandle,
}

struct JoinHandle(tokio::task::JoinHandle<()>);
impl Drop for JoinHandle {
    fn drop(&mut self) {
        self.0.abort();
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
    pub async fn connect(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        own_user_public_key: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
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
        let hello_bytes = postcard::to_allocvec(&hello)
            .map_err(|e| ClientError::Codec(e.to_string()))?;
        write_frame(&mut send, &hello_bytes).await?;

        let ack_bytes = read_frame(&mut recv, MAX_FRAME_BYTES).await?;
        let ack: HelloAck = postcard::from_bytes(&ack_bytes)
            .map_err(|e| ClientError::Codec(e.to_string()))?;
        let now_ns = crate::peer::now_ns();
        let relay_peer =
            auth::verify_hello_ack(&ack, &own_user_public_key, &revocations, now_ns, &client_nonce)?;

        let ack2 = auth::build_hello_ack2(&own_sk, &ack.server_nonce);
        let ack2_bytes = postcard::to_allocvec(&ack2)
            .map_err(|e| ClientError::Codec(e.to_string()))?;
        write_frame(&mut send, &ack2_bytes).await?;

        // Split into pump halves.
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Envelope>();
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<Envelope>();
        let mut reader = FrameReader::new(recv);
        let pump = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    ob = outbound_rx.recv() => {
                        match ob {
                            Some(env) => {
                                let Ok(bytes) = crate::transport::encode(&env) else { break };
                                if send.write_all(&bytes).await.is_err() { break; }
                            }
                            None => break,
                        }
                    }
                    ib = reader.read_next() => {
                        match ib {
                            Ok(Some(env)) => {
                                if inbound_tx.send(env).is_err() { break; }
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
            _connection: conn,
            _pump: JoinHandle(pump),
        })
    }

    /// Convenience: send an envelope.
    pub fn send(&self, env: Envelope) -> Result<(), Envelope> {
        self.outbound.send(env).map_err(|e| e.0)
    }

    /// Convenience: recv the next envelope.
    pub async fn recv(&self) -> Option<Envelope> {
        self.inbound.lock().await.recv().await
    }
}
