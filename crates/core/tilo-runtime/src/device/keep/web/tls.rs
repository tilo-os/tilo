//! Where the browser listener's TLS certificate comes from, and how it is
//! served.
//!
//! - **Local**: what a server with no domain name presents. The person's
//!   local CA on this server ([`LocalCa`]) issues an ECDSA certificate valid
//!   13 days for the names it is reached under. The person's own devices
//!   trust it ([`trusting_person`]); a browser elsewhere does not, and pins
//!   the certificate's hash, which the server's link names
//!   (`serverCertificateHashes`). The certificate (never the CA's key) is
//!   kept under the host's state root and reused across restarts while it
//!   still names every host that needs it.
//! - **Files**: a PEM chain and key on disk, renewed by whatever manages them
//!   — an ACME client (certbot, lego, Caddy) on a home host, or the Cloud
//!   machine's managed certificate. The listener reads the pair again when
//!   either file changes (looked at every 30 s), so a renewal needs no
//!   restart.
//! - **Managed**: ACME inside the host for the domain name it is given. Not
//!   implemented: configure `Files` from an ACME client, or terminate HTTPS
//!   at the Cloud edge.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
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
    pub(crate) chain: Vec<CertificateDer<'static>>,
    pub(crate) key: PrivateKeyDer<'static>,
    /// SHA-256 (lowercase hex) of a certificate the person's local CA issued,
    /// which a browser that does not trust that CA pins.
    pub(crate) fingerprint: Option<String>,
    /// The names the certificate is for, when this host's CA issued it;
    /// `None` for a certificate from files, whose names this host does not
    /// read.
    pub(crate) hosts: Option<Vec<String>>,
    /// The PEM pair to follow for renewals, for certificates from files.
    pub(crate) files: Option<(PathBuf, PathBuf)>,
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
                if let Some(stored) = store
                    .as_deref()
                    .and_then(|path| load_stored(path, hosts, &ca))
                {
                    return Ok(stored);
                }
                let issued = ca.issue(hosts, SystemTime::now())?;
                if let Some(store) = store {
                    save_stored(store, &issued, &ca)?;
                }
                Ok(issued)
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

/// How long a certificate the local CA issues is valid: the longest a browser
/// pins one for (`serverCertificateHashes` takes no more than 14 days).
const ISSUED_FOR: Duration = Duration::from_secs(13 * 24 * 3600);

/// How long a stored certificate is reused: replaced with a day to spare.
const REUSE: Duration = Duration::from_secs(12 * 24 * 3600);

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

    /// A certificate for `hosts`, valid from `from` for [`ISSUED_FOR`], with
    /// its chain up to this CA.
    fn issue(&self, hosts: &[String], from: SystemTime) -> Result<Identity, String> {
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
        let der = certificate.der().clone();
        Ok(Identity {
            fingerprint: Some(hex::encode(sha2::Sha256::digest(der.as_ref()))),
            chain: vec![der, self.issuer.der().clone()],
            key: PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            hosts: Some(hosts.to_vec()),
            files: None,
        })
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

#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    created_unix: u64,
    /// The names the certificate carries, so a host that starts serving a new
    /// name replaces it instead of presenting one that cannot cover it.
    hosts: Vec<String>,
    /// The public key of the CA that issued it.
    ca: [u8; 32],
    certificate: Vec<u8>,
    pkcs8_key: Vec<u8>,
    sha256: String,
}

/// A stored certificate `ca` issued, while it is young enough and names every
/// host it is now needed for.
fn load_stored(path: &Path, hosts: &[String], ca: &LocalCa) -> Option<Identity> {
    let stored: Stored = postcard::from_bytes(&std::fs::read(path).ok()?).ok()?;
    let age = tilo_clock::unix_now().checked_sub(stored.created_unix)?;
    let covers = hosts.iter().all(|host| {
        stored
            .hosts
            .iter()
            .any(|named| named.eq_ignore_ascii_case(host))
    });
    (age < REUSE.as_secs() && covers && stored.ca == ca.public_key).then(|| Identity {
        chain: vec![
            CertificateDer::from(stored.certificate),
            ca.issuer.der().clone(),
        ],
        key: PrivateKeyDer::Pkcs8(stored.pkcs8_key.into()),
        fingerprint: Some(stored.sha256),
        hosts: Some(stored.hosts),
        files: None,
    })
}

fn save_stored(path: &Path, identity: &Identity, ca: &LocalCa) -> Result<(), String> {
    let PrivateKeyDer::Pkcs8(key) = &identity.key else {
        return Err("the issued key is not PKCS#8".into());
    };
    let stored = Stored {
        created_unix: tilo_clock::unix_now(),
        hosts: identity.hosts.clone().unwrap_or_default(),
        ca: ca.public_key,
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
            let config =
                https_server_config(fixed_certificate(certificates, private_key).unwrap()).unwrap();
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
            let hosts = ["localhost".to_string()];
            let ca = LocalCa::new(&server, Some(&server_cert)).unwrap();
            let issued = ca.issue(&hosts, SystemTime::now()).unwrap();

            let adas = trusting_person(ada.verifying_key().to_bytes(), RevocationList::new());
            let bobs = trusting_person(bob.verifying_key().to_bytes(), RevocationList::new());
            presented(&adas, &issued.chain, "localhost").expect("Ada's devices trust her server's CA");
            assert!(
                presented(&adas, &issued.chain, "elsewhere.example").is_err(),
                "a certificate is trusted only for the names it was issued for"
            );
            assert_eq!(
                presented(&bobs, &issued.chain, "localhost").unwrap_err(),
                rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer),
                "another person's devices do not trust Ada's CA"
            );
            let bobs_issued = LocalCa::new(&bobs_server, Some(&bobs_cert))
                .unwrap()
                .issue(&hosts, SystemTime::now())
                .unwrap();
            assert!(presented(&adas, &bobs_issued.chain, "localhost").is_err());

            let month_ago = SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
            let expired = ca.issue(&hosts, month_ago).unwrap();
            assert!(
                matches!(
                    presented(&adas, &expired.chain, "localhost"),
                    Err(rustls::Error::InvalidCertificate(
                        CertificateError::ExpiredContext { .. }
                    ))
                ),
                "an expired certificate is refused"
            );

            let foreign = rcgen::generate_simple_self_signed(hosts.to_vec()).unwrap();
            let foreign = foreign.cert.der().clone();
            assert!(presented(&adas, std::slice::from_ref(&foreign), "localhost").is_err());
            assert!(
                presented(&adas, &[foreign, issued.chain[1].clone()], "localhost").is_err(),
                "a certificate the person's CA did not issue is refused beside it"
            );

            let unendorsed = LocalCa::new(&server, None)
                .unwrap()
                .issue(&hosts, SystemTime::now())
                .unwrap();
            assert_eq!(
                presented(&adas, &unendorsed.chain, "localhost").unwrap_err(),
                rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer),
                "a node not yet installed is trusted by no device"
            );

            // A CA endorsed by a key that is not the device its certificate names.
            let stranger = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
            let forged = LocalCa::new(&stranger, Some(&server_cert))
                .unwrap()
                .issue(&hosts, SystemTime::now())
                .unwrap();
            assert_eq!(
                presented(&adas, &forged.chain, "localhost").unwrap_err(),
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
            let dir = tempfile::tempdir().unwrap();
            let store = Some(dir.path().join("web-certificate"));
            let load = |hosts: &[&str]| {
                let source = TlsSource::Local {
                    hosts: hosts.iter().map(|host| (*host).to_string()).collect(),
                    store: store.clone(),
                };
                Identity::load(&source, &server, Some(&server_cert)).unwrap()
            };
            let first = load(&["localhost", "127.0.0.1"]);
            assert_eq!(
                first.hosts.as_deref(),
                Some(&["localhost".to_string(), "127.0.0.1".to_string()][..])
            );
            let again = load(&["127.0.0.1"]);
            assert_eq!(again.chain, first.chain, "names it covers are reused");
            let wider = load(&["127.0.0.1", "live.example"]);
            assert_ne!(
                wider.chain, first.chain,
                "a start that needs a new name replaces the certificate"
            );
            assert_eq!(
                wider.hosts.unwrap(),
                vec!["127.0.0.1".to_string(), "live.example".to_string()]
            );
            let (other, other_cert) = device(&ada, 6);
            let reissued = Identity::load(
                &TlsSource::Local {
                    hosts: vec!["127.0.0.1".into(), "live.example".into()],
                    store: store.clone(),
                },
                &other,
                Some(&other_cert),
            )
            .unwrap();
            assert_ne!(
                reissued.chain[0], wider.chain[0],
                "a certificate another CA issued is not presented"
            );
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

    /// What `verifier` makes of `chain` presented for `name`.
    fn presented(
        verifier: &Arc<dyn ServerCertVerifier>,
        chain: &[CertificateDer<'static>],
        name: &str,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let (leaf, rest) = chain.split_first().unwrap();
        verifier.verify_server_cert(
            leaf,
            rest,
            &ServerName::try_from(name.to_string()).unwrap(),
            &[],
            UnixTime::now(),
        )
    }
}
