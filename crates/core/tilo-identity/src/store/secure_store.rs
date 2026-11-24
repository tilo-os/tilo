//! Host-side storage for identity signing material.
//!
//! The preferred backend is the system secret store through `tilo-secrets`:
//! the freedesktop Secret Service over D-Bus on Linux, the login Keychain on
//! macOS. A Linux session without one (a rescue shell, CI, or a desktop
//! without a Secret Service) falls back to a mode-0600 file below the
//! mode-0700 identity directory. That fallback is deliberately named *broker
//! file*: the launcher may read it, sandboxed applications never receive
//! either its path or its bytes. macOS always has the Keychain, so there a
//! failing Keychain fails the operation.
//!
//! `SecretProvider` is the hardware-backed boundary. A TPM/FIDO implementation
//! can implement it and pass it to the `_with_provider` APIs; this crate does
//! not claim hardware protection merely because `/dev/tpm*` exists.
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use tilo_secrets::{PutOptions, SecretError, SecretStore};
use zeroize::Zeroizing;

/// The largest signing secret either store accepts. Items are hex text, as
/// `secret-tool` stored them, so this is half of what earlier builds could
/// pass through its bounded stdin.
const MAX_IDENTITY_SECRET_BYTES: u64 = 32 * 1024;

use super::SecretReference;
use tilo_identity_client::KeyProtection;

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

    #[cfg(not(test))]
    fn available(&self) -> bool {
        self.store.probe_blocking().is_ok()
    }
}

fn secret_key(locator: &str) -> String {
    format!("{PURPOSE_ATTRIBUTE}.{}", locator.to_ascii_lowercase())
}

fn store_error(error: SecretError) -> io::Error {
    match error {
        SecretError::Unavailable(_) | SecretError::Denied | SecretError::Unsupported(_) => {
            super::secret_service_unavailable_error()
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
            Err(error) if super::is_secret_service_unavailable_io(&error) => {}
            Err(error) => return Err(error),
        }
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
            .get_blocking(&secret_key(locator))
            .map_err(store_error)?
            .ok_or_else(super::secret_service_unavailable_error)?;
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
            .delete_blocking(&secret_key(locator))
            .map_err(store_error)
    }
}

#[derive(Debug, Clone)]
pub struct BrokerFileProvider {
    root: PathBuf,
}

impl BrokerFileProvider {
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
        self.check_root()?;
        tilo_files::native::private::restrict_directory(&self.root)
    }

    fn check_root(&self) -> io::Result<()> {
        let meta = fs::symlink_metadata(&self.root)?;
        if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity secrets directory must be a real directory",
            ));
        }
        if !tilo_files::native::private::owned_by_me(&meta) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity secrets directory is not owned by the current user",
            ));
        }
        Ok(())
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
        let mut file = match tilo_files::native::private::create_new(&path) {
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
        self.check_root()?;
        let path = self.path(locator)?;
        let file = tilo_files::native::private::read(&path)?;
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
            || !tilo_files::native::private::private_to_me(&meta)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to remove an untrusted identity key file",
            ));
        }
        fs::remove_file(path)
    }
}

/// The backend this computer keeps each account's root in: its private share sealed at rest with
/// a key of that account's login, or of a secret of the account's own where it has no login, and
/// of this computer's device key.
pub const SEALED_FILE_BACKEND: &str = "sealed-file-v1";

/// A private identity share, sealed at rest: the daemon opens it only while that account is
/// signed in, and a copy of the identity directory alone opens nothing.
#[derive(Clone)]
pub struct SealedFileProvider {
    files: BrokerFileProvider,
    key: tilo_seal::SymmetricKey,
}

impl SealedFileProvider {
    /// The provider for the identity directory `identity_dir`, which names the account it
    /// belongs to (`…/accounts/<uid>`).
    ///
    /// # Errors
    /// The directory names no account, or its key cannot be read or made.
    pub fn for_identity_dir(identity_dir: &Path) -> io::Result<Self> {
        Ok(Self {
            files: BrokerFileProvider::new(identity_dir),
            key: account_seal_key(identity_dir)?,
        })
    }

    /// The context a share is sealed under: its locator, so one share never opens as another.
    fn context<'a>(locator: &'a str) -> tilo_seal::Context<'a> {
        tilo_seal::Context::new(tilo_seal::Label::FileSecretStore, locator.as_bytes())
    }
}

impl SecretProvider for SealedFileProvider {
    fn protection(&self) -> KeyProtection {
        KeyProtection::BrokerFile
    }

    fn backend_id(&self) -> &'static str {
        SEALED_FILE_BACKEND
    }

    fn store(&self, locator: &str, secret: &[u8]) -> io::Result<()> {
        // Sealing is randomized, so the same share is sealed differently each time: answer the
        // same share this locator already keeps before writing another.
        match self.load(locator) {
            Ok(current) if current.as_slice() == secret => return Ok(()),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a different key already uses this identity locator",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let sealed = tilo_seal::envelope::seal(&self.key, Self::context(locator), secret)
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.files.store(locator, &sealed)
    }

    fn load(&self, locator: &str) -> io::Result<Zeroizing<Vec<u8>>> {
        let sealed = self.files.load(locator)?;
        let opened = tilo_seal::envelope::open(&self.key, Self::context(locator), &sealed)
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("this account's identity share does not open: {error}"),
                )
            })?;
        Ok(opened)
    }

    fn remove(&self, locator: &str) -> io::Result<()> {
        self.files.remove(locator)
    }
}

/// The account an identity directory belongs to, when it is kept apart for one: the uid under
/// `accounts/`.
#[must_use]
pub fn account_dir_uid(identity_dir: &Path) -> Option<u32> {
    if identity_dir.parent()?.file_name()? != "accounts" {
        return None;
    }
    identity_dir.file_name()?.to_str()?.parse().ok()
}

/// The key the account `identity_dir` belongs to seals its shares with: its login name where
/// the computer has one for the account, else a secret made for the account alone, bound to this
/// computer's device key.
fn account_seal_key(identity_dir: &Path) -> io::Result<tilo_seal::SymmetricKey> {
    let uid = account_dir_uid(identity_dir).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is no account's identity directory",
                identity_dir.display()
            ),
        )
    })?;
    let mut key = blake3::Hasher::new();
    key.update(b"tilo account identity seal v1\0");
    key.update(&uid.to_le_bytes());
    match crate::provisioning::account_login_name(uid) {
        Some(login) => {
            key.update(b"login\0");
            key.update(login.as_bytes());
        }
        None => {
            key.update(b"account secret\0");
            key.update(&account_own_secret(identity_dir)?);
        }
    }
    let device = fs::read(crate::provisioning::collaboration_identity_paths().device_privkey)
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("this computer's device key cannot be read: {error}"),
            )
        })?;
    key.update(b"device\0");
    key.update(&device);
    tilo_seal::SymmetricKey::from_slice(key.finalize().as_bytes())
        .map_err(|error| io::Error::other(error.to_string()))
}

/// A secret made for the account `identity_dir` belongs to, kept private in its directory: what
/// an account the computer has no login for is sealed with.
fn account_own_secret(identity_dir: &Path) -> io::Result<[u8; 32]> {
    let path = identity_dir.join("seal.key");
    match fs::read(&path) {
        Ok(bytes) => <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "this account's sealing secret is damaged",
            )
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut secret = [0_u8; 32];
            getrandom::fill(&mut secret).map_err(|error| io::Error::other(error.to_string()))?;
            let mut file = tilo_files::native::private::create_new(&path)?;
            file.write_all(&secret)?;
            file.sync_all()?;
            Ok(secret)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn provider_for<'a>(
    identity_dir: &'a Path,
    reference: &SecretReference,
) -> io::Result<Box<dyn SecretProvider + 'a>> {
    match reference.backend.as_str() {
        "secret-service-v1" => Ok(Box::new(SecretServiceProvider::system())),
        "broker-file-v1" => Ok(Box::new(BrokerFileProvider::new(identity_dir))),
        SEALED_FILE_BACKEND => Ok(Box::new(SealedFileProvider::for_identity_dir(
            identity_dir,
        )?)),
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
/// and the Credential Manager on Windows, so a failing one fails the
/// operation instead.
pub(crate) const FILE_FALLBACK: bool = !cfg!(any(target_os = "macos", windows));
/// Set to anything, it keeps every identity secret of the session in private
/// files instead of the login Keychain or Secret Service: for tests, and for a
/// locally built release.
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

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    /// A dev build (cargo's test binaries included) has only the linker's
    /// ad-hoc signature, so its Keychain items would not outlive it.
    #[test]
    fn a_debug_build_without_developer_id_keeps_identity_secrets_in_files() {
        assert!(!tilo_secrets::items_outlive_rebuilds());
        assert!(super::file_secrets_requested());
    }
}
