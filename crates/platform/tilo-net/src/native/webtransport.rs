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
