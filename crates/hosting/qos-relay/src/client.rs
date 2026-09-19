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
use crate::framing::{read_frame, write_frame};
use crate::transport::{
    ALPN, ContentEndpointAdvertisement, ContentEndpointRequest, ContentPeerRegistration,
    DISCOVERY_ALPN, Envelope, HomePeerManagementRequest, HomePeerManagementResponse,
    MANAGEMENT_ALPN, MAX_FRAME_BYTES,
};

use crate::transport_v2::{EnvelopeV2, SessionReader};

/// Authenticated relay delivery, never constructible from application payloads.
#[derive(Clone, Debug)]
pub struct CertifiedAction {
    action: crate::peer_actions::DirectedAction,
    certificate: DeviceCert,
}
impl CertifiedAction {
    /// Exact source-bound private request.
    pub fn action(&self) -> &crate::peer_actions::DirectedAction {
        &self.action
    }
    /// Certificate retained from the relay's mutually authenticated source.
    pub fn certificate(&self) -> &DeviceCert {
        &self.certificate
    }
}
/// Private session control or opaque authenticated delivery.
#[derive(Debug)]
pub enum CertifiedActionEvent {
    /// Verified source and recipient from the authenticated session.
    Delivered(CertifiedAction),
    /// Relay control response.
    Control(EnvelopeV2),
}

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
    own_device: [u8; 32],
    action_outbound: Option<mpsc::Sender<EnvelopeV2>>,
    action_inbound: Arc<AsyncMutex<mpsc::Receiver<EnvelopeV2>>>,
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
    /// TLS no_application_protocol: the only permitted state-only fallback.
    #[error("relay protocol unsupported")]
    UnsupportedProtocol,
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

fn classify_connect_error(error: iroh::endpoint::ConnectError) -> ClientError {
    use iroh::endpoint::{ConnectError, ConnectingError, ConnectionError, TransportErrorCode};
    let connection = match &error {
        ConnectError::Connection { source, .. } => Some(source),
        ConnectError::Connecting { source: ConnectingError::ConnectionError { source, .. }, .. } => Some(source),
        _ => None,
    };
    if matches!(connection, Some(ConnectionError::TransportError(error)) if error.code == TransportErrorCode::crypto(120)) {
        ClientError::UnsupportedProtocol
    } else { ClientError::Iroh(error.to_string()) }
}

impl RelayClient {
    /// Execute an owner-authorized lifecycle operation against a pinned home
    /// peer. The mutual handshake proves both enrolled device certificates.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when dialing, framing, or mutual authentication
    /// fails, including when the relay's pinned user or device differs.
    #[allow(
        clippy::too_many_arguments,
        reason = "public compatibility API mirrors the independently supplied endpoint, pinned identity, and caller credentials"
    )]
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
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut nonce);
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
            return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject));
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
    #[allow(
        clippy::too_many_arguments,
        reason = "public compatibility API keeps caller credentials and pinned relay identity explicit"
    )]
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
    #[allow(
        clippy::too_many_arguments,
        reason = "public compatibility API keeps caller credentials and pinned relay identity explicit"
    )]
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
    #[allow(
        clippy::too_many_arguments,
        reason = "public compatibility API keeps caller credentials and the explicit pinned host identity separate"
    )]
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
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut nonce);
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
            return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject));
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
        Self::connect_with_server_trust(
            endpoint,
            relay_addr,
            Some(own_user_public_key),
            None,
            own_cert,
            own_sk,
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
        Self::connect_with_server_trust(
            endpoint,
            relay_addr,
            None,
            None,
            own_cert,
            own_sk,
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
        Self::connect_with_server_trust(
            endpoint,
            relay_addr,
            Some(expected_server_user),
            Some(expected_server_device),
            own_cert,
            own_sk,
            revocations,
        )
        .await
    }

    async fn connect_with_server_trust(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: Option<[u8; 32]>,
        expected_server_device: Option<[u8; 32]>,
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        Self::connect_version(
            endpoint,
            relay_addr,
            expected_server_user,
            expected_server_device,
            own_cert,
            own_sk,
            revocations,
            false,
        )
        .await
    }

    /// Explicit v2 dial with the same certificate and endpoint pinning as an
    /// enrolled home peer. Authentication errors never trigger downgrade.
    /// Durable action execution remains unavailable; private routing is opt-in
    /// per subscribed room via `EnableActions`.
    ///
    /// # Errors
    /// Returns an error for unsupported ALPN, authentication or transport failure.
    pub async fn connect_v2_to_enrolled_home_peer(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        Self::connect_version(
            endpoint,
            relay_addr,
            Some(expected_server_user),
            Some(expected_server_device),
            own_cert,
            own_sk,
            revocations,
            true,
        )
        .await
    }

    async fn connect_version(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: Option<[u8; 32]>,
        expected_server_device: Option<[u8; 32]>,
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
        v2: bool,
    ) -> Result<Self, ClientError> {
        let endpoint_keepalive = endpoint.clone();
        let conn = endpoint
            .connect(
                relay_addr,
                if v2 {
                    crate::transport_v2::ALPN_V2
                } else {
                    ALPN
                },
            )
            .await
            .map_err(classify_connect_error)?;
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
        let now_ns = crate::clock::now_ns();
        let relay_peer = match expected_server_user.as_ref() {
            Some(expected_user) => {
                let relay_peer = auth::verify_hello_ack(
                    &ack,
                    expected_user,
                    &revocations,
                    now_ns,
                    &client_nonce,
                )?;
                if expected_server_device.is_some_and(|expected_device| {
                    relay_peer.cert.device_public_key != expected_device
                }) {
                    return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject));
                }
                relay_peer
            }
            None => auth::verify_pinned_host_ack(&ack, &revocations, now_ns, &client_nonce)?,
        };

        let ack2 = auth::build_hello_ack2(&own_sk, &ack.server_nonce);
        let own_ack2_bytes =
            postcard::to_allocvec(&ack2).map_err(|e| ClientError::Codec(e.to_string()))?;
        write_frame(&mut send, &own_ack2_bytes).await?;

        // Split into pump halves.
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Envelope>(256);
        let (inbound_tx, inbound_rx) = mpsc::channel::<Envelope>(256);
        let (action_tx, mut action_rx) = mpsc::channel::<EnvelopeV2>(32);
        let (action_inbound_tx, action_inbound_rx) = mpsc::channel::<EnvelopeV2>(32);
        let mut reader = SessionReader::new(recv, v2);
        let pump_connection = conn.clone();
        let pump = n0_future::task::spawn(async move {
            // Keep the socket alive through the final queued Unsubscribe and
            // FIN even if the caller handed this client its only Endpoint.
            let _endpoint_keepalive = endpoint_keepalive;
            let mut schedule = V2Schedule::default();
            loop {
                let event = if v2 {
                    next_v2_event(&mut outbound_rx, &mut action_rx, &mut reader, &mut schedule)
                        .await
                } else {
                    // Retain v1 scheduling and state-only behavior verbatim.
                    tokio::select! {
                        biased;
                        state=outbound_rx.recv()=>ClientEvent::State(state),
                        inbound=reader.read_next()=>ClientEvent::Inbound(inbound),
                    }
                };
                match event {
                    ClientEvent::State(Some(env)) => {
                        let Ok(bytes) = crate::transport_v2::encode_state(&env, v2) else {
                            break;
                        };
                        if send.write_all(&bytes).await.is_err() {
                            break;
                        }
                    }
                    ClientEvent::State(None) => {
                        let _ = send.finish();
                        let _ = n0_future::time::timeout(
                            std::time::Duration::from_secs(2),
                            send.stopped(),
                        )
                        .await;
                        break;
                    }
                    ClientEvent::Action(Some(action)) => {
                        let Ok(bytes) = crate::transport_v2::encode(&action) else {
                            break;
                        };
                        if send.write_all(&bytes).await.is_err() {
                            break;
                        }
                    }
                    ClientEvent::Action(None) => break,
                    ClientEvent::Inbound(Ok(Some(EnvelopeV2::State(env)))) => {
                        // State frames apply backpressure instead of closing on a
                        // full receiver. A slow main-thread consumer (a browser peer
                        // typing) otherwise loses the connection during a replay
                        // burst, reconnects, receives the replay again and never
                        // catches up; the pump simply waits until the consumer
                        // drains. Actions below keep the explicit-loss policy.
                        if inbound_tx.send(env).await.is_err() {
                            break;
                        }
                    }
                    ClientEvent::Inbound(Ok(Some(action))) => {
                        // Closing on bounded receiver overflow makes loss explicit;
                        // the future journal owner can recover status on reconnect.
                        if action_inbound_tx.try_send(action).is_err() {
                            break;
                        }
                    }
                    ClientEvent::Inbound(Ok(None) | Err(_)) => break,
                }
            }
            pump_connection.close(0u32.into(), b"relay pump ended");
        });

        Ok(Self {
            relay_peer,
            outbound: outbound_tx,
            inbound: Arc::new(AsyncMutex::new(inbound_rx)),
            subscriptions: Mutex::new(HashSet::new()),
            own_device: own_cert.device_public_key,
            action_outbound: v2.then_some(action_tx),
            action_inbound: Arc::new(AsyncMutex::new(action_inbound_rx)),
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

    /// Queue one private v2 frame. A v1 session reports unavailable explicitly.
    /// State continues to use `send` and `recv`, sharing a single wire reader.
    ///
    /// # Errors
    /// Returns an error on v1, invalid/oversized frames, or bounded backpressure.
    pub fn send_action(&self, frame: EnvelopeV2) -> Result<(), ClientError> {
        let tx = self
            .action_outbound
            .as_ref()
            .ok_or_else(|| ClientError::Codec("peer lacks qos-relay/2 action capability".into()))?;
        if matches!(frame, EnvelopeV2::State(_)) {
            return Err(ClientError::Codec("state uses send".into()));
        }
        crate::transport_v2::encode(&frame).map_err(|e| ClientError::Codec(e.to_string()))?;
        tx.try_send(frame)
            .map_err(|_| ClientError::Codec("private queue unavailable".into()))
    }

    /// Receive authenticated source evidence from this exact relay session.
    /// Only this method can construct the evidence; peer control frames cannot.
    pub async fn recv_certified_action(&self) -> Option<CertifiedActionEvent> {
        match self.recv_action().await? {
            EnvelopeV2::CertifiedDelivered {
                action,
                source_certificate,
            } => {
                let cert: DeviceCert = serde_json::from_slice(&source_certificate).ok()?;
                if action.target != self.own_device
                    || action.source != cert.device_public_key
                    || !action.validate()
                    || cert.capabilities.can_act_as_host
                    || qos_user_identity::verify_device_cert_at(&cert, crate::clock::now_ns())
                        .is_err()
                {
                    return None;
                }
                Some(CertifiedActionEvent::Delivered(CertifiedAction {
                    action,
                    certificate: cert,
                }))
            }
            other => Some(CertifiedActionEvent::Control(other)),
        }
    }

    /// Receive private frames; None means disconnected or v1.
    pub async fn recv_action(&self) -> Option<EnvelopeV2> {
        self.action_outbound.as_ref()?;
        self.action_inbound.lock().await.recv().await
    }

    /// Convenience: recv the next envelope.
    pub async fn recv(&self) -> Option<Envelope> {
        self.inbound.lock().await.recv().await
    }

    /// Disconnect this session without closing an endpoint shared with state peers.
    pub(crate) async fn disconnect(&self) {
        self.connection
            .close(0u32.into(), b"broker lane disconnected");
        self.connection.closed().await;
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
    /// Close this connection and its owned endpoint. Do not use with a shared endpoint.
    pub async fn close(self) {
        self.connection.close(0u32.into(), b"client closed");
        self.connection.closed().await;
        self.endpoint.close().await;
    }
}

/// Rotate the first ready lane after every event. Only capability activation
/// waits for a finite prefix of already-queued state frames, keeping Subscribe
/// before EnableActions without giving later state refills priority forever.
#[derive(Default)]
struct V2Schedule {
    next_lane: usize,
    pending_action: Option<EnvelopeV2>,
    state_prefix: usize,
    actions_closed: bool,
}

enum ClientEvent {
    State(Option<Envelope>),
    Action(Option<EnvelopeV2>),
    Inbound(Result<Option<EnvelopeV2>, crate::transport::FrameError>),
}

// Poll buffered data before the receiver's cooperative-budget check. A Tokio
// receiver may report Pending after exhausting its task budget even while it
// holds items; letting an immediately-ready wire reader win every such poll
// would defeat the rotation. Empty queues still register their normal waker.
fn poll_queued<T>(
    queue: &mut mpsc::Receiver<T>,
    cx: &mut std::task::Context<'_>,
) -> std::task::Poll<Option<T>> {
    match queue.try_recv() {
        Ok(value) => std::task::Poll::Ready(Some(value)),
        Err(mpsc::error::TryRecvError::Disconnected) => std::task::Poll::Ready(None),
        Err(mpsc::error::TryRecvError::Empty) => queue.poll_recv(cx),
    }
}

async fn next_v2_event<R: tokio::io::AsyncRead + Unpin>(
    states: &mut mpsc::Receiver<Envelope>,
    actions: &mut mpsc::Receiver<EnvelopeV2>,
    reader: &mut SessionReader<R>,
    schedule: &mut V2Schedule,
) -> ClientEvent {
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    let inbound = reader.read_next();
    tokio::pin!(inbound);
    poll_fn(|cx| {
        for offset in 0..3 {
            let lane = (schedule.next_lane + offset) % 3;
            match lane {
                0 => {
                    if let Poll::Ready(state) = poll_queued(states, cx) {
                        schedule.state_prefix = schedule.state_prefix.saturating_sub(1);
                        schedule.next_lane = 1;
                        return Poll::Ready(ClientEvent::State(state));
                    }
                }
                1 => {
                    if schedule.pending_action.is_none() && !schedule.actions_closed {
                        match poll_queued(actions, cx) {
                            Poll::Ready(Some(action)) => {
                                // This snapshots at most the bounded state queue
                                // capacity. Later producer refills cannot extend it.
                                schedule.state_prefix =
                                    if matches!(action, EnvelopeV2::EnableActions { .. }) {
                                        states.len()
                                    } else {
                                        0
                                    };
                                schedule.pending_action = Some(action);
                            }
                            Poll::Ready(None) => schedule.actions_closed = true,
                            Poll::Pending => {}
                        }
                    }
                    if schedule.state_prefix == 0 {
                        if let Some(action) = schedule.pending_action.take() {
                            schedule.next_lane = 2;
                            return Poll::Ready(ClientEvent::Action(Some(action)));
                        }
                    }
                }
                _ => {
                    if let Poll::Ready(frame) = inbound.as_mut().poll(cx) {
                        schedule.next_lane = 0;
                        return Poll::Ready(ClientEvent::Inbound(frame));
                    }
                }
            }
        }
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod scheduling_tests {
    use super::*;
    #[test]
    fn only_tls_no_application_protocol_allows_fallback() {
        use iroh::endpoint::{ConnectionError, ConnectError, TransportError, TransportErrorCode};
        for code in [40, 42, 120] {
            let error: ConnectError=ConnectionError::TransportError(TransportError::new(TransportErrorCode::crypto(code),"test".into())).into();
            assert_eq!(matches!(super::classify_connect_error(error),super::ClientError::UnsupportedProtocol),code==120);
        }
        let error:ConnectError=ConnectionError::TimedOut.into();
        assert!(!matches!(super::classify_connect_error(error),super::ClientError::UnsupportedProtocol));
    }
    use crate::peer_actions::{ActionMessage, DirectedAction};
    use uuid::Uuid;

    fn action() -> EnvelopeV2 {
        EnvelopeV2::Directed(DirectedAction {
            room: Uuid::from_u128(1),
            request_id: Uuid::from_u128(2),
            source: [1; 32],
            target: [2; 32],
            message: ActionMessage::CatalogueRefresh,
        })
    }

    #[tokio::test]
    async fn sustained_ready_state_and_private_traffic_cannot_starve_the_reader() {
        let (state_tx, mut states) = mpsc::channel(256);
        let (action_tx, mut actions) = mpsc::channel(32);
        for _ in 0..256 {
            state_tx.try_send(Envelope::Ping).unwrap();
        }
        for _ in 0..32 {
            action_tx.try_send(action()).unwrap();
        }
        let frame = crate::transport_v2::encode(&EnvelopeV2::State(Envelope::Pong)).unwrap();
        let wire = frame.repeat(1024);
        let mut reader = SessionReader::new(std::io::Cursor::new(wire), true);
        let mut schedule = V2Schedule::default();
        // Every lane remains ready for 1024 selections. Assert observable
        // progress per short window, not a probabilistic wall-clock timeout.
        for window in 0..128 {
            let mut counts = [0; 3];
            for _ in 0..8 {
                match next_v2_event(&mut states, &mut actions, &mut reader, &mut schedule).await {
                    ClientEvent::State(Some(Envelope::Ping)) => {
                        counts[0] += 1;
                        state_tx.try_send(Envelope::Ping).unwrap();
                    }
                    ClientEvent::Action(Some(value)) => {
                        assert_eq!(value, action());
                        counts[1] += 1;
                        action_tx.try_send(action()).unwrap();
                    }
                    ClientEvent::Inbound(Ok(Some(EnvelopeV2::State(Envelope::Pong)))) => {
                        counts[2] += 1
                    }
                    _ => panic!("live lane ended"),
                }
            }
            assert!(
                counts.iter().all(|count| *count > 0),
                "ready lanes must each progress under sustained traffic, window {window}: {counts:?}"
            );
        }
    }

    #[tokio::test]
    async fn action_enable_waits_for_queued_subscribe_but_not_an_unbounded_flood() {
        let (state_tx, mut states) = mpsc::channel(256);
        let (action_tx, mut actions) = mpsc::channel(32);
        let room = Uuid::from_u128(1);
        let workspace_id = crate::room_workspace_id(&room.to_string()).unwrap();
        for _ in 0..255 {
            state_tx.try_send(Envelope::Ping).unwrap();
        }
        state_tx
            .try_send(Envelope::Subscribe { workspace_id })
            .unwrap();
        action_tx
            .try_send(EnvelopeV2::EnableActions { room })
            .unwrap();
        let frame = crate::transport_v2::encode(&EnvelopeV2::State(Envelope::Pong)).unwrap();
        let mut reader = SessionReader::new(std::io::Cursor::new(frame.repeat(1024)), true);
        let mut schedule = V2Schedule::default();
        let mut subscribed = false;
        let mut enabled = false;
        let mut received = 0;
        for _ in 0..1024 {
            match next_v2_event(&mut states, &mut actions, &mut reader, &mut schedule).await {
                ClientEvent::State(Some(env)) => {
                    if matches!(env, Envelope::Subscribe { .. }) {
                        subscribed = true;
                    }
                    state_tx.try_send(Envelope::Ping).unwrap();
                }
                ClientEvent::Action(Some(EnvelopeV2::EnableActions { room: got })) => {
                    assert!(subscribed, "private enable overtook its state subscription");
                    assert_eq!(got, room);
                    enabled = true;
                    break;
                }
                ClientEvent::Inbound(Ok(Some(_))) => received += 1,
                _ => panic!("live lane ended"),
            }
        }
        assert!(enabled, "continuous state refills starved private enable");
        assert!(received > 0, "subscription barrier starved the sole reader");
    }
}
