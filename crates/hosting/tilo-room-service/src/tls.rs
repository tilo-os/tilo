//! The public TLS front. A browser reaches `https://<room>.<domain>`; this
//! terminates its TLS with the `*.<domain>` certificate and relays the bytes
//! over a second TLS connection to that room's host, which presents the
//! service's own internal certificate, pinned here.
use anyhow::{Context, Result};
use rustls::pki_types::{PrivateKeyDer, ServerName};
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

/// The public server configuration: the configured chain, or for local
/// development a self-signed `*.<domain>` certificate.
pub fn public_config(
    certificate: Option<&(std::path::PathBuf, std::path::PathBuf)>,
    domain: &str,
) -> Result<Arc<rustls::ServerConfig>> {
    let (chain, key) = if let Some((chain, key)) = certificate {
        tilo_moq::tls::load_pem(chain, key)
            .map_err(anyhow::Error::msg)
            .context("load TILO_ROOMS_TLS_CERT and TILO_ROOMS_TLS_KEY")?
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
    Ok(Arc::new(
        tilo_moq::tls::https_server_config(chain, key).map_err(anyhow::Error::msg)?,
    ))
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
    let host = Arc::new(tilo_net::pinned_client_config(move |certificate| {
        certificate == host_certificate.as_slice()
    })?);
    loop {
        let (socket, _) = listener.accept().await?;
        let (public, suffix, port_of, host) = (
            Arc::clone(&public),
            suffix.clone(),
            Arc::clone(&port_of),
            Arc::clone(&host),
        );
        tokio::spawn(async move {
            let _ = relay(socket, public, &suffix, port_of.as_ref(), &host).await;
        });
    }
}

async fn relay(
    socket: TcpStream,
    public: Arc<rustls::ServerConfig>,
    suffix: &str,
    port_of: &(dyn Fn(&str) -> Option<u16> + Send + Sync),
    host: &rustls::ClientConfig,
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
    let mut config = host.clone();
    config.alpn_protocols = alpn.into_iter().collect();
    let upstream = TcpStream::connect(("127.0.0.1", port)).await?;
    let mut host = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from(INTERNAL_NAME)?, upstream)
        .await?;
    tokio::io::copy_bidirectional(&mut client, &mut host).await?;
    Ok(())
}
