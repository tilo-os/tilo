//! WebTransport on the desktop: web-transport-quinn with the ring provider.

use bytes::Bytes;
use url::Url;

use super::chain;
use crate::NetError;
use crate::webtransport::Options;

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
    let session = client
        .connect(url)
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

/// Every session error means the session is over.
fn session_error(_: web_transport_quinn::SessionError) -> NetError {
    NetError::Closed
}

impl Session {
    pub(crate) async fn open_bi(&self) -> Result<(SendStream, RecvStream), NetError> {
        let (send, recv) = self.0.open_bi().await.map_err(session_error)?;
        Ok((SendStream(send), RecvStream(recv)))
    }

    pub(crate) async fn accept_bi(&self) -> Result<(SendStream, RecvStream), NetError> {
        let (send, recv) = self.0.accept_bi().await.map_err(session_error)?;
        Ok((SendStream(send), RecvStream(recv)))
    }

    pub(crate) async fn open_uni(&self) -> Result<SendStream, NetError> {
        Ok(SendStream(self.0.open_uni().await.map_err(session_error)?))
    }

    pub(crate) async fn accept_uni(&self) -> Result<RecvStream, NetError> {
        Ok(RecvStream(
            self.0.accept_uni().await.map_err(session_error)?,
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
        self.0.send_datagram_wait(data).await.map_err(session_error)
    }

    pub(crate) async fn recv_datagram(&self) -> Result<Bytes, NetError> {
        self.0.read_datagram().await.map_err(session_error)
    }

    pub(crate) fn close(&self, code: u32, reason: &str) {
        self.0.close(code, reason.as_bytes());
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
    pub(crate) async fn finish(&mut self) -> Result<(), NetError> {
        self.0.finish().map_err(|_| NetError::Closed)
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
