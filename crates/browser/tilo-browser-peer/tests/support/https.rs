//! A native [`HttpClient`] for tests: HTTP/1.1 over rustls to one host,
//! trusting exactly the certificate whose SHA-256 the host published — the
//! same pin a page gives WebTransport as `serverCertificateHashes`.

use std::net::SocketAddr;
use std::sync::Arc;

use http_body_util::BodyExt as _;
use rustls::pki_types::ServerName;
use sha2::Digest as _;
use tilo_browser_peer::{HttpClient, HttpFuture};

/// HTTPS to `addr`, pinned to one certificate.
pub struct DevHttps {
    addr: SocketAddr,
    connector: tokio_rustls::TlsConnector,
}

impl DevHttps {
    pub fn new(addr: SocketAddr, sha256_hex: &str) -> Arc<Self> {
        let sha256 = hex::decode(sha256_hex).unwrap();
        let mut config = tilo_net::pinned_client_config(move |certificate| {
            sha2::Sha256::digest(certificate).as_slice() == sha256.as_slice()
        })
        .unwrap();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(Self {
            addr,
            connector: tokio_rustls::TlsConnector::from(Arc::new(config)),
        })
    }

    async fn request(
        &self,
        path: &str,
        session: Option<&str>,
        body: Vec<u8>,
    ) -> Result<(u16, Vec<u8>), String> {
        let tcp = tokio::net::TcpStream::connect(self.addr)
            .await
            .map_err(|error| error.to_string())?;
        let tls = self
            .connector
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .map_err(|error| error.to_string())?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tls))
                .await
                .map_err(|error| error.to_string())?;
        tokio::spawn(connection);
        let mut request = hyper::Request::post(path)
            .header(hyper::header::HOST, "localhost")
            .header(hyper::header::CONTENT_TYPE, "application/octet-stream");
        if let Some(session) = session {
            request = request.header(tilo_relay_protocol::web::SESSION_HEADER, session);
        }
        let request = request
            .body(http_body_util::Full::new(bytes::Bytes::from(body)))
            .map_err(|error| error.to_string())?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|error| error.to_string())?
            .to_bytes()
            .to_vec();
        Ok((status, body))
    }
}

impl HttpClient for DevHttps {
    fn post(&self, path: &str, session: Option<&str>, body: Vec<u8>) -> HttpFuture<'_> {
        let path = path.to_owned();
        let session = session.map(str::to_owned);
        Box::pin(async move { self.request(&path, session.as_deref(), body).await })
    }
}
