//! How a device reaches its server for one thing it asks: the one boundary every
//! dialler goes through, whatever the device runs on.
//!
//! A device asks its server for a few things, each a conversation of its own
//! ([`Purpose`]): the session, certification, and a workspace's live session.
//! [`Dialer::link`] opens one [`Link`] for one of them, over the server's
//! [`Carrier`]:
//!
//! - **iroh** ([`Carrier::Direct`], [`Carrier::Relay`]) — a native device's:
//!   a connection from the device's endpoint on the purpose's ALPN. Only a
//!   native build has it;
//! - **WebTransport** ([`Carrier::WebTransport`]) — a page's, with the
//!   browser's own transport (`tilo_net::webtransport`): a session at the
//!   purpose's path on the server's listener ([`WebLink`]). A native device
//!   can dial it too, which is how the page's way is tested.
//!
//! Over iroh the endpoint's TLS says who the device is; over WebTransport the
//! device proves it with the server handshake on the link's first stream,
//! where a conversation needs to know (the session's own, the live session's,
//! the link a workspace's records travel on). Either way the bytes above a
//! link are the same.

use std::pin::Pin;
use std::task::{Context, Poll};

use tilo_server_protocol::refusal::Refusal;
use tilo_server_protocol::web::{
    LINK_ACTIONS_PATH, LINK_JOIN_PATH, LINK_LIVE_SESSION_PATH, LINK_RECORDS_PATH, WebLink,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::LinkError;
use crate::host::{Carrier, ServerAddress};

/// The most bytes a handshake's frames may declare: a certificate, a nonce and a signature fit
/// many times over.
const MAX_HANDSHAKE_BYTES: usize = 64 * 1024;

/// How long a dial waits for a connection before it is retried.
///
/// A server that is down answers nothing: its stopped addresses drop the
/// packets silently (the UDP socket is not connected, so the port's refusal
/// never reaches it), and a relay carries nothing to an endpoint it does not
/// have. A dial would otherwise wait out QUIC's 30-second idle timeout, which
/// a session spends on every attempt. A working handshake completes within a
/// few round trips, so ten seconds is a stopped server, not a slow one.
const DIAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// One thing a device asks its server, on a link of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Purpose {
    /// A workspace's live session.
    LiveSession,
    /// A workspace's records and governance: the streams of the link two
    /// devices of one workspace sync over (`DIRECT_ALPN`).
    Records,
    /// Actions one device asks another of its workspace to run.
    Actions,
    /// The workspace join exchange a device proves its membership with. Over iroh the
    /// join client dials the host's join service itself, as it dials any host's.
    Join,
}

impl Purpose {
    /// Where a WebTransport session for it opens on the server's listener.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::LiveSession => LINK_LIVE_SESSION_PATH,
            Self::Records => LINK_RECORDS_PATH,
            Self::Actions => LINK_ACTIONS_PATH,
            Self::Join => LINK_JOIN_PATH,
        }
    }

    /// The ALPN an iroh connection for it negotiates. A live session's carrier over
    /// iroh is dialled by the live session itself, which owns its ALPN.
    const fn alpn(self) -> Option<&'static [u8]> {
        match self {
            Self::LiveSession | Self::Join => None,
            Self::Records => Some(tilo_server_protocol::transport::DIRECT_ALPN),
            Self::Actions => Some(tilo_server_protocol::transport::ACTIONS_ALPN),
        }
    }
}

/// Opens links to one server, over its carrier.
///
/// Over iroh it keeps the device's endpoint, bound on the first link and
/// reused for every link after (and by the live session, [`Self::endpoint`]):
/// a device speaks to its server as one endpoint. A device that already runs
/// an endpoint under its key lends it ([`Self::from_endpoint`]), so it never
/// binds a second one: a server holds one connection per endpoint id and
/// drops the older of two, which cut the other endpoint off. Over
/// WebTransport each link is a session of its own.
pub struct Dialer {
    server: ServerAddress,
    /// The key the iroh endpoint speaks as; a fresh one when `None`.
    key: Option<[u8; 32]>,
    endpoint: tokio::sync::OnceCell<iroh::Endpoint>,
    /// Whether the endpoint is the device's own, lent: it outlives this
    /// dialer, which never closes it.
    lent: bool,
}

impl Dialer {
    /// Links to `server`, speaking over iroh as `key` (a fresh key when `None`).
    #[must_use]
    pub fn new(server: ServerAddress, key: Option<[u8; 32]>) -> Self {
        Self {
            server,
            key,
            endpoint: tokio::sync::OnceCell::new(),
            lent: false,
        }
    }

    /// Links to `server` from `endpoint`, the one this device already runs:
    /// its links are connections of that endpoint, and closing the dialer
    /// leaves it running.
    #[must_use]
    pub fn from_endpoint(server: ServerAddress, endpoint: iroh::Endpoint) -> Self {
        Self {
            server,
            key: None,
            endpoint: tokio::sync::OnceCell::new_with(Some(endpoint)),
            lent: true,
        }
    }

    /// The server this dialer reaches.
    #[must_use]
    pub const fn server(&self) -> &ServerAddress {
        &self.server
    }

    /// Open a link for `purpose`.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the server is not reached, or names no
    /// way to reach it over its carrier.
    pub async fn link(&self, purpose: Purpose) -> Result<Link, LinkError> {
        self.link_offering(purpose, &[]).await
    }

    /// [`Self::link`], offering `protocols` as a WebTransport session's
    /// subprotocols (what an iroh connection's ALPN says on its own).
    ///
    /// # Errors
    ///
    /// As [`Self::link`].
    pub async fn link_offering(
        &self,
        purpose: Purpose,
        protocols: &[&str],
    ) -> Result<Link, LinkError> {
        match self.server.carrier() {
            Carrier::WebTransport => {
                let web = self.server.web().ok_or_else(|| {
                    LinkError::Unavailable("the server takes no WebTransport sessions".into())
                })?;
                web_session(web, purpose, protocols)
                    .await
                    .map(|session| Link::of(Inner::Web(Box::new(session))))
            }
            Carrier::Direct | Carrier::Relay => {
                let alpn = purpose.alpn().ok_or_else(|| {
                    LinkError::Unavailable(
                        "the live session and the join exchange dial their own carrier over iroh"
                            .into(),
                    )
                })?;
                let endpoint = self.endpoint().await?;
                let connection = match n0_future::time::timeout(
                    DIAL_DEADLINE,
                    endpoint.connect(self.server.addr(), alpn),
                )
                .await
                {
                    Ok(connection) => connection.map_err(|error| connect_error(&error, alpn))?,
                    Err(_) => {
                        return Err(LinkError::Unavailable(
                            "the server did not answer the dial in time".into(),
                        ));
                    }
                };
                Ok(Link::over(Inner::Iroh(connection), Some(endpoint)))
            }
        }
    }

    /// The iroh endpoint this dialer speaks from, bound on first use.
    ///
    /// # Errors
    ///
    /// As [`ServerAddress::bind`].
    pub async fn endpoint(&self) -> Result<iroh::Endpoint, LinkError> {
        self.endpoint
            .get_or_try_init(|| self.server.bind(self.key))
            .await
            .cloned()
    }

    /// Let go of the server: close the endpoint, waiting for the server to hear
    /// it; a lent endpoint stays open for the device that runs it. Links
    /// over WebTransport close on their own.
    pub async fn close(&self) {
        if let Some(endpoint) = self.endpoint.get().filter(|_| !self.lent) {
            endpoint.close().await;
        }
    }
}

/// Who the host at the other end of [`prove_stream`] must prove to be.
#[derive(Clone, Copy, Debug)]
pub enum ExpectedHost {
    /// A device of the person whose root is `user`, and `device` when it names one: a device of
    /// the person's own, or a host a session was pinned to by its person.
    Person {
        /// The person's root key.
        user: [u8; 32],
        /// The host device, when the caller knows which.
        device: Option<[u8; 32]>,
    },
    /// Exactly the device dialled, of whichever person: the host of a workspace another person
    /// shares, which its signed address names by device.
    Device([u8; 32]),
}

/// The client half of the member handshake over `send`/`recv`, the stream
/// pair a device already opened on a host: hello, the host's proof checked as
/// `expected` names it, and this device's own proof. What a live session and a
/// workspace's link run on the first stream of their link before the stream
/// carries its own protocol.
///
/// # Errors
///
/// The host refused the device, proved another person or device, or answered
/// nothing; or the proof could not be written. A host that refuses a device its
/// person's root retired says so with the entry the root signed
/// ([`Refusal::Revoked`]), which the device takes in to let go of what it holds.
pub async fn prove_stream<R, W>(
    send: &mut W,
    recv: &mut R,
    cert: &tilo_user_identity::DeviceCert,
    signer: &dyn crate::signer::DeviceSigner,
    revocations: &tilo_user_identity::RevocationList,
    expected: ExpectedHost,
) -> Result<tilo_server_protocol::auth::AuthenticatedPeer, Refusal>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tilo_server_protocol::auth::{self, Hello, HelloAck, HelloRefused};
    const MAX_HANDSHAKE_BYTES: usize = 64 * 1024;
    let failed = |error: String| Refusal::Other(error);
    let client_nonce: [u8; 32] = rand::random();
    let hello = Hello::new(cert.clone(), client_nonce);
    let hello = postcard::to_allocvec(&hello).map_err(|error| failed(error.to_string()))?;
    crate::frame::write_frame(send, &hello)
        .await
        .map_err(|error| failed(error.to_string()))?;
    let answer = crate::frame::read_answer(recv, MAX_HANDSHAKE_BYTES)
        .await
        .map_err(|error| failed(error.to_string()))?;
    let ack: HelloAck = match answer {
        Some(crate::frame::Answer::Frame(bytes)) => {
            auth::decode_handshake(&bytes).map_err(|error| failed(error.to_string()))?
        }
        Some(crate::frame::Answer::Refusal(bytes)) => {
            let refused: HelloRefused =
                auth::decode_handshake(&bytes).map_err(|error| failed(error.to_string()))?;
            return Err(refused.revocation.map_or_else(
                || failed(format!("the host refused the device: {}", refused.refusal)),
                |entry| Refusal::Revoked(Box::new(entry)),
            ));
        }
        None => return Err(failed("the host answered nothing".into())),
    };
    let now = tilo_clock::unix_now_ns();
    let (peer, expected_device) = match expected {
        ExpectedHost::Person { user, device } => (
            auth::verify_hello_ack(&ack, &user, revocations, now, &client_nonce),
            device,
        ),
        ExpectedHost::Device(device) => (
            auth::verify_pinned_host_ack(&ack, revocations, now, &client_nonce),
            Some(device),
        ),
    };
    let peer = peer.map_err(|error| failed(error.to_string()))?;
    if expected_device.is_some_and(|expected| peer.cert.device_public_key != expected) {
        return Err(failed(
            "the host proved another device than the one dialled".into(),
        ));
    }
    let ack2 = crate::signer::signed_ack2(signer, &ack.server_nonce)
        .await
        .map_err(failed)?;
    let ack2 = postcard::to_allocvec(&ack2).map_err(|error| failed(error.to_string()))?;
    crate::frame::write_frame(send, &ack2)
        .await
        .map_err(|error| failed(error.to_string()))?;
    Ok(peer)
}

/// A WebTransport session at `purpose`'s path on `web`.
async fn web_session(
    web: &WebLink,
    purpose: Purpose,
    protocols: &[&str],
) -> Result<tilo_net::webtransport::Session, LinkError> {
    let url = format!("{}{}", web.origin.trim_end_matches('/'), purpose.path());
    let options = match web.certificate_sha256 {
        Some(hash) => tilo_net::webtransport::Options::pinned(vec![hash]),
        None => tilo_net::webtransport::Options::default(),
    }
    .with_protocols(
        protocols
            .iter()
            .map(|protocol| (*protocol).to_owned())
            .collect(),
    );
    tilo_net::webtransport::connect(&url, options)
        .await
        .map_err(|error| LinkError::Unavailable(error.to_string()))
}

/// The keying material `connection`'s TLS session exports for an entry
/// ([`tilo_server_protocol::entering::ENTRY_BINDING_LABEL`]).
fn exported(connection: &iroh::endpoint::Connection) -> Option<[u8; 32]> {
    let mut binding = [0u8; 32];
    connection
        .export_keying_material(
            &mut binding,
            tilo_server_protocol::entering::ENTRY_BINDING_LABEL,
            &[],
        )
        .ok()?;
    Some(binding)
}

/// Why an iroh dial failed. TLS `no_application_protocol` (alert 120) means
/// the server is reached but serves nothing at `alpn`, which says more than
/// the transport's words.
fn connect_error(error: &iroh::endpoint::ConnectError, alpn: &[u8]) -> LinkError {
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
        // A live peer sends its TLS alert in a QUIC CONNECTION_CLOSE frame.
        Some(ConnectionError::ConnectionClosed(close)) => {
            close.error_code == TransportErrorCode::crypto(120)
        }
        _ => false,
    };
    if no_application_protocol {
        LinkError::Unavailable(format!(
            "server does not speak {}",
            String::from_utf8_lossy(alpn)
        ))
    } else {
        LinkError::Unavailable(error.to_string())
    }
}

/// One conversation's connection to the server: an iroh connection or a
/// WebTransport session. Cheap to clone; every clone is the same link.
#[derive(Clone)]
pub struct Link {
    inner: Inner,
    /// The endpoint an iroh connection runs on, kept while the link is: iroh
    /// closes a connection once the last handle on the endpoint it runs on
    /// drops, and a link outlives the dialer that opened it.
    _endpoint: Option<iroh::Endpoint>,
    /// What an entry on it is bound to, once read ([`Link::binding`]): a
    /// server offers it once per link.
    binding: std::sync::Arc<tokio::sync::OnceCell<[u8; 32]>>,
    /// The device at the other end, when the link names it: a WebTransport
    /// session names none until the handshake says which device proved itself
    /// on it ([`Link::web_as`]).
    device: Option<[u8; 32]>,
}

#[derive(Clone)]
enum Inner {
    Iroh(iroh::endpoint::Connection),
    Web(Box<tilo_net::webtransport::Session>),
}

impl Link {
    fn of(inner: Inner) -> Self {
        Self::over(inner, None)
    }

    /// [`Self::of`], keeping `endpoint`, the endpoint an iroh connection
    /// runs on, alive for as long as any clone of the link is.
    fn over(inner: Inner, endpoint: Option<iroh::Endpoint>) -> Self {
        Self {
            inner,
            _endpoint: endpoint,
            binding: std::sync::Arc::default(),
            device: None,
        }
    }

    /// A link over a WebTransport session already open (one a listener
    /// accepted, or a test's).
    #[must_use]
    pub fn web(session: tilo_net::webtransport::Session) -> Self {
        Self::of(Inner::Web(Box::new(session)))
    }

    /// [`Self::web`], for a link whose other end is `device`: the device a
    /// session's handshake proved, or the one a page dialled. A WebTransport
    /// session itself names no device.
    #[must_use]
    pub fn web_as(session: tilo_net::webtransport::Session, device: [u8; 32]) -> Self {
        Self {
            device: Some(device),
            ..Self::of(Inner::Web(Box::new(session)))
        }
    }

    /// A link over an iroh connection already open.
    #[must_use]
    pub fn iroh(connection: iroh::endpoint::Connection) -> Self {
        Self::of(Inner::Iroh(connection))
    }

    /// The device at the other end, when the link names it: the endpoint a
    /// connection proved, or the device a session proved itself as.
    #[must_use]
    pub fn remote(&self) -> Option<[u8; 32]> {
        match &self.inner {
            Inner::Iroh(connection) => Some(*connection.remote_id().as_bytes()),
            Inner::Web(_) => self.device,
        }
    }

    /// Open a unidirectional stream, for what the other end only reads.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the link is gone.
    pub async fn open_uni(&self) -> Result<LinkSend, LinkError> {
        let gone = |error: String| LinkError::Unavailable(error);
        match &self.inner {
            Inner::Iroh(connection) => connection
                .open_uni()
                .await
                .map(LinkSend::Iroh)
                .map_err(|error| gone(error.to_string())),
            Inner::Web(session) => session
                .open_uni()
                .await
                .map(LinkSend::Web)
                .map_err(|error| gone(error.to_string())),
        }
    }

    /// Accept the next unidirectional stream the other end opens.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the link is gone.
    pub async fn accept_uni(&self) -> Result<LinkRecv, LinkError> {
        let gone = |error: String| LinkError::Unavailable(error);
        match &self.inner {
            Inner::Iroh(connection) => connection
                .accept_uni()
                .await
                .map(LinkRecv::Iroh)
                .map_err(|error| gone(error.to_string())),
            Inner::Web(session) => session
                .accept_uni()
                .await
                .map(LinkRecv::Web)
                .map_err(|error| gone(error.to_string())),
        }
    }

    /// Open a bidirectional stream.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the link is gone.
    pub async fn open_bi(&self) -> Result<(LinkSend, LinkRecv), LinkError> {
        let gone = |error: String| LinkError::Unavailable(error);
        match &self.inner {
            Inner::Iroh(connection) => connection
                .open_bi()
                .await
                .map(|(send, recv)| (LinkSend::Iroh(send), LinkRecv::Iroh(recv)))
                .map_err(|error| gone(error.to_string())),
            Inner::Web(session) => session
                .open_bi()
                .await
                .map(|(send, recv)| (LinkSend::Web(send), LinkRecv::Web(recv)))
                .map_err(|error| gone(error.to_string())),
        }
    }

    /// Accept the next bidirectional stream the server opens.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the link is gone.
    pub async fn accept_bi(&self) -> Result<(LinkSend, LinkRecv), LinkError> {
        let gone = |error: String| LinkError::Unavailable(error);
        match &self.inner {
            Inner::Iroh(connection) => connection
                .accept_bi()
                .await
                .map(|(send, recv)| (LinkSend::Iroh(send), LinkRecv::Iroh(recv)))
                .map_err(|error| gone(error.to_string())),
            Inner::Web(session) => session
                .accept_bi()
                .await
                .map(|(send, recv)| (LinkSend::Web(send), LinkRecv::Web(recv)))
                .map_err(|error| gone(error.to_string())),
        }
    }

    /// What an entry's proof on this link is bound to, as the server that
    /// accepted it offers it: unique to the link and known to no one outside
    /// it, so a proof captured from one link proves nothing on another. Over
    /// iroh both ends export it from the connection's TLS session; a browser
    /// exports nothing from a WebTransport session, so there the server writes
    /// fresh random bytes on a stream of its own as the session opens, which
    /// reach the device with the session itself. Call it before accepting a
    /// stream: a device that enters waits for it before it says hello.
    ///
    /// `None` when the link carries no binding (then it takes no entry).
    pub async fn offer_binding(&self) -> Option<[u8; 32]> {
        match &self.inner {
            Inner::Iroh(connection) => exported(connection),
            Inner::Web(session) => {
                let binding: [u8; 32] = rand::random();
                let mut send = session.open_uni().await.ok()?;
                send.write_all(&binding).await.ok()?;
                send.finish().await.ok()?;
                Some(binding)
            }
        }
    }

    /// What an entry's proof on this link is bound to, as the device that
    /// dialed it reads it ([`Self::offer_binding`]).
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the server offered none.
    pub async fn binding(&self) -> Result<[u8; 32], LinkError> {
        self.binding
            .get_or_try_init(|| self.read_binding())
            .await
            .copied()
    }

    async fn read_binding(&self) -> Result<[u8; 32], LinkError> {
        let missing =
            |error: String| LinkError::Unavailable(format!("no binding to enter with: {error}"));
        match &self.inner {
            Inner::Iroh(connection) => {
                exported(connection).ok_or_else(|| missing("the connection exports none".into()))
            }
            Inner::Web(session) => {
                let recv = session
                    .accept_uni()
                    .await
                    .map_err(|error| missing(error.to_string()))?;
                let bytes = recv
                    .read_to_end(32)
                    .await
                    .map_err(|error| missing(error.to_string()))?;
                bytes
                    .try_into()
                    .map_err(|_| missing("the server's binding is not 32 bytes".into()))
            }
        }
    }

    /// Prove this device to the host on the link's first stream, as the member proof a page's
    /// WebTransport session carries: the session handshake, for a device whose transport does
    /// not name it — an endpoint keyed by something other than the device key, as a page's is.
    /// The host answers as `server`, the certificate it was pinned by, and the device proved here
    /// is the one the host admits on the link from then on.
    ///
    /// # Errors
    ///
    /// The host refused the device ([`LinkError::Refused`]), answered as a server other than the
    /// one pinned ([`LinkError::ServerChanged`]), or the link ended before it answered
    /// ([`LinkError::Unavailable`]).
    pub async fn prove_member(
        &self,
        cert: &tilo_user_identity::DeviceCert,
        signer: &dyn crate::signer::DeviceSigner,
        server: &tilo_user_identity::DeviceCert,
    ) -> Result<(), LinkError> {
        use tilo_server_protocol::auth::{self, Hello, HelloAck, HelloRefused};

        let (mut send, mut recv) = self.open_bi().await?;
        let client_nonce: [u8; 32] = rand::random();
        let hello = Hello::new(cert.clone(), client_nonce);
        let hello = postcard::to_allocvec(&hello)
            .map_err(|error| LinkError::Malformed(error.to_string()))?;
        crate::frame::write_frame(&mut send, &hello)
            .await
            .map_err(|error| LinkError::Unavailable(error.to_string()))?;
        let answer = crate::frame::read_answer(&mut recv, MAX_HANDSHAKE_BYTES)
            .await
            .map_err(|error| LinkError::Unavailable(error.to_string()))?;
        let now = tilo_clock::unix_now_ns();
        let revocations = tilo_user_identity::RevocationList::new();
        let ack: HelloAck = match answer {
            Some(crate::frame::Answer::Frame(bytes)) => auth::decode_handshake(&bytes)
                .map_err(|error| LinkError::Malformed(error.to_string()))?,
            Some(crate::frame::Answer::Refusal(bytes)) => {
                let refused: HelloRefused = auth::decode_handshake(&bytes)
                    .map_err(|error| LinkError::Malformed(error.to_string()))?;
                // A refusal is believed where its server's ack would be: it is the pinned host's,
                // signing this hello.
                return Err(
                    match auth::verify_pinned_host_ack(&refused, &revocations, now, &client_nonce) {
                        Ok(peer) if peer.device_public_key == server.device_public_key => {
                            LinkError::Refused(refused.refusal.code().to_owned())
                        }
                        Ok(_) => LinkError::ServerChanged,
                        Err(_) => {
                            LinkError::Malformed("a refusal that proves no pinned host".into())
                        }
                    },
                );
            }
            None => return Err(LinkError::Unavailable("the host answered nothing".into())),
        };
        // The same proof a session's handshake takes of its server: the pinned host's device, its
        // certificate and its signature over this hello.
        let peer = auth::verify_pinned_host_ack(&ack, &revocations, now, &client_nonce)
            .map_err(|error| LinkError::Malformed(error.to_string()))?;
        if peer.device_public_key != server.device_public_key {
            return Err(LinkError::ServerChanged);
        }
        let ack2 = crate::signer::signed_ack2(signer, &ack.server_nonce)
            .await
            .map_err(LinkError::Sign)?;
        let ack2 = postcard::to_allocvec(&ack2)
            .map_err(|error| LinkError::Malformed(error.to_string()))?;
        crate::frame::write_frame(&mut send, &ack2)
            .await
            .map_err(|error| LinkError::Unavailable(error.to_string()))?;
        send.finish();
        Ok(())
    }

    /// Close the link at once, telling the server `reason`.
    pub fn close(&self, reason: &str) {
        self.close_with(0, reason);
    }

    /// Close the link at once with `code` and `reason`: a link that ends as a
    /// refusal closes with the code its protocol names, its reason the typed
    /// refusal the other end reads.
    pub fn close_with(&self, code: u32, reason: &str) {
        match &self.inner {
            Inner::Iroh(connection) => connection.close(code.into(), reason.as_bytes()),
            Inner::Web(session) => session.close(code, reason),
        }
    }

    /// Resolves once the link has closed, from either end.
    pub async fn closed(&self) {
        match &self.inner {
            Inner::Iroh(connection) => {
                connection.closed().await;
            }
            Inner::Web(session) => {
                let mut session = (**session).clone();
                std::future::poll_fn(|cx| {
                    web_transport_trait::poll::Session::poll_closed(&mut session, cx)
                })
                .await;
            }
        }
    }

    /// The subprotocol the link's WebTransport session negotiated: `None`
    /// where the handshake negotiated none, and over iroh, whose ALPN says
    /// what a connection speaks.
    #[must_use]
    pub fn protocol(&self) -> Option<&str> {
        match &self.inner {
            Inner::Iroh(_) => None,
            Inner::Web(session) => session.protocol(),
        }
    }

    /// The WebTransport session under the link, when it is one: what the live
    /// live session runs `moq-lite` on.
    #[must_use]
    pub fn web_session(&self) -> Option<tilo_net::webtransport::Session> {
        match &self.inner {
            Inner::Web(session) => Some((**session).clone()),
            Inner::Iroh(_) => None,
        }
    }

    /// The iroh connection under the link, when it is one.
    #[must_use]
    pub fn connection(&self) -> Option<&iroh::endpoint::Connection> {
        match &self.inner {
            Inner::Iroh(connection) => Some(connection),
            Inner::Web(_) => None,
        }
    }
}

/// A link's sending half of one stream.
pub enum LinkSend {
    Iroh(iroh::endpoint::SendStream),
    Web(tilo_net::webtransport::SendStream),
}

/// A link's receiving half of one stream.
pub enum LinkRecv {
    Iroh(iroh::endpoint::RecvStream),
    Web(tilo_net::webtransport::RecvStream),
}

impl AsyncWrite for LinkSend {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Iroh(send) => Pin::new(send)
                .poll_write(cx, buf)
                .map_err(std::io::Error::other),
            Self::Web(send) => Pin::new(send).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Iroh(send) => Pin::new(send).poll_flush(cx),
            Self::Web(send) => Pin::new(send).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Iroh(send) => Pin::new(send).poll_shutdown(cx),
            Self::Web(send) => Pin::new(send).poll_shutdown(cx),
        }
    }
}

impl AsyncRead for LinkRecv {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Iroh(recv) => Pin::new(recv).poll_read(cx, buf),
            Self::Web(recv) => Pin::new(recv).poll_read(cx, buf),
        }
    }
}

impl LinkSend {
    /// Resolves once the server has read everything sent, or stopped reading.
    pub async fn stopped(&mut self) {
        match self {
            Self::Iroh(send) => {
                let _ = send.stopped().await;
            }
            Self::Web(send) => {
                let _ = std::future::poll_fn(|cx| {
                    web_transport_trait::poll::SendStream::poll_closed(send, cx)
                })
                .await;
            }
        }
    }

    /// Say the stream is finished: the server reads its end.
    pub fn finish(&mut self) {
        match self {
            Self::Iroh(send) => {
                let _ = send.finish();
            }
            Self::Web(send) => {
                let _ = web_transport_trait::poll::SendStream::finish(send);
            }
        }
    }
}
