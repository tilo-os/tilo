//! WebTransport on the desktop: web-transport-quinn with the ring provider.

use bytes::Bytes;

use url::Url;

use super::chain;
use crate::NetError;
use crate::webtransport::Options;

#[derive(Clone)]
pub(crate) struct Session(web_transport_quinn::Session);
pub(crate) struct SendStream(web_transport_quinn::SendStream);
pub(crate) struct RecvStream(web_transport_quinn::RecvStream);

pub(crate) async fn connect(url: Url, options: Options) -> Result<Session, NetError> {
    let client = if options.cert_hashes.is_empty() {
        system_trust_client()?
    } else {
        web_transport_quinn::ClientBuilder::new()
            .with_server_certificate_hashes(
                options
                    .cert_hashes
                    .iter()
                    .map(|hash| hash.to_vec())
                    .collect(),
            )
            .map_err(|error| NetError::connect(chain(&error)))?
    };
    let request = web_transport_quinn::proto::ConnectRequest::new(url)
        .with_protocols(options.protocols.iter().cloned());
    let session = client
        .connect(request)
        .await
        .map_err(|error| NetError::connect(chain(&error)))?;
    Ok(Session(session))
}

/// A client that verifies servers as every other Tilo client does
/// ([`client_config`](super::tls::client_config)); web-transport-quinn's own
/// system roots read only the Linux bundle.
fn system_trust_client() -> Result<web_transport_quinn::Client, NetError> {
    use web_transport_quinn::quinn;
    let mut tls =
        super::tls::client_config().map_err(|error| NetError::Protocol(format!("TLS: {error}")))?;
    tls.alpn_protocols = vec![web_transport_quinn::ALPN.as_bytes().to_vec()];
    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|error| NetError::Protocol(format!("QUIC: {error}")))?;
    let endpoint = quinn::Endpoint::client(std::net::SocketAddr::from((
        std::net::Ipv6Addr::UNSPECIFIED,
        0,
    )))
    .map_err(|error| NetError::connect(format!("QUIC endpoint: {error}")))?;
    Ok(web_transport_quinn::Client::new(
        endpoint,
        quinn::ClientConfig::new(std::sync::Arc::new(quic)),
    ))
}

fn session_error(error: &web_transport_quinn::SessionError) -> NetError {
    match web_transport_trait::Error::session_error(error) {
        Some((code, reason)) => NetError::ClosedWith { code, reason },
        None => NetError::Closed,
    }
}

impl Session {
    pub(crate) async fn open_bi(&self) -> Result<(SendStream, RecvStream), NetError> {
        let (send, recv) = self
            .0
            .open_bi()
            .await
            .map_err(|error| session_error(&error))?;
        Ok((SendStream(send), RecvStream(recv)))
    }

    pub(crate) async fn accept_bi(&self) -> Result<(SendStream, RecvStream), NetError> {
        let (send, recv) = self
            .0
            .accept_bi()
            .await
            .map_err(|error| session_error(&error))?;
        Ok((SendStream(send), RecvStream(recv)))
    }

    pub(crate) async fn open_uni(&self) -> Result<SendStream, NetError> {
        Ok(SendStream(
            self.0
                .open_uni()
                .await
                .map_err(|error| session_error(&error))?,
        ))
    }

    pub(crate) async fn accept_uni(&self) -> Result<RecvStream, NetError> {
        Ok(RecvStream(
            self.0
                .accept_uni()
                .await
                .map_err(|error| session_error(&error))?,
        ))
    }

    pub(crate) async fn send_datagram(&self, data: Bytes) -> Result<(), NetError> {
        if data.len() > self.0.max_datagram_size() {
            return Err(NetError::InvalidRequest(format!(
                "a datagram of {} bytes exceeds the path's {}",
                data.len(),
                self.0.max_datagram_size()
            )));
        }
        self.0
            .send_datagram_wait(data)
            .await
            .map_err(|error| session_error(&error))
    }

    pub(crate) async fn recv_datagram(&self) -> Result<Bytes, NetError> {
        self.0
            .read_datagram()
            .await
            .map_err(|error| session_error(&error))
    }

    pub(crate) fn close(&self, code: u32, reason: &str) {
        self.0.close(code, reason.as_bytes());
    }

    pub(crate) fn protocol(&self) -> Option<&str> {
        self.0.protocol()
    }

    pub(crate) fn max_datagram_size(&self) -> usize {
        self.0.max_datagram_size()
    }
}

impl web_transport_trait::poll::Session for Session {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = NetError;

    fn poll_accept_uni(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<RecvStream, NetError>> {
        self.0.poll_accept_uni(cx).map(|stream| {
            stream
                .map(RecvStream)
                .map_err(|error| session_error(&error))
        })
    }

    fn poll_accept_bi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(SendStream, RecvStream), NetError>> {
        self.0.poll_accept_bi(cx).map(|pair| {
            pair.map(|(send, recv)| (SendStream(send), RecvStream(recv)))
                .map_err(|error| session_error(&error))
        })
    }

    fn poll_open_uni(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<SendStream, NetError>> {
        self.0.poll_open_uni(cx).map(|stream| {
            stream
                .map(SendStream)
                .map_err(|error| session_error(&error))
        })
    }

    fn poll_open_bi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(SendStream, RecvStream), NetError>> {
        self.0.poll_open_bi(cx).map(|pair| {
            pair.map(|(send, recv)| (SendStream(send), RecvStream(recv)))
                .map_err(|error| session_error(&error))
        })
    }

    fn poll_send_datagram(
        &mut self,
        cx: &mut std::task::Context<'_>,
        payload: &[u8],
    ) -> std::task::Poll<Result<(), NetError>> {
        self.0
            .poll_send_datagram(cx, payload)
            .map_err(|error| session_error(&error))
    }

    fn poll_recv_datagram(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Bytes, NetError>> {
        self.0
            .poll_recv_datagram(cx)
            .map_err(|error| session_error(&error))
    }

    fn max_datagram_size(&self) -> usize {
        self.0.max_datagram_size()
    }

    fn protocol(&self) -> Option<&str> {
        self.0.protocol()
    }

    fn close(&mut self, code: u32, reason: &str) {
        self.0.close(code, reason.as_bytes());
    }

    fn poll_closed(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<NetError> {
        self.0.poll_closed(cx).map(|error| session_error(&error))
    }

    fn stats(&self) -> impl web_transport_trait::Stats {
        web_transport_trait::Session::stats(&self.0)
    }
}

impl web_transport_trait::poll::SendStream for SendStream {
    type Error = NetError;

    fn poll_write(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, NetError>> {
        self.0.poll_write(cx, buf).map_err(|_| NetError::Closed)
    }

    fn set_priority(&mut self, order: u8) {
        let _ = self.0.set_priority(i32::from(order));
    }

    fn finish(&mut self) -> Result<(), NetError> {
        self.0.finish().map_err(|_| NetError::Closed)
    }

    fn reset(&mut self, code: u32) {
        let _ = self.0.reset(code);
    }

    fn poll_closed(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), NetError>> {
        self.0.poll_closed(cx).map_err(|_| NetError::Closed)
    }
}

impl SendStream {
    pub(crate) async fn write_all(&mut self, data: &[u8]) -> Result<(), NetError> {
        self.0.write_all(data).await.map_err(|_| NetError::Closed)
    }

    #[allow(
        clippy::unused_async,
        reason = "the same signature as the page backend"
    )]
    pub(crate) async fn finish_stream(&mut self) -> Result<(), NetError> {
        self.0.finish().map_err(|_| NetError::Closed)
    }
}

impl web_transport_trait::poll::RecvStream for RecvStream {
    type Error = NetError;

    fn poll_read(
        &mut self,
        cx: &mut std::task::Context<'_>,
        dst: &mut [u8],
    ) -> std::task::Poll<Result<Option<usize>, NetError>> {
        self.0.poll_read(cx, dst).map_err(|_| NetError::Closed)
    }

    fn stop(&mut self, code: u32) {
        let _ = self.0.stop(code);
    }

    fn poll_closed(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), NetError>> {
        self.0.poll_closed(cx).map_err(|_| NetError::Closed)
    }
}

impl RecvStream {
    pub(crate) async fn read(&mut self) -> Result<Option<Bytes>, NetError> {
        let mut buffer = vec![0; 64 * 1024];
        match self.0.read(&mut buffer).await {
            Ok(Some(read)) => {
                buffer.truncate(read);
                Ok(Some(Bytes::from(buffer)))
            }
            Ok(None) => Ok(None),
            Err(_) => Err(NetError::Closed),
        }
    }
}

/// How a [`Listener`] answers a session request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Not served here: 404.
    Refuse,
    /// Served, with no subprotocol.
    Accept,
    /// Served, answering this subprotocol.
    AcceptAs(String),
}

/// A WebTransport server on one UDP socket: HTTP/3 beside a host's HTTPS
/// listener, on the same port and with the same certificate, so a page
/// reaches it at the origin it already trusts.
pub struct Listener {
    endpoint: web_transport_quinn::quinn::Endpoint,
}

/// Handshakes one listener runs at once; a peer past that is refused before
/// its TLS handshake.
const MAX_PENDING_HANDSHAKES: usize = 64;
/// How long a peer has to finish QUIC, TLS and its CONNECT request.
const HANDSHAKE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
/// A session ends after this long without a packet from the peer.
const SERVER_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Keeps an idle session open through NAT bindings and the idle timeout.
const SERVER_KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(10);
/// Streams of each direction one session holds open at once: a session's
/// own, a call's pictures, and room to start over.
const SERVER_MAX_STREAMS: u32 = 256;
/// How much a session sends before its first acknowledgement: the window
/// QUIC's slow start begins from.
///
/// A page entering a workspace waits first for its replay, the workspace's
/// governance and its shared desktop: most of a megabyte for a few hundred
/// members, which the host writes at once. From QUIC's default window
/// (14,720 bytes, RFC 9002) slow start doubles it every round trip, so that
/// took six round trips the visitor watched; from 256 KiB it takes two. The
/// host's iroh endpoint starts from the same window, and the pacer spreads
/// it over the round trip rather than sending it as one burst.
const SERVER_INITIAL_WINDOW: u64 = 256 * 1024;

impl Listener {
    /// Listen on `socket`, presenting the certificate `certificate` resolves.
    ///
    /// # Errors
    ///
    /// [`NetError::Protocol`] when TLS or QUIC rejects the configuration,
    /// [`NetError::Connect`] when the socket cannot be used.
    pub fn bind(
        socket: std::net::UdpSocket,
        certificate: std::sync::Arc<dyn rustls::server::ResolvesServerCert>,
    ) -> Result<Self, NetError> {
        use web_transport_quinn::quinn;
        let mut tls = rustls::ServerConfig::builder_with_provider(super::tls::crypto_provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|error| NetError::Protocol(format!("TLS: {error}")))?
            .with_no_client_auth()
            .with_cert_resolver(certificate);
        tls.alpn_protocols = vec![web_transport_quinn::ALPN.as_bytes().to_vec()];
        tls.max_early_data_size = 0;
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
            .map_err(|error| NetError::Protocol(format!("QUIC: {error}")))?;
        let mut config = quinn::ServerConfig::with_crypto(std::sync::Arc::new(crypto));
        let mut transport = quinn::TransportConfig::default();
        transport
            .max_concurrent_bidi_streams(SERVER_MAX_STREAMS.into())
            .max_concurrent_uni_streams(SERVER_MAX_STREAMS.into())
            .max_idle_timeout(Some(
                quinn::IdleTimeout::try_from(SERVER_IDLE_TIMEOUT)
                    .map_err(|error| NetError::Protocol(error.to_string()))?,
            ))
            .keep_alive_interval(Some(SERVER_KEEP_ALIVE));
        let mut cubic = quinn::congestion::CubicConfig::default();
        cubic.initial_window(SERVER_INITIAL_WINDOW);
        transport.congestion_controller_factory(std::sync::Arc::new(cubic));
        config.transport_config(std::sync::Arc::new(transport));
        let runtime = quinn::default_runtime()
            .ok_or_else(|| NetError::Protocol("no async runtime for the QUIC listener".into()))?;
        let endpoint = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(config),
            socket,
            runtime,
        )
        .map_err(|error| NetError::connect(chain(&error)))?;
        Ok(Self { endpoint })
    }

    /// The address the listener is bound to.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`] when the socket has no local address.
    pub fn local_addr(&self) -> Result<std::net::SocketAddr, NetError> {
        self.endpoint
            .local_addr()
            .map_err(|error| NetError::connect(chain(&error)))
    }

    /// Accept sessions until the listener closes, handing each to `serve`
    /// with the path its CONNECT request named. `serves` reads the path and
    /// the subprotocols the peer offered and gives the [`Answer`]. At most [`MAX_PENDING_HANDSHAKES`]
    /// handshakes run at once, each within [`HANDSHAKE_BUDGET`]; dropping the
    /// future aborts every session it serves.
    pub async fn serve<S, F, Fut>(self, serves: S, serve: F)
    where
        S: Fn(&str, &[String]) -> Answer + Clone + Send + 'static,
        F: Fn(String, crate::webtransport::Session) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let pending = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_HANDSHAKES));
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                incoming = self.endpoint.accept() => {
                    let Some(incoming) = incoming else { break };
                    let Ok(permit) = pending.clone().try_acquire_owned() else {
                        incoming.refuse();
                        continue;
                    };
                    let (serves, serve) = (serves.clone(), serve.clone());
                    tasks.spawn(async move {
                        let request = tokio::time::timeout(HANDSHAKE_BUDGET, async {
                            // The host's HTTP/3 SETTINGS go out with its
                            // handshake (0.5-RTT data) rather than a round trip
                            // later, once the peer's Finished arrives: a browser
                            // asks for its session only once it has read them.
                            // They say what the host takes, nothing secret, and
                            // the peer's request is still read only after the
                            // handshake completes.
                            let connection = match incoming.accept().ok()?.into_0rtt() {
                                Ok((connection, _)) => connection,
                                Err(connecting) => connecting.await.ok()?,
                            };
                            web_transport_quinn::Request::accept(connection).await.ok()
                        })
                        .await
                        .ok()
                        .flatten();
                        let Some(request) = request else { return };
                        let path = request.url.path().to_owned();
                        let answer = if request.url.query().is_none() {
                            serves(&path, &request.protocols)
                        } else {
                            Answer::Refuse
                        };
                        let response = match answer {
                            Answer::Accept => web_transport_quinn::proto::ConnectResponse::OK,
                            Answer::AcceptAs(protocol) => {
                                web_transport_quinn::proto::ConnectResponse::OK.with_protocol(protocol)
                            }
                            Answer::Refuse => {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_millis(100),
                                request.reject(web_transport_quinn::proto::http::StatusCode::NOT_FOUND),
                            )
                            .await;
                            return;
                            }
                        };
                        let session =
                            tokio::time::timeout(HANDSHAKE_BUDGET, request.respond(response))
                        .await;
                        drop(permit);
                        if let Ok(Ok(session)) = session {
                            serve(path, crate::webtransport::Session::from_backend(Session(session))).await;
                        }
                    });
                }
                _ = tasks.join_next(), if !tasks.is_empty() => {}
            }
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        self.endpoint.close(0u32.into(), b"listener closed");
    }
}
