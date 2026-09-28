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

use tilo_user_identity::{DeviceCert, RevocationList};

use crate::frame::{FrameReader, read_frame, write_frame};
use crate::signer::DeviceSigner;
use tilo_relay_protocol::auth::{self, AuthError, AuthenticatedPeer, Hello, HelloAck};
use tilo_relay_protocol::transport::{
    ALPN, ContentEndpointAdvertisement, ContentEndpointRequest, ContentPeerRegistration,
    DISCOVERY_ALPN, Envelope, HomePeerManagementRequest, HomePeerManagementResponse,
    MANAGEMENT_ALPN, MAX_FRAME_BYTES,
};

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
    connection: Connection,
    /// Kept alive for the same reason. A `Connection` does **not** keep
    /// its endpoint open: when the last `Endpoint` handle drops, the
    /// socket it owns goes with it and every connection dialled from it
    /// dies mid-flight. `connect` takes the endpoint by value, so
    /// without this field a caller that hands over its only handle gets
    /// a session that completes the handshake (those bytes are already
    /// on the wire) and then loses the connection before the first
    /// envelope — a failure that surfaces as an unexplained silence on
    /// `recv`, not as a dial error.
    endpoint: Endpoint,
    /// The task driving the send/recv split. Dropping this handle detaches the
    /// task, which owns the endpoint until it can finish the stream cleanly.
    /// A `Connection` alone does not keep its endpoint socket open.
    _pump: n0_future::task::JoinHandle<()>,
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
            self.connection
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
    Auth(#[from] AuthError),
    /// I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The device signer refused or failed.
    #[error("signer: {0}")]
    Sign(String),
}

impl RelayClient {
    /// Execute an owner-authorized lifecycle operation against a pinned home
    /// peer. The mutual handshake proves both enrolled device certificates.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when dialing, framing, or mutual authentication
    /// fails, including when the relay's pinned user or device differs.
    pub async fn manage_home_peer(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
        request: HomePeerManagementRequest,
    ) -> Result<HomePeerManagementResponse, ClientError> {
        let conn = endpoint
            .connect(relay_addr, MANAGEMENT_ALPN)
            .await
            .map_err(|error| ClientError::Iroh(error.to_string()))?;
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|error| ClientError::Iroh(error.to_string()))?;
        let mut nonce = [0; 32];
        {
            use rand::Rng;
            rand::rng().fill_bytes(&mut nonce);
        }
        write_frame(
            &mut send,
            &postcard::to_allocvec(&Hello {
                cert: own_cert,
                client_nonce: nonce,
            })
            .map_err(|error| ClientError::Codec(error.to_string()))?,
        )
        .await?;
        let ack: HelloAck = postcard::from_bytes(&read_frame(&mut recv, MAX_FRAME_BYTES).await?)
            .map_err(|error| ClientError::Codec(error.to_string()))?;
        auth::verify_hello_ack(
            &ack,
            &expected_server_user,
            &revocations,
            crate::clock::now_ns(),
            &nonce,
        )?;
        if ack.cert.device_public_key != expected_server_device {
            return Err(ClientError::Auth(AuthError::PolicyReject));
        }
        let ack2 = auth::build_hello_ack2(&own_sk, &ack.server_nonce);
        write_frame(
            &mut send,
            &postcard::to_allocvec(&ack2).map_err(|error| ClientError::Codec(error.to_string()))?,
        )
        .await?;
        write_frame(
            &mut send,
            &postcard::to_allocvec(&request)
                .map_err(|error| ClientError::Codec(error.to_string()))?,
        )
        .await?;
        let response = postcard::from_bytes(&read_frame(&mut recv, MAX_FRAME_BYTES).await?)
            .map_err(|error| ClientError::Codec(error.to_string()))?;
        conn.close(0u32.into(), b"");
        Ok(response)
    }
    /// Resolve the authenticated direct filesystem endpoint advertised by a
    /// relay. The returned address is never the relay socket itself.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when dialing, framing, or authenticated
    /// discovery fails.
    pub async fn discover_content_endpoint(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        workspace_id: u64,
        own_user_public_key: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<EndpointAddr, ClientError> {
        Ok(Self::discover_content_peer_with_server_trust(
            endpoint,
            relay_addr,
            workspace_id,
            Some((own_user_public_key, expected_server_device)),
            own_cert,
            own_sk,
            revocations,
        )
        .await?
        .endpoint)
    }

    /// Resolve both the content endpoint and authenticated Files workspace
    /// ticket needed by a fresh device to join the live record plane.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when dialing, framing, or authenticated
    /// discovery fails.
    pub async fn discover_content_peer(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        workspace_id: u64,
        own_user_public_key: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<ContentPeerRegistration, ClientError> {
        Self::discover_content_peer_with_server_trust(
            endpoint,
            relay_addr,
            workspace_id,
            Some((own_user_public_key, expected_server_device)),
            own_cert,
            own_sk,
            revocations,
        )
        .await
    }

    /// Resolve content from a relay whose certified user root differs from
    /// this client. The caller pins its derived relay service `NodeId`; the
    /// challenge still proves the server holds its certified device key.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when dialing, framing, or authenticated
    /// discovery fails.
    pub async fn discover_content_endpoint_to_pinned_host(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        workspace_id: u64,
        expected_server_user: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<EndpointAddr, ClientError> {
        Ok(Self::discover_content_peer_with_server_trust(
            endpoint,
            relay_addr,
            workspace_id,
            Some((expected_server_user, expected_server_device)),
            own_cert,
            own_sk,
            revocations,
        )
        .await?
        .endpoint)
    }

    async fn discover_content_peer_with_server_trust(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        workspace_id: u64,
        expected_server: Option<([u8; 32], [u8; 32])>,
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<ContentPeerRegistration, ClientError> {
        let relay_service_id = *relay_addr.id.as_bytes();
        let conn = endpoint
            .connect(relay_addr, DISCOVERY_ALPN)
            .await
            .map_err(|error| ClientError::Iroh(error.to_string()))?;
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|error| ClientError::Iroh(error.to_string()))?;
        let mut nonce = [0; 32];
        {
            use rand::Rng;
            rand::rng().fill_bytes(&mut nonce);
        }
        write_frame(
            &mut send,
            &postcard::to_allocvec(&Hello {
                cert: own_cert.clone(),
                client_nonce: nonce,
            })
            .map_err(|error| ClientError::Codec(error.to_string()))?,
        )
        .await?;
        let ack: HelloAck = postcard::from_bytes(&read_frame(&mut recv, MAX_FRAME_BYTES).await?)
            .map_err(|error| ClientError::Codec(error.to_string()))?;
        match expected_server.as_ref() {
            Some((user, _)) => {
                auth::verify_hello_ack(&ack, user, &revocations, crate::clock::now_ns(), &nonce)?
            }
            None => {
                auth::verify_pinned_host_ack(&ack, &revocations, crate::clock::now_ns(), &nonce)?
            }
        };
        if let Some((_, expected_device)) = expected_server
            && ack.cert.device_public_key != expected_device
        {
            return Err(ClientError::Auth(AuthError::PolicyReject));
        }
        let ack2 = auth::build_hello_ack2(&own_sk, &ack.server_nonce);
        write_frame(
            &mut send,
            &postcard::to_allocvec(&ack2).map_err(|error| ClientError::Codec(error.to_string()))?,
        )
        .await?;
        write_frame(
            &mut send,
            &postcard::to_allocvec(&ContentEndpointRequest { workspace_id })
                .map_err(|error| ClientError::Codec(error.to_string()))?,
        )
        .await?;
        let advertisement: ContentEndpointAdvertisement =
            postcard::from_bytes(&read_frame(&mut recv, MAX_FRAME_BYTES).await?)
                .map_err(|error| ClientError::Codec(error.to_string()))?;
        conn.close(0u32.into(), b"");
        if advertisement.workspace_id != workspace_id {
            return Err(ClientError::Iroh(
                "relay returned a mismatched workspace".into(),
            ));
        }
        if advertisement.schema_version != 1 || advertisement.filesystem_ticket.is_empty() {
            return Err(ClientError::Codec(
                "relay returned an unsupported content registration".into(),
            ));
        }
        let endpoint = advertisement
            .filesystem_endpoints
            .into_iter()
            .next()
            .ok_or_else(|| {
                ClientError::Iroh("relay has no ready filesystem endpoint for workspace".into())
            })?;
        Ok(ContentPeerRegistration {
            endpoint,
            ticket: advertisement.filesystem_ticket,
            relay_service_id,
        })
    }
    /// Dial `relay_addr` on the `tilo-relay/1` ALPN, run the auth
    /// handshake with `own_cert` + `own_sk`, and return a
    /// bidirectional session.
    ///
    /// `own_user_public_key` MUST equal `own_cert.user_public_key`;
    /// the mutual-auth step compares the server cert's user pubkey
    /// against this value.
    ///
    /// The returned client owns `endpoint` and holds it open for its own
    /// lifetime, so a caller may pass its only handle.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] if the dial or bi-stream open fails, the
    /// handshake frames can't be encoded/decoded or fail to write/read,
    /// or the server's `HelloAck` fails auth verification (wrong user
    /// chain, bad signature, revoked, or stale nonce).
    pub async fn connect(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        own_user_public_key: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        Self::connect_with_signer(
            endpoint,
            relay_addr,
            Some(own_user_public_key),
            None,
            own_cert,
            &own_sk,
            revocations,
        )
        .await
    }

    /// Connect to a host whose Iroh endpoint address was obtained from a
    /// trusted workspace configuration/invite. The host may be provider-owned
    /// and therefore chain to a different user root.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] if the dial or bi-stream open fails, the
    /// handshake frames can't be encoded/decoded or fail to write/read,
    /// or the server's `HelloAck` fails auth verification (bad
    /// signature, revoked, or stale nonce).
    pub async fn connect_to_pinned_host(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        Self::connect_with_signer(
            endpoint,
            relay_addr,
            None,
            None,
            own_cert,
            &own_sk,
            revocations,
        )
        .await
    }

    /// Connect to an enrolled home peer, pinning both the user's root
    /// certificate and the exact enrolled device certificate.  The relay
    /// service endpoint is intentionally a separate Iroh key, so endpoint
    /// pinning alone cannot establish the identity behind it.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when dialing, framing, or the pinned
    /// user/device authentication check fails.
    pub async fn connect_to_enrolled_home_peer(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        Self::connect_with_signer(
            endpoint,
            relay_addr,
            Some(expected_server_user),
            Some(expected_server_device),
            own_cert,
            &own_sk,
            revocations,
        )
        .await
    }

    /// Connect with a device key held by `signer`, for devices that cannot
    /// hand over the key bytes (a browser's non-extractable `WebCrypto` key).
    /// `expected_server_user` pins the relay's user root (`None` accepts any
    /// valid host certificate behind the pinned endpoint);
    /// `expected_server_device` additionally pins its device key.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] as [`Self::connect`] does, or
    /// [`ClientError::Sign`] when the signer fails.
    pub async fn connect_with_signer(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: Option<[u8; 32]>,
        expected_server_device: Option<[u8; 32]>,
        own_cert: DeviceCert,
        signer: &dyn DeviceSigner,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        if signer.device_public_key() != own_cert.device_public_key {
            return Err(ClientError::Sign(
                "signer does not hold the certified device key".into(),
            ));
        }
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
            use rand::Rng;
            rand::rng().fill_bytes(&mut client_nonce);
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
        let now = crate::clock::now_ns();
        let relay_peer = match expected_server_user.as_ref() {
            Some(expected_user) => {
                let relay_peer =
                    auth::verify_hello_ack(&ack, expected_user, &revocations, now, &client_nonce)?;
                if expected_server_device.is_some_and(|expected_device| {
                    relay_peer.cert.device_public_key != expected_device
                }) {
                    return Err(ClientError::Auth(AuthError::PolicyReject));
                }
                relay_peer
            }
            None => auth::verify_pinned_host_ack(&ack, &revocations, now, &client_nonce)?,
        };

        let ack2 = crate::signer::signed_ack2(signer, &ack.server_nonce)
            .await
            .map_err(ClientError::Sign)?;
        let own_ack2_bytes =
            postcard::to_allocvec(&ack2).map_err(|e| ClientError::Codec(e.to_string()))?;
        write_frame(&mut send, &own_ack2_bytes).await?;

        // Split into pump halves.
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Envelope>(256);
        let (inbound_tx, inbound_rx) = mpsc::channel::<Envelope>(256);
        let mut reader = FrameReader::new(recv);
        let pump = n0_future::task::spawn(async move {
            // Keep the socket alive through the final queued Unsubscribe and
            // FIN even if the caller handed this client its only Endpoint.
            let _endpoint_keepalive = endpoint_keepalive;
            loop {
                tokio::select! {
                    biased;
                    ob = outbound_rx.recv() => {
                        if let Some(env) = ob {
                            let Ok(bytes) = crate::frame::encode(&env) else { break };
                            if send.write_all(&bytes).await.is_err() { break; }
                        } else {
                            let _ = send.finish();
                            // Keep the endpoint alive long enough to flush the final
                            // Unsubscribe/FIN. Bound the wait so a dead network cannot
                            // strand this detached cleanup task indefinitely.
                            let _ = n0_future::time::timeout(
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
                            Ok(None) | Err(_) => break,
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
            connection: conn,
            endpoint,
            _pump: pump,
        })
    }

    /// Convenience: send an envelope.
    ///
    /// # Errors
    ///
    /// Returns the envelope back to the caller, boxed, if the outbound
    /// channel to the pump task is closed (the connection is gone).
    pub fn send(&self, env: Envelope) -> Result<(), Box<Envelope>> {
        let subscription_update = match &env {
            Envelope::Subscribe { workspace_id } => Some((*workspace_id, true)),
            Envelope::Unsubscribe { workspace_id } => Some((*workspace_id, false)),
            _ => None,
        };
        self.outbound
            .try_send(env)
            .map_err(|e| Box::new(e.into_inner()))?;
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
        self.connection.close(0u32.into(), b"client closed");
        self.connection.closed().await;
        self.endpoint.close().await;
    }
}
