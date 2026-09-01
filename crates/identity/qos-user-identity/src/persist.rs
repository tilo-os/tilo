//! On-disk persistence for user + device identity material.
//!
//! qOS lays out identity files under `/var/lib/qos/`:
//!
//! ```text
//! /var/lib/qos/
//! ├── user-identity/
//! │   ├── pubkey.bin      # 32-byte Ed25519 public key (present on every device)
//! │   └── privkey.bin     # 32-byte Ed25519 secret seed (ONLY on primary devices)
//! └── device/
//!     ├── keypair.bin     # 32-byte Ed25519 secret seed (per-device, always present)
//!     ├── cert.bin        # legacy/default postcard-encoded `DeviceCert`
//!     └── certs/          # one certificate per user identity trusted by this device
//! ```
//!
//! All files are raw byte blobs; the on-disk format is deliberately
//! simple so it can be inspected with `hexdump` and does not depend
//! on `serde_json` at boot.
//!
//! Secret files must not be reachable by users who are not entitled to
//! them, and the mode is checked at read time — a permissive secret file
//! returns [`PersistError::InsecurePermissions`] rather than being read.
//! `privkey.bin` is user-scoped and must be 0600. `keypair.bin` is the
//! device key, read by two components running as two uids, so it may
//! additionally be group-readable ([`DEVICE_KEY_MODE`]) but never
//! reachable by anyone outside that group.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::SigningKey;
use thiserror::Error;

use crate::CanonicalError;
use crate::cert::DeviceCert;

/// Sequence for same-directory temporary device-key claim files.  Final
/// ownership comes from the no-clobber hard link, not this name; `create_new`
/// only makes a stale temporary file impossible to overwrite.
static DEVICE_KEY_CLAIM_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Canonical layout of identity paths under a chosen root.
///
/// Wraps the four paths so callers can inject a test root
/// (`tempdir`) without having to hand-plumb every path through.
#[derive(Clone, Debug)]
pub struct IdentityPaths {
    /// `<root>/user-identity/pubkey.bin`
    pub user_pubkey: std::path::PathBuf,
    /// `<root>/user-identity/privkey.bin`
    pub user_privkey: std::path::PathBuf,
    /// `<root>/device/keypair.bin`
    pub device_privkey: std::path::PathBuf,
    /// `<root>/device/cert.bin`
    pub device_cert: std::path::PathBuf,
}

impl IdentityPaths {
    /// Standard layout under `/var/lib/qos`.
    pub fn default_system() -> Self {
        Self::under("/var/lib/qos")
    }

    /// Custom root — the boot path uses `/var/lib/qos`, tests use
    /// a `tempdir`.
    pub fn under(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            user_pubkey: root.join("user-identity/pubkey.bin"),
            user_privkey: root.join("user-identity/privkey.bin"),
            device_privkey: root.join("device/keypair.bin"),
            device_cert: root.join("device/cert.bin"),
        }
    }

    /// Ensure the two subdirectories exist (best-effort). Does not
    /// change their mode — that's the OS installer's job.
    ///
    /// # Errors
    ///
    /// Returns an error if either subdirectory cannot be created.
    pub fn ensure_directories(&self) -> io::Result<()> {
        if let Some(parent) = self.user_pubkey.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self.device_privkey.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(())
    }

    /// Certificate issued to this device by `user_public_key`.
    ///
    /// A device has one keypair but may act for different local identities in
    /// different workspaces. Keying certificates by their issuing user root
    /// preserves that one-device invariant without crossing identities.
    pub fn device_cert_for_user(&self, user_public_key: &[u8; 32]) -> std::path::PathBuf {
        let device_dir = self
            .device_privkey
            .parent()
            .unwrap_or_else(|| Path::new("device"));
        device_dir
            .join("certs")
            .join(format!("{}.bin", hex_encode(user_public_key)))
    }
}

/// Failure modes for identity persistence.
#[derive(Debug, Error)]
pub enum PersistError {
    /// I/O failure. Preserves the underlying `io::Error` so the
    /// caller can inspect the `ErrorKind`.
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    /// A secret file was reachable by users not entitled to it.
    /// Refused to proceed rather than expose the key.
    #[error(
        "secret file {path} has permissions {mode:o}; a user-scoped secret must be 0600 and \
         the device key at most 0640"
    )]
    InsecurePermissions {
        /// File path that triggered the check.
        path: String,
        /// The mode we found (raw `st_mode & 0o777`).
        mode: u32,
    },
    /// An Ed25519 key file was not 32 bytes.
    #[error("expected 32-byte Ed25519 key, got {0} bytes")]
    WrongKeyLength(usize),
    /// Canonical decode of the cert file failed.
    #[error("cert decode failed: {0}")]
    Canonical(#[from] CanonicalError),
}

/// Read a 32-byte Ed25519 public key from disk.
///
/// # Errors
///
/// Returns [`PersistError::Io`] if `path` cannot be read, or
/// [`PersistError::WrongKeyLength`] if the file is not exactly 32
/// bytes.
pub fn read_public_key(path: &Path) -> Result<[u8; 32], PersistError> {
    let bytes = fs::read(path)?;
    if bytes.len() != 32 {
        return Err(PersistError::WrongKeyLength(bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Write a 32-byte Ed25519 public key to disk. Mode is left at the
/// filesystem default (typically 0644) — public keys are non-secret.
///
/// # Errors
///
/// Returns an error if writing to `path` fails.
pub fn write_public_key(path: &Path, pubkey: &[u8; 32]) -> io::Result<()> {
    fs::write(path, pubkey)
}

/// Read a 32-byte Ed25519 signing key from disk, enforcing 0600
/// permissions.
///
/// On non-Unix targets the permission check is a no-op (still
/// checks length); qOS runs only on Linux, so this is the
/// production path everywhere the compositor ships.
///
/// # Errors
///
/// Returns [`PersistError::InsecurePermissions`] if the file's mode
/// is looser than 0600 (Unix only), [`PersistError::Io`] if it
/// cannot be read, or [`PersistError::WrongKeyLength`] if it is not
/// exactly 32 bytes.
pub fn read_signing_key(path: &Path) -> Result<SigningKey, PersistError> {
    ensure_secret_mode(path)?;
    signing_key_from_file(path)
}

/// The length check and expansion shared by every secret-key reader.
/// The mode policy is the caller's — see [`read_signing_key`] and
/// [`read_device_key`].
fn signing_key_from_file(path: &Path) -> Result<SigningKey, PersistError> {
    let bytes = fs::read(path)?;
    if bytes.len() != 32 {
        return Err(PersistError::WrongKeyLength(bytes.len()));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    Ok(SigningKey::from_bytes(&seed))
}

/// Write a 32-byte Ed25519 signing key to disk with mode 0600.
///
/// # Errors
///
/// Returns an error if `path` has no parent directory, or if the
/// atomic write fails.
pub fn write_signing_key(path: &Path, sk: &SigningKey) -> io::Result<()> {
    // A partial write must leave the previous secret intact, and the mode is
    // set on the temp file so the key is never briefly world-readable at its
    // final path.
    path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    atomic_file::write_with_mode(path, &sk.to_bytes(), 0o600)
}

/// Mode the device key is stored with: owner read/write, group read,
/// nothing for anyone else.
///
/// Group-readable because the device key is read by more than one
/// process running as more than one uid — the filesystem daemon runs as
/// root, the compositor in the user's session — and they are the *same*
/// device, so they must present the same identity. The group is the
/// device-trust boundary; its membership is the session user, and the
/// containing directory is `setgid` so the group is inherited whichever
/// component creates the key first.
///
/// `other` gets nothing, and that is enforced on read.
pub const DEVICE_KEY_MODE: u32 = 0o640;

/// Read the device key, requiring that it not be accessible to users
/// outside the device-trust group.
///
/// # Errors
///
/// Returns [`PersistError::InsecurePermissions`] if the file is
/// accessible to users outside the device-trust group,
/// [`PersistError::Io`] if it cannot be read, or
/// [`PersistError::WrongKeyLength`] if it is not exactly 32 bytes.
pub fn read_device_key(path: &Path) -> Result<SigningKey, PersistError> {
    ensure_not_world_accessible(path)?;
    signing_key_from_file(path)
}

/// Write the device key with [`DEVICE_KEY_MODE`], atomically.
///
/// # Errors
///
/// Returns an error if `path` has no parent directory, or if the
/// atomic write fails.
pub fn write_device_key(path: &Path, sk: &SigningKey) -> io::Result<()> {
    path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    atomic_file::write_with_mode(path, &sk.to_bytes(), DEVICE_KEY_MODE)
}

/// Whether [`load_or_create_device_key`] found a key or minted one.
///
/// Callers care about the difference: minting is a first-boot event
/// that invalidates nothing, whereas *silently* minting when a key was
/// expected means the device just lost its identity. That second case
/// is why this function never generates over an unreadable file — see
/// the doc there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKeyOutcome {
    /// An existing key was read from disk.
    Loaded,
    /// No key file existed, so a fresh one was generated and written.
    Created,
}

/// Load this device's signing key, generating one on first boot.
///
/// This is the root of every network and document identity on the
/// machine: the same 32 bytes derive the Iroh endpoint id, the CRDT
/// author, and the filesystem access subject. There is
/// exactly one per device and no subsystem mints its own.
///
/// **A key file that exists but cannot be read is an error, never a
/// reason to generate.** Overwriting it would make identity loss
/// indistinguishable from first boot: the device would come back with
/// a new name, every grant written against the old one would be dead,
/// and nothing would have reported a problem. A wrong mode, a
/// truncated file, or an I/O failure all surface as
/// [`PersistError`] so an operator can decide whether to restore a
/// backup or deliberately re-enrol.
///
/// # Errors
///
/// Returns [`PersistError::InsecurePermissions`] if an existing key
/// file's permissions are too loose, [`PersistError::WrongKeyLength`]
/// if an existing key file is not 32 bytes, or [`PersistError::Io`]
/// if reading the existing key, creating the parent directories, or
/// writing a freshly generated key fails.
pub fn load_or_create_device_key(
    paths: &IdentityPaths,
) -> Result<(SigningKey, DeviceKeyOutcome), PersistError> {
    let path = paths.device_privkey.as_path();
    match fs::metadata(path) {
        Ok(_) => Ok((read_device_key(path)?, DeviceKeyOutcome::Loaded)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            paths.ensure_directories()?;
            let sk = SigningKey::generate(&mut rand::rngs::OsRng);
            let created = publish_device_key_no_clobber(path, &sk)?;
            // Always reload the final path.  When a simultaneous creator won
            // the atomic claim, returning our locally generated key would
            // make this process use a device identity that was never stored.
            let final_key = read_device_key(path)?;
            Ok((
                final_key,
                if created {
                    DeviceKeyOutcome::Created
                } else {
                    DeviceKeyOutcome::Loaded
                },
            ))
        }
        Err(e) => Err(PersistError::Io(e)),
    }
}

/// Publish a complete device key exactly once.
///
/// `atomic_file::write_with_mode` is intentionally not used here: its final
/// rename correctly replaces a file for ordinary updates, but device-key
/// creation must never replace a key another first-login process just made.
/// The candidate is fully written and synced under a unique same-directory
/// temporary name, then `hard_link` atomically claims the final name only if
/// it is still absent.  A caller that loses returns `Ok(false)` and must load
/// the winner from disk.
fn publish_device_key_no_clobber(path: &Path, key: &SigningKey) -> Result<bool, PersistError> {
    let parent = path.parent().ok_or_else(|| {
        PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "device-key path has no parent",
        ))
    })?;
    let temp = create_device_key_claim_temp(parent, &key.to_bytes())?;
    let claim = fs::hard_link(&temp, path);
    let _ = fs::remove_file(&temp);

    match claim {
        Ok(()) => {
            // The key contents reached storage before the directory entry was
            // published.  Sync the parent as well, so a successful first boot
            // survives a power loss after the hard-link claim.
            fs::File::open(parent)?.sync_all()?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(PersistError::Io(error)),
    }
}

/// Create a private, complete temporary candidate beside the final device-key
/// path.  The mode is applied before this file is ever linked into the
/// shared identity tree, so it cannot briefly become publicly visible.
fn create_device_key_claim_temp(
    parent: &Path,
    bytes: &[u8; 32],
) -> Result<std::path::PathBuf, PersistError> {
    for _ in 0..16 {
        let sequence = DEVICE_KEY_CLAIM_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(
            ".qos-device-key-claim-{}-{sequence}",
            std::process::id()
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(DEVICE_KEY_MODE);
        }
        match options.open(&temp) {
            Ok(mut file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    // `mode` is filtered by the process umask; explicitly
                    // restore the intended shared-device mode while the file
                    // is still private under its temporary name.
                    file.set_permissions(fs::Permissions::from_mode(DEVICE_KEY_MODE))?;
                }
                if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                    let _ = fs::remove_file(&temp);
                    return Err(PersistError::Io(error));
                }
                return Ok(temp);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(PersistError::Io(error)),
        }
    }
    Err(PersistError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a device-key claim temporary file",
    )))
}

/// Adopt an externally-supplied 32-byte seed as this device's key.
///
/// Used once, to carry a pre-existing per-subsystem key over into the
/// single device identity so the machine keeps the name its peers
/// already know it by. Refuses to overwrite an existing device key:
/// that key is what everything else on the machine already derives
/// from, so replacing it is not a migration, it is a rename.
///
/// # Errors
///
/// Returns [`PersistError::Io`] with [`io::ErrorKind::AlreadyExists`]
/// if a device key already exists at the target path, or if creating
/// the parent directories or writing the new key fails.
pub fn adopt_device_key(
    paths: &IdentityPaths,
    seed: &[u8; 32],
) -> Result<SigningKey, PersistError> {
    let path = paths.device_privkey.as_path();
    paths.ensure_directories()?;
    let sk = SigningKey::from_bytes(seed);
    if !publish_device_key_no_clobber(path, &sk)? {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("device key already exists at {}", path.display()),
        )));
    }
    // Read back the claimed final path: this validates the key's required
    // mode and keeps adoption subject to the same persistence contract as
    // ordinary first-boot creation.
    read_device_key(path)
}

/// Read a signed device cert from disk.
///
/// # Errors
///
/// Returns [`PersistError::Io`] if `path` cannot be read, or
/// [`PersistError::Canonical`] if the bytes do not decode as a
/// `DeviceCert`.
pub fn read_device_cert(path: &Path) -> Result<DeviceCert, PersistError> {
    let bytes = fs::read(path)?;
    let cert: DeviceCert = crate::canonical::from_canonical_bytes(&bytes)?;
    Ok(cert)
}

/// Write a signed device cert to disk. Public metadata; default
/// permissions.
///
/// # Errors
///
/// Returns [`PersistError::Canonical`] if `cert` cannot be
/// canonically encoded, or [`PersistError::Io`] if writing to `path`
/// fails.
pub fn write_device_cert(path: &Path, cert: &DeviceCert) -> Result<(), PersistError> {
    let bytes = crate::canonical::canonical_bytes(cert)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    atomic_file::write(path, &bytes)?;
    Ok(())
}

/// # Panics
/// Never in practice: `byte >> 4` and `byte & 0x0f` are both bounded to
/// `0..16` by the bit operation itself (not by any external input), so
/// indexing the 16-entry `HEX` table can never go out of range.
#[allow(
    clippy::indexing_slicing,
    reason = "byte >> 4 and byte & 0x0f are both unconditionally in 0..16, so HEX[..] cannot go out of range; see this fn's # Panics doc"
)]
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(unix)]
fn ensure_secret_mode(path: &Path) -> Result<(), PersistError> {
    reject_modes(path, 0o077)
}

/// Weaker policy, for a secret deliberately shared with a group: no
/// access at all for users outside it.
fn ensure_not_world_accessible(path: &Path) -> Result<(), PersistError> {
    reject_modes(path, 0o007)
}

fn reject_modes(path: &Path, forbidden: u32) -> Result<(), PersistError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(path)?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & forbidden != 0 {
        return Err(PersistError::InsecurePermissions {
            path: path.display().to_string(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_secret_mode(_path: &Path) -> Result<(), PersistError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use std::sync::{Arc, Barrier};

    #[test]
    fn public_key_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pubkey.bin");
        let key = [7u8; 32];
        write_public_key(&path, &key).unwrap();
        let back = read_public_key(&path).unwrap();
        assert_eq!(key, back);
    }

    #[cfg(unix)]
    #[test]
    fn signing_key_round_trip_enforces_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("keypair.bin");
        let (sk, _pk) = crate::UserIdentity::generate("Alice", 1);
        write_signing_key(&path, &sk).unwrap();

        // Written file should be 0600.
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let loaded = read_signing_key(&path).unwrap();
        // Signing with either should give the same signature.
        let msg = b"round-trip";
        assert_eq!(sk.sign(msg).to_bytes(), loaded.sign(msg).to_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn signing_key_rejects_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("keypair.bin");
        let (sk, _pk) = crate::UserIdentity::generate("Alice", 1);
        write_signing_key(&path, &sk).unwrap();
        // Loosen to 0644.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        match read_signing_key(&path).unwrap_err() {
            PersistError::InsecurePermissions { mode, .. } => assert_eq!(mode, 0o644),
            other => panic!("expected InsecurePermissions, got {other:?}"),
        }
    }

    #[test]
    fn wrong_length_key_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pubkey.bin");
        fs::write(&path, [0u8; 16]).unwrap();
        match read_public_key(&path).unwrap_err() {
            PersistError::WrongKeyLength(16) => {}
            other => panic!("expected WrongKeyLength(16), got {other:?}"),
        }
    }

    #[test]
    fn cert_round_trip() {
        use crate::cert::{DeviceCapabilities, sign_device_cert};
        use crate::{DeviceIdentity, UserIdentity};

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cert.bin");
        let (root_sk, user) = UserIdentity::generate("Alice", 1);
        let (_dev_sk, dev) = DeviceIdentity::generate("laptop", 2);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 3,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, &root_sk).unwrap();
        write_device_cert(&path, &cert).unwrap();
        let back = read_device_cert(&path).unwrap();
        assert_eq!(cert, back);
    }

    #[test]
    fn identity_specific_cert_paths_do_not_collide() {
        let paths = IdentityPaths::under("/var/lib/qos");
        let alice = paths.device_cert_for_user(&[1; 32]);
        let bob = paths.device_cert_for_user(&[2; 32]);
        assert_ne!(alice, bob);
        assert!(alice.starts_with("/var/lib/qos/device/certs"));
    }

    /// The device key is deliberately group-readable — two components
    /// running as two uids present the one device identity — but a user
    /// outside that group must not be able to read it, and that is
    /// enforced on every read rather than trusted to whoever set the
    /// mode.
    #[cfg(unix)]
    #[test]
    fn device_key_permits_the_trust_group_and_nobody_else() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path());
        let (sk, _) = load_or_create_device_key(&paths).unwrap();
        let path = &paths.device_privkey;

        let written = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(written, DEVICE_KEY_MODE);
        assert_eq!(written & 0o007, 0, "no bits for users outside the group");

        // Group read: accepted.
        fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(read_device_key(path).unwrap().to_bytes(), sk.to_bytes());
        // Owner only: also fine.
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_device_key(path).unwrap().to_bytes(), sk.to_bytes());

        // Any bit for `other`: refused.
        for mode in [0o644u32, 0o604, 0o601, 0o666] {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
            match read_device_key(path) {
                Err(PersistError::InsecurePermissions { .. }) => {}
                other => panic!("mode {mode:o} must be refused, got {other:?}"),
            }
        }
    }

    /// The relaxation is scoped to the device key. A user-scoped secret
    /// still has to be owner-only — the group that reads the device key
    /// has no business reading the user's own signing key.
    #[cfg(unix)]
    #[test]
    fn user_scoped_secret_still_refuses_group_read() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("privkey.bin");
        let sk = SigningKey::from_bytes(&[8u8; 32]);
        write_signing_key(&path, &sk).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        match read_signing_key(&path) {
            Err(PersistError::InsecurePermissions { mode, .. }) => assert_eq!(mode, 0o640),
            other => panic!("group-readable user secret must be refused, got {other:?}"),
        }
    }

    #[test]
    fn device_key_is_created_once_then_reloaded() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path());

        let (first, outcome) = load_or_create_device_key(&paths).unwrap();
        assert_eq!(outcome, DeviceKeyOutcome::Created);

        let (second, outcome) = load_or_create_device_key(&paths).unwrap();
        assert_eq!(outcome, DeviceKeyOutcome::Loaded);
        assert_eq!(
            first.verifying_key().to_bytes(),
            second.verifying_key().to_bytes(),
            "the device identity must survive a restart"
        );
    }

    #[test]
    fn simultaneous_device_key_creation_uses_one_persisted_key() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path());
        let start = Arc::new(Barrier::new(3));

        let first_paths = paths.clone();
        let first_start = Arc::clone(&start);
        let first = std::thread::spawn(move || {
            first_start.wait();
            load_or_create_device_key(&first_paths).map(|(key, outcome)| (key.to_bytes(), outcome))
        });
        let second_paths = paths.clone();
        let second_start = Arc::clone(&start);
        let second = std::thread::spawn(move || {
            second_start.wait();
            load_or_create_device_key(&second_paths).map(|(key, outcome)| (key.to_bytes(), outcome))
        });
        start.wait();

        let first = first.join().expect("first device-key thread").unwrap();
        let second = second.join().expect("second device-key thread").unwrap();
        assert_eq!(
            usize::from(first.1 == DeviceKeyOutcome::Created)
                + usize::from(second.1 == DeviceKeyOutcome::Created),
            1,
            "one claimant creates the shared device key and the other loads it"
        );
        assert_eq!(
            first.0, second.0,
            "every concurrent caller must return the one persisted device key"
        );
        assert_eq!(
            read_device_key(&paths.device_privkey).unwrap().to_bytes(),
            first.0,
            "the final file matches both callers and was never overwritten"
        );
        assert_eq!(
            fs::read(&paths.device_privkey).unwrap().len(),
            32,
            "the published final device key is complete"
        );
    }

    /// The defining property: an unreadable key file must raise, not
    /// be replaced. A device that silently re-keys drops every grant
    /// written against its old identity and reports nothing.
    #[test]
    fn unreadable_device_key_raises_instead_of_rotating() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path());
        paths.ensure_directories().unwrap();
        fs::write(&paths.device_privkey, b"not a key").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&paths.device_privkey, fs::Permissions::from_mode(0o600)).unwrap();
        }

        match load_or_create_device_key(&paths) {
            Err(PersistError::WrongKeyLength(9)) => {}
            Ok(_) => panic!("a corrupt device key must not be silently replaced"),
            Err(other) => panic!("expected WrongKeyLength(9), got {other:?}"),
        }
        // …and the bytes on disk are untouched, so a restore is still
        // possible.
        assert_eq!(fs::read(&paths.device_privkey).unwrap(), b"not a key");
    }

    #[test]
    fn adopt_device_key_seeds_a_new_device_but_never_replaces_one() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path());
        let seed = [23u8; 32];

        let adopted = adopt_device_key(&paths, &seed).unwrap();
        assert_eq!(adopted.to_bytes(), seed);
        let (loaded, outcome) = load_or_create_device_key(&paths).unwrap();
        assert_eq!(outcome, DeviceKeyOutcome::Loaded);
        assert_eq!(loaded.to_bytes(), seed);

        let err = adopt_device_key(&paths, &[99u8; 32]).unwrap_err();
        match err {
            PersistError::Io(e) => assert_eq!(e.kind(), io::ErrorKind::AlreadyExists),
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
        assert_eq!(
            read_device_key(&paths.device_privkey).unwrap().to_bytes(),
            seed
        );
    }

    #[test]
    fn ensure_directories_creates_expected_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path());
        paths.ensure_directories().unwrap();
        assert!(tmp.path().join("user-identity").is_dir());
        assert!(tmp.path().join("device").is_dir());
    }
}
