//! How a device reaches its host for one thing it asks: the one seam every
//! dialler goes through, whatever the device runs on.
//!
//! A device asks its host for a few things, each a conversation of its own
//! ([`Purpose`]): the session, certification, and a workspace's live plane.
//! [`Dialer::link`] opens one [`Link`] for one of them, over the host's
//! [`Carrier`]:
//!
//! - **iroh** ([`Carrier::Direct`], [`Carrier::Relay`]) — a native device's:
//!   a connection from the device's endpoint on the purpose's ALPN. Only a
//!   native build has it;
//! - **WebTransport** ([`Carrier::WebTransport`]) — a page's, with the
//!   browser's own transport (`tilo_net::webtransport`): a session at the
//!   purpose's path on the host's listener ([`WebLink`]). A native device
//!   can dial it too, which is how the page's way is tested.
//!
//! Over iroh the endpoint's TLS says who the device is; over WebTransport the
//! device proves it with the relay handshake on the link's first stream,
//! where a conversation needs to know (the session's own, and the plane's).
//! Either way the bytes above a link are the same.

use std::pin::Pin;
use std::task::{Context, Poll};

use tilo_relay_protocol::web::{LINK_CERTIFY_PATH, LINK_PLANE_PATH, LINK_SESSION_PATH, WebLink};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::LinkError;
use crate::host::{Carrier, Host};

/// One thing a device asks its host, on a link of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Purpose {
    /// The session: its first stream is the handshake, then the session.
    Session,
    /// Certification: each stream one request or approver command.
    Certify,
    /// A workspace's live plane.
    Plane,
}

impl Purpose {
    /// Where a WebTransport session for it opens on the host's listener.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::Session => LINK_SESSION_PATH,
            Self::Certify => LINK_CERTIFY_PATH,
            Self::Plane => LINK_PLANE_PATH,
        }
    }

    /// The ALPN an iroh connection for it negotiates. A plane's carrier over
    /// iroh is dialled by the live plane itself, which owns its ALPN.
    #[cfg(not(target_arch = "wasm32"))]
    const fn alpn(self) -> Option<&'static [u8]> {
        match self {
            Self::Session => Some(tilo_relay_protocol::transport::ALPN),
            Self::Certify => Some(tilo_relay_protocol::certify::CERTIFY_ALPN),
            Self::Plane => None,
        }
    }
}

/// Opens links to one host, over its carrier.
///
/// Over iroh it keeps the device's endpoint, bound on the first link and
/// reused for every link after (and by the live plane, [`Self::endpoint`]):
/// a device speaks to its host as one endpoint. Over WebTransport each link
/// is a session of its own.
pub struct Dialer {
    host: Host,
    /// The key the iroh endpoint speaks as; a fresh one when `None`.
    #[cfg_attr(
        target_arch = "wasm32",
        expect(dead_code, reason = "a page binds no iroh endpoint")
    )]
    key: Option<[u8; 32]>,
    #[cfg(not(target_arch = "wasm32"))]
    endpoint: tokio::sync::OnceCell<iroh::Endpoint>,
}

impl Dialer {
    /// Links to `host`, speaking over iroh as `key` (a fresh key when `None`).
    #[must_use]
    pub fn new(host: Host, key: Option<[u8; 32]>) -> Self {
        Self {
            host,
            key,
            #[cfg(not(target_arch = "wasm32"))]
            endpoint: tokio::sync::OnceCell::new(),
        }
    }

    /// The host this dialer reaches.
    #[must_use]
    pub const fn host(&self) -> &Host {
        &self.host
    }

    /// Open a link for `purpose`.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the host is not reached, or names no
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
        match self.host.carrier() {
            Carrier::WebTransport => {
                let web = self.host.web().ok_or_else(|| {
                    LinkError::Unavailable("the host takes no WebTransport sessions".into())
                })?;
                web_session(web, purpose, protocols)
                    .await
                    .map(|session| Link::of(Inner::Web(Box::new(session))))
            }
            #[cfg(not(target_arch = "wasm32"))]
            Carrier::Direct | Carrier::Relay => {
                let alpn = purpose.alpn().ok_or_else(|| {
                    LinkError::Unavailable("the live plane dials its own carrier over iroh".into())
                })?;
                let endpoint = self.endpoint().await?;
                let connection = endpoint
                    .connect(self.host.addr(), alpn)
                    .await
                    .map_err(|error| connect_error(&error, alpn))?;
                Ok(Link::of(Inner::Iroh(connection)))
            }
            #[cfg(target_arch = "wasm32")]
            Carrier::Direct | Carrier::Relay => Err(LinkError::Unavailable(
                "a page reaches its host over WebTransport only".into(),
            )),
        }
    }

    /// The iroh endpoint this dialer speaks from, bound on first use.
    ///
    /// # Errors
    ///
    /// As [`Host::bind`].
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn endpoint(&self) -> Result<iroh::Endpoint, LinkError> {
        self.endpoint
            .get_or_try_init(|| self.host.bind(self.key))
            .await
            .cloned()
    }

    /// The endpoint, when one was bound: nothing is bound to ask.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn bound(&self) -> Option<iroh::Endpoint> {
        self.endpoint.get().cloned()
    }

    /// Let go of the host: close the endpoint, waiting for the host to hear
    /// it. Links over WebTransport close on their own.
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            clippy::unused_async,
            reason = "only an iroh endpoint is awaited closed"
        )
    )]
    pub async fn close(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(endpoint) = self.endpoint.get() {
            endpoint.close().await;
        }
    }
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
/// ([`tilo_relay_protocol::entering::ENTRY_BINDING_LABEL`]).
#[cfg(not(target_arch = "wasm32"))]
fn exported(connection: &iroh::endpoint::Connection) -> Option<[u8; 32]> {
    let mut binding = [0u8; 32];
    connection
        .export_keying_material(
            &mut binding,
            tilo_relay_protocol::entering::ENTRY_BINDING_LABEL,
            &[],
        )
        .ok()?;
    Some(binding)
}

/// Why an iroh dial failed. TLS `no_application_protocol` (alert 120) means
/// the host is reached but serves nothing at `alpn`, which says more than
/// the transport's words.
#[cfg(not(target_arch = "wasm32"))]
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
            "host does not speak {}",
            String::from_utf8_lossy(alpn)
        ))
    } else {
        LinkError::Unavailable(error.to_string())
    }
}

/// One conversation's connection to the host: an iroh connection or a
/// WebTransport session. Cheap to clone; every clone is the same link.
#[derive(Clone)]
pub struct Link {
    inner: Inner,
    /// What an entry on it is bound to, once read ([`Link::binding`]): a
    /// host offers it once per link.
    binding: std::sync::Arc<tokio::sync::OnceCell<[u8; 32]>>,
}

#[derive(Clone)]
enum Inner {
    #[cfg(not(target_arch = "wasm32"))]
    Iroh(iroh::endpoint::Connection),
    Web(Box<tilo_net::webtransport::Session>),
}

impl Link {
    fn of(inner: Inner) -> Self {
        Self {
            inner,
            binding: std::sync::Arc::default(),
        }
    }

    /// A link over a WebTransport session already open (one a listener
    /// accepted, or a test's).
    #[must_use]
    pub fn web(session: tilo_net::webtransport::Session) -> Self {
        Self::of(Inner::Web(Box::new(session)))
    }

    /// A link over an iroh connection already open.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn iroh(connection: iroh::endpoint::Connection) -> Self {
        Self::of(Inner::Iroh(connection))
    }

    /// Open a bidirectional stream.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the link is gone.
    pub async fn open_bi(&self) -> Result<(LinkSend, LinkRecv), LinkError> {
        let gone = |error: String| LinkError::Unavailable(error);
        match &self.inner {
            #[cfg(not(target_arch = "wasm32"))]
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

    /// Accept the next bidirectional stream the host opens.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the link is gone.
    pub async fn accept_bi(&self) -> Result<(LinkSend, LinkRecv), LinkError> {
        let gone = |error: String| LinkError::Unavailable(error);
        match &self.inner {
            #[cfg(not(target_arch = "wasm32"))]
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

    /// What an entry's proof on this link is bound to, as the host that
    /// accepted it offers it: unique to the link and known to no one outside
    /// it, so a proof captured from one link proves nothing on another. Over
    /// iroh both ends export it from the connection's TLS session; a browser
    /// exports nothing from a WebTransport session, so there the host writes
    /// fresh random bytes on a stream of its own as the session opens, which
    /// reach the device with the session itself. Call it before accepting a
    /// stream: a device that enters waits for it before it says hello.
    ///
    /// `None` when the link carries no binding (then it takes no entry).
    pub async fn offer_binding(&self) -> Option<[u8; 32]> {
        match &self.inner {
            #[cfg(not(target_arch = "wasm32"))]
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
    /// [`LinkError::Unavailable`] when the host offered none.
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
            #[cfg(not(target_arch = "wasm32"))]
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
                    .map_err(|_| missing("the host's binding is not 32 bytes".into()))
            }
        }
    }

    /// Close the link at once, telling the host `reason`.
    pub fn close(&self, reason: &str) {
        match &self.inner {
            #[cfg(not(target_arch = "wasm32"))]
            Inner::Iroh(connection) => connection.close(0u32.into(), reason.as_bytes()),
            Inner::Web(session) => session.close(0, reason),
        }
    }

    /// Resolves once the link has closed, from either end.
    pub async fn closed(&self) {
        match &self.inner {
            #[cfg(not(target_arch = "wasm32"))]
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

    /// The WebTransport session under the link, when it is one: what the live
    /// plane runs `moq-lite` on.
    #[must_use]
    pub fn web_session(&self) -> Option<tilo_net::webtransport::Session> {
        match &self.inner {
            Inner::Web(session) => Some((**session).clone()),
            #[cfg(not(target_arch = "wasm32"))]
            Inner::Iroh(_) => None,
        }
    }

    /// The iroh connection under the link, when it is one.
    #[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
    Iroh(iroh::endpoint::SendStream),
    Web(tilo_net::webtransport::SendStream),
}

/// A link's receiving half of one stream.
pub enum LinkRecv {
    #[cfg(not(target_arch = "wasm32"))]
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
            #[cfg(not(target_arch = "wasm32"))]
            Self::Iroh(send) => Pin::new(send)
                .poll_write(cx, buf)
                .map_err(std::io::Error::other),
            Self::Web(send) => Pin::new(send).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Iroh(send) => Pin::new(send).poll_flush(cx),
            Self::Web(send) => Pin::new(send).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(not(target_arch = "wasm32"))]
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
            #[cfg(not(target_arch = "wasm32"))]
            Self::Iroh(recv) => Pin::new(recv).poll_read(cx, buf),
            Self::Web(recv) => Pin::new(recv).poll_read(cx, buf),
        }
    }
}

impl LinkSend {
    /// Resolves once the host has read everything sent, or stopped reading.
    pub async fn stopped(&mut self) {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
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

    /// Say the stream is finished: the host reads its end.
    pub fn finish(&mut self) {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Iroh(send) => {
                let _ = send.finish();
            }
            Self::Web(send) => {
                let _ = web_transport_trait::poll::SendStream::finish(send);
            }
        }
    }
}
