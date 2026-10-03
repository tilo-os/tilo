//! Encrypted portable backups of a single-device identity.
//!
//! Runtime identity metadata omits the signing secret after it
//! is secured in Secret Service or the broker-file fallback.  A portable
//! backup therefore has to run inside this crate: it can authenticate the
//! metadata, retrieve the provider-held secret, and immediately seal the
//! complete legacy record.  No public API here returns that record or its
//! secret bytes.

#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use getrandom::{SysRng, rand_core::TryRng as _};
use tilo_seal::envelope::{self, Envelope, KeySource};
use tilo_seal::{Context, Label, PasswordParams};
use zeroize::{Zeroize, Zeroizing};

use super::{IdentityLock, MAX_RECORD_BYTES, identity_from_record};

const MAGIC: &[u8; 8] = b"TLOIBK\0\x01";
/// Format v2: the magic, this version byte, then a `tilo-seal` password
/// envelope that stores its own Argon2id parameters and salt.
const FORMAT_V2: u8 = 2;
const PREFIX_LEN: usize = MAGIC.len() + 1;
/// Room for the envelope header (well under 256 bytes) plus the AEAD tag.
const MAX_BACKUP_BYTES: u64 = MAX_RECORD_BYTES + 256;

/// Parameters for new backups: the OWASP Argon2id baseline, so restoring
/// stays fast on low-end devices. They are stored in the authenticated
/// header, so raising them later needs no new format version.
const V2_PARAMS: PasswordParams = PasswordParams {
    m_cost_kib: 19 * 1024,
    t_cost: 2,
    p_cost: 1,
};
/// The most work a v2 backup may ask a reader to do. Larger stored
/// parameters are refused before any derivation runs.
const V2_MAX_MEMORY_KIB: u32 = 1024 * 1024;
const V2_MAX_ITERATIONS: u32 = 16;
const V2_MAX_LANES: u32 = 8;
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
/// This is the only restoration API: callers can restore an
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
/// Returns an error when the backup cannot be safely opened, authenticated,
/// decoded, or imported, when its identity differs from `expected_key`, or
/// when the destination identity already conflicts with another record.
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

    record.threshold_identity.secret_bytes = Zeroizing::new(secret.to_vec());
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
            secret_bytes: Zeroizing::new(Vec::new()),
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

fn v2_prefix() -> [u8; PREFIX_LEN] {
    let mut prefix = [FORMAT_V2; PREFIX_LEN];
    if let Some(magic) = prefix.get_mut(..MAGIC.len()) {
        magic.copy_from_slice(MAGIC);
    }
    prefix
}

fn seal_record(record: &[u8], passphrase: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    if record.len() as u64 > MAX_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "identity record exceeds the portable backup limit",
        ));
    }
    validate_passphrase(passphrase)?;
    let prefix = v2_prefix();
    let sealed = envelope::seal_with_password(
        passphrase,
        V2_PARAMS,
        Context::new(Label::IdentityBackup, &prefix),
        record,
    )
    .map_err(|_| io::Error::other("could not encrypt portable backup"))?;
    let capacity = PREFIX_LEN
        .checked_add(sealed.len())
        .ok_or_else(|| io::Error::other("portable backup is too large"))?;
    let mut out = Zeroizing::new(Vec::with_capacity(capacity));
    out.extend_from_slice(&prefix);
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn open_record(blob: &[u8], passphrase: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    validate_passphrase(passphrase)?;
    if blob.get(..MAGIC.len()) != Some(MAGIC.as_slice()) {
        return Err(invalid_backup());
    }
    let plaintext = match blob.get(MAGIC.len()).copied() {
        Some(FORMAT_V2) => open_v2(blob, passphrase)?,
        _ => return Err(invalid_backup()),
    };
    if plaintext.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid_backup());
    }
    Ok(plaintext)
}

fn open_v2(blob: &[u8], passphrase: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    let (prefix, sealed) = blob
        .split_at_checked(PREFIX_LEN)
        .ok_or_else(invalid_backup)?;
    // Refuse oversized stored parameters before deriving anything.
    match Envelope::parse(sealed)
        .map_err(|_| invalid_backup())?
        .source
    {
        KeySource::Password { params, .. }
            if params.m_cost_kib <= V2_MAX_MEMORY_KIB
                && params.t_cost <= V2_MAX_ITERATIONS
                && params.p_cost <= V2_MAX_LANES => {}
        _ => return Err(invalid_backup()),
    }
    envelope::open_with_password(
        passphrase,
        Context::new(Label::IdentityBackup, prefix),
        sealed,
    )
    .map_err(|_| invalid_backup())
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
        SysRng.try_fill_bytes(&mut nonce).map_err(random_error)?;
        let candidate = parent.join(format!(".tilo-identity-backup-{}.tmp", hex::encode(nonce)));
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
#[cfg(target_os = "linux")]
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
        return publish_with_link(temp, destination);
    }
    Err(error)
}

#[cfg(not(target_os = "linux"))]
fn publish_new_file(temp: &Path, destination: &Path) -> io::Result<()> {
    publish_with_link(temp, destination)
}

fn publish_with_link(temp: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(temp, destination)?;
    fs::remove_file(temp)
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
    use std::sync::{Arc, Barrier};

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
    fn backups_round_trip_and_fail_closed() {
        // phase: new_backups_are_v2_seal_envelopes_and_reject_tampering
        {
            let passphrase = b"a sufficient passphrase";
            let blob = seal_record(b"{\"record\":true}", passphrase).expect("seal");
            assert_eq!(&blob[..MAGIC.len()], MAGIC);
            assert_eq!(blob[MAGIC.len()], FORMAT_V2);
            assert!(envelope::is_envelope(&blob[PREFIX_LEN..]));
            assert_eq!(
                open_record(&blob, passphrase).unwrap().as_slice(),
                b"{\"record\":true}"
            );
            assert!(open_record(&blob, b"another sufficient passphrase").is_err());
            for index in [0, MAGIC.len(), PREFIX_LEN + 8, blob.len() - 1] {
                let mut tampered = blob.to_vec();
                tampered[index] ^= 0x01;
                assert!(open_record(&tampered, passphrase).is_err(), "byte {index}");
            }
            // Rewriting the version byte cannot downgrade the envelope.
            let mut downgraded = blob.to_vec();
            downgraded[MAGIC.len()] = 1;
            assert!(open_record(&downgraded, passphrase).is_err());
        }
        // phase: encrypted_portable_backup_round_trips_and_imports
        {
            let source = tempfile::tempdir().expect("source");
            let identity = secured_identity(source.path(), "personal", 71);
            let backup_dir = tempfile::tempdir().expect("backup dir");
            let backup = backup_dir.path().join("ada.tilo-identity-backup");
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
        // phase: backup_is_not_metadata_copy_and_wrong_passphrase_fails_closed
        {
            let source = tempfile::tempdir().expect("source");
            let identity = secured_identity(source.path(), "personal", 72);
            let backup_dir = tempfile::tempdir().expect("backup dir");
            let backup = backup_dir.path().join("ada.tilo-identity-backup");
            export_portable_backup(&identity, &backup, b"the passphrase").expect("export");

            let bytes = fs::read(&backup).expect("read envelope");
            assert!(!bytes.starts_with(b"{"), "backup is an encrypted envelope");
            assert!(!bytes.windows(32).any(|part| part == [72_u8; 32]));
            assert!(verify_portable_backup(&backup, b"wrong passphrase").is_err());
            assert!(
                import_portable_backup(&backup, b"wrong passphrase", source.path(), "wrong")
                    .is_err()
            );
        }
    }

    #[test]
    fn imports_refuse_replaced_tampered_linked_or_existing_inputs() {
        // phase: matching_import_rejects_a_backup_replaced_after_verification
        {
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
        // phase: tampering_and_existing_destinations_are_refused
        {
            let source = tempfile::tempdir().expect("source");
            let identity = secured_identity(source.path(), "personal", 73);
            let backup_dir = tempfile::tempdir().expect("backup dir");
            let backup = backup_dir.path().join("ada.tilo-identity-backup");
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
        // phase: backup_reader_rejects_symlink_and_empty_passphrase
        {
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
    }

    #[test]
    fn publication_and_restore_failures_leave_no_partial_state() {
        // phase: concurrent_publication_preserves_the_winner_and_loser_staging_file
        {
            let dir = tempfile::tempdir().expect("dir");
            let first_staging = dir.path().join("first.tmp");
            let second_staging = dir.path().join("second.tmp");
            let destination = dir.path().join("identity.backup");
            fs::write(&first_staging, b"first complete payload").expect("first staging file");
            fs::write(&second_staging, b"second complete payload").expect("second staging file");

            let start = Arc::new(Barrier::new(2));
            let first_start = Arc::clone(&start);
            let first_source = first_staging.clone();
            let first_destination = destination.clone();
            let first = std::thread::spawn(move || {
                first_start.wait();
                publish_new_file(&first_source, &first_destination)
            });
            let second_source = second_staging.clone();
            let second_destination = destination.clone();
            let second = std::thread::spawn(move || {
                start.wait();
                publish_new_file(&second_source, &second_destination)
            });

            let first_result = first.join().expect("first publisher");
            let second_result = second.join().expect("second publisher");
            assert_ne!(
                first_result.is_ok(),
                second_result.is_ok(),
                "exactly one publication must succeed"
            );

            if first_result.is_ok() {
                assert_eq!(fs::read(&destination).unwrap(), b"first complete payload");
                assert!(!first_staging.exists(), "winning staging name is consumed");
                assert_eq!(
                    fs::read(&second_staging).unwrap(),
                    b"second complete payload"
                );
            } else {
                assert_eq!(fs::read(&destination).unwrap(), b"second complete payload");
                assert!(!second_staging.exists(), "winning staging name is consumed");
                assert_eq!(fs::read(&first_staging).unwrap(), b"first complete payload");
            }
        }
        // phase: failed_readback_removes_only_the_exact_new_envelope
        {
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
        // phase: provider_failure_during_restore_leaves_no_identity_metadata
        {
            let source = tempfile::tempdir().expect("source");
            let identity = secured_identity(source.path(), "personal", 75);
            let backup_dir = tempfile::tempdir().expect("backup dir");
            let backup = backup_dir.path().join("ada.tilo-identity-backup");
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
}
