//! The public TLS front. A browser reaches `https://<room>.<domain>`; this
//! terminates its TLS with the `*.<domain>` certificate and relays the bytes
//! over a second TLS connection to that room's host, which presents the
//! service's own internal certificate, pinned here.
use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

/// The name room hosts are reached by inside the service.
const INTERNAL_NAME: &str = "rooms.internal";

/// The certificate every room host presents to this service.
pub struct HostIdentity {
    /// DER, pinned by the relay to the host.
    pub certificate: Vec<u8>,
    pub certificate_pem: String,
    pub key_pem: String,
}

impl HostIdentity {
    pub fn generate() -> Result<Self> {
        let generated = rcgen::generate_simple_self_signed(vec![INTERNAL_NAME.into()])?;
        Ok(Self {
            certificate: generated.cert.der().to_vec(),
            certificate_pem: generated.cert.pem(),
            key_pem: generated.signing_key.serialize_pem(),
        })
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The public server configuration: the configured chain, or for local
/// development a self-signed `*.<domain>` certificate.
pub fn public_config(
    certificate: Option<&(std::path::PathBuf, std::path::PathBuf)>,
    domain: &str,
) -> Result<Arc<rustls::ServerConfig>> {
    let (chain, key) = if let Some((chain, key)) = certificate {
        (
            CertificateDer::pem_file_iter(chain)
                .context("read TILO_ROOMS_TLS_CERT")?
                .collect::<Result<Vec<_>, _>>()
                .context("parse TILO_ROOMS_TLS_CERT")?,
            PrivateKeyDer::from_pem_file(key).context("read TILO_ROOMS_TLS_KEY")?,
        )
    } else {
        eprintln!(
            "tilo-room-service: no TILO_ROOMS_TLS_CERT; serving a self-signed *.{domain} certificate"
        );
        let generated =
            rcgen::generate_simple_self_signed(vec![format!("*.{domain}"), domain.into()])?;
        (
            vec![generated.cert.der().clone()],
            PrivateKeyDer::try_from(generated.signing_key.serialize_der())
                .map_err(anyhow::Error::msg)?,
        )
    };
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(chain, key)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

#[derive(Debug)]
struct Pinned {
    certificate: Vec<u8>,
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
        if end_entity.as_ref() == self.certificate.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "room host certificate differs".into(),
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

/// Serves the TLS front until the listener fails. `port_of` maps a room id
/// to its host's loopback port.
pub async fn serve(
    listener: TcpListener,
    public: Arc<rustls::ServerConfig>,
    domain: String,
    host_certificate: Vec<u8>,
    port_of: impl Fn(&str) -> Option<u16> + Send + Sync + 'static,
) -> Result<()> {
    let port_of = Arc::new(port_of);
    let suffix = format!(".{domain}");
    let provider = provider();
    loop {
        let (socket, _) = listener.accept().await?;
        let (public, suffix, port_of) = (Arc::clone(&public), suffix.clone(), Arc::clone(&port_of));
        let pinned = Arc::new(Pinned {
            certificate: host_certificate.clone(),
            provider: Arc::clone(&provider),
        });
        tokio::spawn(async move {
            let _ = relay(socket, public, &suffix, port_of.as_ref(), pinned).await;
        });
    }
}

async fn relay(
    socket: TcpStream,
    public: Arc<rustls::ServerConfig>,
    suffix: &str,
    port_of: &(dyn Fn(&str) -> Option<u16> + Send + Sync),
    pinned: Arc<Pinned>,
) -> Result<()> {
    let start =
        tokio_rustls::LazyConfigAcceptor::new(rustls::server::Acceptor::default(), socket).await?;
    let port = start
        .client_hello()
        .server_name()
        .and_then(|name| name.strip_suffix(suffix))
        .and_then(port_of)
        .context("unknown room")?;
    let mut client = start.into_stream(public).await?;
    let alpn = client.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::clone(&pinned.provider))
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(pinned)
        .with_no_client_auth();
    config.alpn_protocols = alpn.into_iter().collect();
    let upstream = TcpStream::connect(("127.0.0.1", port)).await?;
    let mut host = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from(INTERNAL_NAME)?, upstream)
        .await?;
    tokio::io::copy_bidirectional(&mut client, &mut host).await?;
    Ok(())
}
