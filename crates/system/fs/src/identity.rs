//! This mount's identity on the network and in the document.
//!
//! The filesystem does not mint a key: it reads the machine's one device key
//! and presents it as its iroh secret key. So the `NodeId` peers dial is the
//! device's Ed25519 public key, every namespace change's author is that same
//! identity (not a fresh anonymous string per mount), and an access-control
//! grant naming the default subject still names the same subject after a reboot.

use std::path::PathBuf;

use anyhow::{Context, Result};
use thiserror::Error;
use tilo_user_identity::{
    DeviceCert, DeviceCertError, DeviceKeyOutcome, IdentityPaths, PersistError, RevocationEntry,
    RevocationError, RevocationList, load_or_create_device_key, verify_device_cert_at,
};

/// Overrides the identity root (`/var/lib/tilo`) — two mounts acting as two devices point this at two roots; tests use a tempdir.
pub const IDENTITY_ROOT_ENV: &str = "TILO_IDENTITY_ROOT";

/// Verified revocations cached beside the user's public identity.
pub fn revocation_list_path(paths: &IdentityPaths) -> PathBuf {
    paths
        .user_pubkey
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("revocations.bin")
}

/// Failures loading the local aggregate of signed device revocations.
#[derive(Debug, Error)]
pub enum RevocationListLoadError {
    /// The revocation cache could not be read.
    #[error("revocation list at {path} could not be read: {source}")]
    Io {
        /// Attempted cache path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// The cache was not a canonical vector of signed entries.
    #[error("revocation list could not be decoded: {0}")]
    Decode(#[from] postcard::Error),
    /// A cached entry did not carry a valid user-root signature.
    #[error("revocation list contains an invalid entry: {0}")]
    InvalidEntry(#[from] RevocationError),
}

/// Load and verify the signed revocation aggregate used for peer admission.
///
/// A missing cache means no revocation has been learned yet. Any other read,
/// decode, or signature failure aborts collaborative networking rather than
/// silently treating corrupt trust state as empty.
///
/// # Errors
///
/// Returns an error when the cache cannot be read or decoded, or when an
/// entry's user-root signature is invalid.
pub fn load_revocation_list(
    paths: &IdentityPaths,
) -> std::result::Result<RevocationList, RevocationListLoadError> {
    let path = revocation_list_path(paths);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RevocationList::new());
        }
        Err(source) => return Err(RevocationListLoadError::Io { path, source }),
    };
    let entries: Vec<RevocationEntry> = postcard::from_bytes(&bytes)?;
    let mut list = RevocationList::new();
    for entry in entries {
        list.insert(entry)?;
    }
    Ok(list)
}

/// The systemd credential a per-user unit receives the device key as.
pub const DEVICE_KEY_CREDENTIAL: &str = "device-key";

/// Where the device key lives — [`IdentityPaths::default_system`] unless [`IDENTITY_ROOT_ENV`] overrides it.
///
/// A sandboxed per-user unit runs in a user namespace that maps only its own
/// account, so the root-owned system key reads as owned by nobody and is
/// refused. Its service manager reads the key for it instead and hands it
/// over as the [`DEVICE_KEY_CREDENTIAL`] credential, owned by the unit's user.
pub fn identity_paths() -> IdentityPaths {
    let paths = match std::env::var(IDENTITY_ROOT_ENV) {
        Ok(v) if !v.is_empty() => IdentityPaths::under(PathBuf::from(v)),
        _ => IdentityPaths::default_system(),
    };
    // The unit's device-key credential comes from the machine identity root.
    // A caller-selected root is a complete identity: keep its key with its
    // certificate rather than combining it with that unrelated credential.
    if paths.device_privkey != IdentityPaths::default_system().device_privkey {
        return paths;
    }
    with_device_key_credential(paths, std::env::var_os("CREDENTIALS_DIRECTORY"))
}

/// `paths` with the device key read from `credentials`, when the service
/// manager passed one. A unit without an enrolled device gets a placeholder
/// that is no key, and keeps the system path, which reports what is missing.
fn with_device_key_credential(
    mut paths: IdentityPaths,
    credentials: Option<std::ffi::OsString>,
) -> IdentityPaths {
    let Some(directory) = credentials.filter(|directory| !directory.is_empty()) else {
        return paths;
    };
    let key = PathBuf::from(directory).join(DEVICE_KEY_CREDENTIAL);
    if std::fs::metadata(&key).is_ok_and(|metadata| metadata.is_file() && metadata.len() == 32) {
        paths.device_privkey = key;
    }
    paths
}

/// The device identity this mount runs as.
pub struct MountIdentity {
    /// iroh secret key — the same 32 bytes as the device key, so the node id is the device's public key.
    pub secret: iroh::SecretKey,
    /// The device's Ed25519 public key; equals `endpoint.id()`'s bytes and is the default access-control subject.
    pub public: [u8; 32],
    /// True when the key was generated by this call (first boot).
    pub created: bool,
    /// The same device signing key retained for certified protocol proofs.
    /// Callers should prefer [`Self::signing_key`] and never persist another
    /// copy of this secret.
    signing: ed25519_dalek::SigningKey,
}

impl MountIdentity {
    /// Device key used for certified challenge responses and signed records.
    pub fn signing_key(&self) -> &ed25519_dalek::SigningKey {
        &self.signing
    }
}

/// Certified credentials presented during collaborative-FS peer auth.
///
/// The constructor is private: every instance has already proved that its
/// certificate names the retained device key.
#[derive(Clone)]
pub struct PeerCredentials {
    cert: DeviceCert,
    signing: ed25519_dalek::SigningKey,
}

/// Failures selecting the human authority that may issue workspace shares.
#[derive(Debug, Error)]
pub enum WorkspaceShareIssuerError {
    /// The primary/root key exists but could not be loaded securely.
    #[error("workspace share issuer could not be loaded: {0}")]
    RootKey(#[from] PersistError),
    /// The private key does not match the human root that certified this
    /// device. Treating it as an issuer would create an unrelated authority.
    #[error("workspace share issuer does not match the certified human identity")]
    RootKeyMismatch,
}

/// Load the human-root signing authority for owner share operations.
///
/// `Ok(None)` is an intentional, fail-closed result: this certified device is
/// not allowed to own workspaces, or it is a secondary device on which the
/// human root private key is correctly absent. Callers must not substitute the
/// device key in either case.
///
/// # Errors
///
/// Returns an error when the root key cannot be loaded securely or does not
/// match the human identity that certified this device.
pub fn load_workspace_share_issuer(
    paths: &IdentityPaths,
    credentials: &PeerCredentials,
) -> std::result::Result<Option<ed25519_dalek::SigningKey>, WorkspaceShareIssuerError> {
    if !credentials.cert.capabilities.can_own_workspaces {
        return Ok(None);
    }
    let issuer = match tilo_user_identity::persist::read_owned_signing_key(&paths.user_privkey) {
        Ok(issuer) => issuer,
        Err(PersistError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    if issuer.verifying_key().to_bytes() != credentials.user_public_key() {
        return Err(WorkspaceShareIssuerError::RootKeyMismatch);
    }
    Ok(Some(issuer))
}

impl PeerCredentials {
    /// Root-signed certificate binding this device to its human identity.
    pub fn cert(&self) -> &DeviceCert {
        &self.cert
    }

    /// Device signing key used only for proof-of-possession and signed ops.
    pub fn signing_key(&self) -> &ed25519_dalek::SigningKey {
        &self.signing
    }

    /// Human root public key certified as owning this device.
    pub fn user_public_key(&self) -> [u8; 32] {
        self.cert.user_public_key
    }

    /// Certified device public key. This equals the mount's Iroh endpoint.
    pub fn device_public_key(&self) -> [u8; 32] {
        self.cert.device_public_key
    }
}

/// A mount identity whose human/device certificate chain is ready for peer
/// authentication.
pub struct CertifiedMountIdentity {
    /// Stable Iroh/document identity, retaining the device signing key.
    pub mount: MountIdentity,
    /// Validated certificate and matching proof key.
    pub credentials: PeerCredentials,
}

impl CertifiedMountIdentity {
    /// Split the certified identity for endpoint construction and protocol
    /// handler installation.
    pub fn into_parts(self) -> (MountIdentity, PeerCredentials) {
        (self.mount, self.credentials)
    }
}

/// Failures loading credentials for collaborative networking.
#[derive(Debug, Error)]
pub enum CertifiedIdentityError {
    /// The stable device key could not be loaded or created securely.
    #[error("device identity could not be loaded: {0}")]
    DeviceKey(#[source] anyhow::Error),
    /// The certificate file was missing, unreadable, or malformed.
    #[error("device certificate could not be loaded: {0}")]
    CertificateFile(#[from] PersistError),
    /// The certificate's root signature is invalid or its deadline passed.
    #[error("device certificate is invalid: {0}")]
    Certificate(#[from] DeviceCertError),
    /// The certificate belongs to a different device key.
    #[error("device certificate does not name this mount's device key")]
    DeviceKeyMismatch,
}

/// Load the device key and shape it into the mount's identity. Fails rather
/// than falling back to a random key: a mount that can't prove which device it is has no business recording authorship.
///
/// # Errors
///
/// Returns an error when the device key at `paths.device_privkey` cannot be loaded or created
/// (unreadable, corrupt, or the directory cannot be created).
pub fn load_mount_identity(paths: &IdentityPaths) -> Result<MountIdentity> {
    let (signing, outcome) = load_or_create_device_key(paths).with_context(|| {
        format!(
            "device key at {} could not be loaded",
            paths.device_privkey.display()
        )
    })?;
    let seed = signing.to_bytes();
    let secret = iroh::SecretKey::from_bytes(&seed);
    let public = signing.verifying_key().to_bytes();
    debug_assert_eq!(
        secret.public().as_bytes(),
        &public,
        "iroh and dalek must agree on the public key derived from one seed"
    );
    Ok(MountIdentity {
        secret,
        public,
        created: outcome == DeviceKeyOutcome::Created,
        signing,
    })
}

/// Load the stable mount key plus its root-signed device certificate.
///
/// Unlike [`load_mount_identity`], this is the required entry point for any
/// collaborative network plane. It fails closed when enrollment has not
/// produced a certificate, when the certificate cannot be decoded or
/// verified, when it is expired/not-yet-valid, or when it names another
/// device key.
///
/// # Errors
///
/// Returns an error when the device key or certificate cannot be loaded, the
/// certificate is invalid at `now_ns`, or it names a different device.
pub fn load_certified_mount_identity(
    paths: &IdentityPaths,
    now_ns: u64,
) -> std::result::Result<CertifiedMountIdentity, CertifiedIdentityError> {
    let mount = load_mount_identity(paths).map_err(CertifiedIdentityError::DeviceKey)?;
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)?;
    verify_device_cert_at(&cert, now_ns)?;
    if cert.device_public_key != mount.public {
        return Err(CertifiedIdentityError::DeviceKeyMismatch);
    }
    let credentials = PeerCredentials {
        cert,
        signing: mount.signing.clone(),
    };
    Ok(CertifiedMountIdentity { mount, credentials })
}

/// [`load_certified_mount_identity`] at the wall clock's reading, waiting
/// while the certificate is refused only because the clock reads a time
/// before its issuance.
///
/// A machine whose real-time clock came up behind refuses its own,
/// intact certificate until network time steps the clock. The daemon stays
/// up and waits for that step (or for the issuance instant) instead of
/// exiting into a restart loop or idling for good: the wait is a timerfd
/// that the kernel ends when the clock changes, and the certificate is read
/// again after each wake, so one written anew meanwhile counts too.
///
/// # Errors
///
/// As [`load_certified_mount_identity`], for every refusal other than a
/// clock behind the certificate; and the clock refusal itself when the
/// clock cannot be waited on.
pub async fn load_certified_mount_identity_once_valid(
    paths: &IdentityPaths,
) -> std::result::Result<CertifiedMountIdentity, CertifiedIdentityError> {
    loop {
        let now_ns = crate::clock::unix_now_ns();
        let refused = match load_certified_mount_identity(paths, now_ns) {
            Err(CertifiedIdentityError::Certificate(DeviceCertError::NotYetValid)) => {
                CertifiedIdentityError::Certificate(DeviceCertError::NotYetValid)
            }
            loaded => return loaded,
        };
        let valid_from_ns =
            tilo_user_identity::persist::read_device_cert(&paths.device_cert)?.issued_at_ns;
        if valid_from_ns <= now_ns {
            // Written anew between the two reads: check it again.
            continue;
        }
        eprintln!(
            "device certificate is valid from {valid_from_ns} ns and the clock reads {now_ns} ns; \
             waiting for the clock"
        );
        let waited = tokio::task::spawn_blocking(move || {
            tilo_user_identity::wait_for_wall_clock(valid_from_ns)
        })
        .await;
        match waited {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                eprintln!("cannot wait for the clock: {error}");
                return Err(refused);
            }
            Err(error) => {
                eprintln!("the wait for the clock ended without an answer: {error}");
                return Err(refused);
            }
        }
    }
}

/// Read an already-certified device for recovery without first-boot key creation.
pub fn load_existing_certified_mount_identity(
    paths: &IdentityPaths,
    now_ns: u64,
) -> std::result::Result<CertifiedMountIdentity, CertifiedIdentityError> {
    let signing = tilo_user_identity::persist::load_device_key(paths)
        .map_err(|error| CertifiedIdentityError::DeviceKey(error.into()))?;
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)?;
    tilo_user_identity::verify_device_cert_at(&cert, now_ns)?;
    let public = signing.verifying_key().to_bytes();
    if cert.device_public_key != public {
        return Err(CertifiedIdentityError::DeviceKeyMismatch);
    }
    let mount = MountIdentity {
        secret: iroh::SecretKey::from_bytes(&signing.to_bytes()),
        public,
        created: false,
        signing: signing.clone(),
    };
    Ok(CertifiedMountIdentity {
        mount,
        credentials: PeerCredentials { cert, signing },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{
        DeviceCapabilities, adopt_device_key, persist::write_device_cert, sign_device_cert,
    };

    const NOW_NS: u64 = 5_000;
    const DEVICE_SEED: [u8; 32] = [41; 32];
    const ROOT_SEED: [u8; 32] = [73; 32];

    fn install_fixed_device(paths: &IdentityPaths) -> ed25519_dalek::SigningKey {
        adopt_device_key(paths, &DEVICE_SEED).unwrap()
    }

    fn signed_cert(
        device_public_key: [u8; 32],
        issued_at_ns: u64,
        expires_at_ns: Option<u64>,
    ) -> DeviceCert {
        let root = ed25519_dalek::SigningKey::from_bytes(&ROOT_SEED);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key,
            device_label: "Deterministic test laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    }

    fn certified_error(paths: &IdentityPaths) -> CertifiedIdentityError {
        match load_certified_mount_identity(paths, NOW_NS) {
            Ok(_) => panic!("certified identity unexpectedly loaded"),
            Err(err) => err,
        }
    }

    #[tokio::test]
    async fn a_certificate_issued_after_the_clocks_reading_loads_once_the_clock_reaches_it() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path());
        let device = install_fixed_device(&paths);
        // The refusal is judged at the reading the certificate was dated from, so a slow
        // node that takes longer than the margin to write it cannot make it valid already.
        let read_at_ns = crate::clock::unix_now_ns();
        let issued_at_ns = read_at_ns + 200_000_000;
        let cert = signed_cert(device.verifying_key().to_bytes(), issued_at_ns, None);
        write_device_cert(&paths.device_cert, &cert).unwrap();
        assert!(matches!(
            load_certified_mount_identity(&paths, read_at_ns),
            Err(CertifiedIdentityError::Certificate(
                DeviceCertError::NotYetValid
            ))
        ));

        let loaded = load_certified_mount_identity_once_valid(&paths)
            .await
            .expect("the daemon waits for the clock instead of giving up");
        assert_eq!(loaded.credentials.cert, cert);
        assert!(crate::clock::unix_now_ns() >= issued_at_ns);

        // Refusals the clock cannot cure are reported at once.
        let other = ed25519_dalek::SigningKey::from_bytes(&[99; 32]);
        let foreign = signed_cert(other.verifying_key().to_bytes(), 1, None);
        write_device_cert(&paths.device_cert, &foreign).unwrap();
        assert!(matches!(
            load_certified_mount_identity_once_valid(&paths).await,
            Err(CertifiedIdentityError::DeviceKeyMismatch)
        ));
    }

    #[test]
    fn the_device_identity_is_the_iroh_key_and_survives_restart() {
        // Phase `iroh_node_key_and_device_key_are_the_same_identity`.
        // The whole point of deriving the node key from the device key: one seed,
        // one public key. If this ever diverges, the node id stops naming the device and every grant against it silently means something else.
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let id = load_mount_identity(&paths).unwrap();
            assert!(id.created);
            assert_eq!(id.secret.public().as_bytes(), &id.public);
        }
        // Phase `identity_survives_a_restart`.
        // A restart must land on the same identity — the property an access-control grant depends on.
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let first = load_mount_identity(&paths).unwrap();
            let second = load_mount_identity(&paths).unwrap();
            assert!(first.created);
            assert!(!second.created, "second load must not mint a key");
            assert_eq!(first.public, second.public);
            assert_eq!(first.secret.public(), second.secret.public());
        }
    }

    /// The bound endpoint's node id is the device public key, verified through
    /// the endpoint binding path.
    #[tokio::test]
    async fn bound_endpoint_is_named_by_the_device_key() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path());
        let id = load_mount_identity(&paths).unwrap();

        let (endpoint, _lookup) = crate::sync::bind_endpoint_with_key(id.secret.clone())
            .await
            .unwrap();
        assert_eq!(endpoint.id().as_bytes(), &id.public);
        endpoint.close().await;

        // Re-binding after a "restart" keeps the same node id.
        let again = load_mount_identity(&paths).unwrap();
        let (endpoint2, _l2) = crate::sync::bind_endpoint_with_key(again.secret.clone())
            .await
            .unwrap();
        assert_eq!(
            endpoint2.id(),
            endpoint.id(),
            "authorship and ACL subject must survive a remount"
        );
        endpoint2.close().await;
    }

    /// An unreadable device key must stop the mount, not be replaced.
    #[cfg(unix)]
    #[test]
    fn corrupt_device_key_fails_the_mount() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path());
        paths.ensure_directories().unwrap();
        std::fs::write(&paths.device_privkey, b"garbage").unwrap();
        std::fs::set_permissions(
            &paths.device_privkey,
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(load_mount_identity(&paths).is_err());
        assert_eq!(std::fs::read(&paths.device_privkey).unwrap(), b"garbage");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn certified_loading_requires_a_valid_current_certificate_for_this_device() {
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let device = install_fixed_device(&paths);
            let cert = signed_cert(device.verifying_key().to_bytes(), NOW_NS - 1, None);
            write_device_cert(&paths.device_cert, &cert).unwrap();

            let certified = load_certified_mount_identity(&paths, NOW_NS).unwrap();
            assert_eq!(certified.mount.public, cert.device_public_key);
            assert_eq!(
                certified.mount.signing_key().verifying_key().to_bytes(),
                cert.device_public_key
            );
            assert_eq!(certified.credentials.cert(), &cert);
            assert_eq!(
                certified
                    .credentials
                    .signing_key()
                    .verifying_key()
                    .to_bytes(),
                cert.device_public_key
            );
            assert_eq!(
                certified.credentials.user_public_key(),
                cert.user_public_key
            );
            assert_eq!(
                certified.credentials.device_public_key(),
                cert.device_public_key
            );
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            install_fixed_device(&paths);
            let err = certified_error(&paths);
            assert!(matches!(
                err,
                CertifiedIdentityError::CertificateFile(PersistError::Io(ref io_err))
                    if io_err.kind() == std::io::ErrorKind::NotFound
            ));
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            install_fixed_device(&paths);
            std::fs::write(&paths.device_cert, b"not a canonical certificate").unwrap();
            assert!(matches!(
                certified_error(&paths),
                CertifiedIdentityError::CertificateFile(PersistError::Canonical(_))
            ));
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let device = install_fixed_device(&paths);
            let mut cert = signed_cert(device.verifying_key().to_bytes(), NOW_NS - 1, None);
            cert.device_label.push_str(" (tampered)");
            write_device_cert(&paths.device_cert, &cert).unwrap();
            assert!(matches!(
                certified_error(&paths),
                CertifiedIdentityError::Certificate(DeviceCertError::BadSignature)
            ));
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let device = install_fixed_device(&paths);
            let cert = signed_cert(
                device.verifying_key().to_bytes(),
                NOW_NS - 100,
                Some(NOW_NS),
            );
            write_device_cert(&paths.device_cert, &cert).unwrap();
            assert!(matches!(
                certified_error(&paths),
                CertifiedIdentityError::Certificate(DeviceCertError::Expired)
            ));

            let cert = signed_cert(device.verifying_key().to_bytes(), NOW_NS + 1, None);
            write_device_cert(&paths.device_cert, &cert).unwrap();
            assert!(matches!(
                certified_error(&paths),
                CertifiedIdentityError::Certificate(DeviceCertError::NotYetValid)
            ));
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            install_fixed_device(&paths);
            let other_device = ed25519_dalek::SigningKey::from_bytes(&[99; 32]);
            let cert = signed_cert(other_device.verifying_key().to_bytes(), NOW_NS - 1, None);
            write_device_cert(&paths.device_cert, &cert).unwrap();
            assert!(matches!(
                certified_error(&paths),
                CertifiedIdentityError::DeviceKeyMismatch
            ));
        }
    }

    #[test]
    fn workspace_share_issuers_load_only_for_the_certified_owner() {
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let device = install_fixed_device(&paths);
            let root = ed25519_dalek::SigningKey::from_bytes(&ROOT_SEED);
            let cert = signed_cert(device.verifying_key().to_bytes(), NOW_NS - 1, None);
            write_device_cert(&paths.device_cert, &cert).unwrap();
            let certified = load_certified_mount_identity(&paths, NOW_NS).unwrap();

            assert!(
                load_workspace_share_issuer(&paths, &certified.credentials)
                    .unwrap()
                    .is_none(),
                "a secondary device must not fall back to its device key"
            );
            tilo_user_identity::persist::write_signing_key(&paths.user_privkey, &root).unwrap();
            let issuer = load_workspace_share_issuer(&paths, &certified.credentials)
                .unwrap()
                .unwrap();
            assert_eq!(
                issuer.verifying_key().to_bytes(),
                certified.credentials.user_public_key()
            );

            let unrelated = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
            tilo_user_identity::persist::write_signing_key(&paths.user_privkey, &unrelated)
                .unwrap();
            assert!(matches!(
                load_workspace_share_issuer(&paths, &certified.credentials),
                Err(WorkspaceShareIssuerError::RootKeyMismatch)
            ));
        }
        {
            let tmp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(tmp.path());
            let device = install_fixed_device(&paths);
            let root = ed25519_dalek::SigningKey::from_bytes(&ROOT_SEED);
            let mut cert = signed_cert(device.verifying_key().to_bytes(), NOW_NS - 1, None);
            cert.capabilities.can_own_workspaces = false;
            sign_device_cert(&mut cert, &root).unwrap();
            write_device_cert(&paths.device_cert, &cert).unwrap();
            tilo_user_identity::persist::write_signing_key(&paths.user_privkey, &root).unwrap();
            let certified = load_certified_mount_identity(&paths, NOW_NS).unwrap();
            assert!(
                load_workspace_share_issuer(&paths, &certified.credentials)
                    .unwrap()
                    .is_none()
            );
        }
    }

    fn credential_fixture(root: &std::path::Path) -> PathBuf {
        let directory = root.join("credentials");
        std::fs::create_dir(&directory).unwrap();
        tilo_user_identity::persist::write_device_key(
            &directory.join(DEVICE_KEY_CREDENTIAL),
            &ed25519_dalek::SigningKey::from_bytes(&[53; 32]),
        )
        .unwrap();
        directory
    }

    #[test]
    fn explicit_identity_root_keeps_its_certified_device_with_machine_credential() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("isolated");
        let paths = IdentityPaths::under(&root);
        let device = install_fixed_device(&paths);
        let cert = signed_cert(device.verifying_key().to_bytes(), NOW_NS - 1, None);
        write_device_cert(&paths.device_cert, &cert).unwrap();
        let credential = credential_fixture(tmp.path());
        let before_key = std::fs::read(&paths.device_privkey).unwrap();
        let before_cert = std::fs::read(&paths.device_cert).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(IDENTITY_ROOT_ENV, &root);
        env.set("CREDENTIALS_DIRECTORY", &credential);

        let selected = identity_paths();
        let certified = load_certified_mount_identity(&selected, NOW_NS).unwrap();
        assert_eq!(selected.device_privkey, paths.device_privkey);
        assert_eq!(
            certified.credentials.device_public_key(),
            cert.device_public_key
        );
        assert!(!certified.mount.created);
        assert_eq!(std::fs::read(paths.device_privkey).unwrap(), before_key);
        assert_eq!(std::fs::read(paths.device_cert).unwrap(), before_cert);
    }

    #[test]
    fn explicit_identity_root_never_repairs_a_wrong_certificate_with_machine_credential() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("isolated");
        let paths = IdentityPaths::under(&root);
        install_fixed_device(&paths);
        let credential = credential_fixture(tmp.path());
        let machine = ed25519_dalek::SigningKey::from_bytes(&[53; 32]);
        let cert = signed_cert(machine.verifying_key().to_bytes(), NOW_NS - 1, None);
        write_device_cert(&paths.device_cert, &cert).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(IDENTITY_ROOT_ENV, &root);
        env.set("CREDENTIALS_DIRECTORY", &credential);

        assert!(matches!(
            load_certified_mount_identity(&identity_paths(), NOW_NS),
            Err(CertifiedIdentityError::DeviceKeyMismatch)
        ));
    }

    #[test]
    fn default_identity_root_retains_the_service_manager_credential() {
        let tmp = tempfile::tempdir().unwrap();
        let credential = credential_fixture(tmp.path());
        let default = IdentityPaths::default_system();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("CREDENTIALS_DIRECTORY", &credential);
        for value in [None, Some(""), Some("/var/lib/tilo")] {
            if let Some(value) = value {
                env.set(IDENTITY_ROOT_ENV, value);
            } else {
                env.remove(IDENTITY_ROOT_ENV);
            }
            let selected = identity_paths();
            assert_eq!(selected.device_cert, default.device_cert);
            assert_eq!(
                selected.device_privkey,
                credential.join(DEVICE_KEY_CREDENTIAL)
            );
            let loaded = load_mount_identity(&selected).unwrap();
            assert!(!loaded.created);
            assert_eq!(
                loaded.public,
                ed25519_dalek::SigningKey::from_bytes(&[53; 32])
                    .verifying_key()
                    .to_bytes()
            );
        }
    }

    #[test]
    fn a_unit_reads_the_device_key_its_service_manager_passed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path().join("system"));
        let system_key = paths.device_privkey.clone();
        let credentials = tmp.path().join("credentials");
        std::fs::create_dir(&credentials).unwrap();

        // Not enrolled: the placeholder is no key, so the system path stays.
        std::fs::write(credentials.join(DEVICE_KEY_CREDENTIAL), b"absent").unwrap();
        let unenrolled =
            with_device_key_credential(paths.clone(), Some(credentials.clone().into_os_string()));
        assert_eq!(unenrolled.device_privkey, system_key);

        let passed = credentials.join(DEVICE_KEY_CREDENTIAL);
        std::fs::remove_file(&passed).unwrap();
        tilo_user_identity::persist::write_device_key(
            &passed,
            &ed25519_dalek::SigningKey::from_bytes(&DEVICE_SEED),
        )
        .unwrap();
        let enrolled = with_device_key_credential(paths, Some(credentials.into_os_string()));
        assert_eq!(enrolled.device_privkey, passed);
        let identity = load_mount_identity(&enrolled).unwrap();
        assert!(!identity.created);
        assert_eq!(
            identity.public,
            ed25519_dalek::SigningKey::from_bytes(&DEVICE_SEED)
                .verifying_key()
                .to_bytes()
        );
    }
}
