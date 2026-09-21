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

use crate::auth::{self, AuthDomain, AuthenticatedPeer, Hello, HelloAck};
use crate::framing::{read_frame, write_frame};
use crate::transport::{
    ALPN, ContentEndpointAdvertisement, ContentEndpointRequest, ContentPeerRegistration,
    DISCOVERY_ALPN, Envelope, HomePeerManagementRequest, HomePeerManagementResponse, LEGACY_ALPN,
    LEGACY_DISCOVERY_ALPN, LEGACY_MANAGEMENT_ALPN, MANAGEMENT_ALPN, MAX_FRAME_BYTES,
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
    /// The private v2 sender has closed with its shared relay transport.
    #[error("private transport closed")]
    PrivateTransportClosed,
}

fn classify_connect_error(error: iroh::endpoint::ConnectError) -> ClientError {
    use iroh::endpoint::{ConnectError, ConnectingError, ConnectionError, TransportErrorCode};
    let connection = match &error {
        ConnectError::Connection { source, .. } => Some(source),
        ConnectError::Connecting {
            source: ConnectingError::ConnectionError { source, .. },
            ..
        } => Some(source),
        _ => None,
    };
    let no_application_protocol = match connection {
        Some(ConnectionError::TransportError(error)) => {
            error.code == TransportErrorCode::crypto(120)
        }
        // A live peer sends its TLS alert in a QUIC CONNECTION_CLOSE frame;
        // synthetic/local failures can surface the same code as TransportError.
        Some(ConnectionError::ConnectionClosed(close)) => {
            close.error_code == TransportErrorCode::crypto(120)
        }
        _ => false,
    };
    if no_application_protocol {
        ClientError::UnsupportedProtocol
    } else {
        ClientError::Iroh(error.to_string())
    }
}

#[derive(Clone, Copy)]
enum CompatibilityGeneration {
    Tilo,
    LegacyQos,
}

impl CompatibilityGeneration {
    fn auth_domain(self) -> AuthDomain {
        match self {
            Self::Tilo => AuthDomain::Tilo,
            Self::LegacyQos => AuthDomain::LegacyQos,
        }
    }

    fn discovery_alpn(self) -> &'static [u8] {
        match self {
            Self::Tilo => DISCOVERY_ALPN,
            Self::LegacyQos => LEGACY_DISCOVERY_ALPN,
        }
    }

    fn management_alpn(self) -> &'static [u8] {
        match self {
            Self::Tilo => MANAGEMENT_ALPN,
            Self::LegacyQos => LEGACY_MANAGEMENT_ALPN,
        }
    }
}

#[derive(Clone, Copy)]
enum RelayWireProtocol {
    TiloV1,
    TiloV2,
    LegacyQosV1,
    LegacyQosV2,
}

impl RelayWireProtocol {
    fn alpn(self) -> &'static [u8] {
        match self {
            Self::TiloV1 => ALPN,
            Self::TiloV2 => crate::transport_v2::ALPN_V2,
            Self::LegacyQosV1 => LEGACY_ALPN,
            Self::LegacyQosV2 => crate::transport_v2::LEGACY_ALPN_V2,
        }
    }

    fn auth_domain(self) -> AuthDomain {
        match self {
            Self::TiloV1 | Self::TiloV2 => AuthDomain::Tilo,
            Self::LegacyQosV1 | Self::LegacyQosV2 => AuthDomain::LegacyQos,
        }
    }

    fn is_v2(self) -> bool {
        matches!(self, Self::TiloV2 | Self::LegacyQosV2)
    }
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
        let current = Self::manage_home_peer_once(
            endpoint,
            relay_addr.clone(),
            expected_server_user,
            expected_server_device,
            own_cert.clone(),
            own_sk.clone(),
            revocations.clone(),
            request.clone(),
            CompatibilityGeneration::Tilo,
        )
        .await;
        if !matches!(current, Err(ClientError::UnsupportedProtocol)) {
            return current;
        }
        Self::manage_home_peer_once(
            endpoint,
            relay_addr,
            expected_server_user,
            expected_server_device,
            own_cert,
            own_sk,
            revocations,
            request,
            CompatibilityGeneration::LegacyQos,
        )
        .await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one protocol attempt keeps the negotiated ALPN and its exact auth domain together"
    )]
    async fn manage_home_peer_once(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
        request: HomePeerManagementRequest,
        generation: CompatibilityGeneration,
    ) -> Result<HomePeerManagementResponse, ClientError> {
        let conn = endpoint
            .connect(relay_addr, generation.management_alpn())
            .await
            .map_err(classify_connect_error)?;
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
            generation.auth_domain(),
            &ack,
            &expected_server_user,
            &revocations,
            crate::clock::now_ns(),
            &nonce,
        )?;
        if ack.cert.device_public_key != expected_server_device {
            return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject));
        }
        let ack2 = auth::build_hello_ack2(generation.auth_domain(), &own_sk, &ack.server_nonce);
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
        let current = Self::discover_content_peer_once(
            endpoint,
            relay_addr.clone(),
            workspace_id,
            expected_server,
            own_cert.clone(),
            own_sk.clone(),
            revocations.clone(),
            CompatibilityGeneration::Tilo,
        )
        .await;
        if !matches!(current, Err(ClientError::UnsupportedProtocol)) {
            return current;
        }
        Self::discover_content_peer_once(
            endpoint,
            relay_addr,
            workspace_id,
            expected_server,
            own_cert,
            own_sk,
            revocations,
            CompatibilityGeneration::LegacyQos,
        )
        .await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one protocol attempt keeps the negotiated ALPN and its exact auth domain together"
    )]
    async fn discover_content_peer_once(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        workspace_id: u64,
        expected_server: Option<([u8; 32], [u8; 32])>,
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
        generation: CompatibilityGeneration,
    ) -> Result<ContentPeerRegistration, ClientError> {
        let relay_service_id = *relay_addr.id.as_bytes();
        let conn = endpoint
            .connect(relay_addr, generation.discovery_alpn())
            .await
            .map_err(classify_connect_error)?;
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
            Some((user, _)) => auth::verify_hello_ack(
                generation.auth_domain(),
                &ack,
                user,
                &revocations,
                crate::clock::now_ns(),
                &nonce,
            )?,
            None => auth::verify_pinned_host_ack(
                generation.auth_domain(),
                &ack,
                &revocations,
                crate::clock::now_ns(),
                &nonce,
            )?,
        };
        if let Some((_, expected_device)) = expected_server
            && ack.cert.device_public_key != expected_device
        {
            return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject));
        }
        let ack2 = auth::build_hello_ack2(generation.auth_domain(), &own_sk, &ack.server_nonce);
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
        let current = Self::connect_version(
            endpoint.clone(),
            relay_addr.clone(),
            expected_server_user,
            expected_server_device,
            own_cert.clone(),
            own_sk.clone(),
            revocations.clone(),
            RelayWireProtocol::TiloV1,
        )
        .await;
        if !matches!(current, Err(ClientError::UnsupportedProtocol)) {
            return current;
        }
        Self::connect_version(
            endpoint,
            relay_addr,
            expected_server_user,
            expected_server_device,
            own_cert,
            own_sk,
            revocations,
            RelayWireProtocol::LegacyQosV1,
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
        let current = Self::connect_version(
            endpoint.clone(),
            relay_addr.clone(),
            Some(expected_server_user),
            Some(expected_server_device),
            own_cert.clone(),
            own_sk.clone(),
            revocations.clone(),
            RelayWireProtocol::TiloV2,
        )
        .await;
        if !matches!(current, Err(ClientError::UnsupportedProtocol)) {
            return current;
        }
        Self::connect_version(
            endpoint,
            relay_addr,
            Some(expected_server_user),
            Some(expected_server_device),
            own_cert,
            own_sk,
            revocations,
            RelayWireProtocol::LegacyQosV2,
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
        protocol: RelayWireProtocol,
    ) -> Result<Self, ClientError> {
        let v2 = protocol.is_v2();
        let endpoint_keepalive = endpoint.clone();
        let conn = endpoint
            .connect(relay_addr, protocol.alpn())
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
                    protocol.auth_domain(),
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
            None => auth::verify_pinned_host_ack(
                protocol.auth_domain(),
                &ack,
                &revocations,
                now_ns,
                &client_nonce,
            )?,
        };

        let ack2 = auth::build_hello_ack2(protocol.auth_domain(), &own_sk, &ack.server_nonce);
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
            if v2 {
                if run_v2_pump(
                    &mut send,
                    &mut reader,
                    &mut outbound_rx,
                    &mut action_rx,
                    &inbound_tx,
                    &action_inbound_tx,
                )
                .await
                {
                    let _ = send.finish();
                    let _ =
                        n0_future::time::timeout(std::time::Duration::from_secs(2), send.stopped())
                            .await;
                }
            } else {
                loop {
                    // Retain v1 scheduling and state-only behavior verbatim.
                    let event = tokio::select! {
                        biased;
                        state=outbound_rx.recv()=>ClientEvent::State(state),
                        inbound=reader.read_next()=>ClientEvent::Inbound(inbound),
                    };
                    match event {
                        ClientEvent::State(Some(env)) => {
                            #[cfg(not(target_arch = "wasm32"))]
                            if crate::persistence::correlated_diagnostics()
                                && let Envelope::Delta {
                                    workspace_id,
                                    authored_at_ns,
                                    ..
                                } = &env
                            {
                                tracing::info!(
                                    phase = "client_delta_write",
                                    workspace_id,
                                    authored_at_ns
                                );
                            }
                            let Ok(bytes) = crate::transport_v2::encode_state(&env, false) else {
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
                        ClientEvent::Inbound(Ok(Some(EnvelopeV2::State(env)))) => {
                            #[cfg(not(target_arch = "wasm32"))]
                            if crate::persistence::correlated_diagnostics()
                                && let Envelope::Delta {
                                    workspace_id,
                                    authored_at_ns,
                                    ..
                                } = &env
                            {
                                tracing::info!(
                                    phase = "client_delta_read",
                                    workspace_id,
                                    authored_at_ns
                                );
                            }
                            if inbound_tx.send(env).await.is_err() {
                                break;
                            }
                        }
                        ClientEvent::Inbound(Ok(Some(action))) => {
                            if action_inbound_tx.try_send(action).is_err() {
                                break;
                            }
                        }
                        ClientEvent::Inbound(Ok(None) | Err(_)) => break,
                        ClientEvent::Action(_) => {
                            unreachable!("v1 pump only selects state and inbound events")
                        }
                    }
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
        #[cfg(not(target_arch = "wasm32"))]
        if crate::persistence::correlated_diagnostics()
            && let Envelope::Delta {
                workspace_id,
                authored_at_ns,
                ..
            } = &env
        {
            tracing::info!(phase = "client_delta_queue", workspace_id, authored_at_ns);
        }
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
            .ok_or_else(|| ClientError::Codec("peer lacks relay-v2 action capability".into()))?;
        if matches!(frame, EnvelopeV2::State(_)) {
            return Err(ClientError::Codec("state uses send".into()));
        }
        crate::transport_v2::encode(&frame).map_err(|e| ClientError::Codec(e.to_string()))?;
        tx.try_send(frame).map_err(private_send_error)
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
                    || tilo_user_identity::verify_device_cert_at(&cert, crate::clock::now_ns())
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

fn private_send_error<T>(error: tokio::sync::mpsc::error::TrySendError<T>) -> ClientError {
    match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => {
            ClientError::Codec("private queue full".into())
        }
        tokio::sync::mpsc::error::TrySendError::Closed(_) => ClientError::PrivateTransportClosed,
    }
}

/// Drive the v2 wire until either half ends. `true` requests a graceful send
/// finish because the state producer closed; all other exits close the session.
async fn run_v2_pump<R, W>(
    send: &mut W,
    reader: &mut SessionReader<R>,
    states: &mut mpsc::Receiver<Envelope>,
    actions: &mut mpsc::Receiver<EnvelopeV2>,
    state_sink: &mpsc::Sender<Envelope>,
    action_sink: &mpsc::Sender<EnvelopeV2>,
) -> bool
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let writing = run_v2_writer(send, states, actions);
    let reading = run_v2_reader(reader, state_sink, action_sink);
    tokio::pin!(writing);
    tokio::pin!(reading);
    tokio::select! {
        finish_send = &mut writing => finish_send,
        () = &mut reading => false,
    }
}

async fn run_v2_writer<W>(
    send: &mut W,
    states: &mut mpsc::Receiver<Envelope>,
    actions: &mut mpsc::Receiver<EnvelopeV2>,
) -> bool
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut schedule = V2WriteSchedule::default();
    loop {
        match next_v2_outbound_event(states, actions, &mut schedule).await {
            ClientEvent::State(Some(env)) => {
                #[cfg(not(target_arch = "wasm32"))]
                if crate::persistence::correlated_diagnostics()
                    && let Envelope::Delta {
                        workspace_id,
                        authored_at_ns,
                        ..
                    } = &env
                {
                    tracing::info!(phase = "client_delta_write", workspace_id, authored_at_ns);
                }
                let Ok(bytes) = crate::transport_v2::encode_state(&env, true) else {
                    return false;
                };
                if tokio::io::AsyncWriteExt::write_all(send, &bytes)
                    .await
                    .is_err()
                {
                    return false;
                }
            }
            ClientEvent::State(None) => return true,
            ClientEvent::Action(Some(action)) => {
                let Ok(bytes) = crate::transport_v2::encode(&action) else {
                    return false;
                };
                if tokio::io::AsyncWriteExt::write_all(send, &bytes)
                    .await
                    .is_err()
                {
                    return false;
                }
            }
            ClientEvent::Action(None) => return false,
            ClientEvent::Inbound(_) => {
                unreachable!("v2 writer only selects outbound events")
            }
        }
    }
}

async fn run_v2_reader<R>(
    reader: &mut SessionReader<R>,
    state_sink: &mpsc::Sender<Envelope>,
    action_sink: &mpsc::Sender<EnvelopeV2>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut events_since_yield = 0usize;
    loop {
        cooperative_yield(&mut events_since_yield).await;
        match reader.read_next().await {
            Ok(Some(EnvelopeV2::State(env))) => {
                #[cfg(not(target_arch = "wasm32"))]
                if crate::persistence::correlated_diagnostics()
                    && let Envelope::Delta {
                        workspace_id,
                        authored_at_ns,
                        ..
                    } = &env
                {
                    tracing::info!(phase = "client_delta_read", workspace_id, authored_at_ns);
                }
                // The bounded sink plus this one decoded frame is the complete
                // inbound state allowance. Do not read the next wire frame
                // until the application has accepted this exact frame.
                if state_sink.send(env).await.is_err() {
                    return;
                }
            }
            Ok(Some(action)) => {
                // Closing on bounded receiver overflow makes loss explicit;
                // the future journal owner can recover status on reconnect.
                if action_sink.try_send(action).is_err() {
                    return;
                }
            }
            Ok(None) | Err(_) => return,
        }
    }
}

async fn cooperative_yield(events_since_yield: &mut usize) {
    use std::future::poll_fn;
    use std::task::Poll;

    if *events_since_yield == 32 {
        *events_since_yield = 0;
        let mut yielded = false;
        poll_fn(|cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }
    *events_since_yield = events_since_yield.saturating_add(1);
}

#[derive(Default)]
struct V2WriteSchedule {
    next_lane: usize,
    events_since_yield: usize,
    pending_action: Option<EnvelopeV2>,
    state_prefix: usize,
    actions_closed: bool,
}

async fn next_v2_outbound_event(
    states: &mut mpsc::Receiver<Envelope>,
    actions: &mut mpsc::Receiver<EnvelopeV2>,
    schedule: &mut V2WriteSchedule,
) -> ClientEvent {
    use std::future::poll_fn;
    use std::task::Poll;

    cooperative_yield(&mut schedule.events_since_yield).await;
    poll_fn(|cx| {
        for offset in 0..2 {
            let lane = (schedule.next_lane + offset) % 2;
            match lane {
                0 => {
                    if let Poll::Ready(state) = poll_queued(states, cx) {
                        schedule.state_prefix = schedule.state_prefix.saturating_sub(1);
                        schedule.next_lane = 1;
                        return Poll::Ready(ClientEvent::State(state));
                    }
                }
                _ => {
                    if schedule.pending_action.is_none() && !schedule.actions_closed {
                        match poll_queued(actions, cx) {
                            Poll::Ready(Some(action)) => {
                                // Snapshot only the already-queued state prefix.
                                // Later refills cannot extend this barrier.
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
                    if schedule.state_prefix == 0
                        && let Some(action) = schedule.pending_action.take()
                    {
                        schedule.next_lane = 0;
                        return Poll::Ready(ClientEvent::Action(Some(action)));
                    }
                }
            }
        }
        Poll::Pending
    })
    .await
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

#[cfg(test)]
mod scheduling_tests {
    use super::*;

    struct GatedWrite {
        started: Option<tokio::sync::oneshot::Sender<()>>,
        release: tokio::sync::oneshot::Receiver<()>,
        released: bool,
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl tokio::io::AsyncWrite for GatedWrite {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            use std::future::Future as _;
            use std::task::Poll;

            if let Some(started) = self.started.take() {
                self.bytes.lock().unwrap().push(bytes[0]);
                let _ = started.send(());
                return Poll::Ready(Ok(1));
            }
            if !self.released {
                match std::pin::Pin::new(&mut self.release).poll(cx) {
                    Poll::Ready(Ok(())) => self.released = true,
                    Poll::Ready(Err(_)) => {
                        return Poll::Ready(Err(std::io::Error::other(
                            "gated write release dropped",
                        )));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    struct RefillingWrite {
        states: mpsc::Sender<Envelope>,
        expected_enable: Vec<u8>,
        enable_seen: Option<tokio::sync::oneshot::Sender<()>>,
        frames: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl tokio::io::AsyncWrite for RefillingWrite {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.frames.lock().unwrap().push(bytes.to_vec());
            if bytes == self.expected_enable
                && let Some(seen) = self.enable_seen.take()
            {
                let _ = seen.send(());
            }
            match self.states.try_send(Envelope::Ping) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    panic!("state producer closed while the writer was active")
                }
            }
            std::task::Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    struct PendingRead {
        started: Option<tokio::sync::oneshot::Sender<()>>,
        polls: Arc<std::sync::atomic::AtomicUsize>,
        waker: Arc<Mutex<Option<std::task::Waker>>>,
    }

    impl tokio::io::AsyncRead for PendingRead {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            _buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.waker.lock().unwrap() = Some(cx.waker().clone());
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            std::task::Poll::Pending
        }
    }

    #[tokio::test]
    async fn blocked_state_write_does_not_delay_inbound_private_response() {
        use tokio::io::AsyncWriteExt as _;

        let (state_tx, mut states) = mpsc::channel(256);
        let (action_tx, mut actions) = mpsc::channel(32);
        let (state_sink, _state_inbound) = mpsc::channel(256);
        let (action_sink, mut action_inbound) = mpsc::channel(32);
        state_tx.send(Envelope::Ping).await.unwrap();

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        let writer = GatedWrite {
            started: Some(started_tx),
            release: release_rx,
            released: false,
            bytes: written.clone(),
        };
        let (mut relay_write, client_read) = tokio::io::duplex(4096);
        let mut reader = SessionReader::new(client_read, true);
        let pump = tokio::spawn(async move {
            let mut writer = writer;
            run_v2_pump(
                &mut writer,
                &mut reader,
                &mut states,
                &mut actions,
                &state_sink,
                &action_sink,
            )
            .await
        });

        started_rx.await.unwrap();
        assert_eq!(written.lock().unwrap().len(), 1);
        let response = EnvelopeV2::ActionsReady {
            room: uuid::Uuid::from_u128(7),
            capability: crate::transport_v2::ActionCapability::TransportOnly,
        };
        relay_write
            .write_all(&crate::transport_v2::encode(&response).unwrap())
            .await
            .unwrap();
        let observed =
            tokio::time::timeout(std::time::Duration::from_secs(2), action_inbound.recv()).await;

        let _ = release_tx.send(());
        drop(state_tx);
        drop(action_tx);
        let finished = tokio::time::timeout(std::time::Duration::from_secs(2), pump)
            .await
            .expect("released v2 pump did not finish")
            .unwrap();
        assert!(
            finished,
            "closed state producer must request a graceful FIN"
        );
        let bytes = written.lock().unwrap().clone();
        let mut written_reader = SessionReader::new(std::io::Cursor::new(bytes), true);
        assert_eq!(
            written_reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Ping))
        );
        assert_eq!(written_reader.read_next().await.unwrap(), None);
        assert_eq!(
            observed.expect("inbound private response stalled behind outbound state write"),
            Some(response)
        );
    }

    #[tokio::test]
    async fn continuously_ready_writer_preserves_inbound_and_enable_prefix_progress() {
        use tokio::io::AsyncWriteExt as _;

        let (state_tx, mut states) = mpsc::channel(256);
        let (action_tx, mut actions) = mpsc::channel(32);
        let (state_sink, _state_inbound) = mpsc::channel(256);
        let (action_sink, mut action_inbound) = mpsc::channel(32);
        let room = Uuid::from_u128(9);
        let workspace_id = crate::room_workspace_id(&room.to_string()).unwrap();
        for _ in 0..255 {
            state_tx.try_send(Envelope::Ping).unwrap();
        }
        state_tx
            .try_send(Envelope::Subscribe { workspace_id })
            .unwrap();
        let enable = EnvelopeV2::EnableActions { room };
        action_tx.try_send(enable.clone()).unwrap();

        let (enable_seen_tx, enable_seen_rx) = tokio::sync::oneshot::channel();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let writer = RefillingWrite {
            states: state_tx,
            expected_enable: crate::transport_v2::encode(&enable).unwrap(),
            enable_seen: Some(enable_seen_tx),
            frames: frames.clone(),
        };
        let (mut relay_write, client_read) = tokio::io::duplex(4096);
        let mut reader = SessionReader::new(client_read, true);
        let pump = tokio::spawn(async move {
            let mut writer = writer;
            run_v2_pump(
                &mut writer,
                &mut reader,
                &mut states,
                &mut actions,
                &state_sink,
                &action_sink,
            )
            .await
        });

        let response = EnvelopeV2::ActionsReady {
            room,
            capability: crate::transport_v2::ActionCapability::TransportOnly,
        };
        relay_write
            .write_all(&crate::transport_v2::encode(&response).unwrap())
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), action_inbound.recv(),)
                .await
                .expect("continuously ready writer starved the inbound response"),
            Some(response)
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), enable_seen_rx)
            .await
            .expect("continuous state refills extended the finite enable prefix")
            .unwrap();

        let encoded_subscribe =
            crate::transport_v2::encode_state(&Envelope::Subscribe { workspace_id }, true).unwrap();
        let encoded_enable = crate::transport_v2::encode(&enable).unwrap();
        let frames = frames.lock().unwrap();
        let subscribe_position = frames
            .iter()
            .position(|frame| frame == &encoded_subscribe)
            .unwrap();
        let enable_position = frames
            .iter()
            .position(|frame| frame == &encoded_enable)
            .unwrap();
        assert!(subscribe_position < enable_position);
        drop(frames);

        drop(relay_write);
        assert!(
            !tokio::time::timeout(std::time::Duration::from_secs(2), pump)
                .await
                .expect("reader EOF did not stop the shared pump")
                .unwrap()
        );
    }

    #[tokio::test]
    async fn reader_end_cancels_a_blocked_writer_without_detaching_it() {
        let (state_tx, mut states) = mpsc::channel(256);
        let (_action_tx, mut actions) = mpsc::channel(32);
        let (state_sink, _state_inbound) = mpsc::channel(256);
        let (action_sink, _action_inbound) = mpsc::channel(32);
        state_tx.send(Envelope::Ping).await.unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let writer = GatedWrite {
            started: Some(started_tx),
            release: release_rx,
            released: false,
            bytes: Arc::new(Mutex::new(Vec::new())),
        };
        let (relay_write, client_read) = tokio::io::duplex(4096);
        let mut reader = SessionReader::new(client_read, true);
        let pump = tokio::spawn(async move {
            let mut writer = writer;
            run_v2_pump(
                &mut writer,
                &mut reader,
                &mut states,
                &mut actions,
                &state_sink,
                &action_sink,
            )
            .await
        });

        started_rx.await.unwrap();
        drop(relay_write);
        assert!(
            !tokio::time::timeout(std::time::Duration::from_secs(2), pump)
                .await
                .expect("reader EOF did not stop the shared pump")
                .unwrap()
        );
        assert!(
            release_tx.send(()).is_err(),
            "writer future survived after the shared pump returned"
        );
    }

    #[tokio::test]
    async fn writer_end_cancels_a_blocked_reader_without_detaching_it() {
        let (state_tx, mut states) = mpsc::channel(256);
        let (_action_tx, mut actions) = mpsc::channel(32);
        let (state_sink, _state_inbound) = mpsc::channel(256);
        let (action_sink, _action_inbound) = mpsc::channel(32);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let waker = Arc::new(Mutex::new(None));
        let reader = PendingRead {
            started: Some(started_tx),
            polls: polls.clone(),
            waker: waker.clone(),
        };
        let pump = tokio::spawn(async move {
            let mut reader = SessionReader::new(reader, true);
            let mut writer = tokio::io::sink();
            run_v2_pump(
                &mut writer,
                &mut reader,
                &mut states,
                &mut actions,
                &state_sink,
                &action_sink,
            )
            .await
        });

        started_rx.await.unwrap();
        drop(state_tx);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), pump)
                .await
                .expect("closed state producer did not stop the shared pump")
                .unwrap()
        );
        let polls_after_return = polls.load(std::sync::atomic::Ordering::SeqCst);
        waker.lock().unwrap().take().unwrap().wake();
        tokio::task::yield_now().await;
        assert_eq!(
            polls.load(std::sync::atomic::Ordering::SeqCst),
            polls_after_return,
            "reader future was still scheduled after the shared pump returned"
        );
    }

    #[test]
    fn only_tls_no_application_protocol_allows_fallback() {
        use iroh::endpoint::{ConnectError, ConnectionError, TransportError, TransportErrorCode};
        for code in [40, 42, 120] {
            let error: ConnectError = ConnectionError::TransportError(TransportError::new(
                TransportErrorCode::crypto(code),
                "test".into(),
            ))
            .into();
            assert_eq!(
                matches!(
                    super::classify_connect_error(error),
                    super::ClientError::UnsupportedProtocol
                ),
                code == 120
            );
        }
        let error: ConnectError = ConnectionError::TimedOut.into();
        assert!(!matches!(
            super::classify_connect_error(error),
            super::ClientError::UnsupportedProtocol
        ));
    }
    use crate::peer_actions::{ActionMessage, DirectedAction};
    use uuid::Uuid;

    fn action() -> EnvelopeV2 {
        EnvelopeV2::Directed(DirectedAction {
            room: Uuid::from_u128(1),
            request_id: Uuid::from_u128(2),
            source: [1; 32],
            target: [2; 32],
            message: ActionMessage::CatalogueRefresh { scope: None },
        })
    }

    #[test]
    fn private_queue_full_and_closed_have_distinct_errors() {
        let (sender, receiver) = mpsc::channel(1);
        sender.try_send(action()).unwrap();
        assert!(matches!(
            private_send_error(sender.try_send(action()).unwrap_err()),
            ClientError::Codec(message) if message == "private queue full"
        ));
        drop(receiver);
        assert!(matches!(
            private_send_error(sender.try_send(action()).unwrap_err()),
            ClientError::PrivateTransportClosed
        ));
    }

    struct CountingRead {
        inner: std::io::Cursor<Vec<u8>>,
        count: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl tokio::io::AsyncRead for CountingRead {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let before = buf.filled().len();
            let result = std::pin::Pin::new(&mut self.inner).poll_read(cx, buf);
            if result.is_ready() {
                self.count.fetch_add(
                    buf.filled().len() - before,
                    std::sync::atomic::Ordering::SeqCst,
                );
            }
            result
        }
    }

    #[tokio::test]
    async fn full_state_consumer_holds_one_decoded_frame_and_resumes_in_order() {
        let frame = |id| Envelope::Ready {
            workspace_id: id,
            pending_deltas: 0,
        };
        let first = crate::transport_v2::encode(&EnvelopeV2::State(frame(256))).unwrap();
        let second = crate::transport_v2::encode(&EnvelopeV2::State(frame(257))).unwrap();
        let first_len = first.len();
        let wire = [first, second].concat();
        let bytes_read = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader = CountingRead {
            inner: std::io::Cursor::new(wire.clone()),
            count: bytes_read.clone(),
        };
        let (incoming, mut sink) = mpsc::channel(256);
        let (actions, _action_sink) = mpsc::channel(32);
        for id in 0..256 {
            incoming.try_send(frame(id)).unwrap();
        }
        let task = tokio::spawn(async move {
            let mut reader = SessionReader::new(reader, true);
            run_v2_reader(&mut reader, &incoming, &actions).await;
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while bytes_read.load(std::sync::atomic::Ordering::SeqCst) < first_len {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reader never reached the full state sink");
        assert_eq!(
            bytes_read.load(std::sync::atomic::Ordering::SeqCst),
            first_len,
            "reader consumed a second frame while the first waited for state capacity"
        );

        for id in 0..=257 {
            assert_eq!(sink.recv().await, Some(frame(id)));
        }
        task.await.unwrap();
        assert_eq!(
            bytes_read.load(std::sync::atomic::Ordering::SeqCst),
            wire.len()
        );
    }

    #[tokio::test]
    async fn always_ready_wire_yields_to_the_actual_bounded_state_consumer() {
        let (incoming, mut consumed) = mpsc::channel(256);
        let (actions, _action_sink) = mpsc::channel(32);
        let consumer = tokio::spawn(async move {
            let mut count = 0;
            while consumed.recv().await.is_some() {
                count += 1;
            }
            count
        });
        let bytes = crate::transport_v2::encode(&EnvelopeV2::State(Envelope::Pong))
            .unwrap()
            .repeat(1024);
        let mut reader = SessionReader::new(std::io::Cursor::new(bytes), true);
        run_v2_reader(&mut reader, &incoming, &actions).await;
        drop(incoming);
        assert_eq!(consumer.await.unwrap(), 1024);
    }

    #[tokio::test]
    async fn sustained_ready_state_and_private_traffic_both_progress() {
        let (state_tx, mut states) = mpsc::channel(256);
        let (action_tx, mut actions) = mpsc::channel(32);
        for _ in 0..256 {
            state_tx.try_send(Envelope::Ping).unwrap();
        }
        for _ in 0..32 {
            action_tx.try_send(action()).unwrap();
        }
        let mut schedule = V2WriteSchedule::default();
        // Both lanes remain ready for 1024 selections. Assert observable
        // progress per short window, not a probabilistic wall-clock timeout.
        for window in 0..128 {
            let mut counts = [0; 2];
            for _ in 0..8 {
                match next_v2_outbound_event(&mut states, &mut actions, &mut schedule).await {
                    ClientEvent::State(Some(Envelope::Ping)) => {
                        counts[0] += 1;
                        state_tx.try_send(Envelope::Ping).unwrap();
                    }
                    ClientEvent::Action(Some(value)) => {
                        assert_eq!(value, action());
                        counts[1] += 1;
                        action_tx.try_send(action()).unwrap();
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
        let mut schedule = V2WriteSchedule::default();
        let mut subscribed = false;
        let mut enabled = false;
        for _ in 0..1024 {
            match next_v2_outbound_event(&mut states, &mut actions, &mut schedule).await {
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
                _ => panic!("live lane ended"),
            }
        }
        assert!(enabled, "continuous state refills starved private enable");
    }

    #[tokio::test]
    async fn private_inbound_overflow_ends_the_shared_v2_pump() {
        let frame = crate::transport_v2::encode(&action()).unwrap();
        let mut reader = SessionReader::new(std::io::Cursor::new(frame), true);
        let (state_tx, mut states) = mpsc::channel(256);
        let (action_tx, mut actions) = mpsc::channel(32);
        let (state_sink, _state_inbound) = mpsc::channel(256);
        let (action_sink, _action_inbound) = mpsc::channel(32);
        for _ in 0..32 {
            action_sink.try_send(action()).unwrap();
        }
        let mut writer = tokio::io::sink();
        assert!(
            !run_v2_pump(
                &mut writer,
                &mut reader,
                &mut states,
                &mut actions,
                &state_sink,
                &action_sink,
            )
            .await,
            "private overflow must close instead of dropping a response"
        );
        assert_eq!(action_sink.capacity(), 0);
        drop(state_tx);
        drop(action_tx);
    }
}
