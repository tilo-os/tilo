//! Where the browser listener's TLS certificate comes from, and how it is
//! served.
//!
//! - **Development**: a generated self-signed ECDSA certificate valid 13
//!   days, kept under the host's state root and reused across restarts while
//!   it still names every host that needs it. A browser trusts it once (or
//!   loads the page from the same origin after accepting it).
//! - **Files**: a PEM chain and key on disk, renewed by whatever manages them
//!   — an ACME client (certbot, lego, Caddy) on a home host, or the Cloud
//!   machine's managed certificate. The listener reads the pair again when
//!   either file changes (looked at every 30 s), so a renewal needs no
//!   restart.
//! - **Managed**: ACME inside the host. Not implemented: configure `Files`
//!   from an ACME client, or terminate HTTPS at the Cloud edge.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use tilo_lock::MutexRecover as _;

/// How the listener obtains its certificate.
#[derive(Clone, Debug)]
pub enum TlsSource {
    /// Generate a short-lived self-signed certificate for these host names
    /// and addresses.
    Development {
        /// Subject alternative names (for example `localhost`, `127.0.0.1`).
        hosts: Vec<String>,
        /// Keep the certificate here (owner-only) and reuse it across
        /// restarts while it has more than a day left, so a page that saved
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
    /// SHA-256 (lowercase hex) of the certificate.
    pub(crate) fingerprint: Option<String>,
    /// The names the certificate is for, when this host generated it; `None`
    /// for a certificate from files, whose names this host does not read.
    pub(crate) hosts: Option<Vec<String>>,
    /// The PEM pair to follow for renewals, for certificates from files.
    pub(crate) files: Option<(PathBuf, PathBuf)>,
}

impl Identity {
    pub(crate) fn load(source: &TlsSource) -> Result<Self, String> {
        match source {
            TlsSource::Development { hosts, store } => {
                if let Some(stored) = store.as_deref().and_then(|path| load_stored(path, hosts)) {
                    return Ok(stored);
                }
                let identity = dev_identity(hosts.clone())?;
                let loaded = Self {
                    chain: identity.chain,
                    key: identity.key,
                    fingerprint: Some(identity.fingerprint),
                    hosts: Some(hosts.clone()),
                    files: None,
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
                let (chain, key) = load_pem(certificate_chain, private_key)?;
                Ok(Self {
                    chain,
                    key,
                    fingerprint: None,
                    hosts: None,
                    files: Some((certificate_chain.clone(), private_key.clone())),
                })
            }
            TlsSource::Managed { domain } => Err(format!(
                "managed certificates for {domain} are not implemented: use TlsSource::Files with \
                 an ACME-managed certificate, or terminate TLS at the Cloud edge"
            )),
        }
    }

    /// The certificate the listener presents: one that follows renewals for
    /// files, the loaded one otherwise.
    pub(crate) fn certificate(&self) -> Result<Arc<dyn ResolvesServerCert>, String> {
        match &self.files {
            Some((chain, key)) => Ok(Arc::new(FileCertificate::load(chain, key)?)),
            None => fixed_certificate(self.chain.clone(), self.key.clone_key()),
        }
    }

    /// rustls server configuration for HTTPS over TCP (HTTP/2 and HTTP/1.1).
    pub(crate) fn https_config(&self) -> Result<rustls::ServerConfig, String> {
        https_server_config(self.certificate()?)
    }
}

/// A certificate chain (leaf first) and its private key, read from PEM
/// files.
///
/// # Errors
///
/// A file cannot be read or parsed, or the chain holds no certificate.
pub(crate) fn load_pem(
    chain: &Path,
    key: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), String> {
    let certificates = CertificateDer::pem_file_iter(chain)
        .map_err(|error| format!("certificate chain {}: {error}", chain.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("certificate chain {}: {error}", chain.display()))?;
    if certificates.is_empty() {
        return Err(format!("certificate chain {} is empty", chain.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key)
        .map_err(|error| format!("private key {}: {error}", key.display()))?;
    Ok((certificates, key))
}

/// The rustls server configuration for HTTPS over TCP: safe default
/// protocol versions, no client authentication, ALPN `h2` then `http/1.1`.
///
/// # Errors
///
/// The protocol versions are unsupported by the provider.
pub(crate) fn https_server_config(
    certificate: Arc<dyn ResolvesServerCert>,
) -> Result<rustls::ServerConfig, String> {
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_cert_resolver(certificate);
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

/// The crypto provider the listener uses.
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn certified(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<CertifiedKey>, String> {
    CertifiedKey::from_der(chain, key, &provider())
        .map(Arc::new)
        .map_err(|error| error.to_string())
}

/// A certificate loaded once and presented for as long as the listener runs.
fn fixed_certificate(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<dyn ResolvesServerCert>, String> {
    Ok(Arc::new(SingleCertAndKey::from(certified(chain, key)?)))
}

/// How often a [`FileCertificate`] looks at its files' modification times.
const RECHECK: Duration = Duration::from_secs(30);

/// A PEM chain and key on disk, read again when either file changes, so a
/// certificate an ACME client or cert-manager renews in place is presented
/// from the next handshake on without restarting the listener. The files
/// are looked at no more than once per [`RECHECK`]; a pair that fails to
/// load (caught mid-write) keeps the previous certificate and is retried.
#[derive(Debug)]
struct FileCertificate {
    chain: PathBuf,
    key: PathBuf,
    recheck: Duration,
    state: Mutex<FileState>,
}

#[derive(Debug)]
struct FileState {
    current: Arc<CertifiedKey>,
    stamp: Stamp,
    checked: Instant,
}

type Stamp = (Option<SystemTime>, Option<SystemTime>);

impl FileCertificate {
    /// Loads the pair now, failing when it is unusable.
    ///
    /// # Errors
    ///
    /// A file cannot be read or parsed, or the key does not match the chain.
    fn load(chain: &Path, key: &Path) -> Result<Self, String> {
        Self::with_recheck(chain, key, RECHECK)
    }

    fn with_recheck(chain: &Path, key: &Path, recheck: Duration) -> Result<Self, String> {
        let stamp = stamp(chain, key);
        let (certificates, private_key) = load_pem(chain, key)?;
        Ok(Self {
            chain: chain.to_path_buf(),
            key: key.to_path_buf(),
            recheck,
            state: Mutex::new(FileState {
                current: certified(certificates, private_key)?,
                stamp,
                checked: Instant::now(),
            }),
        })
    }

    /// The certificate to present now, reloading it first when the files
    /// changed since they were last read.
    fn current(&self) -> Arc<CertifiedKey> {
        let mut state = self.state.lock_recover();
        if state.checked.elapsed() >= self.recheck {
            state.checked = Instant::now();
            let now = stamp(&self.chain, &self.key);
            if now != state.stamp {
                match load_pem(&self.chain, &self.key)
                    .and_then(|(chain, key)| certified(chain, key))
                {
                    Ok(renewed) => {
                        state.current = renewed;
                        state.stamp = now;
                    }
                    Err(error) => eprintln!(
                        "tls: keeping the previous certificate, {} did not load: {error}",
                        self.chain.display()
                    ),
                }
            }
        }
        Arc::clone(&state.current)
    }
}

impl ResolvesServerCert for FileCertificate {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current())
    }
}

fn stamp(chain: &Path, key: &Path) -> Stamp {
    let modified = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    };
    (modified(chain), modified(key))
}

/// A development TLS identity: ECDSA P-256, self-signed, valid 13 days.
struct DevIdentity {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    /// SHA-256 of the certificate (lowercase hex).
    fingerprint: String,
}

/// Generate a [`DevIdentity`] for `hosts`.
fn dev_identity(hosts: Vec<String>) -> Result<DevIdentity, String> {
    let mut params = rcgen::CertificateParams::new(hosts).map_err(|error| error.to_string())?;
    let now = std::time::SystemTime::now();
    params.not_before = now.into();
    params.not_after = now
        .checked_add(Duration::from_secs(13 * 24 * 3600))
        .ok_or("certificate lifetime")?
        .into();
    let key = rcgen::KeyPair::generate().map_err(|error| error.to_string())?;
    let certificate = params
        .self_signed(&key)
        .map_err(|error| error.to_string())?;
    let der = certificate.der().clone();
    let fingerprint = {
        use sha2::Digest as _;
        hex::encode(sha2::Sha256::digest(der.as_ref()))
    };
    Ok(DevIdentity {
        chain: vec![der],
        key: PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        fingerprint,
    })
}

/// How long a stored development certificate is reused: generated ones are
/// valid 13 days; replace them with a day to spare.
const DEV_REUSE: std::time::Duration = std::time::Duration::from_secs(12 * 24 * 3600);

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredDev {
    created_unix: u64,
    /// The names the certificate carries, so a host that starts serving a new
    /// name replaces it instead of presenting one that cannot cover it.
    hosts: Vec<String>,
    certificate: Vec<u8>,
    pkcs8_key: Vec<u8>,
    sha256: String,
}

/// A stored development identity, while it is young enough and names every
/// host it is now needed for.
fn load_stored(path: &std::path::Path, hosts: &[String]) -> Option<Identity> {
    let stored: StoredDev = postcard::from_bytes(&std::fs::read(path).ok()?).ok()?;
    let age = tilo_clock::unix_now().checked_sub(stored.created_unix)?;
    let covers = hosts.iter().all(|host| {
        stored
            .hosts
            .iter()
            .any(|named| named.eq_ignore_ascii_case(host))
    });
    (age < DEV_REUSE.as_secs() && covers).then(|| Identity {
        chain: vec![CertificateDer::from(stored.certificate)],
        key: PrivateKeyDer::Pkcs8(stored.pkcs8_key.into()),
        fingerprint: Some(stored.sha256),
        hosts: Some(stored.hosts),
        files: None,
    })
}

fn save_stored(path: &std::path::Path, identity: &Identity) -> Result<(), String> {
    let PrivateKeyDer::Pkcs8(key) = &identity.key else {
        return Err("development key is not PKCS#8".into());
    };
    let stored = StoredDev {
        created_unix: tilo_clock::unix_now(),
        hosts: identity.hosts.clone().unwrap_or_default(),
        certificate: identity
            .chain
            .first()
            .map(|der| der.to_vec())
            .unwrap_or_default(),
        pkcs8_key: key.secret_pkcs8_der().to_vec(),
        sha256: identity.fingerprint.clone().unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pem_pair_loads_into_an_https_config_and_an_empty_chain_is_refused() {
        let generated = rcgen::generate_simple_self_signed(vec!["rooms.internal".into()]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (chain, key, empty) = (
            dir.path().join("chain.pem"),
            dir.path().join("key.pem"),
            dir.path().join("empty.pem"),
        );
        std::fs::write(&chain, generated.cert.pem()).unwrap();
        std::fs::write(&key, generated.signing_key.serialize_pem()).unwrap();
        std::fs::write(&empty, "").unwrap();

        let (certificates, private_key) = load_pem(&chain, &key).unwrap();
        assert_eq!(certificates, vec![generated.cert.der().clone()]);
        let config =
            https_server_config(fixed_certificate(certificates, private_key).unwrap()).unwrap();
        assert_eq!(
            config.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
        assert!(load_pem(&empty, &key).unwrap_err().contains("is empty"));
    }

    #[test]
    fn a_renewed_pair_on_disk_is_presented_without_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (chain, key) = (dir.path().join("tls.crt"), dir.path().join("tls.key"));
        // Each write stamps its own modification time, so a change never
        // hides inside the filesystem's timestamp granularity.
        let stamp = |path: &Path, second: u64| {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(second))
                .unwrap();
        };
        let write = |name: &str, second: u64| {
            let generated = rcgen::generate_simple_self_signed(vec![name.into()]).unwrap();
            std::fs::write(&chain, generated.cert.pem()).unwrap();
            std::fs::write(&key, generated.signing_key.serialize_pem()).unwrap();
            stamp(&chain, second);
            stamp(&key, second);
            generated.cert.der().clone()
        };
        let first = write("live.example", 1);
        let files = FileCertificate::with_recheck(&chain, &key, Duration::ZERO).unwrap();
        assert_eq!(files.current().cert[0], first);

        std::fs::write(&key, "").unwrap();
        stamp(&key, 2);
        assert_eq!(files.current().cert[0], first);

        let renewed = write("live.example", 3);
        assert_ne!(renewed, first);
        assert_eq!(files.current().cert[0], renewed);
    }

    /// A stored development certificate is reused while it names every host
    /// this start needs, and replaced when a host starts serving a new name:
    /// pages pin the certificate for the origin they dial.
    #[test]
    fn a_stored_development_certificate_is_replaced_when_a_start_needs_a_new_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = Some(dir.path().join("web-dev-identity"));
        let source = |hosts: &[&str]| TlsSource::Development {
            hosts: hosts.iter().map(|host| (*host).to_string()).collect(),
            store: store.clone(),
        };
        let first = Identity::load(&source(&["localhost", "127.0.0.1"])).unwrap();
        assert_eq!(
            first.hosts.as_deref(),
            Some(&["localhost".to_string(), "127.0.0.1".to_string()][..])
        );
        let again = Identity::load(&source(&["127.0.0.1"])).unwrap();
        assert_eq!(again.chain, first.chain, "names it covers are reused");
        let wider = Identity::load(&source(&["127.0.0.1", "live.example"])).unwrap();
        assert_ne!(
            wider.chain, first.chain,
            "a start that needs a new name replaces the certificate"
        );
        assert_eq!(
            wider.hosts.unwrap(),
            vec!["127.0.0.1".to_string(), "live.example".to_string()]
        );
    }
}
