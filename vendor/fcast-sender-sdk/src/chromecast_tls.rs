//! Cast receivers use self-signed transport certificates. Accepting that leaf
//! does not authenticate the receiver: Cast `DeviceAuth` must separately bind a
//! fresh challenge and this exact leaf to a dedicated Cast certificate chain.
//! Until that authority policy is configured, do not claim receiver identity.
//! Even a self-signed transport must prove possession of its private key.

use std::{io, sync::Arc, time::Duration};

use super::{pki, tls_runtime};
use pki::{CertificateDer, ServerName, UnixTime};
use tls_runtime::{TlsConnector, client::TlsStream, rustls};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug)]
struct CastTlsVerifier;

impl rustls::client::danger::ServerCertVerifier for CastTlsVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // This preserves the upstream Cast transport policy. It deliberately
        // grants neither a WebPKI identity nor a Tilo collaborator identity.
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub(super) fn connector() -> io::Result<TlsConnector> {
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(io::Error::other)?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(CastTlsVerifier))
    .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

pub(super) async fn connect<S: AsyncRead + AsyncWrite + Unpin>(
    connector: &TlsConnector,
    name: ServerName<'static>,
    stream: S,
    cancelled: impl std::future::Future<Output = ()>,
    deadline: Duration,
) -> io::Result<Option<TlsStream<S>>> {
    let handshake = tokio::time::timeout(deadline, connector.connect(name, stream));
    tokio::pin!(handshake, cancelled);
    tokio::select! {
            biased;
            () = &mut cancelled => Ok(None),
            result = &mut handshake => {
                result
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Cast TLS handshake timed out"))?
                    .map(Some)
            }
    }
}

#[cfg(test)]
#[path = "chromecast_tls/tests.rs"]
mod tests;
