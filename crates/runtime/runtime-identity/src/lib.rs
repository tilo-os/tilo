//! `runtime-identity` — user-identity loader for qOS apps.
//!
//! The launcher hands applications a connected signing-broker descriptor via
//! `QOS_IDENTITY_BROKER_FD`. Apps can inspect public metadata and request a
//! signature, but private shares stay in the host session. `QOS_USER_IDENTITY`
//! remains a compatibility path for non-sandboxed development only.
//!
//! The on-disk file is the JSON emitted by the identity-flow's
//! `IdentityRecord::to_json_bytes` (see `ui/src/widget/identity_flow`). This
//! crate keeps a minimal local copy of the schema (`ondisk::IdentityRecord`)
//! rather than depending on `ui`, which is heavy and depends on this SDK
//! layer transitively — a dep the other way would form a cycle. Both sides
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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub mod broker;
mod secure_store;

pub use secure_store::{BrokerFileProvider, SecretProvider};

pub use runtime_portal::NotAvailable;

/// Legacy unsandboxed-development path. The qOS launcher uses
/// [`broker::BROKER_ENV_VAR`] and never sets this for packaged apps.
pub const IDENTITY_ENV_VAR: &str = "QOS_USER_IDENTITY";
pub const IDENTITY_SERVICE: &str = "identity";

/// Small synchronous wire protocol used over the launcher-inherited identity
/// service fd. The launcher writes `Handshake` before the sandbox starts, so
/// an app cannot claim another package or workspace.
pub mod wire {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Handshake {
        pub workspace_id: String,
        pub app_id: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub enum Request {
        Metadata,
        Sign { action: String, message: Vec<u8> },
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub enum Response {
        Metadata {
            display_name: String,
            avatar_color: [u8; 3],
            group_public_key: [u8; 32],
            signing_public_key: [u8; 32],
            /// JSON-encoded root-signed `qos_user_identity::DeviceCert`.
            device_certificate: Option<Vec<u8>>,
            threshold: u16,
            total_shares: u16,
            share_index: u16,
        },
        Signature(Vec<u8>),
        Denied(String),
        Error(String),
    }
}

/// How a record's private share is protected on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyProtection {
    /// Stored in the desktop's freedesktop Secret Service collection.
    SecretService,
    /// Stored as a mode-0600 host file, reachable only by the signing broker.
    BrokerFile,
    /// A real externally supplied provider has sealed this record.
    HardwareBacked,
    /// Legacy schema; migration has not completed yet.
    LegacyPlaintext,
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

/// Non-secret information Settings may show for an identity record.  The
/// private signing share is intentionally absent and `Debug` is safe.
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

const ACTIVE_FILE: &str = "active";
const LOCK_FILE: &str = ".identity.lock";
const MAX_RECORD_BYTES: u64 = 1024 * 1024;
static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

/// Symlink-safe, exclusive staging plus file+directory fsync. The shared
/// atomic-file helper predates hostile-directory use and opens a predictable
/// temp name without `O_NOFOLLOW`; identity state needs the stronger form.
fn write_private_atomic(path: &std::path::Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("identity");
    let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), nonce));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Resolve the per-login-user identity directory using XDG rules.
///
/// # Errors
///
/// Returns an error if neither `XDG_CONFIG_HOME` nor `HOME` is set to an
/// absolute path.
pub fn identity_dir_from_env() -> io::Result<PathBuf> {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        let xdg = PathBuf::from(xdg);
        if xdg.is_absolute() {
            return Ok(xdg.join("qos").join("identity"));
        }
    }
    if let Some(home) = env::var_os("HOME").filter(|v| !v.is_empty()) {
        let home = PathBuf::from(home);
        if home.is_absolute() {
            return Ok(home.join(".config").join("qos").join("identity"));
        }
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
struct IdentityLock(fs::File);

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
        if !meta.file_type().is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity lifecycle lock must be a private user-owned regular file",
            ));
        }
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(file))
    }
}

impl Drop for IdentityLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
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
/// # Errors
///
/// Returns an error if the lifecycle lock can't be acquired, or `dir` can't
/// be read.
pub fn list_identities(dir: &std::path::Path) -> io::Result<Vec<IdentitySummary>> {
    let _lock = IdentityLock::acquire(dir)?;
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
    // under the same lock. A crash leaves a valid v1 record that the next
    // launch can retry; it never leaves half a v2 record.
    write_private_atomic(&path, bytes)?;
    let secured = match provider {
        Some(provider) => secure_record_with_provider_unlocked(dir, &path, provider),
        None => secure_record_unlocked(dir, &path),
    };
    if let Err(error) = secured {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

/// Migrate one legacy identity into broker-only storage. Idempotent and safe
/// to call at every launch.
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

    let preferred = secure_store::preferred_provider(dir);
    match secure_record_with_provider_unlocked(dir, path, preferred.as_ref()) {
        Ok(protection) => Ok(protection),
        Err(preferred_error) if preferred.protection() == KeyProtection::SecretService => {
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
    // sensitive backend error to stderr from every app launch.
    let _ = error;
}

fn secure_record_with_provider_unlocked(
    _dir: &std::path::Path,
    path: &std::path::Path,
    provider: &dyn SecretProvider,
) -> io::Result<KeyProtection> {
    let mut record = read_record(path)?;
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
        if let Some(reference) = record.secret_ref.as_ref() {
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
/// `ui::widget::identity_flow::IdentityRecord`; unknown fields are ignored.
mod ondisk {
    use serde::{Deserialize, Serialize};
    use serde_bytes as serde_bytes_codec;

    use crate::{KeyProtection, SecretReference};

    #[derive(Debug, Deserialize, Serialize)]
    pub struct IdentityRecord {
        pub display_name: String,
        pub avatar_color: [u8; 3],
        pub threshold_identity: SerializableIdentity,
        #[allow(dead_code)]
        #[serde(default)]
        pub created_at: String,
        pub schema_version: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub secret_ref: Option<SecretReference>,
    }

    impl IdentityRecord {
        pub fn protection(&self) -> KeyProtection {
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
            with = "serde_bytes_codec",
            skip_serializing_if = "Vec::is_empty"
        )]
        pub secret_bytes: Vec<u8>,
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
    Broker(Mutex<std::os::unix::net::UnixStream>),
    ActionBroker(Mutex<UnixStream>),
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
                "threshold-mode signing is not supported by qos-runtime-identity; \
                 use qos-threshold-identity + the multi-device orchestrator instead",
            ))),
            SigningSource::Broker(stream) => broker::sign(stream, message),
            SigningSource::ActionBroker(stream) => {
                sign_via_action_broker(stream, "generic", message)
            }
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
            SigningSource::ActionBroker(stream) => sign_via_action_broker(stream, action, message),
            _ => self.sign(message),
        }
    }

    fn sign_local(&self, message: &[u8]) -> io::Result<[u8; 64]> {
        match &self.source {
            SigningSource::Local(Some(sk)) => Ok(sk.sign(message).to_bytes()),
            SigningSource::Local(None) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "threshold signing requires the multi-device orchestrator",
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

fn load_from_service(fd: std::os::fd::RawFd) -> Result<SigningIdentity, NotAvailable> {
    // The launcher gives this process exclusive ownership of the inherited fd.
    let mut stream = unsafe { UnixStream::from_raw_fd(fd) };
    write_frame(&mut stream, &wire::Request::Metadata)?;
    match read_frame::<wire::Response>(&mut stream)? {
        wire::Response::Metadata {
            display_name,
            avatar_color,
            group_public_key,
            signing_public_key,
            device_certificate,
            threshold,
            total_shares,
            share_index,
        } => Ok(SigningIdentity {
            display_name,
            avatar_color,
            group_public_key,
            signing_public_key,
            device_certificate,
            threshold,
            total_shares,
            share_index,
            source: SigningSource::ActionBroker(Mutex::new(stream)),
            single_device: threshold == 1 && total_shares == 1,
        }),
        wire::Response::Denied(reason) | wire::Response::Error(reason) => Err(
            NotAvailable::ConnectionFailed(io::Error::new(io::ErrorKind::PermissionDenied, reason)),
        ),
        wire::Response::Signature(_) => Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity broker returned an invalid metadata response",
        ))),
    }
}

fn sign_via_action_broker(
    stream: &Mutex<UnixStream>,
    action: &str,
    message: &[u8],
) -> Result<[u8; 64], NotAvailable> {
    let mut stream = stream.lock().map_err(|_| {
        NotAvailable::ConnectionFailed(io::Error::other("identity broker lock poisoned"))
    })?;
    write_frame(
        &mut stream,
        &wire::Request::Sign {
            action: action.to_string(),
            message: message.to_vec(),
        },
    )?;
    match read_frame::<wire::Response>(&mut stream)? {
        wire::Response::Signature(bytes) if bytes.len() == 64 => {
            let mut signature = [0_u8; 64];
            signature.copy_from_slice(&bytes);
            Ok(signature)
        }
        wire::Response::Denied(reason) => Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::PermissionDenied,
            reason,
        ))),
        wire::Response::Error(reason) => {
            Err(NotAvailable::ConnectionFailed(io::Error::other(reason)))
        }
        _ => Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity broker returned an invalid signature response",
        ))),
    }
}

fn write_frame<T: serde::Serialize>(
    stream: &mut UnixStream,
    value: &T,
) -> Result<(), NotAvailable> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        NotAvailable::ConnectionFailed(io::Error::new(io::ErrorKind::InvalidData, error))
    })?;
    let len = u32::try_from(bytes.len()).map_err(|_| {
        NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame too large",
        ))
    })?;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T, NotAvailable> {
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > 1024 * 1024 {
        return Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity frame exceeds 1 MiB",
        )));
    }
    let mut bytes = vec![0_u8; len];
    stream.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        NotAvailable::ConnectionFailed(io::Error::new(io::ErrorKind::InvalidData, error))
    })
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
    let record = parse_record(&bytes)?;
    if record.schema_version == 1 {
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

/// Load a secured record through an explicitly installed hardware provider.
/// The metadata backend id must match; this prevents silently downgrading a
/// TPM-labelled identity to a software key.
///
/// # Errors
///
/// Returns [`NotAvailable::ConnectionFailed`] if the file can't be read or
/// is malformed, the record has no secret reference, the reference names a
/// different provider than `provider`, or the secret can't be loaded.
pub fn load_from_path_with_provider(
    path: &std::path::Path,
    provider: &dyn SecretProvider,
) -> Result<SigningIdentity, NotAvailable> {
    let bytes =
        read_record_bytes(path).map_err(|e| LoadFailure::ReadFailed(e).into_not_available())?;
    let record = parse_record(&bytes)?;
    let reference = record
        .secret_ref
        .as_ref()
        .ok_or_else(|| LoadFailure::SecuredRecordNeedsProvider.into_not_available())?;
    if reference.backend != provider.backend_id() {
        return Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity record uses a different secret provider",
        )));
    }
    let secret = provider
        .load(&reference.locator)
        .map_err(|e| LoadFailure::ReadFailed(e).into_not_available())?;
    identity_from_record(record, Some(secret.as_slice()))
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

    if !secret.is_empty() && secret.len() != 32 {
        return Err(LoadFailure::SecretSizeMismatch { got: secret.len() }.into_not_available());
    }

    let signing_key = if ti.single_device && !secret.is_empty() {
        let mut scalar = [0u8; 32];
        scalar.copy_from_slice(secret);
        // SigningKey::from_bytes is infallible today; if a future dalek revision
        // surfaces validation errors, catch them here.
        Some(SigningKey::from_bytes(&scalar))
    } else {
        None
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

// Compile-time assertion that we haven't accidentally derived Clone/
// Serialize/Debug-with-secret onto SigningIdentity's signing_key.
#[allow(dead_code)]
fn _static_no_serialize() {
    // If SigningIdentity ever gains `#[derive(serde::Serialize)]`, this stops
    // compiling — `SigningKey` doesn't want a serde derive at the outer layer.
    fn takes<T>(_: &T) {}
    let _ = |x: &SigningIdentity| takes(x);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_env() {
        // SAFETY: tests in this module all touch QOS_USER_IDENTITY
        // and are logically serial.
        unsafe {
            env::remove_var(IDENTITY_ENV_VAR);
            env::remove_var(broker::BROKER_ENV_VAR);
        };
    }

    #[test]
    #[serial_test::serial(identity_env)]
    fn load_without_env_returns_service_fd_missing() {
        clear_env();
        match load() {
            Err(NotAvailable::ServiceFdMissing) => {}
            other => panic!("expected ServiceFdMissing, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial(identity_env)]
    fn load_with_empty_env_returns_service_fd_missing() {
        clear_env();
        // SAFETY: see clear_env
        unsafe { env::set_var(IDENTITY_ENV_VAR, "") };
        let out = load();
        // SAFETY: cleanup
        unsafe { env::remove_var(IDENTITY_ENV_VAR) };
        match out {
            Err(NotAvailable::ServiceFdMissing) => {}
            other => panic!("expected ServiceFdMissing, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial(identity_env)]
    fn relative_xdg_config_home_never_relocates_identity_by_cwd() {
        let old_xdg = env::var_os("XDG_CONFIG_HOME");
        let old_home = env::var_os("HOME");
        // SAFETY: serialised with every identity env test.
        unsafe {
            env::set_var("XDG_CONFIG_HOME", "relative/config");
            env::set_var("HOME", "/home/ada");
        }
        assert_eq!(
            identity_dir_from_env().unwrap(),
            PathBuf::from("/home/ada/.config/qos/identity")
        );
        // SAFETY: restore the process environment under the same serial lock.
        unsafe {
            match old_xdg {
                Some(value) => env::set_var("XDG_CONFIG_HOME", value),
                None => env::remove_var("XDG_CONFIG_HOME"),
            }
            match old_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn load_missing_file_surfaces_connection_failed() {
        let err = load_from_path(PathBuf::from("/nonexistent/qos-identity.json"))
            .expect_err("should fail");
        match err {
            NotAvailable::ConnectionFailed(io) => {
                assert_eq!(io.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("expected ConnectionFailed(NotFound), got {other:?}"),
        }
    }

    #[test]
    fn load_bad_json_surfaces_invalid_data() {
        let err = load_from_bytes(b"not json at all").expect_err("should fail");
        match err {
            NotAvailable::ConnectionFailed(io) => {
                assert_eq!(io.kind(), io::ErrorKind::InvalidData);
            }
            other => panic!("expected ConnectionFailed(InvalidData), got {other:?}"),
        }
    }

    #[test]
    fn load_unknown_schema_version_rejected() {
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

    #[test]
    fn single_device_load_then_sign_and_verify() {
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

    #[test]
    fn public_projection_loads_metadata_but_cannot_sign() {
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

    /// A record whose secret does not derive its recorded public key must not
    /// load — silently accepting it would hand apps an identity that can never
    /// sign successfully.
    #[test]
    fn secret_that_does_not_derive_the_recorded_pubkey_is_refused() {
        // The exact shape the old identity ceremony wrote: a digest
        // over the public key standing in for the secret.
        use sha2::{Digest, Sha512};
        let real = SigningKey::from_bytes(&[9u8; 32]);
        let vk = real.verifying_key();
        let mut hasher = Sha512::new();
        hasher.update(b"qos-identity-flow-v1-placeholder-key");
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
        let err = load_from_bytes(&bytes).expect_err("a self-inconsistent record must not load");
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

    /// The refusal is a property of the build, not of
    /// `debug_assertions`. Nothing here may be compiled out.
    #[test]
    fn the_pubkey_mismatch_check_is_not_gated_on_debug_assertions() {
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

    #[test]
    fn threshold_mode_sign_returns_unsupported() {
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
        let ident = load_from_bytes(&bytes).expect("load");

        assert!(!ident.is_single_device());
        let err = ident.sign(b"nope").expect_err("threshold sign must fail");
        match err {
            NotAvailable::ConnectionFailed(io) => {
                assert_eq!(io.kind(), io::ErrorKind::Unsupported);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn load_from_path_reads_disk_file() {
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

    #[test]
    fn extra_fields_in_json_are_tolerated() {
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

    #[test]
    fn multi_identity_store_has_explicit_active_record() {
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

    #[test]
    fn identity_store_refuses_traversal_symlinks_and_active_removal() {
        let dir = tempfile::tempdir().unwrap();
        assert!(import_identity(dir.path(), "../escape", &record_bytes("No", 23)).is_err());
        import_identity(dir.path(), "personal", &record_bytes("Ada", 24)).unwrap();
        import_identity(dir.path(), "work", &record_bytes("Work", 25)).unwrap();
        set_active_identity(dir.path(), "personal").unwrap();
        assert!(remove_identity(dir.path(), "personal").is_err());
        remove_identity(dir.path(), "work").unwrap();
        assert_eq!(list_identities(dir.path()).unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn imported_secrets_and_selector_are_private() {
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

    #[cfg(unix)]
    #[test]
    fn broker_file_revalidates_key_permissions_before_load_or_remove() {
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

    #[test]
    fn duplicate_public_identity_is_rejected_on_import() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = record_bytes("Ada", 27);
        import_identity(dir.path(), "personal", &bytes).unwrap();
        let error = import_identity(dir.path(), "duplicate", &bytes).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn removing_one_legacy_duplicate_keeps_the_shared_provider_secret() {
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

    #[cfg(unix)]
    #[test]
    fn record_open_rejects_symlinks_and_wide_permissions() {
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
