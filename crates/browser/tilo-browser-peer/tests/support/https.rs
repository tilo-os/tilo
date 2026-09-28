//! A native [`HttpClient`] for tests: HTTP/1.1 over rustls to one host,
//! trusting exactly the certificate whose SHA-256 the host published — the
//! same pin a page gives WebTransport as `serverCertificateHashes`.

use std::net::SocketAddr;
use std::sync::Arc;

use http_body_util::BodyExt as _;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::Digest as _;
use tilo_browser_peer::{HttpClient, HttpFuture};

#[derive(Debug)]
struct Pinned {
    sha256: Vec<u8>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if sha2::Sha256::digest(end_entity.as_ref()).as_slice() == self.sha256.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "certificate is not the pinned one".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// HTTPS to `addr`, pinned to one certificate.
pub struct DevHttps {
    addr: SocketAddr,
    connector: tokio_rustls::TlsConnector,
}

impl DevHttps {
    pub fn new(addr: SocketAddr, sha256_hex: &str) -> Arc<Self> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pinned {
                sha256: hex::decode(sha256_hex).unwrap(),
                provider,
            }))
            .with_no_client_auth();
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
