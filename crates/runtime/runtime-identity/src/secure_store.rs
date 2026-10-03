//! Host-side storage for identity signing material.
//!
//! The preferred backend is the system secret store through `tilo-secrets`:
//! the freedesktop Secret Service over D-Bus on Linux, the login Keychain on
//! macOS. A Linux session without one (a rescue shell, CI, or a desktop
//! without a Secret Service) falls back to a mode-0600 file below the
//! mode-0700 identity directory. That fallback is deliberately named *broker
//! file*: the launcher may read it, sandboxed applications never receive
//! either its path or its bytes. macOS always has the Keychain, so there a
//! failing Keychain fails the operation, and a broker file an earlier build
//! wrote moves into the Keychain the next time its identity is secured.
//!
//! `SecretProvider` is the hardware-backed seam. A TPM/FIDO implementation
//! can implement it and pass it to the `_with_provider` APIs; this crate does
//! not claim hardware protection merely because `/dev/tpm*` exists.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tilo_secrets::{PutOptions, SecretError, SecretStore};
use zeroize::Zeroizing;

/// The largest signing secret either store accepts. Items are hex text, as
/// `secret-tool` stored them, so this is half of what earlier builds could
/// pass through its bounded stdin.
const MAX_IDENTITY_SECRET_BYTES: u64 = 32 * 1024;

use crate::{KeyProtection, SecretReference};

/// The secret-store namespace of identity signing keys.
const SECRETS_APP: &str = "so.tilo.identity";
const PURPOSE_ATTRIBUTE: &str = "tilo-identity-signing-v1";

fn validate_software_locator(locator: &str) -> io::Result<()> {
    if locator.len() == 64 && locator.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid identity secret locator",
        ))
    }
}

/// Pluggable storage boundary for private signing shares.
///
/// Implementations must never log `secret`, must authenticate the retrieved
/// value, and must make `store` idempotent for the same locator/value pair.
pub trait SecretProvider {
    fn protection(&self) -> KeyProtection;
    fn backend_id(&self) -> &'static str;

    /// # Errors
    ///
    /// Returns an error if `secret` can't be stored, or if `locator` is
    /// already in use by a different secret.
    fn store(&self, locator: &str, secret: &[u8]) -> io::Result<()>;

    /// # Errors
    ///
    /// Returns an error if no secret is stored at `locator`, or it can't be
    /// authenticated and retrieved.
    fn load(&self, locator: &str) -> io::Result<Zeroizing<Vec<u8>>>;

    /// # Errors
    ///
    /// Returns an error if the secret at `locator` can't be removed.
    fn remove(&self, locator: &str) -> io::Result<()>;
}

/// The system secret store. Its backend id keeps the name earlier builds
/// recorded in identity files, when this store was reached through
/// `secret-tool`.
#[derive(Clone)]
pub(crate) struct SecretServiceProvider {
    store: SecretStore,
}

impl SecretServiceProvider {
    pub(crate) fn system() -> Self {
        Self::with_store(SecretStore::for_app(SECRETS_APP))
    }

    pub(crate) fn with_store(store: SecretStore) -> Self {
        Self { store }
    }

    /// Whether the store answers on this session.
    #[cfg_attr(test, allow(dead_code, reason = "tests use the broker file provider"))]
    fn available(&self) -> bool {
        self.store.probe_blocking().is_ok()
    }
}

/// The key under which the store holds `locator`'s secret.
fn secret_key(locator: &str) -> String {
    format!("{PURPOSE_ATTRIBUTE}.{}", locator.to_ascii_lowercase())
}

/// The item attributes earlier builds stored `locator`'s secret under,
/// through `secret-tool store tilo-purpose … tilo-identity-key <locator>`.
fn legacy_attributes(locator: &str) -> [(&str, &str); 2] {
    [
        ("tilo-purpose", PURPOSE_ATTRIBUTE),
        ("tilo-identity-key", locator),
    ]
}

fn store_error(error: SecretError) -> io::Error {
    match error {
        SecretError::Unavailable(_) | SecretError::Denied | SecretError::Unsupported(_) => {
            crate::secret_service_unavailable_error()
        }
        SecretError::InvalidKey => {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid identity secret key")
        }
        SecretError::Failed(reason) => io::Error::other(reason),
    }
}

impl SecretProvider for SecretServiceProvider {
    fn protection(&self) -> KeyProtection {
        KeyProtection::SecretService
    }

    fn backend_id(&self) -> &'static str {
        "secret-service-v1"
    }

    fn store(&self, locator: &str, secret: &[u8]) -> io::Result<()> {
        validate_software_locator(locator)?;
        if secret.is_empty() || secret.len() as u64 > MAX_IDENTITY_SECRET_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "identity secret has an invalid length",
            ));
        }
        match self.load(locator) {
            Ok(current) if current.as_slice() == secret => return Ok(()),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a different key already uses this identity locator",
                ));
            }
            Err(error) if crate::is_secret_service_unavailable_io(&error) => {}
            Err(error) => return Err(error),
        }
        // Hex text, as earlier builds stored it: one format for adopted and
        // new items.
        let encoded = Zeroizing::new(hex::encode(secret));
        self.store
            .put_blocking(
                &secret_key(locator),
                encoded.as_bytes(),
                PutOptions {
                    label: Some("Tilo identity signing key".into()),
                },
            )
            .map_err(store_error)?;
        let stored = self.load(locator)?;
        if stored.as_slice() == secret {
            Ok(())
        } else {
            Err(io::Error::other(
                "the secret store did not return the stored identity key",
            ))
        }
    }

    fn load(&self, locator: &str) -> io::Result<Zeroizing<Vec<u8>>> {
        validate_software_locator(locator)?;
        let stored = self
            .store
            .get_or_adopt_blocking(
                &secret_key(locator),
                &legacy_attributes(locator),
                PutOptions {
                    label: Some("Tilo identity signing key".into()),
                },
            )
            .map_err(store_error)?
            .ok_or_else(crate::secret_service_unavailable_error)?;
        // Corruption must surface; never mask it by generating a replacement key.
        let encoded = std::str::from_utf8(stored.expose())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid keyring value"))?;
        let decoded = hex::decode(encoded.trim())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid keyring value"))?;
        if decoded.is_empty() || decoded.len() as u64 > MAX_IDENTITY_SECRET_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity secret has an invalid size",
            ));
        }
        Ok(Zeroizing::new(decoded))
    }

    fn remove(&self, locator: &str) -> io::Result<()> {
        validate_software_locator(locator)?;
        self.store
            .delete_with_legacy_blocking(&secret_key(locator), &legacy_attributes(locator))
            .map_err(store_error)
    }
}

#[derive(Debug, Clone)]
pub struct BrokerFileProvider {
    root: PathBuf,
}

impl BrokerFileProvider {
    /// Construct the compatible host-file fallback below an identity store.
    ///
    /// Host components may use this explicitly in recovery and deterministic
    /// tests. Normal migration should call `secure_identity`, which prefers
    /// Secret Service and exposes the fallback choice in Settings.
    pub fn new(identity_dir: &Path) -> Self {
        Self {
            root: identity_dir.join("secrets"),
        }
    }

    fn path(&self, locator: &str) -> io::Result<PathBuf> {
        validate_software_locator(locator)?;
        Ok(self
            .root
            .join(format!("{}.key", locator.to_ascii_lowercase())))
    }

    fn ensure_root(&self) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let meta = fs::symlink_metadata(&self.root)?;
        if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity secrets directory must be a real directory",
            ));
        }
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity secrets directory is not owned by the current user",
            ));
        }
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
    }
}

impl SecretProvider for BrokerFileProvider {
    fn protection(&self) -> KeyProtection {
        KeyProtection::BrokerFile
    }

    fn backend_id(&self) -> &'static str {
        "broker-file-v1"
    }

    fn store(&self, locator: &str, secret: &[u8]) -> io::Result<()> {
        self.ensure_root()?;
        let path = self.path(locator)?;
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let current = self.load(locator)?;
                if current.as_slice() == secret {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a different key already uses this identity locator",
                ));
            }
            Err(error) => return Err(error),
        };
        file.write_all(secret)?;
        file.sync_all()
    }

    fn load(&self, locator: &str) -> io::Result<Zeroizing<Vec<u8>>> {
        self.ensure_root()?;
        let path = self.path(locator)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.file_type().is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity key fallback must be a private regular file",
            ));
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(MAX_IDENTITY_SECRET_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_IDENTITY_SECRET_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity secret has an invalid size",
            ));
        }
        Ok(bytes)
    }

    fn remove(&self, locator: &str) -> io::Result<()> {
        self.ensure_root()?;
        let path = self.path(locator)?;
        let meta = fs::symlink_metadata(&path)?;
        if !meta.file_type().is_file()
            || meta.file_type().is_symlink()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to remove an untrusted identity key file",
            ));
        }
        fs::remove_file(path)
    }
}

pub(crate) fn provider_for<'a>(
    identity_dir: &'a Path,
    reference: &SecretReference,
) -> io::Result<Box<dyn SecretProvider + 'a>> {
    match reference.backend.as_str() {
        "secret-service-v1" => Ok(Box::new(SecretServiceProvider::system())),
        "broker-file-v1" => Ok(Box::new(BrokerFileProvider::new(identity_dir))),
        "tpm2-sealed-v1" => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this identity is TPM-sealed, but no TPM provider is installed",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown identity secret backend",
        )),
    }
}

/// Whether a session without an answering system store keeps new identity
/// secrets in the broker file. The login Keychain is always there on macOS,
/// so a failing one fails the operation instead.
pub(crate) const FILE_FALLBACK: bool = !cfg!(target_os = "macos");

/// Set (to anything), it keeps every identity secret of the session in
/// private files, out of the user's login Keychain or Secret Service: for
/// tests, and for a locally built release (see [`file_secrets_requested`]).
pub const FILE_SECRETS_ENV: &str = "TILO_IDENTITY_FILE_SECRETS";

/// Whether this session keeps its identity secrets in files.
///
/// A debug build does when told to ([`FILE_SECRETS_ENV`], for tests) or when
/// the system store would not keep them for its next build (on macOS, a
/// build without a Developer ID signature, whose Keychain items belong to
/// that exact build). A release build does only when told to *and* its store
/// would not keep them, so a locally built bundle can start after a rebuild:
/// a Developer ID release never keeps secrets in files, and no release does
/// unasked.
#[must_use]
pub fn file_secrets_requested() -> bool {
    let told = std::env::var_os(FILE_SECRETS_ENV).is_some();
    let kept = tilo_secrets::items_outlive_rebuilds();
    if cfg!(debug_assertions) {
        told || !kept
    } else {
        told && !kept
    }
}

pub(crate) fn preferred_provider(identity_dir: &Path) -> Box<dyn SecretProvider + '_> {
    #[cfg(test)]
    {
        Box::new(BrokerFileProvider::new(identity_dir))
    }
    #[cfg(not(test))]
    {
        if file_secrets_requested() {
            return Box::new(BrokerFileProvider::new(identity_dir));
        }
        // An answering store can still refuse (a locked collection whose
        // prompt is dismissed): callers try the provider and fall back only on
        // a real store failure.
        let system = SecretServiceProvider::system();
        if !FILE_FALLBACK || system.available() {
            Box::new(system)
        } else {
            Box::new(BrokerFileProvider::new(identity_dir))
        }
    }
}

pub(crate) fn fallback_provider(identity_dir: &Path) -> BrokerFileProvider {
    BrokerFileProvider::new(identity_dir)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    /// A dev build (cargo's test binaries included) has only the linker's
    /// ad-hoc signature, so its Keychain items would not outlive it.
    #[test]
    fn a_debug_build_without_developer_id_keeps_identity_secrets_in_files() {
        assert!(!tilo_secrets::items_outlive_rebuilds());
        assert!(super::file_secrets_requested());
    }
}
