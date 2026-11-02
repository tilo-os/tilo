//! The HTTPS server identity every Tilo browser-facing listener builds the
//! same way: a PEM chain and key from disk, served with rustls' ring
//! provider over HTTP/2 and HTTP/1.1. Behind the light `tls` feature, so a
//! listener that is not a `MoQ` relay (the public-room front) takes it
//! without the relay stack.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use tilo_lock::MutexRecover as _;

/// A certificate chain (leaf first) and its private key, read from PEM
/// files.
///
/// # Errors
///
/// A file cannot be read or parsed, or the chain holds no certificate.
pub fn load_pem(
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
pub fn https_server_config(
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

/// The crypto provider every listener here uses.
#[must_use]
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
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
pub fn fixed_certificate(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<dyn ResolvesServerCert>, String> {
    Ok(Arc::new(SingleCertAndKey::from(certified(chain, key)?)))
}

/// How often a [`FileCertificate`] looks at its files' modification times.
pub const RECHECK: Duration = Duration::from_secs(30);

/// A PEM chain and key on disk, read again when either file changes, so a
/// certificate an ACME client or cert-manager renews in place is presented
/// from the next handshake on without restarting the listener. The files
/// are looked at no more than once per [`RECHECK`]; a pair that fails to
/// load (caught mid-write) keeps the previous certificate and is retried.
#[derive(Debug)]
pub struct FileCertificate {
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
    pub fn load(chain: &Path, key: &Path) -> Result<Self, String> {
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
    pub fn current(&self) -> Arc<CertifiedKey> {
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
}
