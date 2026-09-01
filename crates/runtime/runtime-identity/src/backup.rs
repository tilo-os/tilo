//! Encrypted portable backups of a single-device identity.
//!
//! Runtime identity metadata intentionally omits the signing secret after it
//! is secured in Secret Service or the broker-file fallback.  A portable
//! backup therefore has to run inside this crate: it can authenticate the
//! metadata, retrieve the provider-held secret, and immediately seal the
//! complete legacy record.  No public API here returns that record or its
//! secret bytes.

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, Zeroizing};

use super::{IdentityLock, MAX_RECORD_BYTES, identity_from_record};

const MAGIC: &[u8; 8] = b"QOSIBK\0\x01";
const FORMAT_VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
const AEAD_TAG_LEN: usize = 16;
const HEADER_LEN: usize = MAGIC.len() + 1 + 4 + 4 + 4 + SALT_LEN + NONCE_LEN;
const MAX_BACKUP_BYTES: u64 = MAX_RECORD_BYTES + HEADER_LEN as u64 + AEAD_TAG_LEN as u64;

// Immutable format-v1 Argon2id profile. These constants must never be raised
// in place: a stronger future profile gets a new format version so every v1
// backup remains readable. Parameters are encoded and authenticated too, but
// exact matching prevents hostile envelopes from selecting a denial-of-service
// workload.
const ARGON_MEMORY_KIB: u32 = 19 * 1024;
const ARGON_ITERATIONS: u32 = 2;
const ARGON_LANES: u32 = 1;
const MIN_PASSPHRASE_BYTES: usize = 12;

/// Non-secret facts about a successfully written, verified portable backup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableBackupInfo {
    /// The root public key the backup restores. This is safe to display as a
    /// fingerprint and lets a host confirm it backed up the intended identity.
    pub group_public_key: [u8; 32],
    /// Exact number of envelope bytes published at the destination.
    pub bytes_written: u64,
}

/// Encrypt the identity at `identity_path` into a new portable backup.
///
/// `destination` is created exclusively: an existing path, including a
/// symlink, is never replaced. The file is staged in the destination's parent
/// and atomically published where the filesystem supports a safe no-replace
/// primitive. It is then opened again, decrypted, and cryptographically
/// checked before this function returns success.
///
/// The signing secret is retrieved only from the configured provider and is
/// never returned to the caller. Only current 1-of-1 identities are portable
/// in this format; threshold recovery needs its own multi-device ceremony.
///
/// # Errors
///
/// Returns an error when the identity/provider is invalid or unavailable, the
/// passphrase is shorter than 12 bytes, the destination cannot be exclusively
/// published, or the mandatory read-back verification fails. On a verification
/// failure this removes only an unchanged, regular destination containing the
/// exact envelope it just wrote.
pub fn export_portable_backup(
    identity_path: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    passphrase: &[u8],
) -> io::Result<PortableBackupInfo> {
    let identity_path = identity_path.as_ref();
    let destination = destination.as_ref();
    let (record, expected_key) = portable_record(identity_path)?;
    let envelope = seal_record(record.as_slice(), passphrase)?;
    write_new_backup(destination, envelope.as_slice())?;
    checked_readback(destination, passphrase, expected_key, envelope.as_slice())
}

/// Authenticate, decrypt, and validate a portable backup without restoring
/// it. This is the read-back operation an OOBE must require before allowing a
/// user to continue.
///
/// # Errors
///
/// Returns an error when the envelope is malformed, exceeds the size limit,
/// was altered, has the wrong passphrase, or contains an invalid/non-portable
/// identity record.
pub fn verify_portable_backup(
    backup_path: impl AsRef<Path>,
    passphrase: &[u8],
) -> io::Result<PortableBackupInfo> {
    let backup_path = backup_path.as_ref();
    let bytes = read_backup_bytes(backup_path)?;
    let record = open_record(bytes.as_slice(), passphrase)?;
    let identity = super::load_from_bytes(record.as_slice()).map_err(not_available_as_io)?;
    if !identity.is_single_device() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "threshold identities need the multi-device recovery ceremony",
        ));
    }
    Ok(PortableBackupInfo {
        group_public_key: identity.group_public_key(),
        bytes_written: u64::try_from(bytes.len()).map_err(|_| invalid_backup())?,
    })
}

/// Import an encrypted portable backup directly into a runtime identity
/// directory. The decrypted portable record is kept internal and is handed
/// straight to [`super::import_identity`], which validates and moves its key
/// back into the configured secret provider.
///
/// This is intentionally the only restoration API: callers can restore an
/// identity but cannot ask this crate to hand them a raw signing share.
///
/// # Errors
///
/// Returns an error when the backup cannot be authenticated and decoded, the
/// record is not a portable single-device identity, or the normal runtime
/// identity import/provider lifecycle rejects the destination.
pub fn import_portable_backup(
    backup_path: impl AsRef<Path>,
    passphrase: &[u8],
    identity_dir: impl AsRef<Path>,
    id: &str,
) -> io::Result<PathBuf> {
    import_portable_backup_inner(
        backup_path.as_ref(),
        passphrase,
        identity_dir.as_ref(),
        id,
        None,
        None,
    )
}

/// Restore only when the authenticated envelope contains `expected_key`.
/// Trusted registry code uses this after deriving a stable local id from a
/// prior verification, closing the replace-between-verify-and-import race.
///
/// # Errors
///
/// Returns the same errors as [`import_portable_backup`], and also rejects an
/// otherwise valid backup whose authenticated public key differs from
/// `expected_key`.
pub fn import_portable_backup_matching(
    backup_path: impl AsRef<Path>,
    passphrase: &[u8],
    identity_dir: impl AsRef<Path>,
    id: &str,
    expected_key: [u8; 32],
) -> io::Result<PathBuf> {
    import_portable_backup_inner(
        backup_path.as_ref(),
        passphrase,
        identity_dir.as_ref(),
        id,
        Some(expected_key),
        None,
    )
}

/// Restore a portable backup through an explicit provider selected by trusted
/// host code (for example, a hardware-backed identity provider).
///
/// Like [`import_portable_backup`], this never returns decrypted record or key
/// bytes. The provider is only used after envelope authentication and
/// cryptographic identity validation succeed.
///
/// # Errors
///
/// Returns the same errors as [`import_portable_backup`], plus an error when
/// `provider` rejects storing the recovered signing share. A failed provider
/// import removes its staged public metadata before returning.
pub fn import_portable_backup_with_provider(
    backup_path: impl AsRef<Path>,
    passphrase: &[u8],
    identity_dir: impl AsRef<Path>,
    id: &str,
    provider: &dyn super::SecretProvider,
) -> io::Result<PathBuf> {
    import_portable_backup_inner(
        backup_path.as_ref(),
        passphrase,
        identity_dir.as_ref(),
        id,
        None,
        Some(provider),
    )
}

fn import_portable_backup_inner(
    backup_path: &Path,
    passphrase: &[u8],
    identity_dir: &Path,
    id: &str,
    expected_key: Option<[u8; 32]>,
    provider: Option<&dyn super::SecretProvider>,
) -> io::Result<PathBuf> {
    let bytes = read_backup_bytes(backup_path)?;
    let record = open_record(bytes.as_slice(), passphrase)?;
    let identity = super::load_from_bytes(record.as_slice()).map_err(not_available_as_io)?;
    if !identity.is_single_device() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "threshold identities need the multi-device recovery ceremony",
        ));
    }
    if expected_key.is_some_and(|expected| identity.group_public_key() != expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "portable backup identity changed after verification",
        ));
    }
    match provider {
        Some(provider) => {
            super::import_identity_with_provider(identity_dir, id, record.as_slice(), provider)
        }
        None => super::import_identity(identity_dir, id, record.as_slice()),
    }
}

fn checked_readback(
    destination: &Path,
    passphrase: &[u8],
    expected_key: [u8; 32],
    expected_envelope: &[u8],
) -> io::Result<PortableBackupInfo> {
    match verify_portable_backup(destination, passphrase) {
        Ok(info) if info.group_public_key == expected_key => Ok(info),
        Ok(_) => {
            let _ = remove_exact_new_backup(destination, expected_envelope);
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "portable backup read-back does not match the selected identity",
            ))
        }
        Err(error) => {
            let _ = remove_exact_new_backup(destination, expected_envelope);
            Err(error)
        }
    }
}

/// Recover the complete schema-1 record internally. The lifecycle lock stops
/// removal/provider migration from racing a provider lookup and serialization.
fn portable_record(path: &Path) -> io::Result<(Zeroizing<Vec<u8>>, [u8; 32])> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity path has no parent directory",
        )
    })?;
    let _lock = IdentityLock::acquire(dir)?;
    let mut record = super::read_record(path)?;

    let secret = match record.schema_version {
        1 => {
            let bytes = super::read_record_bytes(path)?;
            let identity = super::load_from_bytes(&bytes).map_err(not_available_as_io)?;
            if !identity.is_single_device() {
                record.threshold_identity.secret_bytes.zeroize();
                return Err(threshold_backup_error());
            }
            let group_public_key = identity.group_public_key();
            record.threshold_identity.secret_bytes.zeroize();
            return Ok((Zeroizing::new(bytes), group_public_key));
        }
        2 => {
            let reference = record.secret_ref.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "secured identity has no secret reference",
                )
            })?;
            let provider = super::secure_store::provider_for(dir, reference)?;
            provider.load(&reference.locator)?
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity has an unsupported schema version",
            ));
        }
    };

    let identity = identity_from_record(record_for_validation(&record), Some(secret.as_slice()))
        .map_err(not_available_as_io)?;
    if !identity.is_single_device() {
        return Err(threshold_backup_error());
    }
    let group_public_key = identity.group_public_key();

    record.threshold_identity.secret_bytes = secret.to_vec();
    record.schema_version = 1;
    record.secret_ref = None;
    let portable = serde_json::to_vec_pretty(&record)
        .map(Zeroizing::new)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    record.threshold_identity.secret_bytes.zeroize();
    portable.map(|bytes| (bytes, group_public_key))
}

/// `identity_from_record` takes ownership, but validation must not consume the
/// record we are about to serialize. This duplicates public metadata and the
/// zero-length schema-2 secret field only; the provider-held key remains in a
/// `Zeroizing` buffer owned by the caller.
fn record_for_validation(record: &super::ondisk::IdentityRecord) -> super::ondisk::IdentityRecord {
    super::ondisk::IdentityRecord {
        display_name: record.display_name.clone(),
        avatar_color: record.avatar_color,
        threshold_identity: super::ondisk::SerializableIdentity {
            single_device: record.threshold_identity.single_device,
            group_public_key: record.threshold_identity.group_public_key,
            threshold: record.threshold_identity.threshold,
            total_shares: record.threshold_identity.total_shares,
            share_index: record.threshold_identity.share_index,
            secret_bytes: Vec::new(),
        },
        created_at: record.created_at.clone(),
        schema_version: record.schema_version,
        secret_ref: record.secret_ref.clone(),
    }
}

fn threshold_backup_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "threshold identities need the multi-device recovery ceremony",
    )
}

fn seal_record(record: &[u8], passphrase: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    if record.len() as u64 > MAX_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity record exceeds the portable backup limit",
        ));
    }
    validate_passphrase(passphrase)?;
    let mut salt = [0_u8; SALT_LEN];
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::getrandom(&mut salt).map_err(random_error)?;
    getrandom::getrandom(&mut nonce).map_err(random_error)?;

    let mut key = derive_key(
        passphrase,
        &salt,
        ARGON_MEMORY_KIB,
        ARGON_ITERATIONS,
        ARGON_LANES,
    )?;
    let header = encode_header(
        &salt,
        &nonce,
        ARGON_MEMORY_KIB,
        ARGON_ITERATIONS,
        ARGON_LANES,
    );
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_slice())
        .map_err(|_| io::Error::other("could not initialize portable backup encryption"))?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: record,
                aad: &header,
            },
        )
        .map_err(|_| io::Error::other("could not encrypt portable backup"))?;
    key.zeroize();

    let capacity = header
        .len()
        .checked_add(ciphertext.len())
        .ok_or_else(|| io::Error::other("portable backup is too large"))?;
    let mut out = Zeroizing::new(Vec::with_capacity(capacity));
    out.extend_from_slice(&header);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

#[allow(
    clippy::indexing_slicing,
    reason = "parse_header first proves blob contains the complete fixed-size header"
)]
fn open_record(blob: &[u8], passphrase: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    validate_passphrase(passphrase)?;
    let header = parse_header(blob)?;
    let ciphertext = &blob[HEADER_LEN..];
    if ciphertext.len() < AEAD_TAG_LEN {
        return Err(invalid_backup());
    }
    let mut key = derive_key(
        passphrase,
        &header.salt,
        header.memory_kib,
        header.iterations,
        header.lanes,
    )?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_slice()).map_err(|_| invalid_backup())?;
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&header.nonce),
            Payload {
                msg: ciphertext,
                aad: &blob[..HEADER_LEN],
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| invalid_backup());
    key.zeroize();
    let plaintext = plaintext?;
    if plaintext.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid_backup());
    }
    Ok(plaintext)
}

fn derive_key(
    passphrase: &[u8],
    salt: &[u8; SALT_LEN],
    memory_kib: u32,
    iterations: u32,
    lanes: u32,
) -> io::Result<Zeroizing<[u8; KEY_LEN]>> {
    if memory_kib != ARGON_MEMORY_KIB || iterations != ARGON_ITERATIONS || lanes != ARGON_LANES {
        return Err(invalid_backup());
    }
    let params =
        Params::new(memory_kib, iterations, lanes, Some(KEY_LEN)).map_err(|_| invalid_backup())?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0_u8; KEY_LEN]);
    argon
        .hash_password_into(passphrase, salt, key.as_mut_slice())
        .map_err(|_| io::Error::other("could not derive portable backup key"))?;
    Ok(key)
}

fn validate_passphrase(passphrase: &[u8]) -> io::Result<()> {
    if passphrase.len() < MIN_PASSPHRASE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "portable backup passphrase must be at least 12 bytes",
        ));
    }
    Ok(())
}

struct Header {
    memory_kib: u32,
    iterations: u32,
    lanes: u32,
    salt: [u8; SALT_LEN],
    nonce: [u8; NONCE_LEN],
}

#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "every offset is bounded by fixed HEADER_LEN components"
)]
fn encode_header(
    salt: &[u8; SALT_LEN],
    nonce: &[u8; NONCE_LEN],
    memory_kib: u32,
    iterations: u32,
    lanes: u32,
) -> [u8; HEADER_LEN] {
    let mut out = [0_u8; HEADER_LEN];
    let mut at = 0;
    out[..MAGIC.len()].copy_from_slice(MAGIC);
    at += MAGIC.len();
    out[at] = FORMAT_VERSION;
    at += 1;
    for value in [memory_kib, iterations, lanes] {
        out[at..at + 4].copy_from_slice(&value.to_le_bytes());
        at += 4;
    }
    out[at..at + SALT_LEN].copy_from_slice(salt);
    at += SALT_LEN;
    out[at..at + NONCE_LEN].copy_from_slice(nonce);
    out
}

#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "the initial HEADER_LEN check bounds every fixed-format offset"
)]
fn parse_header(blob: &[u8]) -> io::Result<Header> {
    if blob.len() < HEADER_LEN || &blob[..MAGIC.len()] != MAGIC {
        return Err(invalid_backup());
    }
    let mut at = MAGIC.len();
    if blob[at] != FORMAT_VERSION {
        return Err(invalid_backup());
    }
    at += 1;
    let read_u32 = |offset: &mut usize| {
        let mut raw = [0_u8; 4];
        raw.copy_from_slice(&blob[*offset..*offset + 4]);
        *offset += 4;
        u32::from_le_bytes(raw)
    };
    let memory_kib = read_u32(&mut at);
    let iterations = read_u32(&mut at);
    let lanes = read_u32(&mut at);
    let mut salt = [0_u8; SALT_LEN];
    salt.copy_from_slice(&blob[at..at + SALT_LEN]);
    at += SALT_LEN;
    let mut nonce = [0_u8; NONCE_LEN];
    nonce.copy_from_slice(&blob[at..at + NONCE_LEN]);
    Ok(Header {
        memory_kib,
        iterations,
        lanes,
        salt,
        nonce,
    })
}

fn write_new_backup(destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "portable backup destination has no parent directory",
        )
    })?;
    let parent_meta = fs::symlink_metadata(parent)?;
    if !parent_meta.file_type().is_dir() || parent_meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "portable backup destination parent must be a real directory",
        ));
    }

    let temp = new_temp_path(parent)?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        publish_new_file(&temp, destination)?;
        sync_directory(parent);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Remove a failed read-back's target only when it is still the exact regular
/// file this export published. `symlink_metadata` refuses a substituted link;
/// comparing the encrypted envelope avoids deleting a later file created at
/// the same path by another process.
fn remove_exact_new_backup(destination: &Path, expected: &[u8]) -> io::Result<()> {
    let metadata = fs::symlink_metadata(destination)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to remove a substituted portable backup destination",
        ));
    }
    let actual = read_backup_bytes(destination)?;
    if actual.as_slice() != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to remove a changed portable backup destination",
        ));
    }
    fs::remove_file(destination)?;
    if let Some(parent) = destination.parent() {
        sync_directory(parent);
    }
    Ok(())
}

fn new_temp_path(parent: &Path) -> io::Result<PathBuf> {
    for _ in 0..16 {
        let mut nonce = [0_u8; 16];
        getrandom::getrandom(&mut nonce).map_err(random_error)?;
        let candidate = parent.join(format!(".qos-identity-backup-{}.tmp", hex::encode(nonce)));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate an exclusive portable backup staging file",
    ))
}

/// Atomically publish without replacement. Linux's `renameat2` is the normal
/// path. Filesystems that do not implement it may still support hard links;
/// that fallback retains no-replace semantics and is followed by a directory
/// fsync. A filesystem supporting neither is refused rather than risking an
/// attacker swapping in a destination between a preflight check and rename.
fn publish_new_file(temp: &Path, destination: &Path) -> io::Result<()> {
    let source = CString::new(temp.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "backup path contains a NUL byte",
        )
    })?;
    let target = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "backup path contains a NUL byte",
        )
    })?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP)
    ) {
        fs::hard_link(temp, destination)?;
        fs::remove_file(temp)?;
        return Ok(());
    }
    Err(error)
}

fn sync_directory(path: &Path) {
    if let Ok(dir) = fs::File::open(path) {
        let _ = dir.sync_all();
    }
}

fn read_backup_bytes(path: &Path) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_BACKUP_BYTES {
        return Err(invalid_backup());
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        usize::try_from(metadata.len()).unwrap_or(0),
    ));
    (&mut file)
        .take(MAX_BACKUP_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BACKUP_BYTES {
        return Err(invalid_backup());
    }
    Ok(bytes)
}

fn invalid_backup() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "portable backup could not be authenticated",
    )
}

fn random_error(error: getrandom::Error) -> io::Error {
    io::Error::other(format!(
        "operating-system randomness is unavailable: {error}"
    ))
}

fn not_available_as_io(error: super::NotAvailable) -> io::Error {
    match error {
        super::NotAvailable::ConnectionFailed(error) => error,
        other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct FailingProvider;

    impl super::super::SecretProvider for FailingProvider {
        fn protection(&self) -> super::super::KeyProtection {
            super::super::KeyProtection::HardwareBacked
        }

        fn backend_id(&self) -> &'static str {
            "test-failing-provider"
        }

        fn store(&self, _: &str, _: &[u8]) -> io::Result<()> {
            Err(io::Error::other("deliberate provider failure"))
        }

        fn load(&self, _: &str) -> io::Result<Zeroizing<Vec<u8>>> {
            Err(io::Error::other("deliberate provider failure"))
        }

        fn remove(&self, _: &str) -> io::Result<()> {
            Ok(())
        }
    }

    fn record_bytes(name: &str, seed: u8) -> Vec<u8> {
        let secret = [seed; 32];
        let key = ed25519_dalek::SigningKey::from_bytes(&secret);
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
            "created_at": "2026-09-01T00:00:00Z",
            "schema_version": 1,
        }))
        .expect("record")
    }

    fn secured_identity(dir: &Path, id: &str, seed: u8) -> PathBuf {
        let bytes = record_bytes("Ada", seed);
        let provider = super::super::BrokerFileProvider::new(dir);
        super::super::import_identity_with_provider(dir, id, &bytes, &provider).expect("import");
        dir.join(format!("{id}.json"))
    }

    #[test]
    fn encrypted_portable_backup_round_trips_and_imports() {
        let source = tempfile::tempdir().expect("source");
        let identity = secured_identity(source.path(), "personal", 71);
        let backup_dir = tempfile::tempdir().expect("backup dir");
        let backup = backup_dir.path().join("ada.qos-identity-backup");
        let passphrase = b"correct horse battery staple";

        let info = export_portable_backup(&identity, &backup, passphrase).expect("export");
        assert_eq!(info.bytes_written, fs::metadata(&backup).unwrap().len());
        assert_eq!(verify_portable_backup(&backup, passphrase).unwrap(), info);

        let restored = tempfile::tempdir().expect("restore");
        let imported = import_portable_backup(&backup, passphrase, restored.path(), "restored")
            .expect("restore");
        let identity = super::super::load_from_path(imported).expect("restored identity loads");
        assert_eq!(identity.group_public_key(), info.group_public_key);
    }

    #[test]
    fn backup_is_not_metadata_copy_and_wrong_passphrase_fails_closed() {
        let source = tempfile::tempdir().expect("source");
        let identity = secured_identity(source.path(), "personal", 72);
        let backup_dir = tempfile::tempdir().expect("backup dir");
        let backup = backup_dir.path().join("ada.qos-identity-backup");
        export_portable_backup(&identity, &backup, b"the passphrase").expect("export");

        let bytes = fs::read(&backup).expect("read envelope");
        assert!(!bytes.starts_with(b"{"), "backup is an encrypted envelope");
        assert!(!bytes.windows(32).any(|part| part == [72_u8; 32]));
        assert!(verify_portable_backup(&backup, b"wrong passphrase").is_err());
        assert!(
            import_portable_backup(&backup, b"wrong passphrase", source.path(), "wrong").is_err()
        );
    }

    #[test]
    fn matching_import_rejects_a_backup_replaced_after_verification() {
        let source = tempfile::tempdir().expect("source");
        let first = secured_identity(source.path(), "first", 75);
        let second = secured_identity(source.path(), "second", 76);
        let backup_dir = tempfile::tempdir().expect("backups");
        let first_backup = backup_dir.path().join("first.backup");
        let second_backup = backup_dir.path().join("second.backup");
        let passphrase = b"correct horse battery staple";
        let expected = export_portable_backup(&first, &first_backup, passphrase)
            .expect("first export")
            .group_public_key;
        export_portable_backup(&second, &second_backup, passphrase).expect("second export");

        let restored = tempfile::tempdir().expect("restore");
        let error = import_portable_backup_matching(
            &second_backup,
            passphrase,
            restored.path(),
            "restored",
            expected,
        )
        .expect_err("a different authenticated identity must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!restored.path().join("restored.json").exists());
    }

    #[test]
    fn tampering_and_existing_destinations_are_refused() {
        let source = tempfile::tempdir().expect("source");
        let identity = secured_identity(source.path(), "personal", 73);
        let backup_dir = tempfile::tempdir().expect("backup dir");
        let backup = backup_dir.path().join("ada.qos-identity-backup");
        export_portable_backup(&identity, &backup, b"the passphrase").expect("export");

        let mut bytes = fs::read(&backup).expect("read");
        let at = bytes.len() - 1;
        bytes[at] ^= 0x80;
        fs::write(&backup, bytes).expect("tamper");
        assert!(verify_portable_backup(&backup, b"the passphrase").is_err());

        let existing = backup_dir.path().join("existing.backup");
        fs::write(&existing, b"do not replace").expect("existing");
        assert_eq!(
            export_portable_backup(&identity, &existing, b"the passphrase")
                .expect_err("existing destination must be refused")
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&existing).unwrap(), b"do not replace");
    }

    #[test]
    fn backup_reader_rejects_symlink_and_empty_passphrase() {
        let dir = tempfile::tempdir().expect("dir");
        let identity = secured_identity(dir.path(), "personal", 74);
        let backup = dir.path().join("identity.backup");
        assert!(export_portable_backup(&identity, &backup, b"").is_err());

        let link = dir.path().join("link.backup");
        symlink(&backup, &link).expect("link");
        assert!(verify_portable_backup(&link, b"a sufficient passphrase").is_err());
        assert_eq!(
            fs::metadata(&identity).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn failed_readback_removes_only_the_exact_new_envelope() {
        let dir = tempfile::tempdir().expect("dir");
        let destination = dir.path().join("identity.backup");
        let envelope =
            seal_record(b"not a real identity", b"a sufficient passphrase").expect("envelope");
        write_new_backup(&destination, envelope.as_slice()).expect("publish");

        assert!(
            checked_readback(
                &destination,
                b"a sufficient passphrase",
                [0; 32],
                envelope.as_slice(),
            )
            .is_err()
        );
        assert!(
            !destination.exists(),
            "a final target that fails read-back must not remain"
        );
    }

    #[test]
    fn provider_failure_during_restore_leaves_no_identity_metadata() {
        let source = tempfile::tempdir().expect("source");
        let identity = secured_identity(source.path(), "personal", 75);
        let backup_dir = tempfile::tempdir().expect("backup dir");
        let backup = backup_dir.path().join("ada.qos-identity-backup");
        let passphrase = b"a sufficient passphrase";
        export_portable_backup(&identity, &backup, passphrase).expect("export");

        let restored = tempfile::tempdir().expect("restore");
        let error = import_portable_backup_with_provider(
            &backup,
            passphrase,
            restored.path(),
            "restored",
            &FailingProvider,
        )
        .expect_err("provider failure must abort restore");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(
            !restored.path().join("restored.json").exists(),
            "failed secure-store import must clean up public identity metadata"
        );
    }
}
