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

#[cfg(not(target_arch = "wasm32"))]
use iroh::endpoint::Endpoint;
#[cfg(not(target_arch = "wasm32"))]
use iroh_base::EndpointAddr;
use tilo_relay_client::link::Link;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use tilo_relay_client::DeviceSigner;
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::auth::AuthenticatedPeer;
#[cfg(not(target_arch = "wasm32"))]
use crate::auth::{self, Hello, HelloAck};
use crate::transport::Envelope;
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::{
    ALPN, ContentEndpointAdvertisement, ContentEndpointRequest, ContentPeerRegistration,
    DISCOVERY_ALPN, HomePeerManagementRequest, HomePeerManagementResponse, MANAGEMENT_ALPN,
    MAX_FRAME_BYTES,
};
#[cfg(not(target_arch = "wasm32"))]
use tilo_relay_client::frame::{read_frame, write_frame};
use collab_types::WorkspaceId;

mod handshake;

#[cfg(not(target_arch = "wasm32"))]
use crate::session_carrier::StreamCarrier;
use crate::session_carrier::{CarrierHandle, LinkCarrier, SessionChannels};
use handshake::{authenticate_server, enter_server};
use tilo_delivery_protocol::transport_v2::{EnvelopeV2, SessionReader};

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
    subscriptions: Mutex<HashSet<WorkspaceId>>,
    /// Present only for the opaque signed interactive-owner trust path.
    workspace_scope: Option<InteractiveWorkspaceScope>,
    /// Owns the authenticated pump's transport lifetime, independently of the
    /// state channel and its certification rules.
    carrier: CarrierHandle,
    /// Why an entering session's host did not prove itself: its proof is
    /// read after the session started ([`Entry`]), so a session that ended
    /// before saying anything says here why ([`Self::handshake_failure`]).
    handshake: Arc<Mutex<Option<ClientError>>>,
    /// Who the host had to prove to be, for the entries this device makes on
    /// the session's connection ([`Self::entrance`]).
    server: Option<ExpectedServer>,
}

/// Where a device in a session with its host makes another entry
/// ([`tilo_relay_protocol::entering`]): a stream of its own on the session's
/// connection, whose first message is the entry, ended with the host's
/// answer. A device redeems an invitation this way while it is in other
/// workspaces of the same host.
pub struct Entrance {
    link: Link,
    server: ExpectedServer,
}

impl Entrance {
    /// Enter as `entering` says, as the device `cert` certifies, `signer`
    /// holding its key, checking the host against `revocations`: the host's
    /// answer to the entry.
    ///
    /// # Errors
    ///
    /// The stream did not open, the host did not prove itself or answered no
    /// entry, or the signer failed.
    pub async fn enter(
        &self,
        cert: &DeviceCert,
        signer: &dyn DeviceSigner,
        revocations: RevocationList,
        entering: tilo_relay_protocol::entering::Entering,
    ) -> Result<tilo_relay_protocol::entering::Entered, ClientError> {
        let (mut send, mut recv) = self
            .link
            .open_bi()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let binding = self
            .link
            .binding()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        let awaiting = enter_server(
            &mut send,
            cert,
            signer,
            revocations,
            self.server.clone(),
            (entering, None),
            &binding,
        )
        .await?;
        awaiting.verify(&mut recv).await?;
        let answer = SessionReader::new(recv).read_next().await;
        send.finish();
        match answer {
            Ok(Some(EnvelopeV2::State(Envelope::Entered(entered)))) => Ok(entered),
            Ok(None) => Err(ClientError::NoHandshakeAnswer(
                "the stream ended before the host answered the entry".into(),
            )),
            Ok(Some(_)) => Err(ClientError::Codec(
                "the host answered the entry with something else".into(),
            )),
            Err(error) => Err(ClientError::Codec(error.to_string())),
        }
    }
}

#[derive(Clone, Debug)]
struct InteractiveWorkspaceScope {
    workspace_id: WorkspaceId,
    authority: DeviceCert,
}

/// Who the dialled server must prove to be: a pinned user root and, when
/// known, the exact enrolled device; or an interactive owner scope; or
/// neither (a pinned-host ACK).
#[derive(Clone)]
struct ExpectedServer {
    user: Option<[u8; 32]>,
    device: Option<[u8; 32]>,
    interactive_scope: Option<InteractiveWorkspaceScope>,
}

/// This device's own relay credential: its certificate, the signer holding
/// its device key (a key in memory, or a browser's non-extractable one) and
/// the revocation list it checks the server against.
pub struct ClientCredentials<'a> {
    pub cert: DeviceCert,
    pub signer: &'a dyn DeviceSigner,
    pub revocations: RevocationList,
    /// The workspace a device the host has not admitted yet enters on this
    /// session ([`tilo_relay_protocol::entering`]).
    pub entering: Option<Entry>,
}

/// What a device the host has not admitted yet brings to its entry
/// ([`tilo_relay_protocol::entering`]).
#[derive(Clone, Debug)]
pub struct Entry {
    /// The workspace whose door it enters by, as its hello names it.
    pub entering: tilo_relay_protocol::entering::Entering,
    /// Its signed request for the keys of the workspace's channel group,
    /// which its hello carries ([`tilo_relay_protocol::entering::Entry`]).
    pub access: Option<collab_types::MlsAccessRequestV1>,
    /// The host's certificate, pinned: the session is taken as this host's
    /// from its start, before the host's proof arrives, and that proof is
    /// checked first of all it reads.
    pub host_cert: DeviceCert,
}

fn state_in_scope(scope: &InteractiveWorkspaceScope, envelope: &Envelope) -> bool {
    match envelope {
        Envelope::Subscribe { workspace_id }
        | Envelope::Unsubscribe { workspace_id }
        | Envelope::Delta { workspace_id, .. }
        | Envelope::RetiredPresence { workspace_id, .. }
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
        | Envelope::RetiredLiveJoin { workspace_id }
        | Envelope::LiveLeave { workspace_id }
        | Envelope::RetiredLive { workspace_id, .. }
        | Envelope::RetiredLiveMember { workspace_id, .. }
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
        | Envelope::RecordOpened { workspace_id, .. }
        | Envelope::RecordContentWant { workspace_id, .. }
        | Envelope::HostSaid { workspace_id, .. }
        | Envelope::WorkspaceBootstrap { workspace_id, .. } => *workspace_id == scope.workspace_id,
        // The session's own word on itself, which concerns no other workspace.
        Envelope::Ping | Envelope::Pong | Envelope::ChannelReader => true,
        // Host business, which a workspace-scoped session does not ask: the
        // scope is one workspace's opaque trust, not a session on the host.
        Envelope::PeerRelayHello { .. }
        | Envelope::HostAction { .. }
        | Envelope::WorkspacesRequest { .. }
        | Envelope::WorkspacesReply { .. }
        | Envelope::RetiredHereRequest { .. }
        | Envelope::RetiredHereReply { .. }
        | Envelope::AgentRun { .. }
        | Envelope::AgentRunAnswer { .. }
        | Envelope::AgentRunStopped
        | Envelope::Entered(_)
        | Envelope::FetchGrant { .. } => false,
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
    /// The transport under the session: its dial, or a stream on it.
    #[error("transport: {0}")]
    Transport(String),
    /// Postcard encode/decode.
    #[error("codec: {0}")]
    Codec(String),
    /// Auth check failed.
    #[error("auth: {0}")]
    Auth(#[from] crate::auth::AuthError),
    /// I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
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
#[cfg(not(target_arch = "wasm32"))]
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
    /// this failed: over the relay stream ([`Self::PeerVersion`]), so a
    /// device behind or ahead is told, never only logged.
    #[must_use]
    pub fn peer_version(&self) -> Option<u32> {
        match self {
            Self::PeerVersion { theirs } => Some(*theirs),
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

#[cfg(not(target_arch = "wasm32"))]
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
        ClientError::Transport(error.to_string())
    }
}

/// Dial the relay stream. There is one: the peer protocol version is the
/// handshake's first field, so another version is told apart there, never
/// by a dial that falls back (docs/architecture/compatibility.md).
#[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn manage_home_peer(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: impl DeviceSigner,
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
            .map_err(|error| ClientError::Transport(error.to_string()))?;
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
        let ack2 = tilo_relay_client::signer::signed_ack2(&own_sk, &ack.server_nonce)
            .await
            .map_err(ClientError::Sign)?;
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
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn discover_content_endpoint(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        workspace_id: WorkspaceId,
        own_user_public_key: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: impl DeviceSigner,
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
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn discover_content_peer(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        workspace_id: WorkspaceId,
        own_user_public_key: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: impl DeviceSigner,
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

    #[cfg(not(target_arch = "wasm32"))]
    async fn discover_content_peer_with_server_trust(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        workspace_id: WorkspaceId,
        expected_server: Option<([u8; 32], [u8; 32])>,
        own_cert: DeviceCert,
        own_sk: impl DeviceSigner,
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
            .map_err(|error| ClientError::Transport(error.to_string()))?;
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
        let ack2 = tilo_relay_client::signer::signed_ack2(&own_sk, &ack.server_nonce)
            .await
            .map_err(ClientError::Sign)?;
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
            return Err(ClientError::Transport(
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
                ClientError::Transport(
                    "relay has no ready filesystem endpoint for workspace".into(),
                )
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
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        own_user_public_key: [u8; 32],
        own_cert: DeviceCert,
        own_sk: impl DeviceSigner,
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
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn probe_handshake_version(
        endpoint: &Endpoint,
        relay_addr: EndpointAddr,
        own_cert: &DeviceCert,
    ) -> Result<HandshakeProbe, ClientError> {
        let conn = dial(endpoint, relay_addr).await?;
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
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
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect_to_pinned_host(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        own_cert: DeviceCert,
        own_sk: impl DeviceSigner,
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
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect_to_enrolled_home_peer(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: impl DeviceSigner,
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

    /// Connect with a device key held by `credentials`' signer, for devices
    /// that cannot hand over the key bytes (a browser's non-extractable
    /// `WebCrypto` key).
    /// `expected_server_user` pins the relay's user root (`None` accepts any
    /// valid host certificate behind the pinned endpoint);
    /// `expected_server_device` additionally pins its device key. With
    /// `credentials.entering`, a device the host has not admitted yet enters a workspace
    /// through its door on this session: the hello carries it with its proof,
    /// the client returns as soon as it is written, so requests follow right
    /// behind; the host reads them once the workspace admitted the device.
    /// The host's answer is the session's first envelope
    /// ([`Envelope::Entered`]); its proof is checked before it, and a session
    /// that ends unproved says why in [`Self::handshake_failure`].
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] as [`Self::connect`] does, or
    /// [`ClientError::Sign`] when the signer fails.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect_with_signer(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: Option<[u8; 32]>,
        expected_server_device: Option<[u8; 32]>,
        credentials: ClientCredentials<'_>,
    ) -> Result<Self, ClientError> {
        Self::connect_version(
            endpoint,
            relay_addr,
            ExpectedServer {
                user: expected_server_user,
                device: expected_server_device,
                interactive_scope: None,
            },
            credentials,
        )
        .await
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn connect_with_server_trust(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: Option<[u8; 32]>,
        expected_server_device: Option<[u8; 32]>,
        own_cert: DeviceCert,
        own_sk: impl DeviceSigner,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        Self::connect_version(
            endpoint,
            relay_addr,
            ExpectedServer {
                user: expected_server_user,
                device: expected_server_device,
                interactive_scope: None,
            },
            ClientCredentials {
                cert: own_cert,
                signer: &own_sk,
                revocations,
                entering: None,
            },
        )
        .await
    }

    /// Dial with the same certificate and endpoint pinning as an enrolled
    /// home peer.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect_v2_to_enrolled_home_peer(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        expected_server_user: [u8; 32],
        expected_server_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: impl DeviceSigner,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        Self::connect_version(
            endpoint,
            relay_addr,
            ExpectedServer {
                user: Some(expected_server_user),
                device: Some(expected_server_device),
                interactive_scope: None,
            },
            ClientCredentials {
                cert: own_cert,
                signer: &own_sk,
                revocations,
                entering: None,
            },
        )
        .await
    }

    /// Connect to `expected_device`, one of this person's own devices
    /// (`own_user`) that hosts workspaces itself. The host proves exactly that
    /// device under the person's root; neither side needs host capability.
    /// The session is the person's own: the host answers its workspaces' listing
    /// and entry on it.
    ///
    /// # Errors
    ///
    /// As [`Self::connect`]; [`ClientError::Auth`] when the host is not
    /// that device of this person.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect_v2_to_own_device(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        own_user: [u8; 32],
        expected_device: [u8; 32],
        own_cert: DeviceCert,
        own_sk: impl DeviceSigner,
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
                interactive_scope: None,
            },
            ClientCredentials {
                cert: own_cert,
                signer: &own_sk,
                revocations,
                entering: None,
            },
        )
        .await
    }

    /// A session over `read`/`write`, a byte stream pair to a host in the
    /// same process, through the same handshake and session as over Iroh.
    /// The host must prove `expected_device` of this device's own person; it
    /// needs no host capability.
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
        own_sk: impl DeviceSigner,
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
                interactive_scope: None,
            },
        )
        .await?;
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Envelope>(256);
        let (inbound_tx, inbound_rx) = mpsc::channel::<Envelope>(256);
        let carrier = StreamCarrier::start(async move {
            let handshake = Mutex::new(None);
            if run_v2_pump(
                &mut write,
                read,
                None,
                &handshake,
                &mut outbound_rx,
                &inbound_tx,
                None,
            )
            .await
            {
                use tokio::io::AsyncWriteExt as _;
                let _ = write.shutdown().await;
            }
        });
        Ok(Self::from_authenticated_session(
            relay_peer,
            SessionChannels {
                outbound: outbound_tx,
                inbound: inbound_rx,
            },
            Box::new(carrier),
        ))
    }

    /// Connect to the exact primary device authorized by a verified signed
    /// interactive-owner hint. The opaque trust token binds the service key,
    /// governing certificate and workspace; the resulting client
    /// rejects every state frame outside that scope.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect_v2_to_interactive_workspace_owner(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        trust: tilo_delivery_protocol::workspace_join::InteractiveOwnerTrust,
        own_cert: DeviceCert,
        own_sk: impl DeviceSigner,
        revocations: RevocationList,
    ) -> Result<Self, ClientError> {
        if *relay_addr.id.as_bytes() != trust.service_public_key() {
            return Err(ClientError::Auth(crate::auth::AuthError::PolicyReject));
        }
        let scope = InteractiveWorkspaceScope {
            workspace_id: trust.workspace_id(),
            authority: trust.host().clone(),
        };
        Self::connect_version(
            endpoint,
            relay_addr,
            ExpectedServer {
                user: None,
                device: None,
                interactive_scope: Some(scope),
            },
            ClientCredentials {
                cert: own_cert,
                signer: &own_sk,
                revocations,
                entering: None,
            },
        )
        .await
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn connect_version(
        endpoint: Endpoint,
        relay_addr: EndpointAddr,
        server: ExpectedServer,
        credentials: ClientCredentials<'_>,
    ) -> Result<Self, ClientError> {
        let conn = dial(&endpoint, relay_addr).await?;
        Self::over_link(Link::iroh(conn), server, credentials, Some(endpoint)).await
    }

    /// A session over `link`, a link to the host's session
    /// ([`tilo_relay_client::link::Purpose::Session`]) over whichever carrier
    /// reaches it: the handshake on its first stream, then the session, the
    /// same on every carrier. The host must prove `expected_server_user` and
    /// `expected_server_device` when they are named.
    ///
    /// # Errors
    ///
    /// As [`Self::connect_with_signer`], without the dial.
    pub async fn connect_over_link(
        link: Link,
        expected_server_user: Option<[u8; 32]>,
        expected_server_device: Option<[u8; 32]>,
        credentials: ClientCredentials<'_>,
    ) -> Result<Self, ClientError> {
        Self::over_link(
            link,
            ExpectedServer {
                user: expected_server_user,
                device: expected_server_device,
                interactive_scope: None,
            },
            credentials,
            #[cfg(not(target_arch = "wasm32"))]
            None,
        )
        .await
    }

    async fn over_link(
        link: Link,
        server: ExpectedServer,
        credentials: ClientCredentials<'_>,
        #[cfg(not(target_arch = "wasm32"))] endpoint: Option<Endpoint>,
    ) -> Result<Self, ClientError> {
        let interactive_scope = server.interactive_scope.clone();
        let ClientCredentials {
            cert: own_cert,
            signer,
            revocations,
            entering,
        } = credentials;
        if signer.device_public_key() != own_cert.device_public_key {
            return Err(ClientError::Sign(
                "signer does not hold the certified device key".into(),
            ));
        }
        let (mut send, mut recv) = link
            .open_bi()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let entrance_server = server.clone();
        let (relay_peer, awaiting) = match entering {
            None => (
                authenticate_server(
                    &mut send,
                    &mut recv,
                    &own_cert,
                    signer,
                    &revocations,
                    &server,
                )
                .await?,
                None,
            ),
            Some(entry) => {
                let binding = link
                    .binding()
                    .await
                    .map_err(|error| ClientError::Transport(error.to_string()))?;
                let host = AuthenticatedPeer {
                    user_public_key: entry.host_cert.user_public_key,
                    device_public_key: entry.host_cert.device_public_key,
                    cert: entry.host_cert.clone(),
                };
                let awaiting = enter_server(
                    &mut send,
                    &own_cert,
                    signer,
                    revocations,
                    server,
                    (entry.entering, entry.access),
                    &binding,
                )
                .await?;
                (host, Some(awaiting))
            }
        };

        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Envelope>(256);
        let (inbound_tx, inbound_rx) = mpsc::channel::<Envelope>(256);
        let pump_link = link.clone();
        let pump_scope = interactive_scope.clone();
        let handshake = Arc::new(Mutex::new(None));
        let pump_handshake = Arc::clone(&handshake);
        #[cfg(not(target_arch = "wasm32"))]
        let endpoint_keepalive = endpoint.clone();
        let pump = n0_future::task::spawn(async move {
            // Keep the socket alive through the final queued Unsubscribe and
            // FIN even if the caller handed this client its only endpoint.
            #[cfg(not(target_arch = "wasm32"))]
            let _endpoint_keepalive = endpoint_keepalive;
            if run_v2_pump(
                &mut send,
                recv,
                awaiting,
                &pump_handshake,
                &mut outbound_rx,
                &inbound_tx,
                pump_scope.as_ref(),
            )
            .await
            {
                send.finish();
                let _ = n0_future::time::timeout(std::time::Duration::from_secs(2), send.stopped())
                    .await;
            }
            pump_link.close("relay pump ended");
        });

        let mut client = Self::from_authenticated_session(
            relay_peer,
            SessionChannels {
                outbound: outbound_tx,
                inbound: inbound_rx,
            },
            Box::new(LinkCarrier::new(
                link,
                #[cfg(not(target_arch = "wasm32"))]
                endpoint,
                pump,
            )),
        );
        // Install the verified interactive scope before exposing the client.
        // Keep the carrier assembly boundary independent of trust DTOs.
        client.workspace_scope = interactive_scope;
        client.handshake = handshake;
        client.server = Some(entrance_server);
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
        channels: SessionChannels,
        carrier: CarrierHandle,
    ) -> Self {
        Self {
            relay_peer,
            outbound: channels.outbound,
            inbound: Arc::new(AsyncMutex::new(channels.inbound)),
            subscriptions: Mutex::new(HashSet::new()),
            workspace_scope: None,
            carrier,
            handshake: Arc::default(),
            server: None,
        }
    }

    /// Where this device makes another entry on the session's connection
    /// ([`Entrance`]): `None` for a session that rides no connection of its
    /// own (one to a host in the same process).
    #[must_use]
    pub fn entrance(&self) -> Option<Entrance> {
        Some(Entrance {
            link: self.carrier.link()?,
            server: self.server.clone()?,
        })
    }

    /// Why this entering session's host did not prove itself, once the
    /// session ended without its proof ([`Self::connect_with_signer`]): what
    /// a failed handshake answers, taken once.
    pub fn handshake_failure(&self) -> Option<ClientError> {
        self.handshake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
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
            tracing::info!(phase = "client_delta_queue", workspace_id = %workspace_id, authored_at_ns);
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

    /// Convenience: recv the next envelope.
    pub async fn recv(&self) -> Option<Envelope> {
        self.inbound.lock().await.recv().await
    }

    /// The connection's own packet and frame counters (UDP datagrams each
    /// way, frames by type), read from the transport on each call: what a
    /// bench needs to count packets, at no cost to the session.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub fn transport_stats(&self) -> Option<iroh::endpoint::ConnectionStats> {
        self.carrier.transport_stats()
    }

    /// Disconnect this session, waiting until the relay has been told, and
    /// leave the endpoint it dialled from open for the next connection.
    ///
    /// Prefer this to dropping the handle whenever the disconnect is
    /// deliberate. `Connection::close` only queues `CONNECTION_CLOSE` and there
    /// is no per-connection flush, so the wait is what puts it on the wire;
    /// without it the relay keeps the subscription until its idle timeout —
    /// fanning deltas out to a connection that is gone rather than queueing
    /// them for store-and-forward.
    pub async fn disconnect(&self) {
        self.carrier.disconnect().await;
    }

    /// Disconnect and close the endpoint this client was built on, which
    /// [`RelayClient::connect`] takes by value on the understanding that the
    /// client owns it. A caller that kept a clone and is still using it — a
    /// page redialling from the endpoint beside its planes — calls
    /// [`RelayClient::disconnect`] instead.
    pub async fn close(self) {
        self.carrier.close_owned().await;
    }
}

/// Prove this device to the host on `send`/`recv`, a link's first stream,
/// with the relay handshake the session opens with
/// ([`crate::auth`]): the device signs the host's challenge with the key
/// `cert` names, and the host must prove `expected_server_user` (and
/// `expected_server_device`, when named). What a link over a carrier that
/// does not say who dialled it (WebTransport) carries before the plane's
/// own admission.
///
/// # Errors
///
/// As the session's handshake ([`RelayClient::connect_over_link`]).
pub async fn prove<R, W>(
    send: &mut W,
    recv: &mut R,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    revocations: &RevocationList,
    expected_server_user: [u8; 32],
    expected_server_device: Option<[u8; 32]>,
) -> Result<AuthenticatedPeer, ClientError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    authenticate_server(
        send,
        recv,
        cert,
        signer,
        revocations,
        &ExpectedServer {
            user: Some(expected_server_user),
            device: expected_server_device,
            interactive_scope: None,
        },
    )
    .await
}

/// Drive the v2 wire until either half ends. `true` requests a graceful send
/// finish because the state producer closed; all other exits close the session.
/// Carry a session's frames both ways over `send`/`recv`. With `awaiting`
/// (an entry's hello written, its host's proof not read yet), outbound frames
/// go at once while the host's proof is read and checked first of all
/// inbound; a proof that fails ends the pump, its reason kept in `handshake`.
async fn run_v2_pump<R, W>(
    send: &mut W,
    mut recv: R,
    awaiting: Option<handshake::AwaitingAck>,
    handshake: &Mutex<Option<ClientError>>,
    states: &mut mpsc::Receiver<Envelope>,
    state_sink: &mpsc::Sender<Envelope>,
    scope: Option<&InteractiveWorkspaceScope>,
) -> bool
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let writing = run_v2_writer(send, states, scope);
    let reading = async move {
        if let Some(awaiting) = awaiting
            && let Err(error) = awaiting.verify(&mut recv).await
        {
            *handshake
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
            return;
        }
        let mut reader = SessionReader::new(recv);
        run_v2_reader(&mut reader, state_sink, scope).await;
    };
    tokio::pin!(writing);
    tokio::pin!(reading);
    tokio::select! {
        finish_send = &mut writing => finish_send,
        () = &mut reading => false,
    }
}

/// Write a session's outbound state frames: each frame goes in its own
/// `write_all`.
async fn run_v2_writer<W>(
    send: &mut W,
    states: &mut mpsc::Receiver<Envelope>,
    scope: Option<&InteractiveWorkspaceScope>,
) -> bool
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut schedule = V2WriteSchedule::default();
    loop {
        let Some(env) = next_v2_outbound_event(states, &mut schedule).await else {
            // The producer is gone.
            return true;
        };
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
            tracing::info!(phase = "client_delta_write", workspace_id = %workspace_id, authored_at_ns);
        }
        let mut out = Vec::new();
        if !encode_states(std::slice::from_ref(&env), &mut out) {
            return false;
        }
        if tokio::io::AsyncWriteExt::write_all(send, &out)
            .await
            .is_err()
        {
            return false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        crate::trace_native_publication("client_written", &env, None);
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

async fn run_v2_reader<R>(
    reader: &mut SessionReader<R>,
    state_sink: &mpsc::Sender<Envelope>,
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
                    tracing::info!(phase = "client_delta_read", workspace_id = %workspace_id, authored_at_ns);
                }
                // The bounded sink plus this one decoded frame is the complete
                // inbound state allowance. Do not read the next wire frame
                // until the application has accepted this exact frame.
                if state_sink.send(env).await.is_err() {
                    return;
                }
            }
            // Actions reach another device on its own connection now; a peer
            // that still sends one of their frames speaks a protocol this
            // build does not serve, so the session ends rather than dropping
            // what it cannot route.
            Ok(Some(_)) => {
                tracing::warn!("relay session closed: a peer sent a retired action frame");
                return;
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
    events_since_yield: usize,
}

/// The writer's next outbound envelope: one queued state frame, or `None`
/// once the producer is gone.
async fn next_v2_outbound_event(
    states: &mut mpsc::Receiver<Envelope>,
    schedule: &mut V2WriteSchedule,
) -> Option<Envelope> {
    use std::future::poll_fn;

    cooperative_yield(&mut schedule.events_since_yield).await;
    poll_fn(|cx| poll_queued(states, cx)).await
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
        frames: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl tokio::io::AsyncWrite for RefillingWrite {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.frames.lock().unwrap().push(bytes.to_vec());
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
    async fn blocked_state_write_does_not_delay_inbound_state() {
        use tokio::io::AsyncWriteExt as _;

        let (state_tx, mut states) = mpsc::channel(256);
        let (state_sink, mut state_inbound) = mpsc::channel(256);
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
        let pump = tokio::spawn(async move {
            let mut writer = writer;
            let handshake = Mutex::new(None);
            run_v2_pump(
                &mut writer,
                client_read,
                None,
                &handshake,
                &mut states,
                &state_sink,
                None,
            )
            .await
        });

        started_rx.await.unwrap();
        assert_eq!(written.lock().unwrap().len(), 1);
        let response = Envelope::Subscribe { workspace_id: WorkspaceId::from_u128(7) };
        relay_write
            .write_all(
                &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::State(response.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let observed =
            tokio::time::timeout(tilo_test_env::hang_bound(), state_inbound.recv()).await;

        let _ = release_tx.send(());
        drop(state_tx);
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
            observed.expect("inbound state stalled behind outbound state write"),
            Some(response)
        );
    }

    #[tokio::test]
    async fn continuously_ready_writer_preserves_inbound_progress() {
        use tokio::io::AsyncWriteExt as _;

        let (state_tx, mut states) = mpsc::channel(256);
        let (state_sink, mut state_inbound) = mpsc::channel(256);
        let workspace_id = tilo_relay_protocol::WorkspaceId::from_u128(9);
        for _ in 0..255 {
            state_tx.try_send(Envelope::Ping).unwrap();
        }
        state_tx
            .try_send(Envelope::Subscribe { workspace_id })
            .unwrap();

        let frames = Arc::new(Mutex::new(Vec::new()));
        let writer = RefillingWrite {
            states: state_tx,
            frames: frames.clone(),
        };
        let (mut relay_write, client_read) = tokio::io::duplex(4096);
        let pump = tokio::spawn(async move {
            let mut writer = writer;
            let handshake = Mutex::new(None);
            run_v2_pump(
                &mut writer,
                client_read,
                None,
                &handshake,
                &mut states,
                &state_sink,
                None,
            )
            .await
        });

        let response = Envelope::Subscribe { workspace_id };
        relay_write
            .write_all(
                &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::State(response.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            tilo_test_env::within(
                "continuously ready writer starved the inbound frame",
                state_inbound.recv()
            )
            .await,
            Some(response)
        );
        assert!(
            !frames.lock().unwrap().is_empty(),
            "the writer wrote while the reader made progress"
        );

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
        let (state_sink, _state_inbound) = mpsc::channel(256);
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
        let pump = tokio::spawn(async move {
            let mut writer = writer;
            let handshake = Mutex::new(None);
            run_v2_pump(
                &mut writer,
                client_read,
                None,
                &handshake,
                &mut states,
                &state_sink,
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
        let (state_sink, _state_inbound) = mpsc::channel(256);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let waker = Arc::new(Mutex::new(None));
        let reader = PendingRead {
            started: Some(started_tx),
            polls: polls.clone(),
            waker: waker.clone(),
        };
        let pump = tokio::spawn(async move {
            let mut writer = tokio::io::sink();
            let handshake = Mutex::new(None);
            run_v2_pump(
                &mut writer,
                reader,
                None,
                &handshake,
                &mut states,
                &state_sink,
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
        let frame = |id: u128| Envelope::Ready {
            workspace_id: tilo_relay_protocol::WorkspaceId::from_u128(id),
            pending_deltas: 0,
            records_when_opened: false,
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
        for id in 0..256 {
            incoming.try_send(frame(id)).unwrap();
        }
        let task = tokio::spawn(async move {
            let mut reader = SessionReader::new(reader);
            run_v2_reader(&mut reader, &incoming, None).await;
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
        run_v2_reader(&mut reader, &incoming, None).await;
        drop(incoming);
        assert_eq!(consumer.await.unwrap(), 1024);
    }
}
