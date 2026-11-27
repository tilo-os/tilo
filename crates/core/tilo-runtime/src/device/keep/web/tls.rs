//! Where the browser listener's TLS certificate comes from, and how it is
//! served.
//!
//! - **Local**: what a server with no domain name presents. The person's
//!   local CA on this server ([`LocalCa`]) issues an ECDSA certificate valid
//!   13 days for the names it is reached under. The person's own devices
//!   trust it ([`trusting_person`]); a browser elsewhere does not, and pins
//!   the certificate's hash, which the server's link names
//!   (`serverCertificateHashes`). It is issued again a day before it
//!   expires, live, and kept (never the CA's key) under the host's state
//!   root, reused across restarts while it still names every host that
//!   needs it.
//! - **Files**: a PEM chain and key on disk, renewed by whatever manages them
//!   — an ACME client (certbot, lego, Caddy) on a home host, or the Cloud
//!   machine's managed certificate. The listener reads the pair again when
//!   either file changes (looked at every 30 s), so a renewal needs no
//!   restart.
//! - **Managed**: ACME inside the host for the domain name it is given. Not
//!   implemented: configure `Files` from an ACME client, or terminate HTTPS
//!   at the Cloud edge.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use sha2::Digest as _;
use tilo_lock::MutexRecover as _;
use tilo_user_identity::{DeviceCert, RevocationList};

/// How the listener obtains its certificate.
#[derive(Clone, Debug)]
pub enum TlsSource {
    /// A certificate the person's local CA on this server issues for these
    /// host names and addresses.
    Local {
        /// Subject alternative names (for example `localhost`, `127.0.0.1`).
        hosts: Vec<String>,
        /// Keep the issued certificate here (owner-only) and reuse it across
        /// restarts while it has more than a day left, so a page that pinned
        /// it keeps working. `None` issues one per start.
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
    /// The certificate the listener presents, one for its HTTPS and
    /// WebTransport listeners alike, which follows its renewals.
    pub(crate) certificate: Arc<dyn ResolvesServerCert>,
    /// The certificate the person's local CA issues, when it is that.
    pub(crate) local: Option<Arc<LocalCertificate>>,
}

impl Identity {
    /// The identity `source` names. A local certificate is issued by the CA
    /// of the device `key`, endorsed with the certificate the person's root
    /// signed for it (`cert`); a node not yet installed has none, and its
    /// CA endorses nothing a device trusts.
    pub(crate) fn load(
        source: &TlsSource,
        key: &ed25519_dalek::SigningKey,
        cert: Option<&DeviceCert>,
    ) -> Result<Self, String> {
        match source {
            TlsSource::Local { hosts, store } => {
                let ca = LocalCa::new(key, cert)?;
                let local = Arc::new(LocalCertificate::new(ca, hosts.clone(), store.clone())?);
                Ok(Self {
                    certificate: local.clone(),
                    local: Some(local),
                })
            }
            TlsSource::Files {
                certificate_chain,
                private_key,
                ..
            } => Ok(Self {
                certificate: Arc::new(FileCertificate::load(certificate_chain, private_key)?),
                local: None,
            }),
            TlsSource::Managed { domain } => Err(format!(
                "managed certificates for {domain} are not implemented: use TlsSource::Files with \
                 an ACME-managed certificate, or terminate TLS at the Cloud edge"
            )),
        }
    }

    /// rustls server configuration for HTTPS over TCP (HTTP/2 and HTTP/1.1).
    pub(crate) fn https_config(&self) -> Result<rustls::ServerConfig, String> {
        https_server_config(Arc::clone(&self.certificate))
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

/// How long a certificate the local CA issues is valid: the longest a browser
/// pins one for (`serverCertificateHashes` takes no more than 14 days).
const ISSUED_FOR: Duration = Duration::from_secs(13 * 24 * 3600);

/// How long a certificate is presented before it is issued again: a day
/// before it expires.
const RENEW_AFTER: Duration = Duration::from_secs(12 * 24 * 3600);

/// What the local CA's Ed25519 key is derived from the device key under.
const CA_KEY_CONTEXT: &[u8] = b"tilo/web-ca/key/v1\0";

/// What the device key signs, followed by the CA's public key, to endorse the
/// CA as its own.
const CA_ENDORSEMENT_CONTEXT: &[u8] = b"tilo/web-ca/endorsement/v1\0";

/// The CA certificate's extension that carries its [`Endorsement`]: OID
/// 2.25.6082511997203997251 (a 2.25 arc of our own).
const ENDORSEMENT_OID: &[u64] = &[2, 25, 6_082_511_997_203_997_251];
/// [`ENDORSEMENT_OID`] as DER encodes it.
const ENDORSEMENT_OID_DER: &[u8] = &[
    0x06, 0x0a, 0x69, 0xd4, 0xb4, 0xdb, 0x8d, 0xf5, 0xbb, 0x95, 0xc4, 0x43,
];

/// An Ed25519 PKCS#8 v1 document up to its 32-byte seed.
const ED25519_PKCS8_PREFIX: &[u8] = &[
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// An Ed25519 subject public key info's value up to its 32-byte key.
const ED25519_SPKI_PREFIX: &[u8] = &[0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];

/// The person's local CA on this server, which issues the certificate a
/// server with no domain name presents.
///
/// Its Ed25519 key is derived from the server's device key on each start and
/// written nowhere: the person's root is not on the server to derive it
/// from, and a second key stored beside the device key would be one more
/// secret on its disk that protects nothing the device key does not. The
/// device endorses the CA with its key, and the CA's certificate carries that
/// endorsement with the device certificate the person's root signed, so a
/// device checks the CA back to its own person's root ([`trusting_person`])
/// and revoking the server's device revokes its CA.
pub(crate) struct LocalCa {
    issuer: rcgen::CertifiedIssuer<'static, rcgen::KeyPair>,
    public_key: [u8; 32],
}

/// The device's endorsement of a CA, in its certificate.
#[derive(serde::Serialize, serde::Deserialize)]
struct Endorsement {
    /// The endorsing device's certificate, signed by the person's root.
    device: DeviceCert,
    /// The device key's signature over [`CA_ENDORSEMENT_CONTEXT`] and the
    /// CA's public key.
    signature: Vec<u8>,
}

impl LocalCa {
    /// The CA of the device `key`, endorsed as the device `cert` certifies.
    pub(crate) fn new(
        key: &ed25519_dalek::SigningKey,
        cert: Option<&DeviceCert>,
    ) -> Result<Self, String> {
        let seed = sha2::Sha256::new()
            .chain_update(CA_KEY_CONTEXT)
            .chain_update(key.to_bytes())
            .finalize();
        let pkcs8 = [ED25519_PKCS8_PREFIX, seed.as_slice()].concat();
        let ca_key = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
            &pkcs8.as_slice().into(),
            &rcgen::PKCS_ED25519,
        )
        .map_err(|error| error.to_string())?;
        let public_key: [u8; 32] = ca_key
            .public_key_raw()
            .try_into()
            .map_err(|_| "the CA's key is no Ed25519 key")?;
        let mut params = rcgen::CertificateParams::default();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params.distinguished_name.push(
            rcgen::DnType::CommonName,
            format!("Tilo local CA {}", hex::encode(&public_key[..8])),
        );
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
        if let Some(cert) = cert {
            let endorsement = postcard::to_allocvec(&Endorsement {
                device: cert.clone(),
                signature: ed25519_dalek::Signer::sign(key, &endorsed(&public_key))
                    .to_bytes()
                    .to_vec(),
            })
            .map_err(|error| error.to_string())?;
            params
                .custom_extensions
                .push(rcgen::CustomExtension::from_oid_content(
                    ENDORSEMENT_OID,
                    endorsement,
                ));
        }
        let issuer = rcgen::CertifiedIssuer::self_signed(params, ca_key)
            .map_err(|error| error.to_string())?;
        Ok(Self { issuer, public_key })
    }

    /// A certificate for `hosts`, valid from `from` for [`ISSUED_FOR`].
    fn issue(&self, hosts: &[String], from: SystemTime) -> Result<Issued, String> {
        let mut params =
            rcgen::CertificateParams::new(hosts.to_vec()).map_err(|error| error.to_string())?;
        params.not_before = from.into();
        params.not_after = from
            .checked_add(ISSUED_FOR)
            .ok_or("certificate lifetime")?
            .into();
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let key = rcgen::KeyPair::generate().map_err(|error| error.to_string())?;
        let certificate = params
            .signed_by(&key, &self.issuer)
            .map_err(|error| error.to_string())?;
        Ok(Issued {
            from_unix: from
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
            hosts: hosts.to_vec(),
            ca: self.public_key,
            certificate: certificate.der().to_vec(),
            pkcs8_key: key.serialize_der(),
        })
    }

    /// `issued` and its chain up to this CA.
    fn chain(&self, issued: &Issued) -> Vec<CertificateDer<'static>> {
        vec![
            CertificateDer::from(issued.certificate.clone()),
            self.issuer.der().clone(),
        ]
    }

    /// `issued` as the listener presents it.
    fn certified(&self, issued: &Issued) -> Result<Arc<CertifiedKey>, String> {
        certified(
            self.chain(issued),
            PrivateKeyDer::Pkcs8(issued.pkcs8_key.clone().into()),
        )
    }
}

/// The certificate the person's local CA issues, renewed in place: from the
/// first handshake [`RENEW_AFTER`] after it was issued, a new one is issued
/// and presented, so a server that runs past [`ISSUED_FOR`] never presents an
/// expired certificate and needs no restart. Its time is
/// [`tilo_clock::wall`].
pub(crate) struct LocalCertificate {
    ca: LocalCa,
    hosts: Vec<String>,
    store: Option<PathBuf>,
    current: Mutex<(Issued, Arc<CertifiedKey>)>,
}

impl LocalCertificate {
    /// The stored certificate when it is still current for `hosts`, or a new
    /// one `ca` issues and keeps at `store`.
    fn new(ca: LocalCa, hosts: Vec<String>, store: Option<PathBuf>) -> Result<Self, String> {
        let stored = store
            .as_deref()
            .and_then(|path| load_stored(path, &hosts, &ca));
        let issued = if let Some(stored) = stored {
            stored
        } else {
            let issued = ca.issue(&hosts, tilo_clock::wall())?;
            if let Some(store) = &store {
                save_stored(store, &issued)?;
            }
            issued
        };
        let key = ca.certified(&issued)?;
        Ok(Self {
            ca,
            hosts,
            store,
            current: Mutex::new((issued, key)),
        })
    }

    /// The certificate presented now, issued again first when it is due. One
    /// that cannot be issued again stays presented while it is valid.
    fn current(&self) -> MutexGuard<'_, (Issued, Arc<CertifiedKey>)> {
        let mut current = self.current.lock_recover();
        if !current.0.current() {
            let renewed = self
                .ca
                .issue(&self.hosts, tilo_clock::wall())
                .and_then(|issued| Ok((self.ca.certified(&issued)?, issued)));
            match renewed {
                Ok((key, issued)) => {
                    if let Some(store) = &self.store
                        && let Err(error) = save_stored(store, &issued)
                    {
                        tracing::warn!(%error, "the renewed web certificate is presented, not kept");
                    }
                    *current = (issued, key);
                }
                Err(error) => {
                    tracing::error!(%error, "the web certificate is due and was not issued");
                }
            }
        }
        current
    }

    /// SHA-256 of the certificate presented now, which a browser that does
    /// not trust the person's CA pins.
    pub(crate) fn sha256(&self) -> [u8; 32] {
        sha2::Sha256::digest(&self.current().0.certificate).into()
    }
}

impl std::fmt::Debug for LocalCertificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalCertificate")
            .field("hosts", &self.hosts)
            .finish_non_exhaustive()
    }
}

impl ResolvesServerCert for LocalCertificate {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(&self.current().1))
    }
}

/// What a device signs to endorse the CA whose public key is `ca`.
fn endorsed(ca: &[u8; 32]) -> Vec<u8> {
    [CA_ENDORSEMENT_CONTEXT, ca.as_slice()].concat()
}

/// A verifier that trusts a listener's certificate only when the local CA of
/// one of `person`'s devices issued it: the CA a device endorsed whose
/// certificate `person`'s root signed, which is valid and which `revocations`
/// does not refuse. Another person's CA, a certificate no endorsed CA
/// issued, and an expired one are refused.
#[must_use]
pub fn trusting_person(
    person: [u8; 32],
    revocations: RevocationList,
) -> Arc<dyn ServerCertVerifier> {
    Arc::new(PersonCa {
        person,
        revocations,
        provider: provider(),
    })
}

#[derive(Debug)]
struct PersonCa {
    person: [u8; 32],
    revocations: RevocationList,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl PersonCa {
    /// `ca` as the one root to trust, when it is the endorsed CA of one of
    /// the person's devices at `now`.
    fn endorsed_root(
        &self,
        ca: &CertificateDer<'_>,
        now: UnixTime,
    ) -> Result<rustls::RootCertStore, rustls::Error> {
        let refused = rustls::Error::InvalidCertificate;
        let endorsement =
            endorsement_in(ca.as_ref()).ok_or(refused(CertificateError::UnknownIssuer))?;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.clone().into_owned())?;
        let public_key: [u8; 32] = roots
            .roots
            .first()
            .map(|anchor| anchor.subject_public_key_info.as_ref())
            .and_then(|spki| spki.strip_prefix(ED25519_SPKI_PREFIX))
            .and_then(|key| key.try_into().ok())
            .ok_or(refused(CertificateError::UnknownIssuer))?;
        let device = &endorsement.device;
        if device.user_public_key != self.person {
            return Err(refused(CertificateError::UnknownIssuer));
        }
        // The handshake's time counts whole seconds: a device is checked at the last instant of
        // that second, so one certified within it is in date.
        let now_ns = now
            .as_secs()
            .saturating_add(1)
            .saturating_mul(1_000_000_000)
            .saturating_sub(1);
        tilo_user_identity::verify_device_cert_at(device, now_ns)
            .map_err(|_| refused(CertificateError::ApplicationVerificationFailure))?;
        if self.revocations.refuses(device) {
            return Err(refused(CertificateError::Revoked));
        }
        let signature = ed25519_dalek::Signature::from_slice(&endorsement.signature)
            .map_err(|_| refused(CertificateError::BadSignature))?;
        ed25519_dalek::VerifyingKey::from_bytes(&device.device_public_key)
            .and_then(|key| key.verify_strict(&endorsed(&public_key), &signature))
            .map_err(|_| refused(CertificateError::BadSignature))?;
        Ok(roots)
    }
}

impl ServerCertVerifier for PersonCa {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let [ca] = intermediates else {
            return Err(rustls::Error::InvalidCertificate(
                CertificateError::UnknownIssuer,
            ));
        };
        let roots = self.endorsed_root(ca, now)?;
        rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::clone(&self.provider),
        )
        .build()
        .map_err(|error| rustls::Error::General(error.to_string()))?
        .verify_server_cert(end_entity, &[], server_name, ocsp_response, now)
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

/// The [`Endorsement`] in the CA certificate `ca`: the value of the
/// extension [`ENDORSEMENT_OID`] names. Only located here; what it says is
/// believed once its signatures verify.
fn endorsement_in(ca: &[u8]) -> Option<Endorsement> {
    let at = ca
        .windows(ENDORSEMENT_OID_DER.len())
        .position(|window| window == ENDORSEMENT_OID_DER)?
        .checked_add(ENDORSEMENT_OID_DER.len())?;
    let mut rest = ca.get(at..)?;
    // An extension's criticality, when it is written.
    if rest.first() == Some(&0x01) {
        rest = rest.get(3..)?;
    }
    let (0x04, value) = der_value(rest)? else {
        return None;
    };
    postcard::from_bytes(value).ok()
}

/// The tag and value of the DER element `input` starts with.
fn der_value(input: &[u8]) -> Option<(u8, &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let (bytes, rest) = rest.split_at_checked(usize::from(first & 0x7f))?;
        if bytes.is_empty() || bytes.len() > 4 {
            return None;
        }
        let len = bytes
            .iter()
            .fold(0usize, |len, byte| (len << 8) | usize::from(*byte));
        (len, rest)
    };
    Some((tag, rest.get(..len)?))
}

/// A certificate the local CA issued, and its key, as the host keeps it.
#[derive(serde::Serialize, serde::Deserialize)]
struct Issued {
    from_unix: u64,
    /// The names the certificate carries, so a host that starts serving a new
    /// name replaces it instead of presenting one that cannot cover it.
    hosts: Vec<String>,
    /// The public key of the CA that issued it.
    ca: [u8; 32],
    certificate: Vec<u8>,
    pkcs8_key: Vec<u8>,
}

impl Issued {
    /// Whether it is presented still, short of [`RENEW_AFTER`].
    fn current(&self) -> bool {
        tilo_clock::unix_now()
            .checked_sub(self.from_unix)
            .is_some_and(|age| age < RENEW_AFTER.as_secs())
    }
}

/// A stored certificate `ca` issued, while it is current and names every host
/// it is now needed for.
fn load_stored(path: &Path, hosts: &[String], ca: &LocalCa) -> Option<Issued> {
    let stored: Issued = postcard::from_bytes(&std::fs::read(path).ok()?).ok()?;
    let covers = hosts.iter().all(|host| {
        stored
            .hosts
            .iter()
            .any(|named| named.eq_ignore_ascii_case(host))
    });
    (covers && stored.ca == ca.public_key && stored.current()).then_some(stored)
}

fn save_stored(path: &Path, issued: &Issued) -> Result<(), String> {
    let bytes = postcard::to_allocvec(issued).map_err(|error| error.to_string())?;
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

    tilo_test_env::scenario! {
        fn tls_identities_load_renew_and_are_replaced_when_names_change;

        phase a_pem_pair_loads_into_an_https_config_and_an_empty_chain_is_refused;
        {
            let generated = rcgen::generate_simple_self_signed(vec!["workspaces.internal".into()]).unwrap();
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
            assert!(certified(certificates, private_key).is_ok());
            let config =
                https_server_config(Arc::new(FileCertificate::load(&chain, &key).unwrap())).unwrap();
            assert_eq!(
                config.alpn_protocols,
                vec![b"h2".to_vec(), b"http/1.1".to_vec()]
            );
            assert!(load_pem(&empty, &key).unwrap_err().contains("is empty"));
        }

        phase a_renewed_pair_on_disk_is_presented_without_a_restart;
        {
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


        phase a_local_certificate_is_trusted_by_its_person_and_refused_by_anyone_else;
        {
            let ada = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
            let bob = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
            let (server, server_cert) = device(&ada, 3);
            let (bobs_server, bobs_cert) = device(&bob, 4);
            let now = SystemTime::now();
            let issued = |ca: &LocalCa, from| ca.chain(&ca.issue(&["localhost".into()], from).unwrap());
            let ca = LocalCa::new(&server, Some(&server_cert)).unwrap();
            let adas_chain = issued(&ca, now);

            let adas = trusting_person(ada.verifying_key().to_bytes(), RevocationList::new());
            let bobs = trusting_person(bob.verifying_key().to_bytes(), RevocationList::new());
            presented(&adas, &adas_chain, "localhost", now).expect("Ada's devices trust her server's CA");
            assert!(
                presented(&adas, &adas_chain, "elsewhere.example", now).is_err(),
                "a certificate is trusted only for the names it was issued for"
            );
            assert_eq!(
                presented(&bobs, &adas_chain, "localhost", now).unwrap_err(),
                rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer),
                "another person's devices do not trust Ada's CA"
            );
            let bobs_ca = LocalCa::new(&bobs_server, Some(&bobs_cert)).unwrap();
            assert!(presented(&adas, &issued(&bobs_ca, now), "localhost", now).is_err());

            let month_ago = now - Duration::from_secs(30 * 24 * 3600);
            assert!(
                matches!(
                    presented(&adas, &issued(&ca, month_ago), "localhost", now),
                    Err(rustls::Error::InvalidCertificate(
                        CertificateError::ExpiredContext { .. }
                    ))
                ),
                "an expired certificate is refused"
            );

            let foreign = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let foreign = foreign.cert.der().clone();
            assert!(presented(&adas, std::slice::from_ref(&foreign), "localhost", now).is_err());
            assert!(
                presented(&adas, &[foreign, adas_chain[1].clone()], "localhost", now).is_err(),
                "a certificate the person's CA did not issue is refused beside it"
            );

            let unendorsed = LocalCa::new(&server, None).unwrap();
            assert_eq!(
                presented(&adas, &issued(&unendorsed, now), "localhost", now).unwrap_err(),
                rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer),
                "a node not yet installed is trusted by no device"
            );

            // A CA endorsed by a key that is not the device its certificate names.
            let stranger = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
            let forged = LocalCa::new(&stranger, Some(&server_cert)).unwrap();
            assert_eq!(
                presented(&adas, &issued(&forged, now), "localhost", now).unwrap_err(),
                rustls::Error::InvalidCertificate(CertificateError::BadSignature)
            );
        }

        phase a_stored_certificate_is_replaced_when_a_start_needs_a_new_name;
        // A stored certificate is reused while it names every host this start
        // needs, and replaced when a host starts serving a new name: pages pin
        // the certificate for the origin they dial.
        {
            let ada = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
            let (server, server_cert) = device(&ada, 3);
            let (other, other_cert) = device(&ada, 6);
            let dir = tempfile::tempdir().unwrap();
            let store = Some(dir.path().join("web-certificate"));
            let load = |device: (&ed25519_dalek::SigningKey, &DeviceCert), hosts: &[&str]| {
                let hosts = hosts.iter().map(|host| (*host).to_string()).collect();
                let ca = LocalCa::new(device.0, Some(device.1)).unwrap();
                LocalCertificate::new(ca, hosts, store.clone()).unwrap().sha256()
            };
            let first = load((&server, &server_cert), &["localhost", "127.0.0.1"]);
            let again = load((&server, &server_cert), &["127.0.0.1"]);
            assert_eq!(again, first, "names it covers are reused");
            let wider = load((&server, &server_cert), &["127.0.0.1", "live.example"]);
            assert_ne!(wider, first, "a start that needs a new name replaces the certificate");
            let reissued = load((&other, &other_cert), &["127.0.0.1", "live.example"]);
            assert_ne!(reissued, wider, "a certificate another CA issued is not presented");
        }

        phase a_due_certificate_is_issued_again_live_and_never_presented_expired;
        // A server that runs on presents a new certificate from the handshake
        // after it is due, without a restart, and a device trusts that one long
        // after the first expired.
        {
            let ada = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
            let (server, server_cert) = device(&ada, 3);
            let dir = tempfile::tempdir().unwrap();
            let store = dir.path().join("web-certificate");
            let start = tilo_clock::wall();
            let day = |days: u64| start + Duration::from_secs(days * 24 * 3600);
            let ca = || LocalCa::new(&server, Some(&server_cert)).unwrap();
            let hosts = vec!["localhost".to_string()];
            let local = tilo_clock::testing::with_wall(day(0), || {
                LocalCertificate::new(ca(), hosts.clone(), Some(store.clone())).unwrap()
            });
            let presented_on = |days| {
                tilo_clock::testing::with_wall(day(days), || {
                    let presented = local.current().1.cert.clone();
                    (presented, local.sha256())
                })
            };
            let (first, first_pin) = presented_on(0);
            assert_eq!(presented_on(11).0, first, "a current certificate stays");

            let (renewed, renewed_pin) = presented_on(12);
            assert_ne!(renewed, first, "a due certificate is issued again, live");
            assert_ne!(renewed_pin, first_pin, "a page pins the one presented now");
            let adas = trusting_person(ada.verifying_key().to_bytes(), RevocationList::new());
            assert!(matches!(
                presented(&adas, &first, "localhost", day(14)),
                Err(rustls::Error::InvalidCertificate(
                    CertificateError::ExpiredContext { .. }
                ))
            ));
            presented(&adas, &renewed, "localhost", day(14))
                .expect("the renewed certificate is trusted after the first expired");

            let restarted = tilo_clock::testing::with_wall(day(12), || {
                LocalCertificate::new(ca(), hosts.clone(), Some(store.clone())).unwrap().sha256()
            });
            assert_eq!(restarted, renewed_pin, "the renewed certificate is the one kept");
        }
    }

    /// A device of `root`'s person, its key from `seed`.
    fn device(
        root: &ed25519_dalek::SigningKey,
        seed: u8,
    ) -> (ed25519_dalek::SigningKey, DeviceCert) {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "server".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        (key, cert)
    }

    /// What `verifier` makes of `chain` presented for `name` at `at`.
    fn presented(
        verifier: &Arc<dyn ServerCertVerifier>,
        chain: &[CertificateDer<'static>],
        name: &str,
        at: SystemTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let (leaf, rest) = chain.split_first().unwrap();
        verifier.verify_server_cert(
            leaf,
            rest,
            &ServerName::try_from(name.to_string()).unwrap(),
            &[],
            UnixTime::since_unix_epoch(at.duration_since(SystemTime::UNIX_EPOCH).unwrap()),
        )
    }
}
