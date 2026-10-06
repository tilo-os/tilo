//! Where the browser listener's TLS certificate comes from.
//!
//! Browsers need a certificate they trust for both HTTPS and WebTransport:
//!
//! - **Development**: a generated self-signed ECDSA certificate valid 13
//!   days, kept under the host's state root and reused across restarts. The
//!   page pins it for WebTransport with `serverCertificateHashes` (the
//!   SHA-256 is published in `HostInfo`); for HTTPS the developer trusts it
//!   once (or loads the page from the same origin after accepting it).
//! - **Files**: a PEM chain and key on disk, renewed by whatever manages them
//!   — an ACME client (certbot, lego, Caddy) on a home host, or the Cloud
//!   machine's managed certificate. The listener reloads nothing by itself;
//!   restart it after renewal.
//! - **Managed**: ACME inside the host. Not implemented: configure `Files`
//!   from an ACME client, or terminate HTTPS at the Cloud edge. WebTransport
//!   is HTTP/3 over UDP, so an edge in front of it must pass UDP through (or
//!   terminate HTTP/3 and forward WebTransport), which ordinary HTTP load
//!   balancers do not.

use std::path::PathBuf;

use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// How the listener obtains its certificate.
#[derive(Clone, Debug)]
pub enum TlsSource {
    /// Generate a short-lived self-signed certificate for these host names
    /// and addresses.
    Development {
        /// Subject alternative names (for example `localhost`, `127.0.0.1`).
        hosts: Vec<String>,
        /// Keep the certificate here (owner-only) and reuse it across
        /// restarts while it has more than a day left, so a page that pinned
        /// it keeps working. `None` generates one per start.
        store: Option<PathBuf>,
    },
    /// PEM files managed outside the host.
    Files {
        /// Certificate chain, leaf first.
        certificate_chain: PathBuf,
        /// PKCS#8, PKCS#1 or SEC1 private key.
        private_key: PathBuf,
        /// The name the certificate is for, which browsers dial. Needed for
        /// addresses the host hands out when it listens on every address.
        host: Option<String>,
    },
    /// ACME inside the host (not implemented).
    Managed {
        /// The domain to obtain a certificate for.
        domain: String,
    },
}

/// A loaded TLS identity.
pub(crate) struct Identity {
    pub(crate) chain: Vec<CertificateDer<'static>>,
    pub(crate) key: PrivateKeyDer<'static>,
    /// Set for development certificates, which browsers pin by hash.
    pub(crate) pinned_sha256: Option<String>,
}

impl Identity {
    pub(crate) fn load(source: &TlsSource) -> Result<Self, String> {
        match source {
            TlsSource::Development { hosts, store } => {
                if let Some(stored) = store.as_deref().and_then(load_stored) {
                    return Ok(stored);
                }
                let identity = tilo_moq::carrier::dev_identity(hosts.clone())?;
                let loaded = Self {
                    chain: identity.chain,
                    key: identity.key,
                    pinned_sha256: Some(identity.fingerprint),
                };
                if let Some(store) = store {
                    save_stored(store, &loaded)?;
                }
                Ok(loaded)
            }
            TlsSource::Files {
                certificate_chain,
                private_key,
                ..
            } => {
                let chain = CertificateDer::pem_file_iter(certificate_chain)
                    .map_err(|error| format!("certificate chain: {error}"))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| format!("certificate chain: {error}"))?;
                if chain.is_empty() {
                    return Err("certificate chain is empty".into());
                }
                let key = PrivateKeyDer::from_pem_file(private_key)
                    .map_err(|error| format!("private key: {error}"))?;
                Ok(Self {
                    chain,
                    key,
                    pinned_sha256: None,
                })
            }
            TlsSource::Managed { domain } => Err(format!(
                "managed certificates for {domain} are not implemented: use TlsSource::Files with \
                 an ACME-managed certificate, or terminate TLS at the Cloud edge"
            )),
        }
    }

    /// rustls server configuration for HTTPS over TCP (HTTP/2 and HTTP/1.1).
    pub(crate) fn https_config(&self) -> Result<rustls::ServerConfig, String> {
        let mut config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_single_cert(self.chain.clone(), self.key.clone_key())
        .map_err(|error| error.to_string())?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(config)
    }
}

/// How long a stored development certificate is reused: generated ones are
/// valid 13 days; replace them with a day to spare.
const DEV_REUSE: std::time::Duration = std::time::Duration::from_secs(12 * 24 * 3600);

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredDev {
    created_unix: u64,
    certificate: Vec<u8>,
    pkcs8_key: Vec<u8>,
    sha256: String,
}

fn load_stored(path: &std::path::Path) -> Option<Identity> {
    let stored: StoredDev = postcard::from_bytes(&std::fs::read(path).ok()?).ok()?;
    let age = tilo_clock::unix_now().checked_sub(stored.created_unix)?;
    (age < DEV_REUSE.as_secs()).then(|| Identity {
        chain: vec![CertificateDer::from(stored.certificate)],
        key: PrivateKeyDer::Pkcs8(stored.pkcs8_key.into()),
        pinned_sha256: Some(stored.sha256),
    })
}

fn save_stored(path: &std::path::Path, identity: &Identity) -> Result<(), String> {
    let PrivateKeyDer::Pkcs8(key) = &identity.key else {
        return Err("development key is not PKCS#8".into());
    };
    let stored = StoredDev {
        created_unix: tilo_clock::unix_now(),
        certificate: identity
            .chain
            .first()
            .map(|der| der.to_vec())
            .unwrap_or_default(),
        pkcs8_key: key.secret_pkcs8_der().to_vec(),
        sha256: identity.pinned_sha256.clone().unwrap_or_default(),
    };
    let bytes = postcard::to_allocvec(&stored).map_err(|error| error.to_string())?;
    // A host awaiting its certificate serves before anything else created
    // its state directory.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
        .map_err(|error| error.to_string())
}
