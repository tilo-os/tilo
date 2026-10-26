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

use iroh::endpoint::Endpoint;
use iroh_base::EndpointAddr;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use tilo_relay_client::DeviceSigner;
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::auth::{self, AuthenticatedPeer, Hello, HelloAck};
use crate::transport::{
    ALPN, ContentEndpointAdvertisement, ContentEndpointRequest, ContentPeerRegistration,
    DISCOVERY_ALPN, Envelope, HomePeerManagementRequest, HomePeerManagementResponse,
    MANAGEMENT_ALPN, MAX_FRAME_BYTES,
};
use tilo_relay_client::frame::{read_frame, write_frame};

mod handshake;

#[cfg(not(target_arch = "wasm32"))]
use crate::session_carrier::StreamCarrier;
use crate::session_carrier::{CarrierHandle, HttpsCarrier, IrohCarrier, SessionChannels};
use handshake::authenticate_server;
use tilo_delivery_protocol::peer_actions::SignedDirectedAction;
use tilo_delivery_protocol::transport_v2::{EnvelopeV2, SessionReader};

/// Authenticated relay delivery, never constructible from application payloads.
#[derive(Clone, Debug)]
pub struct CertifiedAction {
    action: tilo_delivery_protocol::peer_actions::DirectedAction,
    certificate: DeviceCert,
    // Boxed: the signed bytes would otherwise make every delivery event as
    // large as the largest one.
    signed: Option<Box<SignedDirectedAction>>,
}
impl CertifiedAction {
    /// Exact source-bound private request.
    pub fn action(&self) -> &tilo_delivery_protocol::peer_actions::DirectedAction {
        &self.action
    }
    /// Certificate the relay attached for the session's authenticated source.
    /// The relay can attach any certificate it has seen; only [`Self::signed`]
    /// proves the certificate's device sent this action.
    pub fn certificate(&self) -> &DeviceCert {
        &self.certificate
    }
    /// The source device's signature over exactly [`Self::action`], already
    /// verified against [`Self::certificate`]'s device key on receipt. `None`
    /// for an unsigned delivery, which a broker may serve only for an
    /// effect-free read ([`ActionMessage::requires_signature`]).
    ///
    /// [`ActionMessage::requires_signature`]: tilo_delivery_protocol::peer_actions::ActionMessage::requires_signature
    pub fn signed(&self) -> Option<&SignedDirectedAction> {
        self.signed.as_deref()
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
    /// Present only for the opaque signed interactive-owner trust path.
    workspace_scope: Option<InteractiveWorkspaceScope>,
    own_device: [u8; 32],
    action_outbound: Option<mpsc::Sender<EnvelopeV2>>,
    action_inbound: Arc<AsyncMutex<mpsc::Receiver<EnvelopeV2>>>,
    /// This device's key, present for a session that sends private actions:
    /// every private action this device sends as its own source is signed
    /// with it.
    action_key: Option<ed25519_dalek::SigningKey>,
    /// Owns the authenticated pump's transport lifetime, independently of the
    /// canonical state/private channels and their certification rules.
    carrier: CarrierHandle,
}

#[derive(Clone, Debug)]
struct InteractiveWorkspaceScope {
    workspace_id: u64,
    room: uuid::Uuid,
    authority: DeviceCert,
}

/// Who the dialled server must prove to be: a pinned user root and, when
/// known, the exact enrolled device; or an interactive owner scope; or
/// neither (a pinned-host ACK).
struct ExpectedServer {
    user: Option<[u8; 32]>,
    device: Option<[u8; 32]>,
    /// The pinned device is one of this person's own, hosting rooms
    /// itself: it proves `user` and `device` but needs no host capability.
    own_device: bool,
    interactive_scope: Option<InteractiveWorkspaceScope>,
}

/// This device's own relay credential: its certificate, the signer holding
/// its device key (a key in memory, or a browser's non-extractable one) and
/// the revocation list it checks the server against. A private-action
/// session also holds the key itself, to sign each action it sends.
struct ClientCredentials<'a> {
    cert: DeviceCert,
    signer: &'a dyn DeviceSigner,
    revocations: RevocationList,
    action_key: Option<ed25519_dalek::SigningKey>,
}

fn state_in_scope(scope: &InteractiveWorkspaceScope, envelope: &Envelope) -> bool {
    match envelope {
        Envelope::Subscribe { workspace_id }
        | Envelope::Unsubscribe { workspace_id }
        | Envelope::Delta { workspace_id, .. }
        | Envelope::Presence { workspace_id, .. }
        | Envelope::Ready { workspace_id, .. }
        | Envelope::NotAuthorised { workspace_id, .. }
        | Envelope::SyncRequest { workspace_id, .. }
        | Envelope::SyncResponse { workspace_id, .. }
        | Envelope::RemoteAction { workspace_id, .. }
        | Envelope::RemoteActionUndeliverable { workspace_id, .. }
        | Envelope::RecordContent { workspace_id, .. }
        | Envelope::Commit { workspace_id, .. }
        | Envelope::Committed { workspace_id, .. }
        | Envelope::CommitRefused { workspace_id, .. }
        | Envelope::LiveJoin { workspace_id }
        | Envelope::LiveLeave { workspace_id }
        | Envelope::Live { workspace_id, .. }
        | Envelope::LiveMember { workspace_id, .. }
        | Envelope::SubscribeFrom { workspace_id, .. }
        | Envelope::Position { workspace_id, .. }
        | Envelope::CompactVolunteer { workspace_id }
        | Envelope::CompactRequest { workspace_id, .. }
        | Envelope::Compacted { workspace_id, .. }
        | Envelope::VoiceCall { workspace_id, .. }
        | Envelope::VoiceCallResult { workspace_id, .. }
        | Envelope::VoiceJoin { workspace_id, .. }
        | Envelope::VoiceGranted { workspace_id, .. }
        | Envelope::VoiceRefused { workspace_id, .. }
        | Envelope::VoiceLeave { workspace_id, .. }
        | Envelope::VoiceCalls { workspace_id }
        | Envelope::VoiceCallList { workspace_id, .. }
        | Envelope::VoiceShow { workspace_id, .. }
        | Envelope::OpenRecord { workspace_id, .. }
        | Envelope::RecordOpened { workspace_id, .. } => *workspace_id == scope.workspace_id,
        Envelope::Ping | Envelope::Pong => true,
        Envelope::PeerRelayHello { .. } => false,
    }
}

fn action_in_scope(scope: &InteractiveWorkspaceScope, envelope: &EnvelopeV2) -> bool {
    match envelope {
        EnvelopeV2::State(state) => state_in_scope(scope, state),
        EnvelopeV2::UnsignedDelta { workspace_id, .. }
        | EnvelopeV2::PresenceBatch { workspace_id, .. } => *workspace_id == scope.workspace_id,
        EnvelopeV2::EnableActions { room }
        | EnvelopeV2::ActionsReady { room, .. }
        | EnvelopeV2::Unavailable { room, .. }
        | EnvelopeV2::EnableBrokerClient { room }
        | EnvelopeV2::RegisterBroker { room, .. } => *room == scope.room,
        EnvelopeV2::Directed(action)
        | EnvelopeV2::Delivered { action, .. }
        | EnvelopeV2::CertifiedDelivered { action, .. } => action.room == scope.room,
        EnvelopeV2::SignedDirected(signed)
        | EnvelopeV2::CertifiedSignedDelivered { action: signed, .. } => {
            signed.action().room == scope.room
        }
        // The session reader rebuilds a coded delta into `State`; one that
        // reached this check whole names no scope.
        EnvelopeV2::CodedDelta { .. } => false,
    }
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
            self.carrier.abort(b"client cleanup queue saturated");
        }
    }
}

/// Failure modes.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// TLS `no_application_protocol`: the other side serves no relay stream.
    #[error("relay protocol unsupported")]
    UnsupportedProtocol,
    /// Iroh dial / bi-stream.
    #[error("iroh: {0}")]
    Iroh(String),
    /// The HTTPS carrier did not reach the host, or the host refused it.
    #[error("https: {0}")]
    Https(tilo_relay_client::LinkError),
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
    /// The device signer refused or failed.
    #[error("signer: {0}")]
    Sign(String),
    /// The other device speaks peer protocol `theirs`, not this one's: they
    /// do not sync until the one behind has updated
    /// (docs/architecture/compatibility.md).
    #[error(
        "the other device speaks peer protocol {theirs}, this one {}",
        crate::auth::PEER_PROTOCOL_VERSION
    )]
    PeerVersion {
        /// The other device's version.
        theirs: u32,
    },
    /// The host closed the relay stream without answering this device's
    /// hello at all. A host that refuses a device closes it this way, and so
    /// does one from before versioned handshakes, which cannot read the
    /// hello; [`RelayClient::probe_handshake_version`] tells the two apart.
    #[error("the host closed the stream without answering: {0}")]
    NoHandshakeAnswer(String),
}

/// What a host answered a hello without a leading version
/// ([`RelayClient::probe_handshake_version`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandshakeProbe {
    /// It answered with a handshake of its own, as a host from before
    /// versioned handshakes reads that hello: it is behind every versioned
    /// device ([`crate::auth::UNVERSIONED_PEER_PROTOCOL`]).
    Unversioned,
    /// It answered with its version alone, as every versioned host answers
    /// a hello it cannot read.
    Versioned(u32),
    /// It answered nothing, or something else.
    Unknown,
}

impl HandshakeProbe {
    /// What `frame`, a host's first answer to an unversioned hello, says.
    #[must_use]
    pub fn read(frame: Option<&[u8]>) -> Self {
        let Some(frame) = frame else {
            return Self::Unknown;
        };
        match postcard::take_from_bytes::<u32>(frame) {
            // A versioned host's answer is its version and nothing else.
            Ok((theirs, [])) => Self::Versioned(theirs),
            // An unversioned host's ack starts with its certificate.
            _ if postcard::from_bytes::<UnversionedHelloAck>(frame).is_ok() => Self::Unversioned,
            _ => Self::Unknown,
        }
    }
}

/// A hello as hosts read it before versioned handshakes: no leading
/// version.
#[derive(serde::Serialize)]
struct UnversionedHello<'a> {
    cert: &'a DeviceCert,
    client_nonce: [u8; 32],
}

/// The ack a host from before versioned handshakes answers with.
#[derive(serde::Deserialize)]
struct UnversionedHelloAck {
    #[allow(dead_code, reason = "read only to recognise the ack's shape")]
    cert: DeviceCert,
    #[allow(dead_code, reason = "read only to recognise the ack's shape")]
    server_nonce: [u8; 32],
    #[allow(dead_code, reason = "read only to recognise the ack's shape")]
    sig_over_client_nonce: Vec<u8>,
}

impl ClientError {
    /// The peer protocol version the other side speaks, when that is why
    /// this failed: over the relay stream ([`Self::PeerVersion`]) or over
    /// its HTTPS fallback ([`tilo_relay_client::LinkError::PeerVersion`]),
    /// so a device behind or ahead is told either way, never only logged.
    #[must_use]
    pub fn peer_version(&self) -> Option<u32> {
        match self {
            Self::PeerVersion { theirs }
            | Self::Https(tilo_relay_client::LinkError::PeerVersion { theirs }) => Some(*theirs),
            _ => None,
        }
    }
}

/// A handshake frame read as this device's version, or the other side's
/// version when it speaks another.
fn handshake_frame<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ClientError> {
    crate::auth::decode_handshake(bytes).map_err(|error| match error {
        crate::auth::HandshakeFrameError::Version { theirs } => ClientError::PeerVersion { theirs },
        crate::auth::HandshakeFrameError::Codec(error) => ClientError::Codec(error.to_string()),
    })
}

fn classify_connect_error(error: &iroh::endpoint::ConnectError) -> ClientError {
    use iroh::endpoint::{ConnectError, ConnectingError, ConnectionError, TransportErrorCode};
    let connection = match error {
        ConnectError::Connection { source, .. }
        | ConnectError::Connecting {
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

/// Dial the relay stream. There is one: the peer protocol version is the
/// handshake's first field, so another version is told apart there, never
/// by a dial that falls back (docs/architecture/compatibility.md).
async fn dial(
    endpoint: &Endpoint,
    relay_addr: EndpointAddr,
) -> Result<iroh::endpoint::Connection, ClientError> {
    endpoint
        .connect(relay_addr, ALPN)
        .await
        .map_err(|error| classify_connect_error(&error))
}

impl RelayClient {
    /// Execute an owner-authorized lifecycle operation against a pinned home
    /// peer. The mutual handshake proves both enrolled device certificates.
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
            .map_err(|error| classify_connect_error(&error))?;
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
            &postcard::to_allocvec(&Hello::new(own_cert, nonce))
                .map_err(|error| ClientError::Codec(error.to_string()))?,
        )
        .await?;
        let ack: HelloAck = handshake_frame(&read_frame(&mut recv, MAX_FRAME_BYTES).await?)?;
        auth::verify_hello_ack(
            &ack,
            &expected_server_user,
            &revocations,
            tilo_clock::unix_now_ns(),
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
            .map_err(|error| classify_connect_error(&error))?;
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
            &postcard::to_allocvec(&Hello::new(own_cert.clone(), nonce))
                .map_err(|error| ClientError::Codec(error.to_string()))?,
        )
        .await?;
        let ack: HelloAck = handshake_frame(&read_frame(&mut recv, MAX_FRAME_BYTES).await?)?;
        match expected_server.as_ref() {
            Some((user, _)) => {
                auth::verify_hello_ack(&ack, user, &revocations, tilo_clock::unix_now_ns(), &nonce)?
            }
            None => {
                auth::verify_pinned_host_ack(&ack, &revocations, tilo_clock::unix_now_ns(), &nonce)?
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
    /// Dial `relay_addr` on the relay ALPN, run the auth
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

    /// Ask the host at `relay_addr` whether it predates versioned
    /// handshakes: send it a hello as such a host reads it (no leading
    /// version) and read its first answer. A host that closed the stream on
    /// this device's versioned hello without a word may have refused it, or
    /// may be one that cannot read it; this tells the two apart. Nothing is
    /// authenticated or kept: the connection closes after the answer.
    pub async fn probe_handshake_version(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        own_cert: &DeviceCert,
    ) -> Result<HandshakeProbe, ClientError> {
        let conn = dial(endpoint, relay_addr).await?;
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Iroh(e.to_string()))?;
        let hello = postcard::to_allocvec(&UnversionedHello {
            cert: own_cert,
            client_nonce: rand::random(),
        })
        .map_err(|e| ClientError::Codec(e.to_string()))?;
        write_frame(&mut send, &hello).await?;
        let answer = tilo_relay_client::frame::read_frame_or_eof(&mut recv, MAX_FRAME_BYTES)
            .await
            .ok()
            .flatten();
        conn.close(0u32.into(), b"probe answered");
        Ok(HandshakeProbe::read(answer.as_deref()))
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
        Self::connect_version(
            endpoint,
            relay_addr,
            ExpectedServer {
                user: expected_server_user,
                device: expected_server_device,
                own_device: false,
                interactive_scope: None,
            },
            ClientCredentials {
                cert: own_cert,
                signer,
                revocations,
                action_key: None,
            },
        )
        .await
    }

    /// Connect to the host behind `http`, a host that publishes no iroh
    /// address, over its HTTPS routes. The host must present `host_device`
    /// (pinned from its `/host` answer or a membership kept for it), under any
    /// person's root: the host of a room this device joined. The session
    /// carries state envelopes only; it has no private action channel.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Https`] when the host is not reached, presents
    /// another device, or refuses this device.
    pub async fn connect_https(
        http: Arc<dyn tilo_relay_client::HttpClient>,
        host_device: [u8; 32],
        own_cert: DeviceCert,
        signer: &dyn DeviceSigner,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        if signer.device_public_key() != own_cert.device_public_key {
            return Err(ClientError::Sign(
                "signer does not hold the certified device key".into(),
            ));
        }
        let link = tilo_relay_client::HttpsLink::connect(
            http,
            &own_cert,
            host_device,
            &revocations,
            signer,
        )
        .await
        .map_err(ClientError::Https)?;
        let relay_peer = link.host().clone();
        let (carrier, channels) = HttpsCarrier::start(link);
        Ok(Self::from_authenticated_session(
            relay_peer,
            own_cert.device_public_key,
            channels,
            Box::new(carrier),
        ))
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
            ExpectedServer {
                user: expected_server_user,
                device: expected_server_device,
                own_device: false,
                interactive_scope: None,
            },
            ClientCredentials {
                cert: own_cert,
                signer: &own_sk,
                revocations,
                action_key: None,
            },
        )
        .await
    }

    /// Dial with the same certificate and endpoint pinning as an enrolled
    /// home peer, signing the private actions this device sends. Durable
    /// action execution remains unavailable; private routing is opt-in per
    /// subscribed room via `EnableActions`.
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
            ExpectedServer {
                user: Some(expected_server_user),
                device: Some(expected_server_device),
                own_device: false,
                interactive_scope: None,
            },
            ClientCredentials {
                cert: own_cert,
                signer: &own_sk,
                revocations,
                action_key: Some(own_sk.clone()),
            },
        )
        .await
    }

    /// Connect to `expected_device`, one of this person's own devices
    /// (`own_user`) that hosts rooms itself, signing the private actions
    /// this device sends. The host proves exactly that device under the
    /// person's root; neither side needs host capability. The session is
    /// the person's own: the host answers its rooms' listing and entry on
    /// it.
    ///
    /// # Errors
    ///
    /// As [`Self::connect`]; [`ClientError::Auth`] when the host is not
    /// that device of this person.
    pub async fn connect_v2_to_own_device(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        own_user: [u8; 32],
        expected_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        if own_cert.user_public_key != own_user {
            return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject));
        }
        Self::connect_version(
            endpoint,
            relay_addr,
            ExpectedServer {
                user: Some(own_user),
                device: Some(expected_device),
                own_device: true,
                interactive_scope: None,
            },
            ClientCredentials {
                cert: own_cert,
                signer: &own_sk,
                revocations,
                action_key: Some(own_sk.clone()),
            },
        )
        .await
    }

    /// A session over `read`/`write`, a byte stream pair to a host in the
    /// same process, through the same handshake and session as over Iroh,
    /// private action channel included. The host must prove
    /// `expected_device` of this device's own person; it needs no host
    /// capability.
    ///
    /// # Errors
    ///
    /// As [`Self::connect`], without the dial.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect_v2_over_stream<R, W>(
        mut read: R,
        mut write: W,
        expected_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let relay_peer = authenticate_server(
            &mut write,
            &mut read,
            &own_cert,
            &own_sk,
            &revocations,
            &ExpectedServer {
                user: Some(own_cert.user_public_key),
                device: Some(expected_device),
                own_device: true,
                interactive_scope: None,
            },
        )
        .await?;
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Envelope>(256);
        let (inbound_tx, inbound_rx) = mpsc::channel::<Envelope>(256);
        let (action_tx, mut action_rx) = mpsc::channel::<EnvelopeV2>(32);
        let (action_inbound_tx, action_inbound_rx) = mpsc::channel::<EnvelopeV2>(32);
        let carrier = StreamCarrier::start(async move {
            let mut reader = SessionReader::new(read);
            if run_v2_pump(
                &mut write,
                &mut reader,
                &mut outbound_rx,
                &mut action_rx,
                &inbound_tx,
                &action_inbound_tx,
                None,
            )
            .await
            {
                use tokio::io::AsyncWriteExt as _;
                let _ = write.shutdown().await;
            }
        });
        let mut client = Self::from_authenticated_session(
            relay_peer,
            own_cert.device_public_key,
            SessionChannels {
                outbound: outbound_tx,
                inbound: inbound_rx,
                action_outbound: Some(action_tx),
                action_inbound: action_inbound_rx,
            },
            Box::new(carrier),
        );
        client.action_key = Some(own_sk);
        Ok(client)
    }

    /// Connect to the exact primary device authorized by a verified signed
    /// interactive-owner hint. The opaque trust token binds the service key,
    /// governing certificate, room, and workspace; the resulting client
    /// rejects every state or action frame outside that scope.
    pub async fn connect_v2_to_interactive_workspace_owner(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        trust: tilo_delivery_protocol::workspace_join::InteractiveOwnerTrust,
        own_cert: DeviceCert,
        own_sk: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        if *relay_addr.id.as_bytes() != trust.service_public_key() {
            return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject));
        }
        let room = uuid::Uuid::parse_str(trust.room())
            .map_err(|_| ClientError::Auth(crate::auth::AuthError::PolicyReject))?;
        let scope = InteractiveWorkspaceScope {
            workspace_id: trust.workspace_id(),
            room,
            authority: trust.authority().clone(),
        };
        Self::connect_version(
            endpoint,
            relay_addr,
            ExpectedServer {
                user: None,
                device: None,
                own_device: false,
                interactive_scope: Some(scope),
            },
            ClientCredentials {
                cert: own_cert,
                signer: &own_sk,
                revocations,
                action_key: Some(own_sk.clone()),
            },
        )
        .await
    }

    async fn connect_version(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        server: ExpectedServer,
        credentials: ClientCredentials<'_>,
    ) -> Result<Self, ClientError> {
        let interactive_scope = server.interactive_scope.clone();
        let ClientCredentials {
            cert: own_cert,
            signer,
            revocations,
            action_key,
        } = credentials;
        if signer.device_public_key() != own_cert.device_public_key {
            return Err(ClientError::Sign(
                "signer does not hold the certified device key".into(),
            ));
        }
        let endpoint_keepalive = endpoint.clone();
        let conn = dial(&endpoint, relay_addr).await?;
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Iroh(e.to_string()))?;
        let relay_peer = authenticate_server(
            &mut send,
            &mut recv,
            &own_cert,
            signer,
            &revocations,
            &server,
        )
        .await?;

        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Envelope>(256);
        let (inbound_tx, inbound_rx) = mpsc::channel::<Envelope>(256);
        let (action_tx, mut action_rx) = mpsc::channel::<EnvelopeV2>(32);
        let (action_inbound_tx, action_inbound_rx) = mpsc::channel::<EnvelopeV2>(32);
        let mut reader = SessionReader::new(recv);
        let pump_connection = conn.clone();
        let pump_scope = interactive_scope.clone();
        let pump = n0_future::task::spawn(async move {
            // Keep the socket alive through the final queued Unsubscribe and
            // FIN even if the caller handed this client its only Endpoint.
            let _endpoint_keepalive = endpoint_keepalive;
            if run_v2_pump(
                &mut send,
                &mut reader,
                &mut outbound_rx,
                &mut action_rx,
                &inbound_tx,
                &action_inbound_tx,
                pump_scope.as_ref(),
            )
            .await
            {
                let _ = send.finish();
                let _ = n0_future::time::timeout(std::time::Duration::from_secs(2), send.stopped())
                    .await;
            }
            pump_connection.close(0u32.into(), b"relay pump ended");
        });

        let mut client = Self::from_authenticated_session(
            relay_peer,
            own_cert.device_public_key,
            SessionChannels {
                outbound: outbound_tx,
                inbound: inbound_rx,
                action_outbound: Some(action_tx),
                action_inbound: action_inbound_rx,
            },
            Box::new(IrohCarrier::new(conn, endpoint, pump)),
        );
        // Install the verified interactive scope before exposing the client.
        // Keep the carrier assembly boundary independent of trust DTOs.
        client.workspace_scope = interactive_scope;
        client.action_key = action_key;
        Ok(client)
    }

    /// Internal assembly boundary after successful protocol authentication.
    ///
    /// The caller must have verified the peer's mutual proof and required host
    /// pin, and bind every channel to that exact session. This is deliberately
    /// crate-private: applications cannot turn payload DTOs into a certified
    /// transport. A future v3 verifier must satisfy the same trust boundary.
    pub(crate) fn from_authenticated_session(
        relay_peer: AuthenticatedPeer,
        own_device: [u8; 32],
        channels: SessionChannels,
        carrier: CarrierHandle,
    ) -> Self {
        Self {
            relay_peer,
            outbound: channels.outbound,
            inbound: Arc::new(AsyncMutex::new(channels.inbound)),
            subscriptions: Mutex::new(HashSet::new()),
            workspace_scope: None,
            own_device,
            action_outbound: channels.action_outbound,
            action_inbound: Arc::new(AsyncMutex::new(channels.action_inbound)),
            action_key: None,
            carrier,
        }
    }

    /// Convenience: send an envelope.
    pub fn send(&self, env: Envelope) -> Result<(), Box<Envelope>> {
        if self
            .workspace_scope
            .as_ref()
            .is_some_and(|scope| !state_in_scope(scope, &env))
        {
            return Err(Box::new(env));
        }
        #[cfg(not(target_arch = "wasm32"))]
        if crate::correlated_diagnostics()
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

    /// Queue one private frame. A session without a private channel (the
    /// HTTPS carrier) reports unavailable explicitly. State continues to use
    /// `send` and `recv`, sharing a single wire reader.
    pub fn send_action(&self, frame: EnvelopeV2) -> Result<(), ClientError> {
        let tx = self
            .action_outbound
            .as_ref()
            .ok_or_else(|| ClientError::Codec("this session has no private channel".into()))?;
        if matches!(frame, EnvelopeV2::State(_)) {
            return Err(ClientError::Codec("state uses send".into()));
        }
        if self
            .workspace_scope
            .as_ref()
            .is_some_and(|scope| !action_in_scope(scope, &frame))
        {
            return Err(ClientError::Codec(
                "interactive owner trust does not cover this action room".into(),
            ));
        }
        let frame = self.sign_own_action(frame)?;
        tilo_delivery_protocol::transport_v2::encode(&frame)
            .map_err(|e| ClientError::Codec(e.to_string()))?;
        tx.try_send(frame)
            .map_err(|error| private_send_error(&error))
    }

    /// Sign a private action this device sends as its own source, when the
    /// relay negotiated signed actions. An action naming another source stays
    /// unsigned: the relay refuses it as it always has.
    fn sign_own_action(&self, frame: EnvelopeV2) -> Result<EnvelopeV2, ClientError> {
        match (frame, &self.action_key) {
            (EnvelopeV2::Directed(action), Some(key))
                if action.source == key.verifying_key().to_bytes() =>
            {
                SignedDirectedAction::sign(&action, tilo_clock::unix_now_ns(), key)
                    .map(EnvelopeV2::SignedDirected)
                    .map_err(|error| ClientError::Codec(error.to_string()))
            }
            (frame, _) => Ok(frame),
        }
    }

    /// Receive authenticated source evidence from this exact relay session.
    /// Only this method can construct the evidence; peer control frames cannot.
    ///
    /// A signed delivery is accepted only if the attached certificate's device
    /// signed exactly this action; the relay cannot author or alter one. An
    /// unsigned delivery is evidence of the relay's word alone, and is marked
    /// so ([`CertifiedAction::signed`] is `None`).
    pub async fn recv_certified_action(&self) -> Option<CertifiedActionEvent> {
        let (action, source_certificate, signed) = match self.recv_action().await? {
            EnvelopeV2::CertifiedDelivered {
                action,
                source_certificate,
            } => (action, source_certificate, None),
            EnvelopeV2::CertifiedSignedDelivered {
                action,
                source_certificate,
            } => (
                action.action().clone(),
                source_certificate,
                Some(Box::new(action)),
            ),
            other => return Some(CertifiedActionEvent::Control(other)),
        };
        let cert: DeviceCert = serde_json::from_slice(&source_certificate).ok()?;
        if action.target != self.own_device
            || action.source != cert.device_public_key
            || !action.validate()
            || cert.capabilities.can_act_as_host
            || tilo_user_identity::verify_device_cert_at(&cert, tilo_clock::unix_now_ns()).is_err()
            || signed
                .as_ref()
                .is_some_and(|signed| signed.verify(&cert.device_public_key).is_err())
        {
            return None;
        }
        Some(CertifiedActionEvent::Delivered(CertifiedAction {
            action,
            certificate: cert,
            signed,
        }))
    }

    /// Receive private frames; None means disconnected, or a session without
    /// a private channel.
    pub async fn recv_action(&self) -> Option<EnvelopeV2> {
        self.action_outbound.as_ref()?;
        self.action_inbound.lock().await.recv().await
    }

    /// Convenience: recv the next envelope.
    pub async fn recv(&self) -> Option<Envelope> {
        self.inbound.lock().await.recv().await
    }

    /// The connection's own packet and frame counters (UDP datagrams each
    /// way, frames by type), read from the transport on each call: what a
    /// bench needs to count packets, at no cost to the session.
    #[must_use]
    pub fn transport_stats(&self) -> Option<iroh::endpoint::ConnectionStats> {
        self.carrier.transport_stats()
    }

    /// Disconnect this session without closing an endpoint shared with state peers.
    pub(crate) async fn disconnect(&self) {
        self.carrier.disconnect().await;
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
        self.carrier.close_owned().await;
    }
}

fn private_send_error<T>(error: &tokio::sync::mpsc::error::TrySendError<T>) -> ClientError {
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
    scope: Option<&InteractiveWorkspaceScope>,
) -> bool
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let writing = run_v2_writer(send, states, actions, scope);
    let reading = run_v2_reader(reader, state_sink, action_sink, scope);
    tokio::pin!(writing);
    tokio::pin!(reading);
    tokio::select! {
        finish_send = &mut writing => finish_send,
        () = &mut reading => false,
    }
}

/// Write a session's outbound state and private frames. Pointer presence is
/// paced per slot ([`crate::pointer_pacing`]); each wake writes what it took,
/// and every held pointer that fell due, in one `write_all`.
async fn run_v2_writer<W>(
    send: &mut W,
    states: &mut mpsc::Receiver<Envelope>,
    actions: &mut mpsc::Receiver<EnvelopeV2>,
    scope: Option<&InteractiveWorkspaceScope>,
) -> bool
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use crate::pointer_pacing::{Admitted, PointerPacer};
    use n0_future::time::Instant;

    let mut schedule = V2WriteSchedule::default();
    let mut pointers = PointerPacer::default();
    loop {
        let due = pointers.next_due();
        // Queued envelopes first: a newer move of a held pointer that is
        // already waiting replaces it before the gap's end writes it.
        let event = tokio::select! {
            biased;
            event = next_v2_outbound_event(states, actions, &mut schedule) => Some(event),
            () = pointer_due(due) => None,
        };
        let now = Instant::now();
        let mut out = Vec::new();
        let mut written = None;
        let mut closing = false;
        match event {
            Some(ClientEvent::State(Some(env))) => {
                #[cfg(not(target_arch = "wasm32"))]
                crate::trace_native_publication("client_write_started", &env, None);
                if scope.is_some_and(|scope| !state_in_scope(scope, &env)) {
                    return false;
                }
                #[cfg(not(target_arch = "wasm32"))]
                if crate::correlated_diagnostics()
                    && let Envelope::Delta {
                        workspace_id,
                        authored_at_ns,
                        ..
                    } = &env
                {
                    tracing::info!(phase = "client_delta_write", workspace_id, authored_at_ns);
                }
                if let Admitted::Write { ahead, envelope } = pointers.admit(env, now) {
                    if !encode_states(&ahead, &mut out)
                        || !encode_states(std::slice::from_ref(&envelope), &mut out)
                    {
                        return false;
                    }
                    written = Some(envelope);
                }
            }
            Some(ClientEvent::State(None)) => {
                // The producer is gone; the resting positions still go.
                if !encode_states(&pointers.take_all(now), &mut out) {
                    return false;
                }
                closing = true;
            }
            Some(ClientEvent::Action(Some(action))) => {
                if scope.is_some_and(|scope| !action_in_scope(scope, &action)) {
                    return false;
                }
                let Ok(bytes) = tilo_delivery_protocol::transport_v2::encode(&action) else {
                    return false;
                };
                out.extend_from_slice(&bytes);
            }
            // The private producer is gone: fail closed.
            Some(ClientEvent::Action(None)) => return false,
            None => {}
        }
        // Whatever fell due goes in the same write.
        if !encode_states(&pointers.take_due(now), &mut out) {
            return false;
        }
        if !out.is_empty()
            && tokio::io::AsyncWriteExt::write_all(send, &out)
                .await
                .is_err()
        {
            return false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(env) = &written {
            crate::trace_native_publication("client_written", env, None);
        }
        #[cfg(target_arch = "wasm32")]
        drop(written);
        if closing {
            return true;
        }
    }
}

/// Append `envelopes` to `out` as state frames, in order; `false` when one
/// cannot be encoded.
fn encode_states(envelopes: &[Envelope], out: &mut Vec<u8>) -> bool {
    envelopes.iter().all(|envelope| {
        tilo_delivery_protocol::transport_v2::encode_state(envelope)
            .map(|bytes| out.extend_from_slice(&bytes))
            .is_ok()
    })
}

/// Wait until a held pointer falls due, or forever while none is held.
async fn pointer_due(due: Option<n0_future::time::Instant>) {
    match due {
        Some(due) => n0_future::time::sleep_until(due).await,
        None => std::future::pending().await,
    }
}

async fn run_v2_reader<R>(
    reader: &mut SessionReader<R>,
    state_sink: &mpsc::Sender<Envelope>,
    action_sink: &mpsc::Sender<EnvelopeV2>,
    scope: Option<&InteractiveWorkspaceScope>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut events_since_yield = 0usize;
    loop {
        cooperative_yield(&mut events_since_yield).await;
        match reader.read_next().await {
            Ok(Some(EnvelopeV2::State(env))) => {
                if scope.is_some_and(|scope| !state_in_scope(scope, &env)) {
                    return;
                }
                #[cfg(not(target_arch = "wasm32"))]
                if crate::correlated_diagnostics()
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
                if scope.is_some_and(|scope| !action_in_scope(scope, &action)) {
                    return;
                }
                // Closing on bounded receiver overflow makes loss explicit;
                // the future journal owner can recover status on reconnect.
                if let Err(error) = action_sink.try_send(action) {
                    let (reason, frame) = match error {
                        mpsc::error::TrySendError::Full(frame) => ("its reader is behind", frame),
                        mpsc::error::TrySendError::Closed(frame) => ("nobody reads it", frame),
                    };
                    let frame: String = format!("{frame:?}").chars().take(120).collect();
                    tracing::warn!(
                        reason,
                        frame,
                        capacity = action_sink.max_capacity(),
                        "relay session closed: a private frame found its queue full or closed"
                    );
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
            let lane = schedule.next_lane.wrapping_add(offset) % 2;
            if lane == 0 {
                if let Poll::Ready(state) = poll_queued(states, cx) {
                    schedule.state_prefix = schedule.state_prefix.saturating_sub(1);
                    schedule.next_lane = 1;
                    return Poll::Ready(ClientEvent::State(state));
                }
            } else {
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
                    return Poll::Ready(ClientEvent::Action(Some(Box::new(action))));
                }
            }
        }
        Poll::Pending
    })
    .await
}

/// The writer's next outbound frame. A private frame is boxed: the largest
/// `EnvelopeV2` is several times any state envelope, and private frames are
/// the rare lane.
enum ClientEvent {
    State(Option<Envelope>),
    Action(Option<Box<EnvelopeV2>>),
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
#[allow(
    clippy::similar_names,
    reason = "paired test fixtures are named after the roles they mirror"
)]
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
        let mut reader = SessionReader::new(client_read);
        let pump = tokio::spawn(async move {
            let mut writer = writer;
            run_v2_pump(
                &mut writer,
                &mut reader,
                &mut states,
                &mut actions,
                &state_sink,
                &action_sink,
                None,
            )
            .await
        });

        started_rx.await.unwrap();
        assert_eq!(written.lock().unwrap().len(), 1);
        let response = EnvelopeV2::ActionsReady {
            room: uuid::Uuid::from_u128(7),
            capability: tilo_delivery_protocol::transport_v2::ActionCapability::TransportOnly,
        };
        relay_write
            .write_all(&tilo_delivery_protocol::transport_v2::encode(&response).unwrap())
            .await
            .unwrap();
        let observed =
            tokio::time::timeout(tilo_test_env::hang_bound(), action_inbound.recv()).await;

        let _ = release_tx.send(());
        drop(state_tx);
        drop(action_tx);
        let finished = tilo_test_env::within("released v2 pump did not finish", pump)
            .await
            .unwrap();
        assert!(
            finished,
            "closed state producer must request a graceful FIN"
        );
        let bytes = written.lock().unwrap().clone();
        let mut written_reader = SessionReader::new(std::io::Cursor::new(bytes));
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
            expected_enable: tilo_delivery_protocol::transport_v2::encode(&enable).unwrap(),
            enable_seen: Some(enable_seen_tx),
            frames: frames.clone(),
        };
        let (mut relay_write, client_read) = tokio::io::duplex(4096);
        let mut reader = SessionReader::new(client_read);
        let pump = tokio::spawn(async move {
            let mut writer = writer;
            run_v2_pump(
                &mut writer,
                &mut reader,
                &mut states,
                &mut actions,
                &state_sink,
                &action_sink,
                None,
            )
            .await
        });

        let response = EnvelopeV2::ActionsReady {
            room,
            capability: tilo_delivery_protocol::transport_v2::ActionCapability::TransportOnly,
        };
        relay_write
            .write_all(&tilo_delivery_protocol::transport_v2::encode(&response).unwrap())
            .await
            .unwrap();
        assert_eq!(
            tilo_test_env::within(
                "continuously ready writer starved the inbound response",
                action_inbound.recv()
            )
            .await,
            Some(response)
        );
        tilo_test_env::within(
            "continuous state refills extended the finite enable prefix",
            enable_seen_rx,
        )
        .await
        .unwrap();

        let encoded_subscribe =
            tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Subscribe {
                workspace_id,
            })
            .unwrap();
        let encoded_enable = tilo_delivery_protocol::transport_v2::encode(&enable).unwrap();
        let (subscribe_position, enable_position) = {
            let frames = frames.lock().unwrap();
            (
                frames
                    .iter()
                    .position(|frame| frame == &encoded_subscribe)
                    .unwrap(),
                frames
                    .iter()
                    .position(|frame| frame == &encoded_enable)
                    .unwrap(),
            )
        };
        assert!(subscribe_position < enable_position);

        drop(relay_write);
        assert!(
            !tilo_test_env::within("reader EOF did not stop the shared pump", pump)
                .await
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
        let mut reader = SessionReader::new(client_read);
        let pump = tokio::spawn(async move {
            let mut writer = writer;
            run_v2_pump(
                &mut writer,
                &mut reader,
                &mut states,
                &mut actions,
                &state_sink,
                &action_sink,
                None,
            )
            .await
        });

        started_rx.await.unwrap();
        drop(relay_write);
        assert!(
            !tilo_test_env::within("reader EOF did not stop the shared pump", pump)
                .await
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
            let mut reader = SessionReader::new(reader);
            let mut writer = tokio::io::sink();
            run_v2_pump(
                &mut writer,
                &mut reader,
                &mut states,
                &mut actions,
                &state_sink,
                &action_sink,
                None,
            )
            .await
        });

        started_rx.await.unwrap();
        drop(state_tx);
        assert!(
            tilo_test_env::within("closed state producer did not stop the shared pump", pump)
                .await
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
    fn only_tls_no_application_protocol_reads_as_an_unsupported_relay() {
        use iroh::endpoint::{ConnectError, ConnectionError, TransportError, TransportErrorCode};
        for code in [40, 42, 120] {
            let error: ConnectError = ConnectionError::TransportError(TransportError::new(
                TransportErrorCode::crypto(code),
                "test".into(),
            ))
            .into();
            assert_eq!(
                matches!(
                    super::classify_connect_error(&error),
                    super::ClientError::UnsupportedProtocol
                ),
                code == 120
            );
        }
        let error: ConnectError = ConnectionError::TimedOut.into();
        assert!(!matches!(
            super::classify_connect_error(&error),
            super::ClientError::UnsupportedProtocol
        ));
    }

    use tilo_delivery_protocol::peer_actions::{ActionMessage, DirectedAction};
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
            private_send_error(&sender.try_send(action()).unwrap_err()),
            ClientError::Codec(message) if message == "private queue full"
        ));
        drop(receiver);
        assert!(matches!(
            private_send_error(&sender.try_send(action()).unwrap_err()),
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
                    buf.filled().len().saturating_sub(before),
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
        let first =
            tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::State(frame(256))).unwrap();
        let second =
            tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::State(frame(257))).unwrap();
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
            let mut reader = SessionReader::new(reader);
            run_v2_reader(&mut reader, &incoming, &actions, None).await;
        });

        tilo_test_env::within("reader never reached the full state sink", async {
            while bytes_read.load(std::sync::atomic::Ordering::SeqCst) < first_len {
                tokio::task::yield_now().await;
            }
        })
        .await;
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
        let bytes =
            tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::State(Envelope::Pong))
                .unwrap()
                .repeat(1024);
        let mut reader = SessionReader::new(std::io::Cursor::new(bytes));
        run_v2_reader(&mut reader, &incoming, &actions, None).await;
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
                        assert_eq!(*value, action());
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
                ClientEvent::Action(Some(action)) => {
                    let EnvelopeV2::EnableActions { room: got } = *action else {
                        panic!("an unexpected private frame");
                    };
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
        let frame = tilo_delivery_protocol::transport_v2::encode(&action()).unwrap();
        let mut reader = SessionReader::new(std::io::Cursor::new(frame));
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
                None,
            )
            .await,
            "private overflow must close instead of dropping a response"
        );
        assert_eq!(action_sink.capacity(), 0);
        drop(state_tx);
        drop(action_tx);
    }
}

/// The production session writer, driven on a paused clock: what it puts on
/// the wire for pointer presence, and when.
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod pointer_pacing_writer_tests {
    use super::*;
    use crate::coedit::POINTER_GAP;
    use crate::pointer_pacing::tests::{caret, left, pointer};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::time::{Duration, Instant};

    /// One `write_all`'s bytes and when it went.
    type Chunk = (Instant, Vec<u8>);

    /// Every `write_all` the writer makes, as one chunk.
    #[derive(Clone, Default)]
    struct Wire {
        chunks: Arc<Mutex<Vec<Chunk>>>,
        written: Arc<tokio::sync::Notify>,
    }

    impl tokio::io::AsyncWrite for Wire {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.chunks
                .lock()
                .unwrap()
                .push((Instant::now(), bytes.to_vec()));
            self.written.notify_one();
            Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl Wire {
        /// Each write, as the state envelopes a relay decodes from it.
        async fn writes(&self) -> Vec<(Instant, Vec<Envelope>)> {
            let chunks = self.chunks.lock().unwrap().clone();
            let mut writes = Vec::new();
            for (at, bytes) in chunks {
                let mut reader = SessionReader::new(std::io::Cursor::new(bytes));
                let mut envelopes = Vec::new();
                while let Some(frame) = reader.read_next().await.unwrap() {
                    let EnvelopeV2::State(envelope) = frame else {
                        panic!("a state write carried {frame:?}");
                    };
                    envelopes.push(envelope);
                }
                writes.push((at, envelopes));
            }
            writes
        }
    }

    struct Writer {
        states: mpsc::Sender<Envelope>,
        _actions: mpsc::Sender<EnvelopeV2>,
        wire: Wire,
        task: tokio::task::JoinHandle<bool>,
    }

    fn writer() -> Writer {
        let (states_tx, mut states) = mpsc::channel(256);
        let (actions_tx, mut actions) = mpsc::channel(32);
        let wire = Wire::default();
        let mut send = wire.clone();
        let task =
            tokio::spawn(
                async move { run_v2_writer(&mut send, &mut states, &mut actions, None).await },
            );
        Writer {
            states: states_tx,
            _actions: actions_tx,
            wire,
            task,
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[tokio::test(start_paused = true)]
    async fn a_30_hz_pointer_is_written_at_20_hz_from_its_first_move_to_its_resting_position() {
        let pump = writer();
        let t0 = Instant::now();
        for step in 0..10_u32 {
            tokio::time::sleep_until(t0 + ms(33) * step).await;
            pump.states
                .send(pointer("tile", f64::from(step)))
                .await
                .unwrap();
        }
        let last_posted = t0 + ms(33 * 9);
        tokio::time::sleep(POINTER_GAP * 4).await;

        let writes = pump.wire.writes().await;
        assert_eq!(
            writes.first(),
            Some(&(t0, vec![pointer("tile", 0.0)])),
            "the first move after a pause goes at once"
        );
        for pair in writes.windows(2) {
            assert!(
                pair[1].0 - pair[0].0 >= POINTER_GAP,
                "one slot written twice within the gap: {:?}",
                pair.iter().map(|(at, _)| *at - t0).collect::<Vec<_>>()
            );
        }
        let (rested_at, rested) = writes.last().unwrap();
        assert_eq!(
            rested,
            &vec![pointer("tile", 9.0)],
            "the resting position goes"
        );
        assert!(*rested_at <= last_posted + POINTER_GAP);
        assert!(writes.iter().all(|(_, envelopes)| envelopes.len() == 1));
        // Ten moves over 297 ms: the first at once, then one per gap.
        assert_eq!(writes.len(), 7);

        drop(pump.states);
        assert!(pump.task.await.unwrap());
    }

    /// Count actual production writer calls under a controlled sample clock.
    /// Network packet counters include path probes, ACKs and retransmissions,
    /// which cannot say how much presence work the session writer performed.
    #[tokio::test(start_paused = true)]
    async fn sixty_100_hz_moves_cost_thirteen_writes_and_deliver_the_resting_position() {
        let pump = writer();
        let t0 = Instant::now();
        for step in 0..60_u32 {
            tokio::time::sleep_until(t0 + ms(10) * step).await;
            pump.states
                .send(pointer("tile", f64::from(step)))
                .await
                .unwrap();
        }
        let last = pointer("tile", 59.0);
        let writes = tokio::time::timeout(POINTER_GAP * 2, async {
            loop {
                let writes = pump.wire.writes().await;
                if writes
                    .last()
                    .is_some_and(|(_, frames)| frames.last() == Some(&last))
                {
                    break writes;
                }
                // notify_one retains a permit if a write races this check.
                pump.wire.written.notified().await;
            }
        })
        .await
        .expect("the resting pointer is written without another input");
        println!(
            "POINTER_PACING_WORK moves=60 writes={} frames={}",
            writes.len(),
            writes.iter().map(|(_, frames)| frames.len()).sum::<usize>()
        );
        assert_eq!(
            writes.len(),
            13,
            "one leading write, then one per logical 50 ms gap"
        );
        assert_eq!(writes.first(), Some(&(t0, vec![pointer("tile", 0.0)])));
        assert_eq!(writes.last(), Some(&(t0 + ms(600), vec![last])));
        assert!(writes.iter().all(|(_, frames)| frames.len() == 1));
        for pair in writes.windows(2) {
            assert!(pair[1].0 - pair[0].0 >= POINTER_GAP);
        }
        drop(pump.states);
        assert!(pump.task.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn an_event_takes_the_held_pointer_ahead_of_it_in_one_write() {
        let pump = writer();
        let t0 = Instant::now();
        pump.states.send(pointer("tile", 1.0)).await.unwrap();
        tokio::time::sleep(ms(10)).await;
        pump.states.send(pointer("tile", 2.0)).await.unwrap();
        pump.states.send(left()).await.unwrap();
        tokio::time::sleep(POINTER_GAP * 4).await;

        assert_eq!(
            pump.wire.writes().await,
            vec![
                (t0, vec![pointer("tile", 1.0)]),
                (t0 + ms(10), vec![pointer("tile", 2.0), left()]),
            ],
            "an app's Left must never overtake its last pointer"
        );
        drop(pump.states);
        assert!(pump.task.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn a_caret_is_not_held_and_may_pass_a_held_pointer() {
        let pump = writer();
        let t0 = Instant::now();
        pump.states.send(pointer("tile", 1.0)).await.unwrap();
        tokio::time::sleep(ms(10)).await;
        pump.states.send(pointer("tile", 2.0)).await.unwrap();
        pump.states.send(caret(4)).await.unwrap();
        tokio::time::sleep(POINTER_GAP * 4).await;

        assert_eq!(
            pump.wire.writes().await,
            vec![
                (t0, vec![pointer("tile", 1.0)]),
                (t0 + ms(10), vec![caret(4)]),
                (t0 + POINTER_GAP, vec![pointer("tile", 2.0)]),
            ]
        );
        drop(pump.states);
        assert!(pump.task.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn closing_the_session_still_writes_the_resting_position() {
        let pump = writer();
        let t0 = Instant::now();
        pump.states.send(pointer("tile", 1.0)).await.unwrap();
        tokio::time::sleep(ms(10)).await;
        pump.states.send(pointer("tile", 2.0)).await.unwrap();
        drop(pump.states);

        assert!(
            pump.task.await.unwrap(),
            "a closed producer finishes gracefully"
        );
        assert_eq!(
            pump.wire.writes().await,
            vec![
                (t0, vec![pointer("tile", 1.0)]),
                (t0 + ms(10), vec![pointer("tile", 2.0)]),
            ]
        );
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod session_carrier_tests {
    use super::*;
    use crate::session_carrier::{SessionCarrier, SessionChannels};
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    struct TestCarrier {
        events: Arc<Mutex<Vec<&'static str>>>,
        finished: Arc<tokio::sync::Notify>,
    }

    impl SessionCarrier for TestCarrier {
        fn abort(&self, reason: &'static [u8]) {
            assert_eq!(reason, b"client cleanup queue saturated");
            self.events.lock().unwrap().push("abort");
        }

        fn disconnect(&self) -> n0_future::boxed::BoxFuture<()> {
            let events = self.events.clone();
            let finished = self.finished.clone();
            Box::pin(async move {
                events.lock().unwrap().push("disconnect");
                finished.notified().await;
            })
        }

        fn close_owned(&self) -> n0_future::boxed::BoxFuture<()> {
            let events = self.events.clone();
            let finished = self.finished.clone();
            Box::pin(async move {
                events.lock().unwrap().push("close-owned");
                finished.notified().await;
            })
        }
    }

    struct Fixture {
        client: RelayClient,
        states: mpsc::Receiver<Envelope>,
        actions: mpsc::Receiver<EnvelopeV2>,
        incoming: mpsc::Sender<EnvelopeV2>,
        events: Arc<Mutex<Vec<&'static str>>>,
        finished: Arc<tokio::sync::Notify>,
    }

    fn certificate(device: u8, capabilities: DeviceCapabilities) -> DeviceCert {
        let root = ed25519_dalek::SigningKey::from_bytes(&[10; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[device; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "session-carrier-test".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    }

    fn fixture(private: bool) -> Fixture {
        let host_key = ed25519_dalek::SigningKey::from_bytes(&[20; 32]);
        let host_cert = certificate(20, DeviceCapabilities::host());
        let ack = auth::build_hello_ack(host_cert.clone(), &host_key, [2; 32], &[1; 32]);
        let peer = auth::verify_hello_ack(
            &ack,
            &host_cert.user_public_key,
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
            &[1; 32],
        )
        .unwrap();
        let (outbound, states) = mpsc::channel(1);
        let (_state_incoming, inbound) = mpsc::channel(1);
        let (action_outbound, actions) = mpsc::channel(1);
        let (incoming, action_inbound) = mpsc::channel(1);
        let events = Arc::new(Mutex::new(vec![]));
        let finished = Arc::new(tokio::sync::Notify::new());
        let carrier = Box::new(TestCarrier {
            events: events.clone(),
            finished: finished.clone(),
        });
        let client = RelayClient::from_authenticated_session(
            peer,
            [30; 32],
            SessionChannels {
                outbound,
                inbound,
                action_outbound: private.then_some(action_outbound),
                action_inbound,
            },
            carrier,
        );
        Fixture {
            client,
            states,
            actions,
            incoming,
            events,
            finished,
        }
    }

    #[tokio::test]
    async fn drop_unsubscribes_without_aborting_a_draining_session() {
        let Fixture {
            client,
            mut states,
            events,
            ..
        } = fixture(true);
        client
            .send(Envelope::Subscribe { workspace_id: 7 })
            .unwrap();
        assert!(matches!(
            states.recv().await,
            Some(Envelope::Subscribe { workspace_id: 7 })
        ));
        drop(client);
        assert!(matches!(
            states.recv().await,
            Some(Envelope::Unsubscribe { workspace_id: 7 })
        ));
        assert!(states.recv().await.is_none());
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn saturated_unsubscribe_aborts_the_carrier() {
        let Fixture {
            client,
            mut states,
            events,
            ..
        } = fixture(true);
        client
            .send(Envelope::Subscribe { workspace_id: 7 })
            .unwrap();
        states.recv().await.unwrap();
        client.send(Envelope::Ping).unwrap();
        drop(client);
        assert_eq!(*events.lock().unwrap(), ["abort"]);
        assert!(matches!(states.recv().await, Some(Envelope::Ping)));
        assert!(states.recv().await.is_none());
    }

    #[tokio::test]
    async fn rejected_subscribe_does_not_create_a_cleanup_subscription() {
        let Fixture {
            client,
            mut states,
            events,
            ..
        } = fixture(true);
        client.send(Envelope::Ping).unwrap();
        assert!(matches!(
            *client
                .send(Envelope::Subscribe { workspace_id: 7 })
                .unwrap_err(),
            Envelope::Subscribe { workspace_id: 7 }
        ));
        states.recv().await.unwrap();
        drop(client);
        assert!(states.recv().await.is_none());
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn disconnect_waits_without_releasing_owned_resources() {
        let Fixture {
            client,
            events,
            finished,
            ..
        } = fixture(true);
        let closing = client.disconnect();
        tokio::pin!(closing);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut closing)
                .await
                .is_err()
        );
        assert_eq!(*events.lock().unwrap(), ["disconnect"]);
        finished.notify_one();
        tilo_test_env::within("closing to complete", &mut closing).await;
        assert_eq!(*events.lock().unwrap(), ["disconnect"]);
    }

    #[tokio::test]
    async fn close_waits_for_owned_resource_shutdown() {
        let Fixture {
            client,
            events,
            finished,
            ..
        } = fixture(true);
        let closing = client.close();
        tokio::pin!(closing);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut closing)
                .await
                .is_err()
        );
        assert_eq!(*events.lock().unwrap(), ["close-owned"]);
        finished.notify_one();
        tilo_test_env::within("closing to complete", &mut closing).await;
    }

    #[tokio::test]
    async fn private_backpressure_and_state_only_unavailability_remain_explicit() {
        let Fixture {
            client, actions, ..
        } = fixture(true);
        let room = uuid::Uuid::from_u128(1);
        client
            .send_action(EnvelopeV2::EnableActions { room })
            .unwrap();
        assert!(matches!(
            client.send_action(EnvelopeV2::EnableActions { room }),
            Err(ClientError::Codec(_))
        ));
        drop(actions);
        assert!(matches!(
            client.send_action(EnvelopeV2::EnableActions { room }),
            Err(ClientError::PrivateTransportClosed)
        ));
        let state_only = fixture(false);
        assert!(matches!(
            state_only
                .client
                .send_action(EnvelopeV2::EnableActions { room }),
            Err(ClientError::Codec(_))
        ));
        assert!(state_only.client.recv_action().await.is_none());
    }

    #[tokio::test]
    async fn private_evidence_still_binds_source_certificate_and_recipient() {
        let fixture = fixture(true);
        let cert = certificate(40, DeviceCapabilities::primary());
        let action = tilo_delivery_protocol::peer_actions::DirectedAction {
            room: uuid::Uuid::from_u128(1),
            request_id: uuid::Uuid::from_u128(2),
            source: cert.device_public_key,
            target: [30; 32],
            message: tilo_delivery_protocol::peer_actions::ActionMessage::CatalogueRefresh {
                scope: None,
            },
        };
        let frame = |action| EnvelopeV2::CertifiedDelivered {
            action,
            source_certificate: serde_json::to_vec(&cert).unwrap(),
        };
        fixture.incoming.send(frame(action.clone())).await.unwrap();
        let Some(CertifiedActionEvent::Delivered(evidence)) =
            fixture.client.recv_certified_action().await
        else {
            panic!("verified delivery missing");
        };
        assert_eq!(evidence.action(), &action);
        assert_eq!(
            evidence.certificate().device_public_key,
            cert.device_public_key
        );
        let mut wrong_source = action.clone();
        wrong_source.source = [41; 32];
        fixture.incoming.send(frame(wrong_source)).await.unwrap();
        assert!(fixture.client.recv_certified_action().await.is_none());
        let mut wrong_target = action;
        wrong_target.target = [31; 32];
        fixture.incoming.send(frame(wrong_target)).await.unwrap();
        assert!(fixture.client.recv_certified_action().await.is_none());
    }

    fn signed_request(
        by: u8,
        source: [u8; 32],
    ) -> tilo_delivery_protocol::peer_actions::SignedDirectedAction {
        let action = tilo_delivery_protocol::peer_actions::DirectedAction {
            room: uuid::Uuid::from_u128(1),
            request_id: uuid::Uuid::from_u128(2),
            source: ed25519_dalek::SigningKey::from_bytes(&[by; 32])
                .verifying_key()
                .to_bytes(),
            target: [30; 32],
            message: tilo_delivery_protocol::peer_actions::ActionMessage::CatalogueRefresh {
                scope: None,
            },
        };
        let signed = tilo_delivery_protocol::peer_actions::SignedDirectedAction::sign(
            &action,
            tilo_clock::unix_now_ns(),
            &ed25519_dalek::SigningKey::from_bytes(&[by; 32]),
        )
        .unwrap();
        if source == action.source {
            return signed;
        }
        // Re-encode the claimed source as a host would, keeping device A's signature.
        let mut wire = postcard::to_allocvec(&signed).unwrap();
        let at = wire
            .windows(32)
            .position(|window| window == action.source)
            .unwrap();
        wire.get_mut(at..at.saturating_add(32))
            .unwrap()
            .copy_from_slice(&source);
        postcard::from_bytes(&wire).unwrap()
    }

    #[tokio::test]
    async fn signed_delivery_is_evidence_only_under_its_source_certificate() {
        let fixture = fixture(true);
        let cert = certificate(40, DeviceCapabilities::primary());
        let frame = |action| EnvelopeV2::CertifiedSignedDelivered {
            action,
            source_certificate: serde_json::to_vec(&cert).unwrap(),
        };
        let signed = signed_request(40, cert.device_public_key);
        fixture.incoming.send(frame(signed.clone())).await.unwrap();
        let Some(CertifiedActionEvent::Delivered(evidence)) =
            fixture.client.recv_certified_action().await
        else {
            panic!("verified signed delivery missing");
        };
        assert_eq!(evidence.action(), signed.action());
        assert_eq!(evidence.signed(), Some(&signed));
        // Device 41 signs an action the host then attributes to device 40.
        fixture
            .incoming
            .send(frame(signed_request(41, cert.device_public_key)))
            .await
            .unwrap();
        assert!(fixture.client.recv_certified_action().await.is_none());
        // An unsigned delivery is the relay's word alone, and says so.
        fixture
            .incoming
            .send(EnvelopeV2::CertifiedDelivered {
                action: signed.action().clone(),
                source_certificate: serde_json::to_vec(&cert).unwrap(),
            })
            .await
            .unwrap();
        let Some(CertifiedActionEvent::Delivered(unsigned)) =
            fixture.client.recv_certified_action().await
        else {
            panic!("unsigned delivery missing");
        };
        assert!(unsigned.signed().is_none());
    }

    #[tokio::test]
    async fn negotiated_session_signs_only_its_own_actions() {
        let mut fixture = fixture(true);
        let key = ed25519_dalek::SigningKey::from_bytes(&[30; 32]);
        let own = signed_request(30, key.verifying_key().to_bytes())
            .action()
            .clone();
        fixture.client.action_key = Some(key.clone());
        fixture
            .client
            .send_action(EnvelopeV2::Directed(own.clone()))
            .unwrap();
        let Some(EnvelopeV2::SignedDirected(signed)) = fixture.actions.recv().await else {
            panic!("own action left unsigned");
        };
        assert_eq!(signed.action(), &own);
        signed.verify(&own.source).unwrap();
        let mut foreign = own;
        foreign.source = [41; 32];
        fixture
            .client
            .send_action(EnvelopeV2::Directed(foreign.clone()))
            .unwrap();
        assert_eq!(
            fixture.actions.recv().await,
            Some(EnvelopeV2::Directed(foreign))
        );
    }

    #[test]
    fn native_client_retains_spawn_compatibility() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<RelayClient>();
    }
}

#[cfg(test)]
mod peer_version_tests {
    use super::ClientError;

    /// A host at another peer protocol version is told apart from any other
    /// failure on both transports, so the person hears who has to update
    /// rather than seeing a link that silently never comes up.
    #[test]
    fn another_version_is_named_on_the_stream_and_over_https() {
        let ours = crate::auth::PEER_PROTOCOL_VERSION;
        let behind = ours.saturating_sub(1);
        assert_eq!(
            ClientError::PeerVersion { theirs: behind }.peer_version(),
            Some(behind)
        );
        assert_eq!(
            ClientError::Https(tilo_relay_client::LinkError::PeerVersion { theirs: behind })
                .peer_version(),
            Some(behind)
        );
        assert_eq!(ClientError::PrivateTransportClosed.peer_version(), None);
        assert_eq!(
            ClientError::Https(tilo_relay_client::LinkError::Malformed("x".into())).peer_version(),
            None
        );
    }
}

#[cfg(test)]
mod handshake_probe_tests {
    use super::HandshakeProbe;

    /// The ack a host from before versioned handshakes sent (its shape at
    /// relay 8fca8fe781): its certificate first, no version.
    #[derive(serde::Serialize)]
    struct OldAck {
        cert: tilo_user_identity::DeviceCert,
        server_nonce: [u8; 32],
        // `serde_bytes` on the host: a length and the bytes, as a `Vec`.
        sig_over_client_nonce: Vec<u8>,
    }

    #[test]
    fn a_probe_tells_a_host_from_before_versions_from_one_that_refused_this_device() {
        let ours = crate::auth::PEER_PROTOCOL_VERSION;
        // A versioned host answers a hello it cannot read with its version.
        let versioned = postcard::to_allocvec(&ours).unwrap();
        assert_eq!(
            HandshakeProbe::read(Some(&versioned)),
            HandshakeProbe::Versioned(ours),
            "a host at this version that closed on the hello refused this device"
        );
        let behind = postcard::to_allocvec(&(ours - 1)).unwrap();
        assert_eq!(
            HandshakeProbe::read(Some(&behind)),
            HandshakeProbe::Versioned(ours - 1)
        );
        // A host from before versions reads the unversioned hello and acks.
        let old = postcard::to_allocvec(&OldAck {
            cert: tilo_user_identity::DeviceCert {
                user_public_key: [6; 32],
                device_public_key: [7; 32],
                device_label: "lounge room host".into(),
                capabilities: tilo_user_identity::DeviceCapabilities::host(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [9; 64],
            },
            server_nonce: [3; 32],
            sig_over_client_nonce: vec![4; 64],
        })
        .unwrap();
        assert_eq!(
            HandshakeProbe::read(Some(&old)),
            HandshakeProbe::Unversioned
        );
        assert_eq!(HandshakeProbe::read(None), HandshakeProbe::Unknown);
        assert_eq!(
            HandshakeProbe::read(Some(b"\xff\xff")),
            HandshakeProbe::Unknown
        );
    }
}
