//! `runtime-identity` — user-identity loader for Tilo apps.
//!
//! The launcher hands applications a connected signing-broker descriptor via
//! `TILO_IDENTITY_BROKER_FD`. Apps can inspect public metadata and request a
//! signature, but private shares stay in the host session. `TILO_USER_IDENTITY`
//! remains a compatibility path for non-sandboxed development only.
//!
//! The on-disk file is the JSON emitted by
//! `tilo_identity_ceremony::IdentityRecord::to_json_bytes`. This crate keeps
//! a minimal local copy of the schema (`ondisk::IdentityRecord`) rather than
//! depending on the ceremony crate. Both sides
//! bump the version tag together on a breaking schema change.
//!
//! In 1-of-1 (single-device) mode this crate signs locally via
//! ed25519-dalek; the loader refuses (not just warns about) a record whose
//! `secret_bytes` does not derive its own recorded `group_public_key`. In
//! threshold mode (`t > 1`) signing needs a multi-device FROST round-trip
//! this SDK crate does not drive — [`SigningIdentity::sign`] returns
//! [`NotAvailable::ConnectionFailed`] and apps needing it should reach for
//! `threshold-identity` directly.
//!
//! ```ignore
//! let identity = runtime_identity::load()?;
//! let sig = identity.sign_for("document.save", b"hello")?;
//! let pk = identity.signing_public_key();
//! let cert = identity.device_certificate();
//! ```
//!
//! Missing env var / missing file / bad JSON all surface as
//! [`NotAvailable`] — never a panic.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::fd::BorrowedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use threshold_identity::{
    ThresholdIdentity,
    signing::{SigningRound1, SigningRound1Commitment, SigningRound2Partial, SigningSessionNonces},
};
use zeroize::Zeroizing;

#[path = "backup.rs"]
mod backup;
#[path = "broker.rs"]
pub mod broker;
#[path = "secure_store.rs"]
mod secure_store;
#[cfg(test)]
#[path = "service_connection_tests.rs"]
mod service_connection_tests;

pub use backup::{
    PortableBackupInfo, export_portable_backup, import_portable_backup,
    import_portable_backup_matching, import_portable_backup_with_provider, verify_portable_backup,
};
pub use secure_store::{
    BrokerFileProvider, FILE_SECRETS_ENV, SecretProvider, file_secrets_requested,
};

use crate::{
    BrokerSigningIdentity, IDENTITY_SERVICE, NotAvailable, collaboration_join_signing_bytes,
};
#[cfg(test)]
use crate::{
    client::{read_frame, write_frame},
    wire,
};

/// True when a secured identity's Secret Service item could not be obtained.
/// The storage provider marks this condition with `ENOKEY`; keeping the
/// classification typed avoids making startup policy depend on a store's
/// localized diagnostics.
pub fn is_secret_service_unavailable(error: &NotAvailable) -> bool {
    matches!(
        error,
        NotAvailable::ConnectionFailed(io_error)
            if is_secret_service_unavailable_io(io_error)
    )
}

/// Equivalent classifier for callers that add context while preserving the
/// original provider error as the source of an `io::Error`.
pub fn is_secret_service_unavailable_io(error: &io::Error) -> bool {
    #[cfg(target_os = "linux")]
    {
        error.raw_os_error() == Some(libc::ENOKEY)
    }
    #[cfg(not(target_os = "linux"))]
    {
        error
            .get_ref()
            .is_some_and(<dyn std::error::Error + Send + Sync>::is::<SecretServiceUnavailable>)
    }
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
struct SecretServiceUnavailable;
#[cfg(not(target_os = "linux"))]
impl std::fmt::Display for SecretServiceUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("identity Secret Service item is unavailable")
    }
}
#[cfg(not(target_os = "linux"))]
impl std::error::Error for SecretServiceUnavailable {}

/// Construct the portable marker for an unavailable Secret Service identity item.
pub fn secret_service_unavailable_error() -> io::Error {
    #[cfg(target_os = "linux")]
    {
        io::Error::from_raw_os_error(libc::ENOKEY)
    }
    #[cfg(not(target_os = "linux"))]
    {
        io::Error::other(SecretServiceUnavailable)
    }
}

/// Legacy unsandboxed-development path. The Tilo launcher uses
/// [`broker::BROKER_ENV_VAR`] and never sets this for packaged apps.
pub const IDENTITY_ENV_VAR: &str = "TILO_USER_IDENTITY";
/// How a record's private share is protected on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyProtection {
    /// Stored in the desktop's freedesktop Secret Service collection.
    SecretService,
    /// Stored as a mode-0600 host file, reachable only by the signing broker.
    BrokerFile,
    /// An externally supplied provider has sealed this record.
    HardwareBacked,
    /// Legacy schema; migration has not completed yet.
    LegacyPlaintext,
    /// No share of the identity's key is on this device: a device joined to
    /// the identity acts through its root-signed device certificate.
    DeviceCertificate,
    /// Metadata names a backend this session cannot safely resolve.
    Unavailable,
}

impl KeyProtection {
    pub fn label(self) -> &'static str {
        match self {
            Self::SecretService => "Secret Service + app-scoped signing broker",
            Self::BrokerFile => "Private session file + app-scoped signing broker",
            Self::HardwareBacked => "Hardware-backed + app-scoped signing broker",
            Self::LegacyPlaintext => "Migration required",
            Self::DeviceCertificate => {
                "No identity key here; this device signs with its certificate"
            }
            Self::Unavailable => "Protection provider unavailable",
        }
    }
}

/// Opaque locator stored in public identity metadata. It never contains key
/// bytes or a host filesystem path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretReference {
    pub backend: String,
    pub locator: String,
}

/// Non-secret information Settings may show for an identity record. The
/// private signing share is absent and `Debug` is safe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentitySummary {
    pub id: String,
    pub display_name: String,
    pub public_key: [u8; 32],
    pub threshold: u16,
    pub total_shares: u16,
    pub active: bool,
    pub protection: KeyProtection,
}

/// Public fields from an identity record. Reading these never unlocks or
/// materializes the account signing share; app-facing brokers use the
/// certified device key for signatures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityMetadata {
    pub display_name: String,
    pub avatar_color: [u8; 3],
    pub group_public_key: [u8; 32],
    pub threshold: u16,
    pub total_shares: u16,
    pub share_index: u16,
}

const ACTIVE_FILE: &str = "active";
const LOCK_FILE: &str = ".identity.lock";
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

/// Publish privately through the framework's retained parent and preserve its
/// failure phase. Lifecycle callers admit the identity namespace and hold its
/// lock; provisioning that parent durably is a separate responsibility.
fn write_private_atomic(path: &std::path::Path, bytes: &[u8]) -> io::Result<()> {
    tilo_files::native::atomic::replace(path, bytes, tilo_files::native::atomic::Privacy::OwnerOnly)
        .map_err(tilo_files::native::atomic::PublicationError::into_io_error)
}

/// Resolve the per-login-user identity directory using XDG rules.
///
/// # Errors
///
/// Returns an error if neither `XDG_CONFIG_HOME` nor `HOME` is set to an
/// absolute path.
pub fn identity_dir_from_env() -> io::Result<PathBuf> {
    if let Some(config) = tilo_paths::xdg::config_home().filter(|dir| dir.is_absolute()) {
        return Ok(config.join("tilo").join("identity"));
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "neither an absolute XDG_CONFIG_HOME nor an absolute HOME is set",
    ))
}

fn valid_identity_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit() && i > 0
                || matches!(b, b'_' | b'-') && i > 0
        })
}

fn identity_path(dir: &std::path::Path, id: &str) -> io::Result<PathBuf> {
    if !valid_identity_id(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity id must start with a lowercase letter and contain only lowercase letters, numbers, '-' or '_'",
        ));
    }
    Ok(dir.join(format!("{id}.json")))
}

fn ensure_private_dir(dir: &std::path::Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "identity root must be a real directory",
        ));
    }
    #[cfg(unix)]
    {
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity root is not owned by the current user",
            ));
        }
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Advisory process lock covering all identity lifecycle transitions. It
/// closes the activate/remove and concurrent-import TOCTOU windows while the
/// atomic writer gives each individual metadata update crash safety.
struct IdentityLock {
    _lock: advisory_lock::FileLock,
}

impl IdentityLock {
    fn acquire(dir: &std::path::Path) -> io::Result<Self> {
        ensure_private_dir(dir)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir.join(LOCK_FILE))?;
        let meta = file.metadata()?;
        if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity lifecycle lock must be a private user-owned regular file",
            ));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity lifecycle lock must be private",
            ));
        }
        Ok(Self {
            _lock: advisory_lock::FileLock::exclusive(file)?,
        })
    }
}

/// Active record chosen for newly launched apps.  The selector contains only
/// an id, never key material.  A malformed selector fails closed to the
/// backwards-compatible `primary.json`, and a selected symlink is refused.
pub fn resolve_active_identity(dir: &std::path::Path) -> Option<PathBuf> {
    let id = read_private_selector(&dir.join(ACTIVE_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| valid_identity_id(s))
        .unwrap_or_else(|| "primary".to_string());
    let path = identity_path(dir, &id).ok()?;
    let meta = fs::symlink_metadata(&path).ok()?;
    (meta.file_type().is_file() && !meta.file_type().is_symlink()).then_some(path)
}

fn read_private_selector(path: &std::path::Path) -> io::Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
        || meta.len() > 128
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "active identity selector is not a private regular file",
        ));
    }
    let mut value = String::new();
    file.take(129).read_to_string(&mut value)?;
    Ok(value)
}

/// Inspect all valid records without exposing their signing shares.
///
/// Listing never writes: it takes no lock and creates or re-modes nothing.
/// Every writer replaces a record by rename, so each record is read whole. A
/// store that does not exist yet lists nothing.
///
/// # Errors
///
/// Returns an error if `dir` is not a real directory owned by this user, or
/// can't be read.
pub fn list_identities(dir: &std::path::Path) -> io::Result<Vec<IdentitySummary>> {
    let meta = match fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    if !meta.file_type().is_dir() || meta.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "identity root must be a real directory owned by the current user",
        ));
    }
    list_identities_unlocked(dir)
}

fn list_identities_unlocked(dir: &std::path::Path) -> io::Result<Vec<IdentitySummary>> {
    let active = resolve_active_identity(dir);
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if !meta.file_type().is_file() || meta.file_type().is_symlink() {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|id| valid_identity_id(id))
        else {
            continue;
        };
        let Ok(record) = read_record(&path) else {
            continue;
        };
        let protection = record.protection();
        out.push(IdentitySummary {
            id: id.to_string(),
            display_name: record.display_name,
            public_key: record.threshold_identity.group_public_key,
            threshold: record.threshold_identity.threshold,
            total_shares: record.threshold_identity.total_shares,
            active: active.as_deref() == Some(path.as_path()),
            protection,
        });
    }
    out.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

/// Import a complete, already-provisioned identity record.  The record is
/// cryptographically self-checked before its first byte reaches the final
/// path; creation is exclusive and mode 0600.
///
/// # Errors
///
/// Returns an error if `bytes` fails to parse as a valid identity record,
/// the lifecycle lock can't be acquired, an identity with this id or public
/// key already exists, or the record can't be written and secured.
pub fn import_identity(dir: &std::path::Path, id: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let imported = load_from_bytes(bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("identity record rejected: {e}"),
        )
    })?;
    let _lock = IdentityLock::acquire(dir)?;
    import_identity_unlocked(dir, id, bytes, &imported, None)
}

/// Import through an explicitly selected secret provider. Host provisioning
/// and recovery code can use this to install a hardware-backed provider; the
/// provider's reported protection is persisted only after the record passes
/// the same cryptographic and filesystem checks as a normal import.
///
/// # Errors
///
/// Returns an error if `bytes` fails to parse as a valid identity record,
/// the lifecycle lock can't be acquired, an identity with this id or public
/// key already exists, or the record can't be written and secured.
pub fn import_identity_with_provider(
    dir: &std::path::Path,
    id: &str,
    bytes: &[u8],
    provider: &dyn SecretProvider,
) -> io::Result<PathBuf> {
    let imported = load_from_bytes(bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("identity record rejected: {e}"),
        )
    })?;
    let _lock = IdentityLock::acquire(dir)?;
    import_identity_unlocked(dir, id, bytes, &imported, Some(provider))
}

fn import_identity_unlocked(
    dir: &std::path::Path,
    id: &str,
    bytes: &[u8],
    imported: &SigningIdentity,
    provider: Option<&dyn SecretProvider>,
) -> io::Result<PathBuf> {
    let path = identity_path(dir, id)?;
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "an identity with this id already exists",
        ));
    }
    if list_identities_unlocked(dir)?
        .iter()
        .any(|existing| existing.public_key == imported.group_public_key())
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "this signing identity is already present under another name",
        ));
    }
    // Stage the legacy bytes only after validation, then immediately migrate
    // under the same lock. A crash leaves a valid v1 record that sign-in or
    // Settings secures (a launch refuses it); it never leaves half a v2
    // record.
    write_private_atomic(&path, bytes)?;
    let secured = match provider {
        Some(provider) => secure_record_with_provider_unlocked(dir, &path, provider),
        None => secure_record_unlocked(dir, &path),
    };
    if let Err(error) = secured {
        // Both securing paths preserve every metadata-writer failure phase,
        // including across preferred-provider fallback. Their unclassified
        // errors arise only before metadata publication (read, validation,
        // provider access or serialization), so the staged v1 may be removed.
        if matches!(
            tilo_files::native::atomic::publication_state(&error),
            None | Some(tilo_files::native::atomic::PublicationState::NotPublished)
        ) {
            let _ = fs::remove_file(&path);
        }
        return Err(error);
    }
    Ok(path)
}

/// Move one record's inline key into its secret provider. Idempotent. Only
/// provisioning and Settings secure records; an app launch reads them through
/// [`load_for_launch`] and never writes the store.
///
/// # Errors
///
/// Returns an error if the lifecycle lock can't be acquired, the record
/// can't be read, or no secret provider can secure it.
pub fn secure_identity(dir: &std::path::Path, id: &str) -> io::Result<KeyProtection> {
    let _lock = IdentityLock::acquire(dir)?;
    let path = identity_path(dir, id)?;
    secure_record_unlocked(dir, &path)
}

/// Migrate using an explicitly supplied provider, for a hardware-backed
/// integration. The provider's `protection()` is what Settings reports; merely
/// detecting a TPM device never changes the label.
///
/// # Errors
///
/// Returns an error if the lifecycle lock can't be acquired, the record
/// can't be read, or the provider fails to secure it.
pub fn secure_identity_with_provider(
    dir: &std::path::Path,
    id: &str,
    provider: &dyn SecretProvider,
) -> io::Result<KeyProtection> {
    let _lock = IdentityLock::acquire(dir)?;
    let path = identity_path(dir, id)?;
    secure_record_with_provider_unlocked(dir, &path, provider)
}

fn secure_record_unlocked(
    dir: &std::path::Path,
    path: &std::path::Path,
) -> io::Result<KeyProtection> {
    let record = read_record(path)?;
    if is_public_member_projection(&record) {
        return Ok(KeyProtection::DeviceCertificate);
    }
    #[cfg(not(test))]
    let preferred = secure_store::preferred_provider(dir);
    #[cfg(test)]
    let preferred = tests::preferred_provider(dir);
    if record.schema_version == 2 {
        let reference = record.secret_ref.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "secured identity has no secret reference",
            )
        })?;
        let provider = secure_store::provider_for(dir, reference)?;
        let secret = provider.load(&reference.locator)?;
        identity_from_record(record, Some(secret.as_slice()))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        return Ok(provider.protection());
    }

    match secure_record_with_provider_unlocked(dir, path, preferred.as_ref()) {
        Ok(protection) => Ok(protection),
        Err(preferred_error)
            if secure_store::FILE_FALLBACK
                && preferred.protection() == KeyProtection::SecretService
                && matches!(
                    tilo_files::native::atomic::publication_state(&preferred_error),
                    None | Some(tilo_files::native::atomic::PublicationState::NotPublished)
                ) =>
        {
            // Errors without a publication phase occur before this attempt's
            // metadata write. Once it may have published, preserve its error:
            // a fallback could hide that phase or replace a referenced key.
            tracing_warning_for_secret_service(&preferred_error);
            let fallback = secure_store::fallback_provider(dir);
            secure_record_with_provider_unlocked(dir, path, &fallback)
        }
        Err(error) => Err(error),
    }
}

fn tracing_warning_for_secret_service(error: &io::Error) {
    // This crate deliberately has no tracing dependency. Keep the fallback
    // observable to Settings via KeyProtection rather than writing a possibly
    // sensitive backend error to stderr from every sign-in.
    let _ = error;
}

fn secure_record_with_provider_unlocked(
    _dir: &std::path::Path,
    path: &std::path::Path,
    provider: &dyn SecretProvider,
) -> io::Result<KeyProtection> {
    let mut record = read_record(path)?;
    // A joined device's public projection has no secret to move into a
    // provider; giving it an empty one would leave a reference that never
    // loads.
    if is_public_member_projection(&record) {
        return Ok(KeyProtection::DeviceCertificate);
    }
    if record.schema_version == 2 {
        let reference = record.secret_ref.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "secured identity has no secret reference",
            )
        })?;
        if reference.backend != provider.backend_id() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "secured identity uses a different secret provider",
            ));
        }
        let secret = provider.load(&reference.locator)?;
        identity_from_record(record, Some(secret.as_slice()))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        return Ok(provider.protection());
    }

    // Validate self-consistency before storing a byte outside the legacy
    // record. The key buffer is zeroized after the provider consumes it.
    identity_from_record(
        ondisk::IdentityRecord {
            display_name: record.display_name.clone(),
            avatar_color: record.avatar_color,
            threshold_identity: ondisk::SerializableIdentity {
                single_device: record.threshold_identity.single_device,
                group_public_key: record.threshold_identity.group_public_key,
                threshold: record.threshold_identity.threshold,
                total_shares: record.threshold_identity.total_shares,
                share_index: record.threshold_identity.share_index,
                secret_bytes: record.threshold_identity.secret_bytes.clone(),
            },
            created_at: record.created_at.clone(),
            schema_version: record.schema_version,
            secret_ref: None,
        },
        None,
    )
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let secret = Zeroizing::new(std::mem::take(&mut record.threshold_identity.secret_bytes));
    let locator = hex::encode(record.threshold_identity.group_public_key);
    provider.store(&locator, secret.as_slice())?;

    record.schema_version = 2;
    record.secret_ref = Some(SecretReference {
        backend: provider.backend_id().to_string(),
        locator,
    });
    let metadata = serde_json::to_vec_pretty(&record)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if let Err(error) = write_private_atomic(path, &metadata) {
        if tilo_files::native::atomic::publication_state(&error)
            == Some(tilo_files::native::atomic::PublicationState::NotPublished)
            && let Some(reference) = record.secret_ref.as_ref()
        {
            let _ = provider.remove(&reference.locator);
        }
        return Err(error);
    }
    Ok(provider.protection())
}

/// Select which identity the launcher brokers into newly started apps.
/// Running apps retain the old capability until relaunched, avoiding a
/// mid-operation author change.
///
/// # Errors
///
/// Returns an error if the lifecycle lock can't be acquired, `id` doesn't
/// name a regular-file identity record, the record can't be secured or
/// re-validated, or the active-identity selector can't be written.
pub fn set_active_identity(dir: &std::path::Path, id: &str) -> io::Result<()> {
    let _lock = IdentityLock::acquire(dir)?;
    let path = identity_path(dir, id)?;
    let meta = fs::symlink_metadata(&path)?;
    if !meta.file_type().is_file() || meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity record must be a regular file",
        ));
    }
    secure_record_unlocked(dir, &path)?;
    load_from_path(path).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("identity record rejected: {e}"),
        )
    })?;
    write_private_atomic(&dir.join(ACTIVE_FILE), format!("{id}\n").as_bytes())
}

/// Remove a non-active identity.  The active identity and final remaining
/// identity are protected; callers must first select another identity and
/// confirm removal in their UI.
///
/// # Errors
///
/// Returns an error if the lifecycle lock can't be acquired, `id` is the
/// active or the only remaining identity, `id` doesn't name a regular-file
/// identity record, or its secret can't be removed from its provider.
pub fn remove_identity(dir: &std::path::Path, id: &str) -> io::Result<()> {
    let _lock = IdentityLock::acquire(dir)?;
    let path = identity_path(dir, id)?;
    if resolve_active_identity(dir).as_deref() == Some(path.as_path()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "select another identity before removing the active one",
        ));
    }
    if list_identities_unlocked(dir)?.len() <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the final identity cannot be removed",
        ));
    }
    let meta = fs::symlink_metadata(&path)?;
    if !meta.file_type().is_file() || meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity record must be a regular file",
        ));
    }
    let record = read_record(&path)?;
    if let Some(reference) = record.secret_ref.as_ref() {
        // Existing stores may contain duplicate imported copies from before
        // duplicate-key rejection. Do not clear their shared provider entry
        // until the last metadata record referencing it is removed.
        let shared = list_identities_unlocked(dir)?.iter().any(|other| {
            if other.id == id {
                return false;
            }
            identity_path(dir, &other.id)
                .ok()
                .and_then(|other_path| read_record(&other_path).ok())
                .and_then(|other_record| other_record.secret_ref)
                .is_some_and(|other_ref| other_ref == *reference)
        });
        if !shared {
            let provider = secure_store::provider_for(dir, reference)?;
            provider.remove(&reference.locator)?;
        }
    }
    // Delete metadata only after the secret was removed. A crash before this
    // point leaves a visible record that reports its unavailable secret; a
    // crash after it cannot resurrect an exportable key.
    fs::remove_file(path)
}

/// Wire-format types the loader deserialises from disk, mirroring
/// `tilo_identity_ceremony::IdentityRecord`; unknown fields are ignored.
mod ondisk {
    use std::fmt;

    use serde::de::{SeqAccess, Visitor};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use zeroize::Zeroizing;

    use crate::{KeyProtection, SecretReference};

    #[derive(Debug, Deserialize, Serialize)]
    pub struct IdentityRecord {
        pub display_name: String,
        pub avatar_color: [u8; 3],
        pub threshold_identity: SerializableIdentity,
        #[serde(default)]
        pub created_at: String,
        pub schema_version: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub secret_ref: Option<SecretReference>,
    }

    impl IdentityRecord {
        pub fn protection(&self) -> KeyProtection {
            if super::is_public_member_projection(self) {
                return KeyProtection::DeviceCertificate;
            }
            match self
                .secret_ref
                .as_ref()
                .map(|reference| reference.backend.as_str())
            {
                Some("secret-service-v1") => KeyProtection::SecretService,
                Some("broker-file-v1") => KeyProtection::BrokerFile,
                None if self.schema_version == 1 => KeyProtection::LegacyPlaintext,
                // Includes "tpm2-sealed-v1": the default session has no
                // installed TPM provider, and a string in editable metadata
                // is not evidence of hardware sealing.
                _ => KeyProtection::Unavailable,
            }
        }
    }

    #[derive(Debug, Deserialize, Serialize)]
    pub struct SerializableIdentity {
        pub single_device: bool,
        pub group_public_key: [u8; 32],
        pub threshold: u16,
        pub total_shares: u16,
        pub share_index: u16,
        #[serde(
            default,
            with = "zeroizing_bytes_codec",
            skip_serializing_if = "zeroizing_bytes_codec::is_empty"
        )]
        pub secret_bytes: Zeroizing<Vec<u8>>,
    }

    /// `serde_bytes` preserves the compact on-disk representation, but its
    /// normal deserializer returns a plain `Vec`. Root signing shares parsed
    /// from a decrypted portable record must remain zeroizing even on every
    /// error path after deserialization.
    mod zeroizing_bytes_codec {
        use super::{Deserializer, SeqAccess, Serializer, Visitor, Zeroizing, fmt};

        pub fn is_empty(bytes: &Zeroizing<Vec<u8>>) -> bool {
            bytes.is_empty()
        }

        pub fn serialize<S>(bytes: &Zeroizing<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            serde_bytes::serialize(bytes.as_slice(), serializer)
        }

        pub fn deserialize<'de, D>(deserializer: D) -> Result<Zeroizing<Vec<u8>>, D::Error>
        where
            D: Deserializer<'de>,
        {
            struct ZeroizingBytesVisitor;

            impl<'de> Visitor<'de> for ZeroizingBytesVisitor {
                type Value = Zeroizing<Vec<u8>>;

                fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str("a byte sequence")
                }

                fn visit_bytes<E>(self, bytes: &[u8]) -> Result<Self::Value, E>
                where
                    E: serde::de::Error,
                {
                    Ok(Zeroizing::new(bytes.to_vec()))
                }

                fn visit_byte_buf<E>(self, bytes: Vec<u8>) -> Result<Self::Value, E>
                where
                    E: serde::de::Error,
                {
                    Ok(Zeroizing::new(bytes))
                }

                fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
                where
                    A: SeqAccess<'de>,
                {
                    let mut bytes = Zeroizing::new(Vec::new());
                    while let Some(byte) = sequence.next_element()? {
                        bytes.push(byte);
                    }
                    Ok(bytes)
                }
            }

            deserializer.deserialize_byte_buf(ZeroizingBytesVisitor)
        }
    }
}

/// Reason the identity record failed to load; bundled into
/// `NotAvailable::ConnectionFailed` so callers match one error type.
#[derive(Debug)]
enum LoadFailure {
    ReadFailed(io::Error),
    JsonMalformed(serde_json::Error),
    UnknownSchemaVersion(u32),
    SecuredRecordNeedsProvider,
    /// A store record still carries its signing key inline: it was never
    /// secured by provisioning.
    KeyNotSecured,
    SecretSizeMismatch {
        got: usize,
    },
    /// Boxed: every other variant here is pointer-sized or smaller, and two
    /// inline 32-byte keys would make every `LoadFailure` pay for the
    /// biggest one.
    PublicKeyMismatch {
        derived: Box<[u8; 32]>,
        recorded: Box<[u8; 32]>,
    },
}

impl std::fmt::Display for LoadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadFailed(e) => write!(f, "reading identity file failed: {e}"),
            Self::JsonMalformed(e) => write!(f, "identity JSON malformed: {e}"),
            Self::UnknownSchemaVersion(v) => write!(f, "unknown identity schema version: {v}"),
            Self::SecuredRecordNeedsProvider => {
                write!(f, "secured identity record needs its host secret provider")
            }
            Self::KeyNotSecured => write!(
                f,
                "this identity's signing key is not secured yet, so apps cannot use it; \
                 choose \"Protect now\" for it in Settings > Users, then open the app again"
            ),
            Self::SecretSizeMismatch { got } => {
                write!(f, "identity secret_bytes must be 32 bytes, got {got}")
            }
            Self::PublicKeyMismatch { derived, recorded } => write!(
                f,
                "identity secret derives Ed25519 pubkey {} but the record claims {}; \
                 this identity cannot sign for itself",
                hex4(derived),
                hex4(recorded),
            ),
        }
    }
}

/// First four bytes of a key, hex, for error text — never the whole key,
/// since an error string can end up in a log a third party reads.
fn hex4(key: &[u8; 32]) -> String {
    hex::encode(&key[..4])
}

impl LoadFailure {
    fn into_not_available(self) -> NotAvailable {
        let kind = match &self {
            LoadFailure::ReadFailed(e) => e.kind(),
            LoadFailure::JsonMalformed(_)
            | LoadFailure::UnknownSchemaVersion(_)
            | LoadFailure::SecuredRecordNeedsProvider
            | LoadFailure::KeyNotSecured
            | LoadFailure::SecretSizeMismatch { .. }
            | LoadFailure::PublicKeyMismatch { .. } => io::ErrorKind::InvalidData,
        };
        NotAvailable::ConnectionFailed(io::Error::new(kind, self.to_string()))
    }
}

/// The loaded user identity; owns the private signing material for the
/// process lifetime (wrap in an `Arc` to share across tasks).
///
/// **Never** serialise or log this struct — it holds the raw Ed25519
/// secret. `Debug` deliberately omits `signing_key`.
pub struct SigningIdentity {
    display_name: String,
    avatar_color: [u8; 3],
    group_public_key: [u8; 32],
    /// Key that verifies [`Self::sign_for`]. A broker supplies the enrolled
    /// device key rather than exposing or using the account/root key.
    signing_public_key: [u8; 32],
    /// Opaque JSON-encoded root-signed device certificate on the broker path.
    device_certificate: Option<Vec<u8>>,
    threshold: u16,
    total_shares: u16,
    share_index: u16,
    source: SigningSource,
    single_device: bool,
}

#[allow(
    clippy::large_enum_variant,
    reason = "Local carries a bare signing key while Broker/ActionBroker carry a live socket \
              behind a Mutex; boxing the socket variants would touch every match arm across \
              this file (11 call sites) for a few dozen bytes of stack that never round-trips \
              through a hot path — this type is constructed once per identity, not per message"
)]
enum SigningSource {
    Local(Option<SigningKey>),
    /// Restored device-local FROST share. It stays inside the host and is
    /// usable only by the trusted quorum coordinator, never exported to apps.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when this is not a host-local threshold share,
    /// the identity lock is poisoned, or FROST round-one generation fails.
    LocalThreshold(Mutex<ThresholdIdentity>),
    Broker(Mutex<std::os::unix::net::UnixStream>),
    ActionBroker(Arc<BrokerSigningIdentity>),
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "`source` is deliberately never printed by field name — it carries a signing key \
              or a live socket, and the redacted \"signing_source\" entry below stands in for \
              it on purpose, not by omission"
)]
impl std::fmt::Debug for SigningIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningIdentity")
            .field("display_name", &self.display_name)
            .field("avatar_color", &self.avatar_color)
            .field("group_public_key", &"<32 bytes>")
            .field("signing_public_key", &"<32 bytes>")
            .field(
                "device_certificate",
                &self.device_certificate.as_ref().map(|_| "<present>"),
            )
            .field("threshold", &self.threshold)
            .field("total_shares", &self.total_shares)
            .field("share_index", &self.share_index)
            .field("single_device", &self.single_device)
            .field("signing_source", &"<redacted capability>")
            .finish()
    }
}

impl SigningIdentity {
    /// Start one host-coordinated FROST signing round without exporting this
    /// device's share. This succeeds only for a threshold identity loaded by
    /// the trusted launcher; sandbox clients receive a broker source and
    /// cannot invoke the local-share path.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when this is not a host-local threshold share,
    /// the identity lock is poisoned, or FROST round-one setup fails.
    pub fn threshold_sign_round1<R: rand_core::CryptoRng + ?Sized>(
        &self,
        rng: &mut R,
    ) -> Result<SigningRound1, NotAvailable> {
        let SigningSource::LocalThreshold(identity) = &self.source else {
            return Err(NotAvailable::ConnectionFailed(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "threshold rounds are available only to the trusted identity host",
            )));
        };
        identity
            .lock()
            .map_err(|_| {
                NotAvailable::ConnectionFailed(io::Error::other("identity lock poisoned"))
            })?
            .sign_round1(rng)
            .map_err(|error| {
                NotAvailable::ConnectionFailed(io::Error::other(format!(
                    "threshold round 1 failed: {error}"
                )))
            })
    }

    /// Produce this device's partial after the trusted coordinator has
    /// collected and human-authorized a quorum of commitments.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when this is not a host-local threshold share,
    /// the identity lock is poisoned, or the FROST session data is invalid.
    pub fn threshold_sign_round2(
        &self,
        nonces: SigningSessionNonces,
        message: &[u8],
        commitments: &[SigningRound1Commitment],
    ) -> Result<SigningRound2Partial, NotAvailable> {
        let SigningSource::LocalThreshold(identity) = &self.source else {
            return Err(NotAvailable::ConnectionFailed(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "threshold rounds are available only to the trusted identity host",
            )));
        };
        identity
            .lock()
            .map_err(|_| {
                NotAvailable::ConnectionFailed(io::Error::other("identity lock poisoned"))
            })?
            .sign_round2_raw(nonces, message, commitments)
            .map_err(|error| {
                NotAvailable::ConnectionFailed(io::Error::other(format!(
                    "threshold round 2 failed: {error}"
                )))
            })
    }

    /// Aggregate a confirmed quorum's partials into the ordinary Ed25519
    /// signature consumed by the rest of Tilo.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when this is not a host-local threshold share,
    /// the identity lock is poisoned, or the partial quorum is invalid.
    pub fn aggregate_threshold_signature(
        &self,
        partials: &[SigningRound2Partial],
    ) -> Result<[u8; 64], NotAvailable> {
        let SigningSource::LocalThreshold(identity) = &self.source else {
            return Err(NotAvailable::ConnectionFailed(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "threshold aggregation is available only to the trusted identity host",
            )));
        };
        identity
            .lock()
            .map_err(|_| {
                NotAvailable::ConnectionFailed(io::Error::other("identity lock poisoned"))
            })?
            .aggregate_signature(partials)
            .map(|signature| *signature.as_bytes())
            .map_err(|error| {
                NotAvailable::ConnectionFailed(io::Error::other(format!(
                    "threshold aggregation failed: {error}"
                )))
            })
    }

    /// Human-picked display name (chrome, presence pills); may be empty.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Avatar-accent RGB the user picked at first boot.
    pub fn avatar_color(&self) -> [u8; 3] {
        self.avatar_color
    }

    /// The 32-byte group public key — byte-compatible with a standard Ed25519
    /// verifier, stable across threshold enrolments until the next DKG rotation.
    pub fn group_public_key(&self) -> [u8; 32] {
        self.group_public_key
    }

    /// Public key that verifies signatures returned by [`Self::sign_for`].
    pub fn signing_public_key(&self) -> [u8; 32] {
        self.signing_public_key
    }

    /// Root-signed certificate binding [`Self::signing_public_key`] to
    /// [`Self::group_public_key`], encoded as JSON. Direct legacy record
    /// loading has no brokered device chain and returns `None`.
    pub fn device_certificate(&self) -> Option<&[u8]> {
        self.device_certificate.as_deref()
    }

    /// `t` in t-of-n; `1` while the user is still in single-device bootstrap mode.
    pub fn threshold(&self) -> u16 {
        self.threshold
    }

    /// `n` in t-of-n.
    pub fn total_shares(&self) -> u16 {
        self.total_shares
    }

    /// 1-indexed share number this device holds.
    pub fn share_index(&self) -> u16 {
        self.share_index
    }

    /// True while the identity is still in 1-of-1 mode, the fast local-signing path.
    pub fn is_single_device(&self) -> bool {
        self.single_device
    }

    /// Sign a message locally. In single-device mode, returns the raw 64-byte
    /// Ed25519 signature; in threshold mode, returns
    /// [`NotAvailable::ConnectionFailed`] — threshold signing needs the
    /// multi-device FROST ceremony via `threshold-identity` instead.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::ConnectionFailed`] in threshold mode, or if
    /// signing through the broker fails.
    pub fn sign(&self, message: &[u8]) -> Result<[u8; 64], NotAvailable> {
        match &self.source {
            SigningSource::Local(Some(sk)) => Ok(sk.sign(message).to_bytes()),
            SigningSource::Local(None) => Err(NotAvailable::ConnectionFailed(io::Error::new(
                io::ErrorKind::Unsupported,
                "threshold-mode signing is not supported by tilo-runtime-identity; \
                 use tilo-threshold-identity + the multi-device orchestrator instead",
            ))),
            SigningSource::LocalThreshold(_) => {
                Err(NotAvailable::ConnectionFailed(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "threshold signature is awaiting approval from another enrolled device",
                )))
            }
            SigningSource::Broker(stream) => broker::sign(stream, message),
            SigningSource::ActionBroker(stream) => stream.sign_for("generic", message),
        }
    }

    /// The person's secret-sync key: an X25519 key derived from the root,
    /// which secrets every device of the person must open (disk-unlock
    /// sibling halves) are sealed to. Only a single-device root held by
    /// this process derives it; the root itself never leaves.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::ConnectionFailed`] for a threshold share or a
    /// broker connection, which hold no root to derive from.
    pub fn secret_sync_key(&self) -> Result<tilo_seal::SecretKey, NotAvailable> {
        match &self.source {
            SigningSource::Local(Some(sk)) => {
                let seed = Zeroizing::new(sk.to_bytes());
                let derived =
                    Zeroizing::new(blake3::derive_key("tilo secret-sync key v1", seed.as_ref()));
                Ok(tilo_seal::SecretKey::from_bytes(*derived))
            }
            _ => Err(NotAvailable::ConnectionFailed(io::Error::new(
                io::ErrorKind::Unsupported,
                "the secret-sync key needs the person's root on this device",
            ))),
        }
    }

    /// Ask the workspace identity broker to sign one named app action. The
    /// shell binds the package and workspace before the app inherits this
    /// connection, then evaluates the exact action capability.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::ConnectionFailed`] if signing through the
    /// action broker fails, or whatever [`Self::sign`] can return when this
    /// identity has no action broker connection.
    pub fn sign_for(&self, action: &str, message: &[u8]) -> Result<[u8; 64], NotAvailable> {
        match &self.source {
            SigningSource::ActionBroker(stream) => stream.sign_for(action, message),
            _ => self.sign(message),
        }
    }

    /// Prove that this process can ask the workspace-bound identity broker to
    /// use the certified device key. Unlike [`Self::sign_for`], this is not an
    /// application action grant: the launcher has already bound the broker
    /// connection to exactly one workspace, and the broker rejects any other
    /// workspace id before signing the fixed collaboration-join domain.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::ConnectionFailed`] when the requested
    /// workspace differs from the launcher's handshake, the device is no
    /// longer enrolled, or the signing service is unavailable.
    pub fn sign_collaboration_join(&self, workspace_id: &str) -> Result<[u8; 64], NotAvailable> {
        match &self.source {
            SigningSource::ActionBroker(stream) => stream.sign_collaboration_join(workspace_id),
            _ => self.sign(&collaboration_join_signing_bytes(workspace_id)),
        }
    }

    /// Ask the workspace-bound identity broker for the content keys of one
    /// document route; see [`crate::wire::Request::ChannelKeys`].
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::ConnectionFailed`] if this is not a
    /// workspace-bound broker connection or the broker refuses the route.
    pub fn channel_keys(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        known_generation: u64,
    ) -> Result<crate::wire::ChannelKeysReply, NotAvailable> {
        match &self.source {
            SigningSource::ActionBroker(stream) => {
                stream.channel_keys(workspace_id, app_id, record_id, known_generation)
            }
            SigningSource::Local(_)
            | SigningSource::LocalThreshold(_)
            | SigningSource::Broker(_) => Err(NotAvailable::ConnectionFailed(io::Error::new(
                io::ErrorKind::Unsupported,
                "channel keys require the workspace identity service",
            ))),
        }
    }

    /// Ask the workspace-bound identity broker to authorize and sign one
    /// typed collaboration document operation. The broker parses the complete
    /// unsigned envelope and checks that its route-derived artifact, author,
    /// operation class, timestamp, and typed payload match this launch before
    /// the certified device key signs it.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::ConnectionFailed`] if this is not a
    /// workspace-bound broker connection, the route or operation is invalid,
    /// the current role cannot edit, or the device is untrusted/revoked.
    pub fn sign_collaboration_op(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        unsigned_op: &[u8],
    ) -> Result<[u8; 64], NotAvailable> {
        match &self.source {
            SigningSource::ActionBroker(stream) => {
                stream.sign_collaboration_op(workspace_id, app_id, record_id, unsigned_op)
            }
            SigningSource::Local(_)
            | SigningSource::LocalThreshold(_)
            | SigningSource::Broker(_) => Err(NotAvailable::ConnectionFailed(io::Error::new(
                io::ErrorKind::Unsupported,
                "certified collaboration operations require the workspace identity service",
            ))),
        }
    }

    fn sign_local(&self, message: &[u8]) -> io::Result<[u8; 64]> {
        match &self.source {
            SigningSource::Local(Some(sk)) => Ok(sk.sign(message).to_bytes()),
            SigningSource::Local(None) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "threshold signing requires the multi-device orchestrator",
            )),
            SigningSource::LocalThreshold(_) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "threshold signature requires a confirmed broker quorum",
            )),
            SigningSource::Broker(_) | SigningSource::ActionBroker(_) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to chain identity brokers",
            )),
        }
    }

    fn from_local_parts(
        display_name: String,
        avatar_color: [u8; 3],
        group_public_key: [u8; 32],
        threshold: u16,
        total_shares: u16,
        share_index: u16,
        signing_key: Option<SigningKey>,
    ) -> Self {
        Self {
            display_name,
            avatar_color,
            group_public_key,
            signing_public_key: group_public_key,
            device_certificate: None,
            threshold,
            total_shares,
            share_index,
            single_device: signing_key.is_some(),
            source: SigningSource::Local(signing_key),
        }
    }

    fn from_threshold_parts(
        display_name: String,
        avatar_color: [u8; 3],
        identity: ThresholdIdentity,
    ) -> Self {
        let group_public_key = *identity.group_public_key().as_bytes();
        let threshold = identity.threshold;
        let total_shares = identity.total_shares;
        let share_index = identity.this_share_index;
        Self {
            display_name,
            avatar_color,
            group_public_key,
            signing_public_key: group_public_key,
            device_certificate: None,
            threshold,
            total_shares,
            share_index,
            source: SigningSource::LocalThreshold(Mutex::new(identity)),
            single_device: false,
        }
    }
}

/// Load the user's identity from the inherited broker, falling back to the
/// legacy [`IDENTITY_ENV_VAR`] development path. Returns
/// `Err(NotAvailable::ServiceFdMissing)` when neither is present,
/// `Err(NotAvailable::ConnectionFailed(_))` when the file is missing,
/// unreadable, or malformed, and `Ok(SigningIdentity)` on success.
///
/// # Errors
///
/// Returns [`NotAvailable::ServiceFdMissing`] when neither the broker nor
/// [`IDENTITY_ENV_VAR`] is present, or [`NotAvailable::ConnectionFailed`]
/// when the identity file is missing, unreadable, or malformed.
pub fn load() -> Result<SigningIdentity, NotAvailable> {
    if let Some(fd) = runtime_portal::service_fd(IDENTITY_SERVICE) {
        return load_from_service(fd);
    }
    if std::env::var_os(broker::BROKER_ENV_VAR).is_some() {
        return broker::connect_from_env();
    }
    let path_str = env::var(IDENTITY_ENV_VAR).map_err(|_| NotAvailable::ServiceFdMissing)?;
    if path_str.is_empty() {
        return Err(NotAvailable::ServiceFdMissing);
    }
    load_from_path(PathBuf::from(path_str))
}

/// The public fields of the identity [`load`] would return. A record named
/// by [`IDENTITY_ENV_VAR`] is read without contacting its secret provider,
/// so a caller that only shows who this is never waits on (or prompts for)
/// the system secret store.
///
/// # Errors
///
/// As [`load`].
pub fn load_metadata() -> Result<IdentityMetadata, NotAvailable> {
    if runtime_portal::service_fd(IDENTITY_SERVICE).is_none()
        && std::env::var_os(broker::BROKER_ENV_VAR).is_none()
    {
        let path = env::var(IDENTITY_ENV_VAR).map_err(|_| NotAvailable::ServiceFdMissing)?;
        if path.is_empty() {
            return Err(NotAvailable::ServiceFdMissing);
        }
        return load_metadata_from_path(std::path::Path::new(&path));
    }
    let identity = load()?;
    Ok(IdentityMetadata {
        display_name: identity.display_name().to_owned(),
        avatar_color: identity.avatar_color(),
        group_public_key: identity.group_public_key(),
        threshold: identity.threshold(),
        total_shares: identity.total_shares(),
        share_index: identity.share_index(),
    })
}

fn load_from_service(fd: std::os::fd::RawFd) -> Result<SigningIdentity, NotAvailable> {
    let identity = shared_service_connection(fd)?;
    Ok(SigningIdentity {
        display_name: identity.display_name().to_owned(),
        avatar_color: identity.avatar_color(),
        group_public_key: identity.group_public_key(),
        signing_public_key: identity.signing_public_key(),
        device_certificate: identity.device_certificate().map(<[u8]>::to_vec),
        threshold: identity.threshold(),
        total_shares: identity.total_shares(),
        share_index: identity.share_index(),
        single_device: identity.is_single_device(),
        source: SigningSource::ActionBroker(identity),
    })
}

type IdentityConnections =
    Mutex<std::collections::HashMap<std::os::fd::RawFd, Weak<BrokerSigningIdentity>>>;

fn shared_service_connection(
    fd: std::os::fd::RawFd,
) -> Result<Arc<BrokerSigningIdentity>, NotAvailable> {
    static CONNECTIONS: OnceLock<IdentityConnections> = OnceLock::new();
    let mut connections = CONNECTIONS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| {
            NotAvailable::ConnectionFailed(io::Error::other(
                "identity connection registry poisoned",
            ))
        })?;
    connections.retain(|_, connection| connection.strong_count() > 0);
    if let Some(connection) = connections.get(&fd).and_then(Weak::upgrade) {
        return Ok(connection);
    }
    // SAFETY: fcntl operates on the validated inherited descriptor only.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(NotAvailable::ConnectionFailed(io::Error::last_os_error()));
        }
    }
    // SAFETY: borrow only long enough to duplicate the launcher-owned handle.
    let owned = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
    let identity = Arc::new(BrokerSigningIdentity::from_connected_io(UnixStream::from(
        owned,
    ))?);
    connections.insert(fd, Arc::downgrade(&identity));
    Ok(identity)
}

/// Same as [`load`] but reads from a caller-specified path — for tests, or
/// apps that resolve the identity path themselves.
///
/// # Errors
///
/// Returns [`NotAvailable::ConnectionFailed`] if the file can't be read, is
/// malformed, has no installed secret provider for a secured record, or the
/// record fails its self-consistency check.
#[allow(
    clippy::needless_pass_by_value,
    reason = "public API also called with an owned PathBuf from crates/packaging/launcher, outside this pass's scope to update"
)]
pub fn load_from_path(path: PathBuf) -> Result<SigningIdentity, NotAvailable> {
    let bytes =
        read_record_bytes(&path).map_err(|e| LoadFailure::ReadFailed(e).into_not_available())?;
    load_record(&path, parse_record(&bytes)?)
}

fn load_record(
    path: &std::path::Path,
    record: ondisk::IdentityRecord,
) -> Result<SigningIdentity, NotAvailable> {
    if record.schema_version == 1 {
        return identity_from_record(record, None);
    }
    if is_public_member_projection(&record) {
        return identity_from_record(record, None);
    }
    let reference = record
        .secret_ref
        .as_ref()
        .ok_or_else(|| LoadFailure::SecuredRecordNeedsProvider.into_not_available())?;
    let dir = path.parent().ok_or_else(|| {
        LoadFailure::ReadFailed(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity path has no parent",
        ))
        .into_not_available()
    })?;
    let provider = secure_store::provider_for(dir, reference)
        .map_err(|e| LoadFailure::ReadFailed(e).into_not_available())?;
    let secret = provider
        .load(&reference.locator)
        .map_err(|e| LoadFailure::ReadFailed(e).into_not_available())?;
    identity_from_record(record, Some(secret.as_slice()))
}

/// Load a record from the identity store for an app launch, strictly
/// read-only.
///
/// The record must already be in the form provisioning leaves it: a secured
/// record whose key its secret provider holds, or a joined device's public
/// record. One still carrying its key inline is refused, never rewritten:
/// securing it belongs to provisioning and Settings, not to a launch.
///
/// # Errors
///
/// As [`load_from_path`], and [`NotAvailable::ConnectionFailed`] naming
/// Settings when the record's key is not secured.
pub fn load_for_launch(path: &std::path::Path) -> Result<SigningIdentity, NotAvailable> {
    let bytes =
        read_record_bytes(path).map_err(|e| LoadFailure::ReadFailed(e).into_not_available())?;
    let record = parse_record(&bytes)?;
    if record.protection() == KeyProtection::LegacyPlaintext {
        return Err(LoadFailure::KeyNotSecured.into_not_available());
    }
    load_record(path, record)
}

/// Read the public presentation and quorum fields of a private identity
/// record without contacting its secret provider.
///
/// The record still receives the same ownership, symlink, size, JSON and
/// schema validation as [`load_from_path`]. Callers that authorize an identity
/// must bind `group_public_key` to their independently validated principal.
///
/// # Errors
///
/// Returns [`NotAvailable::ConnectionFailed`] if the record cannot be read or
/// parsed safely.
pub fn load_metadata_from_path(path: &std::path::Path) -> Result<IdentityMetadata, NotAvailable> {
    let bytes = read_record_bytes(path)
        .map_err(|error| LoadFailure::ReadFailed(error).into_not_available())?;
    let record = parse_record(&bytes)?;
    let identity = record.threshold_identity;
    Ok(IdentityMetadata {
        display_name: record.display_name,
        avatar_color: record.avatar_color,
        group_public_key: identity.group_public_key,
        threshold: identity.threshold,
        total_shares: identity.total_shares,
        share_index: identity.share_index,
    })
}

/// Same as [`load`] but parses from an in-memory byte slice.
///
/// # Errors
///
/// Returns [`NotAvailable::ConnectionFailed`] if `bytes` is malformed, is a
/// secured (schema version 2) record, or fails its self-consistency check.
pub fn load_from_bytes(bytes: &[u8]) -> Result<SigningIdentity, NotAvailable> {
    let record = parse_record(bytes)?;
    if record.schema_version != 1 {
        return Err(LoadFailure::SecuredRecordNeedsProvider.into_not_available());
    }
    identity_from_record(record, None)
}

/// Serialize a public-only identity record for an enrolled member device.
///
/// The result identifies the same person principal but carries no root
/// signing share. The device authenticates through its separately
/// installed root-signed device certificate and signing broker.
///
/// # Errors
///
/// Returns an I/O error if the public identity record cannot be serialized.
pub fn public_member_record(
    display_name: &str,
    avatar_color: [u8; 3],
    group_public_key: [u8; 32],
) -> io::Result<Vec<u8>> {
    let record = ondisk::IdentityRecord {
        display_name: display_name.trim().to_owned(),
        avatar_color,
        threshold_identity: ondisk::SerializableIdentity {
            single_device: false,
            group_public_key,
            threshold: 1,
            total_shares: 1,
            share_index: 0,
            secret_bytes: Zeroizing::new(Vec::new()),
        },
        created_at: String::new(),
        schema_version: 1,
        secret_ref: None,
    };
    serde_json::to_vec_pretty(&record).map_err(io::Error::other)
}

fn parse_record(bytes: &[u8]) -> Result<ondisk::IdentityRecord, NotAvailable> {
    let record: ondisk::IdentityRecord = serde_json::from_slice(bytes)
        .map_err(|e| LoadFailure::JsonMalformed(e).into_not_available())?;

    if !matches!(record.schema_version, 1 | 2) {
        return Err(LoadFailure::UnknownSchemaVersion(record.schema_version).into_not_available());
    }
    Ok(record)
}

fn read_record_bytes(path: &std::path::Path) -> io::Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "identity record must be a regular file",
        ));
    }
    #[cfg(unix)]
    if meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "identity record must be owned by the current user and mode 0600",
        ));
    }
    if meta.len() > MAX_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity record exceeds the size limit",
        ));
    }
    // Bounded by the MAX_RECORD_BYTES check just above (1 MiB).
    #[allow(
        clippy::cast_possible_truncation,
        reason = "meta.len() is checked <= MAX_RECORD_BYTES (1 MiB) just above"
    )]
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    (&mut file)
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity record exceeds the size limit",
        ));
    }
    Ok(bytes)
}

fn read_record(path: &std::path::Path) -> io::Result<ondisk::IdentityRecord> {
    let bytes = read_record_bytes(path)?;
    parse_record(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

fn identity_from_record(
    record: ondisk::IdentityRecord,
    external_secret: Option<&[u8]>,
) -> Result<SigningIdentity, NotAvailable> {
    let ti = record.threshold_identity;
    let secret = external_secret.unwrap_or(&ti.secret_bytes);

    if ti.single_device && !secret.is_empty() && secret.len() != 32 {
        return Err(LoadFailure::SecretSizeMismatch { got: secret.len() }.into_not_available());
    }

    if !ti.single_device {
        if secret.is_empty() {
            if ti.threshold == 1 && ti.total_shares == 1 && ti.share_index == 0 {
                return Ok(SigningIdentity::from_local_parts(
                    record.display_name,
                    record.avatar_color,
                    ti.group_public_key,
                    ti.threshold,
                    ti.total_shares,
                    ti.share_index,
                    None,
                ));
            }
            return Err(LoadFailure::SecuredRecordNeedsProvider.into_not_available());
        }
        let identity =
            ThresholdIdentity::from_device_secret(secret, ti.total_shares).map_err(|e| {
                NotAvailable::ConnectionFailed(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("threshold device share rejected: {e}"),
                ))
            })?;
        if identity.group_public_key().as_bytes() != &ti.group_public_key
            || identity.threshold != ti.threshold
            || identity.this_share_index != ti.share_index
        {
            return Err(LoadFailure::PublicKeyMismatch {
                derived: Box::new(*identity.group_public_key().as_bytes()),
                recorded: Box::new(ti.group_public_key),
            }
            .into_not_available());
        }
        return Ok(SigningIdentity::from_threshold_parts(
            record.display_name,
            record.avatar_color,
            identity,
        ));
    }

    let signing_key = if secret.is_empty() {
        None
    } else {
        let mut scalar = Zeroizing::new([0u8; 32]);
        scalar.copy_from_slice(secret);
        // SigningKey::from_bytes is infallible today; if a future dalek revision
        // surfaces validation errors, catch them here.
        Some(SigningKey::from_bytes(&scalar))
    };

    // The record's two halves must agree: expanding `secret_bytes` must derive
    // `group_public_key`. A record that fails this signs under a key nobody
    // verifies it by, so every op it authors is silently rejected downstream
    // while the app believes itself authenticated — refuse to load it.
    //
    // This check is fatal in every build: one that only warns under
    // `debug_assertions` ships exactly the configuration that skips it.
    if let Some(sk) = &signing_key {
        let derived = sk.verifying_key().to_bytes();
        if derived != ti.group_public_key {
            return Err(LoadFailure::PublicKeyMismatch {
                derived: Box::new(derived),
                recorded: Box::new(ti.group_public_key),
            }
            .into_not_available());
        }
    }

    Ok(SigningIdentity::from_local_parts(
        record.display_name,
        record.avatar_color,
        ti.group_public_key,
        ti.threshold,
        ti.total_shares,
        ti.share_index,
        signing_key,
    ))
}

fn is_public_member_projection(record: &ondisk::IdentityRecord) -> bool {
    let identity = &record.threshold_identity;
    !identity.single_device
        && identity.secret_bytes.is_empty()
        && record.secret_ref.is_none()
        && identity.threshold == 1
        && identity.total_shares == 1
        && identity.share_index == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use getrandom::{SysRng, rand_core::UnwrapErr};

    // Only the external keyring transport is emulated. Both its provider
    // storage and all identity metadata publication use real files.
    struct SecretServiceFixture {
        storage: BrokerFileProvider,
        reject_store: bool,
    }

    impl SecretServiceFixture {
        fn new(dir: &std::path::Path, reject_store: bool) -> Self {
            Self {
                storage: BrokerFileProvider::new(&dir.join("test-keyring")),
                reject_store,
            }
        }
    }

    impl SecretProvider for SecretServiceFixture {
        fn protection(&self) -> KeyProtection {
            KeyProtection::SecretService
        }
        fn backend_id(&self) -> &'static str {
            "secret-service-v1"
        }
        fn store(&self, locator: &str, secret: &[u8]) -> io::Result<()> {
            if self.reject_store {
                return Err(io::Error::other("test keyring refused storage"));
            }
            self.storage.store(locator, secret)
        }
        fn load(&self, locator: &str) -> io::Result<Zeroizing<Vec<u8>>> {
            self.storage.load(locator)
        }
        fn remove(&self, locator: &str) -> io::Result<()> {
            self.storage.remove(locator)
        }
    }

    thread_local! {
        static PREFERRED_SECRET_SERVICE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    }

    pub(super) fn preferred_provider(dir: &std::path::Path) -> Box<dyn SecretProvider + '_> {
        match PREFERRED_SECRET_SERVICE.get() {
            Some(reject_store) => Box::new(SecretServiceFixture::new(dir, reject_store)),
            None => secure_store::preferred_provider(dir),
        }
    }

    fn with_preferred_secret_service<T>(reject_store: bool, operation: impl FnOnce() -> T) -> T {
        struct Restore(Option<bool>);
        impl Drop for Restore {
            fn drop(&mut self) {
                PREFERRED_SECRET_SERVICE.set(self.0);
            }
        }
        let _restore = Restore(PREFERRED_SECRET_SERVICE.replace(Some(reject_store)));
        operation()
    }

    #[test]
    fn the_secret_provider_is_classified_and_unlocked_only_to_sign() {
        // phase: secret_service_unavailability_has_a_typed_classification
        {
            let unavailable = NotAvailable::ConnectionFailed(secret_service_unavailable_error());
            assert!(is_secret_service_unavailable(&unavailable));
            let other = NotAvailable::ConnectionFailed(io::Error::from_raw_os_error(libc::EACCES));
            assert!(!is_secret_service_unavailable(&other));
        }
        // phase: public_metadata_does_not_unlock_the_secret_provider
        {
            let dir = tempfile::tempdir().expect("identity directory");
            let path = dir.path().join("secured.json");
            fs::write(
                &path,
                serde_json::json!({
                    "display_name": "Ada",
                    "avatar_color": [12, 34, 56],
                    "threshold_identity": {
                        "single_device": true,
                        "group_public_key": vec![7u8; 32],
                        "threshold": 1,
                        "total_shares": 1,
                        "share_index": 0
                    },
                    "created_at": "proof",
                    "schema_version": 2,
                    "secret_ref": {
                        "backend": "secret-service-v1",
                        "locator": "00".repeat(32)
                    }
                })
                .to_string(),
            )
            .expect("identity record");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private mode");

            let metadata = load_metadata_from_path(&path).expect("public metadata");
            assert_eq!(metadata.display_name, "Ada");
            assert_eq!(metadata.avatar_color, [12, 34, 56]);
            assert_eq!(metadata.group_public_key, [7; 32]);
            assert_eq!(
                (
                    metadata.threshold,
                    metadata.total_shares,
                    metadata.share_index
                ),
                (1, 1, 0)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn identity_files_stay_owner_only() {
        // phase: a_shared_lifecycle_lock_is_refused_and_listing_takes_none
        {
            let dir = tempfile::tempdir().expect("identity directory");
            let lock = dir.path().join(LOCK_FILE);
            fs::write(&lock, []).expect("shared lock");
            fs::set_permissions(&lock, fs::Permissions::from_mode(0o660)).expect("shared mode");

            assert!(import_identity(dir.path(), "personal", &record_bytes("Ada", 25)).is_err());
            assert_eq!(list_identities(dir.path()).expect("list").len(), 0);
            assert_eq!(
                fs::metadata(&lock).expect("lock").permissions().mode() & 0o777,
                0o660,
                "listing never touches the lock"
            );
            assert!(
                list_identities(&dir.path().join("missing"))
                    .unwrap()
                    .is_empty()
            );
            assert!(!dir.path().join("missing").exists());
        }
        // phase: imported_secrets_and_selector_are_private
        {
            let dir = tempfile::tempdir().unwrap();
            let path = import_identity(dir.path(), "personal", &record_bytes("Ada", 26)).unwrap();
            set_active_identity(dir.path(), "personal").unwrap();
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(dir.path().join(ACTIVE_FILE))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let metadata = fs::read_to_string(&path).unwrap();
            assert!(metadata.contains("\"schema_version\": 2"));
            assert!(metadata.contains("\"secret_ref\""));
            assert!(
                !metadata.contains("secret_bytes"),
                "private signing material remained in mounted-style JSON"
            );
        }
        // phase: broker_file_revalidates_key_permissions_before_load_or_remove
        {
            let dir = tempfile::tempdir().unwrap();
            let provider = BrokerFileProvider::new(dir.path());
            let locator = "ab".repeat(32);
            provider.store(&locator, &[30; 32]).unwrap();
            let key_path = dir.path().join("secrets").join(format!("{locator}.key"));
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();

            assert_eq!(
                provider.load(&locator).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(
                provider.remove(&locator).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        // phase: record_open_rejects_symlinks_and_wide_permissions
        {
            use std::os::unix::fs::symlink;

            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("target.json");
            fs::write(&target, record_bytes("Ada", 29)).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            let link = dir.path().join("link.json");
            symlink(&target, &link).unwrap();
            assert!(load_from_path(link).is_err());

            fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
            let error = load_from_path(target).unwrap_err();
            match error {
                NotAvailable::ConnectionFailed(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn action_frames_keep_multi_megabyte_document_bytes_compact() {
        let (mut writer, mut reader) = UnixStream::pair().expect("socket pair");
        let document = vec![0xa5; 2 * 1024 * 1024];
        let sent = document.clone();
        let sender = std::thread::spawn(move || {
            write_frame(
                &mut writer,
                &wire::Request::CollaborationOp {
                    workspace_id: "workspace".into(),
                    app_id: "so.tilo.notes".into(),
                    record_id: "record".into(),
                    unsigned_op: sent,
                },
            )
            .expect("compact action frame");
        });
        let wire::Request::CollaborationOp { unsigned_op, .. } =
            read_frame::<wire::Request>(&mut reader).expect("action frame round trip")
        else {
            panic!("wrong request variant");
        };
        sender.join().expect("writer thread");
        assert_eq!(unsigned_op, document);
    }

    fn clear_env() -> tilo_test_env::ScopedEnv {
        let mut scoped = tilo_test_env::ScopedEnv::new();
        scoped
            .remove(IDENTITY_ENV_VAR)
            .remove(broker::BROKER_ENV_VAR);
        scoped
    }

    /// The compositor's broker thread asked who this person is for every
    /// presence frame an app sent. For an identity kept in a desktop's
    /// Secret Service that loaded the signing secret over D-Bus from inside
    /// the broker's runtime, which aborted the session.
    #[test]
    #[serial_test::serial(identity_env)]
    fn who_this_is_never_reaches_for_the_signing_secret() {
        let dir = tempfile::tempdir().unwrap();
        let in_store = secure_store::SecretServiceProvider::with_store(
            tilo_secrets::SecretStore::memory("so.tilo.identity"),
        );
        let path = import_identity_with_provider(
            dir.path(),
            "personal",
            &record_bytes("Ada", 31),
            &in_store,
        )
        .unwrap();
        let record = read_record(&path).unwrap();
        assert_eq!(
            record
                .secret_ref
                .as_ref()
                .map(|reference| reference.backend.as_str()),
            Some("secret-service-v1"),
            "the record names the Secret Service, as on a desktop that has one"
        );

        let mut env = clear_env();
        env.set(IDENTITY_ENV_VAR, &path).set(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", dir.path().join("no-bus").display()),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (metadata, full) = runtime.block_on(async { (load_metadata(), load()) });
        let metadata = metadata.expect("the public fields answer without the store");
        assert_eq!(metadata.display_name, "Ada");
        assert_eq!(
            metadata.group_public_key,
            SigningKey::from_bytes(&[31; 32]).verifying_key().to_bytes()
        );
        assert!(
            full.is_err(),
            "the signing secret needs the (absent) store, and says so"
        );
        drop(env);
    }

    #[test]
    #[serial_test::serial(identity_env)]
    fn the_environment_never_relocates_or_fakes_an_identity() {
        // phase: load_without_env_returns_service_fd_missing
        {
            let _env = clear_env();
            match load() {
                Err(NotAvailable::ServiceFdMissing) => {}
                other => panic!("expected ServiceFdMissing, got {other:?}"),
            }
        }
        // phase: load_with_empty_env_returns_service_fd_missing
        {
            let mut env = clear_env();
            env.set(IDENTITY_ENV_VAR, "");
            let out = load();
            drop(env);
            match out {
                Err(NotAvailable::ServiceFdMissing) => {}
                other => panic!("expected ServiceFdMissing, got {other:?}"),
            }
        }
        // phase: relative_xdg_config_home_never_relocates_identity_by_cwd
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", "relative/config")
                .set("HOME", "/home/ada");
            assert_eq!(
                identity_dir_from_env().unwrap(),
                PathBuf::from("/home/ada/.config/tilo/identity")
            );
        }
    }

    #[test]
    fn loading_an_identity_file_validates_its_shape() {
        // phase: load_missing_file_surfaces_connection_failed
        {
            let err = load_from_path(PathBuf::from("/nonexistent/tilo-identity.json"))
                .expect_err("should fail");
            match err {
                NotAvailable::ConnectionFailed(io) => {
                    assert_eq!(io.kind(), io::ErrorKind::NotFound);
                }
                other => panic!("expected ConnectionFailed(NotFound), got {other:?}"),
            }
        }
        // phase: load_bad_json_surfaces_invalid_data
        {
            let err = load_from_bytes(b"not json at all").expect_err("should fail");
            match err {
                NotAvailable::ConnectionFailed(io) => {
                    assert_eq!(io.kind(), io::ErrorKind::InvalidData);
                }
                other => panic!("expected ConnectionFailed(InvalidData), got {other:?}"),
            }
        }
        // phase: load_unknown_schema_version_rejected
        {
            let json = r#"{
            "display_name": "Alice",
            "avatar_color": [255, 0, 0],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
                "threshold": 1,
                "total_shares": 1,
                "share_index": 1,
                "secret_bytes": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]
            },
            "created_at": "1970-01-01T00:00:00Z",
            "schema_version": 42
        }"#;
            let err = load_from_bytes(json.as_bytes()).expect_err("should fail");
            match err {
                NotAvailable::ConnectionFailed(io) => {
                    assert!(io.to_string().contains("42"), "wrong error text: {io}");
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
        // phase: extra_fields_in_json_are_tolerated
        {
            // Forward-compat: a newer launcher may write extra fields;
            // an older SDK must NOT hard-fail. This test asserts that.
            let secret = [3u8; 32];
            let sk = SigningKey::from_bytes(&secret);
            let vk = sk.verifying_key();
            let record = serde_json::json!({
                "display_name": "Alice",
                "avatar_color": [0, 0, 0],
                "threshold_identity": {
                    "single_device": true,
                    "group_public_key": vk.to_bytes(),
                    "threshold": 1_u16,
                    "total_shares": 1_u16,
                    "share_index": 1_u16,
                    "secret_bytes": secret.to_vec(),
                    "some_new_field_from_the_future": "should be ignored",
                },
                "created_at": "2026-07-02T00:00:00Z",
                "schema_version": 1,
                "some_top_level_new_field": 99,
            });
            let bytes = serde_json::to_vec(&record).unwrap();
            load_from_bytes(&bytes).expect("must tolerate unknown fields");
        }
        // phase: load_from_path_reads_disk_file
        {
            use std::io::Write;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("primary.json");

            let secret = [11u8; 32];
            let sk = SigningKey::from_bytes(&secret);
            let vk = sk.verifying_key();
            let record = serde_json::json!({
                "display_name": "Bob",
                "avatar_color": [1, 2, 3],
                "threshold_identity": {
                    "single_device": true,
                    "group_public_key": vk.to_bytes(),
                    "threshold": 1_u16,
                    "total_shares": 1_u16,
                    "share_index": 1_u16,
                    "secret_bytes": secret.to_vec(),
                },
                "created_at": "2026-07-02T00:00:00Z",
                "schema_version": 1,
            });
            let mut f = fs::File::create(&path).unwrap();
            f.set_permissions(fs::Permissions::from_mode(0o600))
                .unwrap();
            f.write_all(&serde_json::to_vec(&record).unwrap()).unwrap();
            drop(f);

            let ident = load_from_path(path).expect("load_from_path");
            assert_eq!(ident.display_name(), "Bob");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn a_local_secret_must_derive_the_recorded_key_to_sign() {
        // phase: single_device_load_then_sign_and_verify
        {
            // The end-to-end happy path apps will hit.
            use ed25519_dalek::{Verifier, VerifyingKey};

            let secret = [7u8; 32];
            let sk = SigningKey::from_bytes(&secret);
            let vk = sk.verifying_key();
            let record = serde_json::json!({
                "display_name": "Alice",
                "avatar_color": [200, 100, 50],
                "threshold_identity": {
                    "single_device": true,
                    "group_public_key": vk.to_bytes(),
                    "threshold": 1_u16,
                    "total_shares": 1_u16,
                    "share_index": 1_u16,
                    "secret_bytes": secret.to_vec(),
                },
                "created_at": "2026-07-02T00:00:00Z",
                "schema_version": 1,
            });
            let bytes = serde_json::to_vec(&record).unwrap();
            let ident = load_from_bytes(&bytes).expect("load");

            assert_eq!(ident.display_name(), "Alice");
            assert_eq!(ident.avatar_color(), [200, 100, 50]);
            assert_eq!(ident.group_public_key(), vk.to_bytes());
            assert_eq!(ident.signing_public_key(), vk.to_bytes());
            assert!(ident.device_certificate().is_none());
            assert_eq!(ident.threshold(), 1);
            assert_eq!(ident.total_shares(), 1);
            assert_eq!(ident.share_index(), 1);
            assert!(ident.is_single_device());

            let msg = b"the quick brown fox";
            let sig_bytes = ident.sign(msg).expect("sign");
            let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
            let verifier = VerifyingKey::from_bytes(&vk.to_bytes()).unwrap();
            verifier
                .verify(msg, &sig)
                .expect("verification should succeed");
        }
        // phase: public_projection_loads_metadata_but_cannot_sign
        {
            let bytes = serde_json::to_vec(&serde_json::json!({
                "display_name": "Alice",
                "avatar_color": [2, 4, 6],
                "threshold_identity": {
                    "single_device": true,
                    "group_public_key": vec![8_u8; 32],
                    "threshold": 1_u16,
                    "total_shares": 1_u16,
                    "share_index": 1_u16,
                    "secret_bytes": Vec::<u8>::new(),
                },
                "schema_version": 1,
            }))
            .expect("encode");
            let identity = load_from_bytes(&bytes).expect("metadata loads");
            assert_eq!(identity.display_name(), "Alice");
            assert_eq!(identity.group_public_key(), [8; 32]);
            assert_eq!(identity.signing_public_key(), [8; 32]);
            assert!(identity.device_certificate().is_none());
            assert!(!identity.is_single_device());
            assert!(identity.sign(b"forbidden").is_err());
        }
        // phase: secret_that_does_not_derive_the_recorded_pubkey_is_refused
        // A record whose secret does not derive its recorded public key must not
        // load — silently accepting it would hand apps an identity that can never
        // sign successfully.
        {
            // The exact shape the old identity ceremony wrote: a digest
            // over the public key standing in for the secret.
            use sha2::{Digest, Sha512};
            let real = SigningKey::from_bytes(&[9u8; 32]);
            let vk = real.verifying_key();
            let mut hasher = Sha512::new();
            hasher.update(b"tilo-identity-flow-v1-placeholder-key");
            hasher.update(vk.to_bytes());
            let bogus_secret = hasher.finalize()[..32].to_vec();

            let record = serde_json::json!({
                "display_name": "Alice",
                "avatar_color": [0, 0, 0],
                "threshold_identity": {
                    "single_device": true,
                    "group_public_key": vk.to_bytes(),
                    "threshold": 1_u16,
                    "total_shares": 1_u16,
                    "share_index": 1_u16,
                    "secret_bytes": bogus_secret,
                },
                "created_at": "2026-07-30T00:00:00Z",
                "schema_version": 1,
            });
            let bytes = serde_json::to_vec(&record).unwrap();
            let err =
                load_from_bytes(&bytes).expect_err("a self-inconsistent record must not load");
            match err {
                NotAvailable::ConnectionFailed(io) => {
                    assert_eq!(io.kind(), io::ErrorKind::InvalidData);
                    assert!(
                        io.to_string().contains("cannot sign for itself"),
                        "unexpected error text: {io}"
                    );
                }
                other => panic!("expected ConnectionFailed(InvalidData), got {other:?}"),
            }
        }
        // phase: the_pubkey_mismatch_check_is_not_gated_on_debug_assertions
        // The refusal is a property of the build, not of
        // `debug_assertions`. Nothing here may be compiled out.
        {
            let real = SigningKey::from_bytes(&[4u8; 32]);
            let other = SigningKey::from_bytes(&[5u8; 32]);
            let record = serde_json::json!({
                "display_name": "",
                "avatar_color": [0, 0, 0],
                "threshold_identity": {
                    "single_device": true,
                    "group_public_key": other.verifying_key().to_bytes(),
                    "threshold": 1_u16,
                    "total_shares": 1_u16,
                    "share_index": 1_u16,
                    "secret_bytes": real.to_bytes().to_vec(),
                },
                "created_at": "2026-07-30T00:00:00Z",
                "schema_version": 1,
            });
            let bytes = serde_json::to_vec(&record).unwrap();
            assert!(
                load_from_bytes(&bytes).is_err(),
                "mismatched key material must be refused in every build profile"
            );
        }
    }

    #[test]
    fn threshold_shares_must_be_real_to_participate() {
        // phase: synthetic_threshold_bytes_are_rejected
        {
            let zero32: Vec<u8> = vec![0; 32];
            let record = serde_json::json!({
                "display_name": "Alice",
                "avatar_color": [0, 0, 0],
                "threshold_identity": {
                    "single_device": false,
                    "group_public_key": zero32,
                    "threshold": 2_u16,
                    "total_shares": 3_u16,
                    "share_index": 1_u16,
                    "secret_bytes": zero32,
                },
                "created_at": "2026-07-02T00:00:00Z",
                "schema_version": 1,
            });
            let bytes = serde_json::to_vec(&record).unwrap();
            assert!(load_from_bytes(&bytes).is_err());
        }
        // phase: real_threshold_share_restores_and_participates_in_broker_rounds
        {
            let mut rng = UnwrapErr(SysRng);
            let mut a = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
            let mut b = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
            let (mut session_a, round1_a) = a.begin_enrolment(1, 2, 2, &mut rng).unwrap();
            let (mut session_b, round1_b) = b.begin_enrolment(2, 2, 2, &mut rng).unwrap();
            let out_a = session_a.receive_round1(&[round1_b]).unwrap();
            let out_b = session_b.receive_round1(&[round1_a]).unwrap();
            session_a.receive_round2(&[out_b[&1].clone()]).unwrap();
            session_b.receive_round2(&[out_a[&2].clone()]).unwrap();
            a.adopt_dkg_outcome(session_a).unwrap();
            b.adopt_dkg_outcome(session_b).unwrap();

            let secret = a.export_device_secret().unwrap();
            let record = serde_json::json!({
                "display_name": "Alice",
                "avatar_color": [12, 34, 56],
                "threshold_identity": {
                    "single_device": false,
                    "group_public_key": a.group_public_key().as_bytes(),
                    "threshold": 2_u16,
                    "total_shares": 2_u16,
                    "share_index": 1_u16,
                    "secret_bytes": secret.as_slice(),
                },
                "created_at": "2026-07-02T00:00:00Z",
                "schema_version": 1,
            });
            let identity = load_from_bytes(&serde_json::to_vec(&record).unwrap()).unwrap();
            assert!(!identity.is_single_device());

            let message = b"confirmed operation";
            let signing_round_a = identity.threshold_sign_round1(&mut rng).unwrap();
            let signing_round_b = b.sign_round1(&mut rng).unwrap();
            let commitments = vec![
                signing_round_a.commitment.clone(),
                signing_round_b.commitment.clone(),
            ];
            let partial_a = identity
                .threshold_sign_round2(signing_round_a.nonces, message, &commitments)
                .unwrap();
            let partial_b = b
                .sign_round2_raw(signing_round_b.nonces, message, &commitments)
                .unwrap();
            let signature = identity
                .aggregate_threshold_signature(&[partial_a, partial_b])
                .unwrap();
            ed25519_dalek::VerifyingKey::from_bytes(a.group_public_key().as_bytes())
                .unwrap()
                .verify_strict(message, &ed25519_dalek::Signature::from_bytes(&signature))
                .unwrap();
        }
    }

    fn record_bytes(name: &str, seed: u8) -> Vec<u8> {
        let secret = [seed; 32];
        let key = SigningKey::from_bytes(&secret);
        serde_json::to_vec(&serde_json::json!({
            "display_name": name,
            "avatar_color": [10, 20, 30],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": key.verifying_key().to_bytes(),
                "threshold": 1_u16,
                "total_shares": 1_u16,
                "share_index": 1_u16,
                "secret_bytes": secret.to_vec(),
            },
            "created_at": "2026-08-30T00:00:00Z",
            "schema_version": 1,
        }))
        .unwrap()
    }

    fn assert_published_identity_keeps_secret(
        path: &std::path::Path,
        provider: &dyn SecretProvider,
        seed: u8,
    ) {
        let record = read_record(path).expect("published metadata must survive");
        assert_eq!(record.schema_version, 2);
        assert!(record.threshold_identity.secret_bytes.is_empty());
        let reference = record.secret_ref.as_ref().expect("published reference");
        assert_eq!(reference.backend, provider.backend_id());
        assert_eq!(
            reference.locator,
            hex::encode(
                SigningKey::from_bytes(&[seed; 32])
                    .verifying_key()
                    .to_bytes()
            )
        );
        let secret = provider
            .load(&reference.locator)
            .expect("published metadata must still resolve its provider secret");
        assert_eq!(secret.as_slice(), [seed; 32]);
        let identity = identity_from_record(record, Some(secret.as_slice())).unwrap();
        let message = b"publication failure must preserve the signing identity";
        let signature = identity.sign(message).unwrap();
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .verify_strict(
                message,
                &ed25519_dalek::Signature::from_slice(&signature).unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn secure_record_publication_cleanup_preserves_published_secret() {
        use tilo_files::native::atomic::PublicationState;
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};

        for (checkpoint, expected) in [
            (
                Checkpoint::AfterPublish,
                PublicationState::PublicationUnknown,
            ),
            (
                Checkpoint::BeforeCommit,
                PublicationState::PublishedDurabilityUnknown,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("personal.json");
            let provider = BrokerFileProvider::new(dir.path());
            write_private_atomic(&path, &record_bytes("Ada", 71)).unwrap();
            let error = with_failure(checkpoint, || {
                secure_identity_with_provider(dir.path(), "personal", &provider)
            })
            .expect_err("the real metadata writer must report the injected failure");
            assert_eq!(
                tilo_files::native::atomic::publication_state(&error),
                Some(expected)
            );
            assert_published_identity_keeps_secret(&path, &provider, 71);
        }
    }

    #[test]
    fn secure_record_publication_cleanup_before_publish_removes_new_secret() {
        use tilo_files::native::atomic::PublicationState;
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personal.json");
        let provider = BrokerFileProvider::new(dir.path());
        let original = record_bytes("Ada", 72);
        write_private_atomic(&path, &original).unwrap();
        let error = with_failure(Checkpoint::BeforePublish, || {
            secure_identity_with_provider(dir.path(), "personal", &provider)
        })
        .expect_err("the real metadata writer must report the injected failure");
        assert_eq!(
            tilo_files::native::atomic::publication_state(&error),
            Some(PublicationState::NotPublished)
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        let locator = hex::encode(SigningKey::from_bytes(&[72; 32]).verifying_key().to_bytes());
        assert_eq!(
            provider.load(&locator).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn import_publication_cleanup_preserves_second_write_and_secret() {
        use tilo_files::native::atomic::PublicationState;
        use tilo_files::native::atomic::test_support::{
            Checkpoint, with_failure_on_nth, with_observer,
        };
        for (checkpoint, expected) in [
            (
                Checkpoint::AfterPublish,
                PublicationState::PublicationUnknown,
            ),
            (
                Checkpoint::BeforeCommit,
                PublicationState::PublishedDurabilityUnknown,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let provider = BrokerFileProvider::new(dir.path());
            let completed = std::rc::Rc::new(std::cell::Cell::new(0));
            let observed = completed.clone();
            let error = with_observer(
                move |event| {
                    if event.checkpoint == Checkpoint::Committed {
                        observed.set(observed.get() + 1);
                    }
                },
                || {
                    with_failure_on_nth(checkpoint, 2, || {
                        import_identity_with_provider(
                            dir.path(),
                            "personal",
                            &record_bytes("Ada", 73),
                            &provider,
                        )
                    })
                },
            )
            .expect_err("v2 publication must report the injected failure");
            assert_eq!(completed.get(), 1, "v1 must commit before v2 fails");
            assert_eq!(
                tilo_files::native::atomic::publication_state(&error),
                Some(expected)
            );
            assert_published_identity_keeps_secret(
                &dir.path().join("personal.json"),
                &provider,
                73,
            );
        }
    }

    #[test]
    fn import_publication_cleanup_before_second_publish_removes_record_and_secret() {
        use tilo_files::native::atomic::PublicationState;
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure_on_nth};
        let dir = tempfile::tempdir().unwrap();
        let provider = BrokerFileProvider::new(dir.path());
        let error = with_failure_on_nth(Checkpoint::BeforePublish, 2, || {
            import_identity_with_provider(
                dir.path(),
                "personal",
                &record_bytes("Ada", 74),
                &provider,
            )
        })
        .unwrap_err();
        assert_eq!(
            tilo_files::native::atomic::publication_state(&error),
            Some(PublicationState::NotPublished)
        );
        assert!(!dir.path().join("personal.json").exists());
        let locator = hex::encode(SigningKey::from_bytes(&[74; 32]).verifying_key().to_bytes());
        assert_eq!(
            provider.load(&locator).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn import_publication_cleanup_unknown_before_rename_preserves_record_and_secret() {
        use tilo_files::native::atomic::PublicationState;
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure_on_nth};
        let dir = tempfile::tempdir().unwrap();
        let provider = BrokerFileProvider::new(dir.path());
        let original = record_bytes("Ada", 75);
        let error = with_failure_on_nth(Checkpoint::PublishSubmitted, 2, || {
            import_identity_with_provider(dir.path(), "personal", &original, &provider)
        })
        .unwrap_err();
        assert_eq!(
            tilo_files::native::atomic::publication_state(&error),
            Some(PublicationState::PublicationUnknown)
        );
        assert_eq!(
            fs::read(dir.path().join("personal.json")).unwrap(),
            original
        );
        let locator = hex::encode(SigningKey::from_bytes(&[75; 32]).verifying_key().to_bytes());
        assert_eq!(provider.load(&locator).unwrap().as_slice(), [75; 32]);
    }

    #[test]
    fn import_publication_cleanup_preferred_provider_does_not_fallback_after_publication() {
        use tilo_files::native::atomic::PublicationState;
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure_on_nth};
        for (checkpoint, expected) in [
            (
                Checkpoint::AfterPublish,
                PublicationState::PublicationUnknown,
            ),
            (
                Checkpoint::BeforeCommit,
                PublicationState::PublishedDurabilityUnknown,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let error = with_preferred_secret_service(false, || {
                with_failure_on_nth(checkpoint, 2, || {
                    import_identity(dir.path(), "personal", &record_bytes("Ada", 76))
                })
            })
            .expect_err("fallback must not hide an uncertain metadata publication");
            assert_eq!(
                tilo_files::native::atomic::publication_state(&error),
                Some(expected)
            );
            assert_published_identity_keeps_secret(
                &dir.path().join("personal.json"),
                &SecretServiceFixture::new(dir.path(), false),
                76,
            );
            assert!(
                !dir.path().join("secrets").exists(),
                "fallback provider must not be used"
            );
        }
    }

    /// Where the platform has no file fallback (macOS), a system store that
    /// fails fails the import, and leaves neither a record nor a file secret.
    fn assert_import_failed_closed(dir: &std::path::Path, imported: &io::Result<PathBuf>) {
        assert!(imported.is_err(), "{imported:?}");
        assert!(!dir.join("personal.json").exists());
        assert!(!dir.join("secrets").exists(), "a file fallback was written");
    }

    #[test]
    fn import_publication_cleanup_preferred_provider_may_fallback_before_publication() {
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure_on_nth};
        let dir = tempfile::tempdir().unwrap();
        let imported = with_preferred_secret_service(false, || {
            with_failure_on_nth(Checkpoint::BeforePublish, 2, || {
                import_identity(dir.path(), "personal", &record_bytes("Ada", 77))
            })
        });
        if !secure_store::FILE_FALLBACK {
            assert_import_failed_closed(dir.path(), &imported);
            return;
        }
        let path = imported.unwrap();
        assert_published_identity_keeps_secret(&path, &BrokerFileProvider::new(dir.path()), 77);
        let locator = hex::encode(SigningKey::from_bytes(&[77; 32]).verifying_key().to_bytes());
        assert_eq!(
            SecretServiceFixture::new(dir.path(), false)
                .load(&locator)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn import_publication_cleanup_provider_store_failure_is_prepublication() {
        let dir = tempfile::tempdir().unwrap();
        let error = import_identity_with_provider(
            dir.path(),
            "personal",
            &record_bytes("Ada", 78),
            &SecretServiceFixture::new(dir.path(), true),
        )
        .unwrap_err();
        assert!(tilo_files::native::atomic::publication_state(&error).is_none());
        assert!(!dir.path().join("personal.json").exists());

        let imported = with_preferred_secret_service(true, || {
            import_identity(dir.path(), "personal", &record_bytes("Ada", 78))
        });
        if !secure_store::FILE_FALLBACK {
            assert_import_failed_closed(dir.path(), &imported);
            return;
        }
        let path = imported.unwrap();
        assert_published_identity_keeps_secret(&path, &BrokerFileProvider::new(dir.path()), 78);
    }

    #[test]
    fn a_launch_refuses_an_unsecured_record_without_rewriting_it() {
        let dir = tempfile::tempdir().unwrap();
        ensure_private_dir(dir.path()).unwrap();
        let bytes = record_bytes("Ada", 27);
        let path = dir.path().join("primary.json");
        write_private_atomic(&path, &bytes).unwrap();

        let Err(error) = load_for_launch(&path) else {
            panic!("an inline key must be refused");
        };
        assert!(error.to_string().contains("Settings > Users"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!dir.path().join("secrets").exists());
        assert!(!dir.path().join(LOCK_FILE).exists());

        // Once secured, the same record launches.
        secure_identity_with_provider(dir.path(), "primary", &BrokerFileProvider::new(dir.path()))
            .unwrap();
        assert!(load_for_launch(&path).unwrap().sign(b"secured").is_ok());
    }

    #[test]
    fn the_identity_store_keeps_explicit_safe_records() {
        // phase: multi_identity_store_has_explicit_active_record
        {
            let dir = tempfile::tempdir().unwrap();
            import_identity(dir.path(), "personal", &record_bytes("Ada", 21)).unwrap();
            import_identity(dir.path(), "work", &record_bytes("Ada at Work", 22)).unwrap();
            set_active_identity(dir.path(), "work").unwrap();
            let listed = list_identities(dir.path()).unwrap();
            assert_eq!(listed.len(), 2);
            assert_eq!(listed[0].id, "work");
            assert!(listed[0].active);
            assert_eq!(listed[1].display_name, "Ada");
            let active = resolve_active_identity(dir.path()).unwrap();
            assert_eq!(active.file_name().unwrap(), "work.json");
        }
        // phase: identity_store_refuses_traversal_symlinks_and_active_removal
        {
            let dir = tempfile::tempdir().unwrap();
            assert!(import_identity(dir.path(), "../escape", &record_bytes("No", 23)).is_err());
            import_identity(dir.path(), "personal", &record_bytes("Ada", 24)).unwrap();
            import_identity(dir.path(), "work", &record_bytes("Work", 25)).unwrap();
            set_active_identity(dir.path(), "personal").unwrap();
            assert!(remove_identity(dir.path(), "personal").is_err());
            remove_identity(dir.path(), "work").unwrap();
            assert_eq!(list_identities(dir.path()).unwrap().len(), 1);
        }
        // phase: duplicate_public_identity_is_rejected_on_import
        {
            let dir = tempfile::tempdir().unwrap();
            let bytes = record_bytes("Ada", 27);
            import_identity(dir.path(), "personal", &bytes).unwrap();
            let error = import_identity(dir.path(), "duplicate", &bytes).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        }
        // phase: schema_two_public_member_projection_loads_without_becoming_a_signer
        {
            let dir = tempfile::tempdir().unwrap();
            let mut record: ondisk::IdentityRecord = serde_json::from_slice(
                &public_member_record("Member", [1, 2, 3], [9; 32]).unwrap(),
            )
            .unwrap();
            record.schema_version = 2;
            let path = dir.path().join("member.json");
            write_private_atomic(&path, &serde_json::to_vec(&record).unwrap()).unwrap();

            let identity = load_from_path(path).unwrap();
            assert_eq!(identity.group_public_key(), [9; 32]);
            assert!(identity.sign(b"must stay public-only").is_err());
        }
        // phase: a_joined_public_projection_is_left_as_written_when_secured
        {
            let dir = tempfile::tempdir().unwrap();
            ensure_private_dir(dir.path()).unwrap();
            let bytes = public_member_record("Member", [1, 2, 3], [9; 32]).unwrap();
            let path = dir.path().join("identity-joined.json");
            write_private_atomic(&path, &bytes).unwrap();

            // Sign-in and Settings may secure the selected record again.
            for _ in 0..2 {
                assert_eq!(
                    secure_identity(dir.path(), "identity-joined").unwrap(),
                    KeyProtection::DeviceCertificate
                );
            }

            assert_eq!(
                fs::read(&path).unwrap(),
                bytes,
                "a public projection must not gain a reference to an empty secret"
            );
            assert_eq!(load_from_path(path).unwrap().group_public_key(), [9; 32]);
            assert_eq!(
                list_identities(dir.path()).unwrap()[0].protection,
                KeyProtection::DeviceCertificate
            );
        }
        // phase: schema_two_missing_reference_rejects_private_or_malformed_shapes
        {
            let dir = tempfile::tempdir().unwrap();
            let template = public_member_record("Member", [1, 2, 3], [9; 32]).unwrap();
            let mut single: ondisk::IdentityRecord = serde_json::from_slice(&template).unwrap();
            single.schema_version = 2;
            single.threshold_identity.single_device = true;
            let single_path = dir.path().join("single.json");
            write_private_atomic(&single_path, &serde_json::to_vec(&single).unwrap()).unwrap();
            assert!(load_from_path(single_path).is_err());

            let mut malformed: ondisk::IdentityRecord = serde_json::from_slice(&template).unwrap();
            malformed.schema_version = 2;
            malformed.threshold_identity.threshold = 2;
            malformed.threshold_identity.total_shares = 2;
            let malformed_path = dir.path().join("malformed.json");
            write_private_atomic(&malformed_path, &serde_json::to_vec(&malformed).unwrap())
                .unwrap();
            assert!(load_from_path(malformed_path).is_err());
        }
        // phase: removing_one_legacy_duplicate_keeps_the_shared_provider_secret
        {
            let dir = tempfile::tempdir().unwrap();
            ensure_private_dir(dir.path()).unwrap();
            let bytes = record_bytes("Ada", 28);
            write_private_atomic(&dir.path().join("personal.json"), &bytes).unwrap();
            write_private_atomic(&dir.path().join("copy.json"), &bytes).unwrap();
            let provider = secure_store::fallback_provider(dir.path());
            secure_identity_with_provider(dir.path(), "personal", &provider).unwrap();
            secure_identity_with_provider(dir.path(), "copy", &provider).unwrap();

            remove_identity(dir.path(), "copy").unwrap();
            let remaining = load_from_path(dir.path().join("personal.json")).unwrap();
            let signature = remaining.sign(b"still usable").unwrap();
            assert_eq!(signature.len(), 64);
        }
    }
}

#[cfg(test)]
mod secret_service_error_bridge_tests {
    #[test]
    fn only_the_secret_provider_marker_is_classified_as_unavailable() {
        let error = super::secret_service_unavailable_error();
        assert!(super::is_secret_service_unavailable_io(&error));
        assert!(super::is_secret_service_unavailable(
            &super::NotAvailable::ConnectionFailed(error)
        ));
        assert!(!super::is_secret_service_unavailable_io(
            &std::io::Error::other("unrelated failure")
        ));
        assert!(!super::is_secret_service_unavailable_io(
            &std::io::Error::from(std::io::ErrorKind::NotFound)
        ));
    }
}
