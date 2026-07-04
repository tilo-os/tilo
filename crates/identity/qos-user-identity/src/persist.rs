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
//!     └── cert.bin        # postcard-encoded `DeviceCert` (present after enrollment)
//! ```
//!
//! All files are raw byte blobs; the on-disk format is deliberately
//! simple so it can be inspected with `hexdump` and does not depend
//! on `serde_json` at boot.
//!
//! Secret files (`privkey.bin`, `keypair.bin`) MUST be mode 0600.
//! Everything here checks the mode at read time; a permissive
//! secret file returns [`PersistError::InsecurePermissions`].

use std::fs;
use std::io;
use std::path::Path;

use ed25519_dalek::SigningKey;
use thiserror::Error;

use crate::CanonicalError;
use crate::cert::DeviceCert;

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
    pub fn ensure_directories(&self) -> io::Result<()> {
        if let Some(parent) = self.user_pubkey.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self.device_privkey.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(())
    }
}

/// Failure modes for identity persistence.
#[derive(Debug, Error)]
pub enum PersistError {
    /// I/O failure. Preserves the underlying `io::Error` so the
    /// caller can inspect the ErrorKind.
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    /// A secret file (`privkey.bin`, `keypair.bin`) was more
    /// permissive than 0600. Refused to proceed rather than expose
    /// the key.
    #[error("secret file {path} has permissions {mode:o}; must be 0600")]
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
pub fn write_public_key(path: &Path, pubkey: &[u8; 32]) -> io::Result<()> {
    fs::write(path, pubkey)
}

/// Read a 32-byte Ed25519 signing key from disk, enforcing 0600
/// permissions.
///
/// On non-Unix targets the permission check is a no-op (still
/// checks length); qOS runs only on Linux, so this is the
/// production path everywhere the compositor ships.
pub fn read_signing_key(path: &Path) -> Result<SigningKey, PersistError> {
    ensure_secret_mode(path)?;
    let bytes = fs::read(path)?;
    if bytes.len() != 32 {
        return Err(PersistError::WrongKeyLength(bytes.len()));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    Ok(SigningKey::from_bytes(&seed))
}

/// Write a 32-byte Ed25519 signing key to disk with mode 0600.
pub fn write_signing_key(path: &Path, sk: &SigningKey) -> io::Result<()> {
    // Write via a tempfile in the same directory + rename, so a
    // partial write leaves the previous secret intact. The mode is
    // set on the tempfile before the rename atomically publishes it.
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("keypair")
    ));
    fs::write(&tmp, sk.to_bytes())?;
    apply_secret_mode(&tmp)?;
    fs::rename(&tmp, path)
}

/// Read a signed device cert from disk.
pub fn read_device_cert(path: &Path) -> Result<DeviceCert, PersistError> {
    let bytes = fs::read(path)?;
    let cert: DeviceCert = crate::canonical::from_canonical_bytes(&bytes)?;
    Ok(cert)
}

/// Write a signed device cert to disk. Public metadata; default
/// permissions.
pub fn write_device_cert(path: &Path, cert: &DeviceCert) -> Result<(), PersistError> {
    let bytes = crate::canonical::canonical_bytes(cert)?;
    fs::write(path, bytes)?;
    Ok(())
}

#[cfg(unix)]
fn apply_secret_mode(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o600);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn apply_secret_mode(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn ensure_secret_mode(path: &Path) -> Result<(), PersistError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(path)?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
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
    fn ensure_directories_creates_expected_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(tmp.path());
        paths.ensure_directories().unwrap();
        assert!(tmp.path().join("user-identity").is_dir());
        assert!(tmp.path().join("device").is_dir());
    }
}
