//! On-disk persistence for user + device identity material.
//!
//! Tilo lays out identity files under `/var/lib/tilo/`:
//!
//! ```text
//! /var/lib/tilo/
//! ├── user-identity/
//! │   ├── pubkey.bin      # 32-byte Ed25519 public key (present on every device)
//! │   └── privkey.bin     # 32-byte Ed25519 secret seed (ONLY on primary devices)
//! ├── device-private/
//! │   └── keypair.bin     # 32-byte Ed25519 secret seed (per-device, always present)
//! └── device/
//!     ├── cert.bin        # legacy/default postcard-encoded `DeviceCert`
//!     └── certs/          # one certificate per user identity trusted by this device
//! ```
//!
//! All files are raw byte blobs; the on-disk format stays simple so it
//! can be inspected with `hexdump` and does not depend
//! on `serde_json` at boot.
//!
//! Secret files must not be reachable by users who are not entitled to
//! them, and the mode is checked at read time — a permissive secret file
//! returns [`PersistError::InsecurePermissions`] rather than being read.
//! `privkey.bin` is user-scoped and must be 0600. `keypair.bin` is the
//! device key, read by two components running as two uids, so it may
//! additionally be group-readable ([`DEVICE_KEY_MODE`]) but must be owned by
//! root (or the reader for an injected/test store) and never reachable by
//! anyone outside that group.

use getrandom::{SysRng, rand_core::UnwrapErr};
#[cfg(unix)]
use std::ffi::CString;
use std::fs;
#[cfg(not(windows))]
use std::io::Write;
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd as _;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
#[cfg(not(windows))]
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::SigningKey;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::CanonicalError;
use crate::cert::DeviceCert;

const MAX_DEVICE_CERT_BYTES: usize = 64 * 1024;

fn protect_secret_process() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // A caller that loads identity key material must not leave it eligible
        // for ptrace/core-dump capture, even when invoked outside the shipped
        // systemd units. It remains disabled unless the caller deliberately
        // re-enables it; shipped services independently enforce RLIMIT_CORE=0.
        // SAFETY: PR_SET_DUMPABLE consumes scalar arguments only.
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_regular_without_symlinks(path: &Path) -> io::Result<fs::File> {
    const RESOLVE_BENEATH: u64 = 0x08;
    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;

    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    // Fedora Atomic deliberately exposes /home as a symlink to /var/home.
    // Resolving an absolute path with RESOLVE_NO_SYMLINKS therefore rejects a
    // perfectly normal login before it ever reaches Tilo's identity tree.
    // Managed IdentityPaths can sit below that distribution-owned symlink.
    // Anchor them at the parent of the nearest canonical identity directory;
    // resolving the remainder beneath this descriptor preserves symlink
    // rejection inside Tilo's storage. Explicit CLI paths have no such managed
    // boundary, so retain the strict absolute-path behavior for them.
    let managed = path
        .ancestors()
        .zip(path.ancestors().skip(1))
        .find_map(|(child, parent)| {
            let name = child.file_name()?.to_str()?;
            if !matches!(name, "user-identity" | "device-private" | "device") {
                return None;
            }
            Some((parent, path.strip_prefix(parent).ok()?))
        });
    let (root, candidate, resolve) = if let Some((root, relative)) = managed {
        let canonical_root = fs::canonicalize(root)?;
        let root_path = CString::new(canonical_root.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "identity root path contains an embedded NUL byte",
            )
        })?;
        let candidate = CString::new(relative.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "identity artifact path contains an embedded NUL byte",
            )
        })?;
        let parent_flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY;
        // SAFETY: root_path is a live NUL-terminated string for this call.
        let root_fd = unsafe { libc::open(root_path.as_ptr(), parent_flags) };
        if root_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: libc::open returned an owned descriptor.
        let root = unsafe { fs::File::from_raw_fd(root_fd) };
        (
            Some(root),
            candidate,
            RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
        )
    } else {
        let candidate = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "identity artifact path contains an embedded NUL byte",
            )
        })?;
        (None, candidate, RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS)
    };
    let flags = u64::try_from(libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .map_err(|_| io::Error::other("open flags are outside the Linux u64 ABI"))?;
    let how = OpenHow {
        flags,
        mode: 0,
        resolve,
    };
    let directory = root
        .as_ref()
        .map_or(libc::AT_FDCWD, std::os::fd::AsRawFd::as_raw_fd);
    // SAFETY: path and open_how remain live for this syscall. A successful
    // descriptor is uniquely transferred into File below.
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory,
            candidate.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| io::Error::other("openat2 returned an invalid descriptor"))?;
    // SAFETY: openat2 returned this owned descriptor and no other File owns it.
    Ok(unsafe { fs::File::from_raw_fd(descriptor) })
}

#[cfg(not(target_os = "linux"))]
fn open_regular_without_symlinks(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let flags = libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        // Darwin can reject symlinks in every path component atomically,
        // matching Linux openat2 RESOLVE_NO_SYMLINKS.
        #[cfg(target_os = "macos")]
        let flags = (flags & !libc::O_NOFOLLOW) | libc::O_NOFOLLOW_ANY;
        options.custom_flags(flags);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Migration validates certificate directories as well as files. Open
        // directory handles without following a final reparse point.
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "authorization artifact is a reparse point",
            ));
        }
    }
    Ok(file)
}

/// Sequence for same-directory temporary device-key claim files. Final
/// ownership comes from the no-clobber rename, not this name; `create_new`
/// only makes a stale temporary file impossible to overwrite.
#[cfg(not(windows))]
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
    /// `<root>/device-private/keypair.bin`
    pub device_privkey: std::path::PathBuf,
    /// `<root>/device/cert.bin`
    pub device_cert: std::path::PathBuf,
}

impl IdentityPaths {
    /// Standard layout under `/var/lib/tilo`.
    pub fn default_system() -> Self {
        Self::under("/var/lib/tilo")
    }

    /// Custom root — the boot path uses `/var/lib/tilo`, tests use
    /// a `tempdir`.
    pub fn under(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            user_pubkey: root.join("user-identity/pubkey.bin"),
            user_privkey: root.join("user-identity/privkey.bin"),
            device_privkey: root.join("device-private/keypair.bin"),
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
            #[cfg(windows)]
            private_fs::directory(parent)?;
            #[cfg(not(windows))]
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self.device_privkey.parent() {
            #[cfg(windows)]
            private_fs::directory(parent)?;
            #[cfg(not(windows))]
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self.device_cert.parent() {
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
            .device_cert
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
    /// A private key inode is reachable through another directory entry.
    #[error("secret file {path} has {links} hard links; expected exactly one")]
    InsecureLinkCount {
        /// Key path whose opened inode had multiple names.
        path: String,
        /// Number of directory entries referencing the inode.
        links: u64,
    },
    /// A secret key inode is not owned by the authority expected for that key.
    #[error("secret key {path} is owned by uid {owner}; expected {expected}")]
    InsecureOwner {
        /// Key path whose opened inode has an unexpected owner.
        path: String,
        /// Owner reported by the opened inode.
        owner: u32,
        /// Human-readable ownership rule for this key class.
        expected: String,
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
    let bytes = read_regular_bounded(path, 32)?;
    if bytes.len() != 32 {
        return Err(PersistError::WrongKeyLength(bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Read one authorization artifact without following its final symlink or
/// accepting a blocking device/FIFO. The byte after `limit` is consumed only
/// to distinguish an exact-size artifact from an oversized one.
fn read_regular_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, PersistError> {
    let file = open_regular_without_symlinks(path).map_err(|error| {
        PersistError::Io(io::Error::new(
            error.kind(),
            format!(
                "cannot open authorization artifact {}: {error}",
                path.display()
            ),
        ))
    })?;
    if !file.metadata()?.is_file() {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "authorization artifact is not a regular file: {}",
                path.display()
            ),
        )));
    }
    let mut bytes = Vec::with_capacity(limit.saturating_add(1));
    file.take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "authorization artifact {} exceeds {limit} bytes",
                path.display()
            ),
        )));
    }
    Ok(bytes)
}

/// Atomically write a 32-byte Ed25519 public key to disk. Mode is left at the
/// filesystem default (typically 0644) — public keys are non-secret. Atomic
/// replacement avoids both partial trust roots and following a final symlink.
///
/// # Errors
///
/// Returns an error if writing to `path` fails.
pub fn write_public_key(path: &Path, pubkey: &[u8; 32]) -> io::Result<()> {
    path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    atomic_file::write(path, pubkey)
}

/// Read a 32-byte Ed25519 signing key from disk, enforcing 0600
/// permissions.
///
/// Windows validates ownership and a user-only DACL on the opened handle,
/// refusing reparse points rather than following a substituted secret path.
///
/// # Errors
///
/// Returns [`PersistError::InsecurePermissions`] if the file's mode
/// is looser than 0600 (Unix only), [`PersistError::Io`] if it
/// cannot be read, or [`PersistError::WrongKeyLength`] if it is not
/// exactly 32 bytes.
pub fn read_signing_key(path: &Path) -> Result<SigningKey, PersistError> {
    signing_key_from_file(path, 0o077, SecretOwner::Any)
}

/// Read a user/root signing key owned by the invoking account.
///
/// This is the correct boundary for offline signing commands: they require no
/// privilege, and accepting another account's mode-0600 inode would let a
/// writable parent directory silently substitute the signing authority.
///
/// # Errors
///
/// Applies [`read_signing_key`]'s checks and additionally returns
/// [`PersistError::InsecureOwner`] when the opened inode is not caller-owned.
pub fn read_owned_signing_key(path: &Path) -> Result<SigningKey, PersistError> {
    signing_key_from_file(path, 0o077, SecretOwner::Reader)
}

/// The length check and expansion shared by every secret-key reader.
/// The mode policy is the caller's — see [`read_signing_key`] and
/// [`read_device_key`].
fn signing_key_from_file(
    path: &Path,
    forbidden_mode: u32,
    owner_policy: SecretOwner,
) -> Result<SigningKey, PersistError> {
    protect_secret_process()?;
    // Inspect and read the same opened inode while rejecting symlinks in every
    // component. A leaf-only O_NOFOLLOW check still permits an exchanged
    // parent directory to redirect an otherwise trusted key path.
    #[cfg(windows)]
    let file = private_fs::read(path)?;
    #[cfg(not(windows))]
    let file = open_regular_without_symlinks(path).map_err(|error| {
        PersistError::Io(io::Error::new(
            error.kind(),
            format!("cannot open secret key {}: {error}", path.display()),
        ))
    })?;
    #[cfg(not(unix))]
    let _ = (forbidden_mode, owner_policy);
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("secret key is not a regular file: {}", path.display()),
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        reject_metadata_mode(path, &metadata, forbidden_mode)?;
        if owner_policy != SecretOwner::Any {
            // The installed key is provisioned by root and then shared
            // read-only with the device-identity group. Custom/test stores may
            // be owned by the caller, but a third account may never substitute
            // the signing identity merely by matching its mode.
            // SAFETY: geteuid takes no arguments and has no failure mode.
            let reader = unsafe { libc::geteuid() };
            if !secret_key_owner_is_accepted(owner_policy, metadata.uid(), reader) {
                return Err(PersistError::InsecureOwner {
                    path: path.display().to_string(),
                    owner: metadata.uid(),
                    expected: match owner_policy {
                        SecretOwner::Any => unreachable!(),
                        SecretOwner::Reader => format!("uid {reader}"),
                        SecretOwner::RootOrReader => format!("root or uid {reader}"),
                    },
                });
            }
        }
        if metadata.nlink() != 1 {
            return Err(PersistError::InsecureLinkCount {
                path: path.display().to_string(),
                links: metadata.nlink(),
            });
        }
    }

    // Read one byte beyond the only valid size so a malicious pseudo-file or
    // oversized secret is rejected without an unbounded allocation.
    let mut bytes = Zeroizing::new(Vec::with_capacity(33));
    file.take(33).read_to_end(&mut bytes)?;
    if bytes.len() != 32 {
        return Err(PersistError::WrongKeyLength(bytes.len()));
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    seed.copy_from_slice(&bytes);
    Ok(SigningKey::from_bytes(&seed))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SecretOwner {
    Any,
    Reader,
    RootOrReader,
}

#[cfg(unix)]
fn secret_key_owner_is_accepted(policy: SecretOwner, owner: u32, reader: u32) -> bool {
    match policy {
        SecretOwner::Any => true,
        SecretOwner::Reader => owner == reader,
        SecretOwner::RootOrReader => owner == 0 || owner == reader,
    }
}

/// Write a 32-byte Ed25519 signing key to disk with mode 0600.
///
/// # Errors
///
/// Returns an error if `path` has no parent directory, or if the
/// atomic write fails.
pub fn write_signing_key(path: &Path, sk: &SigningKey) -> io::Result<()> {
    protect_secret_process()?;
    // A partial write must leave the previous secret intact, and the mode is
    // set on the temp file so the key is never briefly world-readable at its
    // final path.
    path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let seed = Zeroizing::new(sk.to_bytes());
    #[cfg(windows)]
    return private_fs::write_atomic(path, seed.as_ref());
    #[cfg(not(windows))]
    atomic_file::write_with_mode(path, seed.as_ref(), 0o600)
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
/// [`PersistError::InsecureOwner`] if it is owned by neither root nor the
/// reader,
/// [`PersistError::Io`] if it cannot be read, or
/// [`PersistError::WrongKeyLength`] if it is not exactly 32 bytes.
pub fn read_device_key(path: &Path) -> Result<SigningKey, PersistError> {
    signing_key_from_file(path, 0o007, SecretOwner::RootOrReader)
}

/// Read the former group-owned key during its one-time privileged migration.
/// Historical versions allowed the session account to create that inode, so
/// ownership can be tightened only after adoption into the protected store.
/// New callers must use [`read_device_key`].
///
/// # Errors
///
/// Applies the same bounded no-follow, regular-file, mode, link-count, and
/// length checks as [`read_device_key`], except for the historical owner rule.
pub fn read_legacy_device_key(path: &Path) -> Result<SigningKey, PersistError> {
    signing_key_from_file(path, 0o007, SecretOwner::Any)
}

/// Write the device key with [`DEVICE_KEY_MODE`], atomically.
///
/// # Errors
///
/// Returns an error if `path` has no parent directory, or if the
/// atomic write fails.
pub fn write_device_key(path: &Path, sk: &SigningKey) -> io::Result<()> {
    protect_secret_process()?;
    path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let seed = Zeroizing::new(sk.to_bytes());
    #[cfg(windows)]
    return private_fs::write_atomic(path, seed.as_ref());
    #[cfg(not(windows))]
    atomic_file::write_with_mode(path, seed.as_ref(), DEVICE_KEY_MODE)
}

/// Whether [`load_or_create_device_key`] found a key or minted one.
///
/// Minting is a first-boot event that invalidates nothing. Generating a
/// replacement when a key was expected would change the device identity;
/// this function therefore never generates over an unreadable file.
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
/// A key file that exists but cannot be read is an error. Replacing it would
/// change the device identity and invalidate grants written against the old
/// key without reporting the cause. A wrong mode, truncated file, or I/O
/// failure returns [`PersistError`] so an operator can restore a backup or
/// re-enrol the device.
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
    protect_secret_process()?;
    let path = paths.device_privkey.as_path();
    match fs::metadata(path) {
        Ok(_) => Ok((read_device_key(path)?, DeviceKeyOutcome::Loaded)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            paths.ensure_directories()?;
            let sk = SigningKey::generate(&mut UnwrapErr(SysRng));
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
/// temporary name, then Linux `renameat2(RENAME_NOREPLACE)` atomically claims
/// the final name only if it is still absent. Unlike a hard-link claim, the
/// private key is never briefly reachable through two names. A caller that
/// loses returns `Ok(false)` and must load the winner from disk.
#[cfg(not(windows))]
fn publish_device_key_no_clobber(path: &Path, key: &SigningKey) -> Result<bool, PersistError> {
    protect_secret_process()?;
    let seed = Zeroizing::new(key.to_bytes());
    publish_identity_bytes_no_clobber(path, seed.as_ref(), DEVICE_KEY_MODE)
}

#[cfg(not(windows))]
fn publish_identity_bytes_no_clobber(
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<bool, PersistError> {
    let parent = path.parent().ok_or_else(|| {
        PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "device-key path has no parent",
        ))
    })?;
    let destination = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "device-key path contains a NUL byte",
        ))
    })?;
    // Validate the caller-controlled final path before persisting a temporary
    // secret. Otherwise an invalid NUL-containing basename would strand the
    // complete candidate in the shared identity directory.
    // Reject intermediate links before placing any secret in the directory.
    let directory = open_regular_without_symlinks(parent)?;
    if !directory.metadata()?.is_dir() {
        return Err(io::Error::other("identity parent is not a directory").into());
    }
    let temp = create_device_key_claim_temp(parent, bytes, mode)?;
    let source = CString::new(temp.as_os_str().as_bytes()).map_err(|_| {
        let _ = fs::remove_file(&temp);
        PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "device-key temporary path contains a NUL byte",
        ))
    })?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: both paths are live NUL-terminated strings. RENAME_EXCL is the
    // Darwin atomic no-replace claim; it cannot overwrite another creator.
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let result = {
        let _ = fs::remove_file(&temp);
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace identity publication is unsupported on this platform",
        )));
    };
    let claim = if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    };
    if claim.is_err() {
        let _ = fs::remove_file(&temp);
    }

    match claim {
        Ok(()) => {
            // The key contents reached storage before the directory entry was
            // published.  Sync the parent as well, so a successful first boot
            // survives a power loss after the no-clobber rename.
            atomic_file::sync_directory(parent)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(PersistError::Io(error)),
    }
}

// Windows migration artifacts use a user-only DACL, including public metadata.
// Unix modes cannot express that boundary; use the same ACL-validated atomic
// publication as newly generated keys, never the generic file writer.
#[cfg(windows)]
fn publish_identity_bytes_no_clobber(
    path: &Path,
    bytes: &[u8],
    _mode: u32,
) -> Result<bool, PersistError> {
    Ok(private_fs::create_atomic(path, bytes)?)
}

/// Publish a Windows key through the private ACL-validated no-clobber path.
#[cfg(windows)]
fn publish_device_key_no_clobber(path: &Path, key: &SigningKey) -> Result<bool, PersistError> {
    protect_secret_process()?;
    let seed = Zeroizing::new(key.to_bytes());
    Ok(private_fs::create_atomic(path, seed.as_ref())?)
}

/// Create a private, complete temporary candidate beside the final device-key
/// path. The mode is applied before publication into the shared identity tree.
#[cfg(not(windows))]
fn create_device_key_claim_temp(
    parent: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<std::path::PathBuf, PersistError> {
    for _ in 0..16 {
        let sequence = DEVICE_KEY_CLAIM_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(
            ".tilo-device-key-claim-{}-{sequence}",
            std::process::id()
        ));
        let candidate = {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(mode);
            }
            options.open(&temp)
        };
        match candidate {
            Ok(mut file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    // `mode` is filtered by the process umask; explicitly
                    // restore the intended shared-device mode while the file
                    // is still private under its temporary name.
                    file.set_permissions(fs::Permissions::from_mode(mode))?;
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
    protect_secret_process()?;
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

/// Adopt the former qOS identity into the canonical Tilo layout without
/// deleting or overwriting any existing material. A different established
/// canonical device identity takes precedence and never receives legacy roots.
/// Matching device keys allow an interrupted adoption to resume.
///
/// # Errors
/// Rejects unreadable, symlinked, malformed, or conflicting identity artifacts.
/// Every certificate is verified and must certify the adopted device key.
pub fn adopt_legacy_system_identity(
    paths: &IdentityPaths,
    legacy_root: &Path,
) -> Result<bool, PersistError> {
    let old = IdentityPaths::under(legacy_root);
    let legacy_key = if artifact_present(&old.device_privkey)? {
        old.device_privkey.clone()
    } else {
        legacy_root.join("device/keypair.bin")
    };
    if !artifact_present(&legacy_key)? {
        // An incomplete identity is not a new machine: never silently re-key.
        if artifact_present(&old.user_pubkey)?
            || artifact_present(&old.user_privkey)?
            || artifact_present(&old.device_cert)?
        {
            return Err(io::Error::other("legacy identity exists without its device key").into());
        }
        return Ok(false);
    }
    let key = read_legacy_device_key(&legacy_key)?;
    let current_exists = artifact_present(&paths.device_privkey)?;
    if current_exists && read_device_key(&paths.device_privkey)?.to_bytes() != key.to_bytes() {
        return Ok(false);
    }
    let mut artifacts: Vec<(std::path::PathBuf, Zeroizing<Vec<u8>>, u32)> = Vec::new();
    let root = if artifact_present(&old.user_pubkey)? {
        let public = read_public_key(&old.user_pubkey)?;
        artifacts.push((
            paths.user_pubkey.clone(),
            Zeroizing::new(public.to_vec()),
            0o644,
        ));
        Some(public)
    } else {
        None
    };
    if artifact_present(&old.user_privkey)? {
        let signing = read_signing_key(&old.user_privkey)?;
        if root != Some(signing.verifying_key().to_bytes()) {
            return Err(
                io::Error::other("legacy private and public identity roots disagree").into(),
            );
        }
        artifacts.push((
            paths.user_privkey.clone(),
            Zeroizing::new(signing.to_bytes().to_vec()),
            0o600,
        ));
    }
    let expected_default_root = match root {
        Some(root) => Some(root),
        None if artifact_present(&paths.user_pubkey)? => Some(read_public_key(&paths.user_pubkey)?),
        None => None,
    };
    let mut certificates = Vec::new();
    if artifact_present(&old.device_cert)? {
        certificates.push((old.device_cert.clone(), paths.device_cert.clone(), true));
    }
    let certs_dir = legacy_root.join("device/certs");
    if artifact_present(&certs_dir)? {
        let directory = open_regular_without_symlinks(&certs_dir)?;
        if !directory.metadata()?.is_dir() {
            return Err(io::Error::other("legacy certificate store is not a directory").into());
        }
        for entry in fs::read_dir(&certs_dir)? {
            let entry = entry?;
            let certificate = read_device_cert(&entry.path())?;
            let expected = old.device_cert_for_user(&certificate.user_public_key);
            if entry.path() != expected {
                return Err(io::Error::other(
                    "legacy certificate filename does not match its root",
                )
                .into());
            }
            certificates.push((
                entry.path(),
                paths.device_cert_for_user(&certificate.user_public_key),
                false,
            ));
        }
    }
    for (source, destination, is_default) in certificates {
        let certificate = read_device_cert(&source)?;
        crate::verify_device_cert(&certificate)
            .map_err(|error| io::Error::other(error.to_string()))?;
        if certificate.device_public_key != key.verifying_key().to_bytes()
            || (is_default
                && expected_default_root.is_some_and(|root| root != certificate.user_public_key))
        {
            return Err(io::Error::other(
                "legacy certificate does not match its device or identity root",
            )
            .into());
        }
        artifacts.push((
            destination,
            Zeroizing::new(crate::canonical::canonical_bytes(&certificate)?),
            0o644,
        ));
    }
    // Check all conflicts before publishing even the device key. Existing
    // paths are read with the same no-symlink, bounded artifact reader.
    for (destination, bytes, _) in &artifacts {
        if destination == &paths.user_privkey && artifact_present(destination)? {
            read_signing_key(destination)?;
        }
        if artifact_present(destination)?
            && read_regular_bounded(destination, bytes.len())? != **bytes
        {
            return Err(io::Error::other(format!(
                "canonical identity artifact conflicts with legacy: {}",
                destination.display()
            ))
            .into());
        }
    }
    paths.ensure_directories()?;
    if !current_exists {
        // A concurrent publisher may win; only its identical key is safe.
        publish_device_key_no_clobber(&paths.device_privkey, &key)?;
        if read_device_key(&paths.device_privkey)?.to_bytes() != key.to_bytes() {
            return Err(io::Error::other(
                "concurrent canonical device identity differs from legacy",
            )
            .into());
        }
    }
    for (destination, bytes, mode) in artifacts {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        if !publish_identity_bytes_no_clobber(&destination, &bytes, mode)?
            && read_regular_bounded(&destination, bytes.len())? != *bytes
        {
            return Err(io::Error::other(
                "concurrent canonical identity artifact differs from legacy",
            )
            .into());
        }
    }
    Ok(true)
}

fn artifact_present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Read a signed device cert from disk.
///
/// # Errors
///
/// Returns [`PersistError::Io`] if `path` cannot be read, or
/// [`PersistError::Canonical`] if the bytes do not decode as a
/// `DeviceCert`.
pub fn read_device_cert(path: &Path) -> Result<DeviceCert, PersistError> {
    let bytes = read_regular_bounded(path, MAX_DEVICE_CERT_BYTES)?;
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
fn reject_metadata_mode(
    path: &Path,
    metadata: &fs::Metadata,
    forbidden: u32,
) -> Result<(), PersistError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & forbidden != 0 {
        return Err(PersistError::InsecurePermissions {
            path: path.display().to_string(),
            mode,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_tempdir() -> tempfile::TempDir {
        // macOS exposes /var via a symlink; security fixtures must start
        // from the actual directory before adding their deliberate links.
        tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
    }
    use ed25519_dalek::Signer;
    use std::sync::{Arc, Barrier};

    #[cfg(target_os = "linux")]
    #[test]
    fn secret_operations_make_the_process_non_dumpable() {
        protect_secret_process().expect("disable identity core dumps");
        // SAFETY: PR_GET_DUMPABLE consumes scalar arguments only.
        assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) }, 0);
    }

    #[test]
    fn legacy_system_identity_is_adopted_without_deleting_old_material() {
        let root = identity_tempdir();
        let legacy = root.path().join("qos");
        let current = IdentityPaths::under(root.path().join("tilo"));
        let old = IdentityPaths::under(&legacy);
        old.ensure_directories().unwrap();
        let key = SigningKey::from_bytes(&[41; 32]);
        write_device_key(&legacy.join("device/keypair.bin"), &key).unwrap();
        write_public_key(&old.user_pubkey, &[7; 32]).unwrap();
        adopt_legacy_system_identity(&current, &legacy).unwrap();
        assert_eq!(
            read_device_key(&current.device_privkey).unwrap().to_bytes(),
            [41; 32]
        );
        assert_eq!(read_public_key(&current.user_pubkey).unwrap(), [7; 32]);
        assert_eq!(
            read_device_key(&legacy.join("device/keypair.bin"))
                .unwrap()
                .to_bytes(),
            [41; 32]
        );
    }

    #[test]
    fn legacy_system_adoption_resumes_after_key_claim() {
        let root = identity_tempdir();
        let legacy = root.path().join("qos");
        let current = IdentityPaths::under(root.path().join("tilo"));
        let old = IdentityPaths::under(&legacy);
        old.ensure_directories().unwrap();
        let key = SigningKey::from_bytes(&[41; 32]);
        write_device_key(&legacy.join("device/keypair.bin"), &key).unwrap();
        write_public_key(&old.user_pubkey, &[7; 32]).unwrap();
        adopt_device_key(&current, &[41; 32]).unwrap();
        adopt_legacy_system_identity(&current, &legacy).unwrap();
        assert_eq!(read_public_key(&current.user_pubkey).unwrap(), [7; 32]);
        adopt_legacy_system_identity(&current, &legacy).unwrap();
        assert_eq!(
            read_device_key(&current.device_privkey).unwrap().to_bytes(),
            [41; 32]
        );
    }

    #[test]
    fn legacy_system_adoption_never_mixes_different_canonical_identity() {
        let root = identity_tempdir();
        let legacy = root.path().join("qos");
        let current = IdentityPaths::under(root.path().join("tilo"));
        let old = IdentityPaths::under(&legacy);
        old.ensure_directories().unwrap();
        write_device_key(
            &legacy.join("device/keypair.bin"),
            &SigningKey::from_bytes(&[41; 32]),
        )
        .unwrap();
        write_public_key(&old.user_pubkey, &[7; 32]).unwrap();
        adopt_device_key(&current, &[42; 32]).unwrap();
        assert!(!adopt_legacy_system_identity(&current, &legacy).unwrap());
        assert!(!current.user_pubkey.exists());
        assert_eq!(
            read_device_key(&current.device_privkey).unwrap().to_bytes(),
            [42; 32]
        );
    }

    #[test]
    fn legacy_system_adoption_refuses_conflicting_root_before_claiming_key() {
        let root = identity_tempdir();
        let legacy = root.path().join("qos");
        let current = IdentityPaths::under(root.path().join("tilo"));
        let old = IdentityPaths::under(&legacy);
        old.ensure_directories().unwrap();
        current.ensure_directories().unwrap();
        write_device_key(
            &legacy.join("device/keypair.bin"),
            &SigningKey::from_bytes(&[41; 32]),
        )
        .unwrap();
        write_public_key(&old.user_pubkey, &[7; 32]).unwrap();
        write_public_key(&current.user_pubkey, &[8; 32]).unwrap();
        assert!(adopt_legacy_system_identity(&current, &legacy).is_err());
        assert!(!current.device_privkey.exists());
        assert_eq!(read_public_key(&current.user_pubkey).unwrap(), [8; 32]);
    }

    #[test]
    fn legacy_system_adoption_preserves_valid_certificates_and_private_root() {
        let root = identity_tempdir();
        let legacy = root.path().join("qos");
        let current = IdentityPaths::under(root.path().join("tilo"));
        let old = IdentityPaths::under(&legacy);
        old.ensure_directories().unwrap();
        let (user_key, user) = crate::UserIdentity::generate("Owner", 1);
        let (device_key, device) = crate::DeviceIdentity::generate("Machine", 2);
        write_device_key(&legacy.join("device/keypair.bin"), &device_key).unwrap();
        write_public_key(&old.user_pubkey, &user.public_key).unwrap();
        write_signing_key(&old.user_privkey, &user_key).unwrap();
        let mut certificate = crate::DeviceCert {
            user_public_key: user.public_key,
            device_public_key: device.public_key,
            device_label: device.label,
            capabilities: crate::cert::DeviceCapabilities::primary(),
            issued_at_ns: 3,
            expires_at_ns: None,
            signature: [0; 64],
        };
        crate::cert::sign_device_cert(&mut certificate, &user_key).unwrap();
        write_device_cert(&old.device_cert, &certificate).unwrap();
        write_device_cert(&old.device_cert_for_user(&user.public_key), &certificate).unwrap();
        adopt_legacy_system_identity(&current, &legacy).unwrap();
        assert_eq!(
            read_signing_key(&current.user_privkey).unwrap().to_bytes(),
            user_key.to_bytes()
        );
        let migrated = read_device_cert(&current.device_cert).unwrap();
        crate::verify_device_cert(&migrated).unwrap();
        assert_eq!(migrated.device_public_key, device.public_key);
        assert_eq!(
            read_device_cert(&current.device_cert_for_user(&user.public_key))
                .unwrap()
                .signature,
            certificate.signature
        );
        assert!(old.device_cert.exists());
    }

    #[cfg(unix)]
    #[test]
    fn legacy_system_adoption_refuses_symlinked_source() {
        let root = identity_tempdir();
        let legacy = root.path().join("qos");
        let current = IdentityPaths::under(root.path().join("tilo"));
        let old = IdentityPaths::under(&legacy);
        old.ensure_directories().unwrap();
        write_device_key(
            &legacy.join("device/keypair.bin"),
            &SigningKey::from_bytes(&[41; 32]),
        )
        .unwrap();
        let outside = root.path().join("outside.pubkey");
        write_public_key(&outside, &[7; 32]).unwrap();
        std::os::unix::fs::symlink(&outside, &old.user_pubkey).unwrap();
        assert!(adopt_legacy_system_identity(&current, &legacy).is_err());
        assert!(!current.device_privkey.exists());
    }

    #[test]
    fn replacing_a_signing_key_preserves_private_readability() {
        let root = identity_tempdir();
        let path = root.path().join("signing.bin");
        let first = SigningKey::from_bytes(&[17; 32]);
        let second = SigningKey::from_bytes(&[29; 32]);
        write_signing_key(&path, &first).unwrap();
        assert_eq!(
            read_signing_key(&path).unwrap().to_bytes(),
            first.to_bytes()
        );
        write_signing_key(&path, &second).unwrap();
        assert_eq!(
            read_signing_key(&path).unwrap().to_bytes(),
            second.to_bytes()
        );
    }

    #[cfg(windows)]
    #[test]
    fn public_windows_key_is_neither_read_nor_silently_replaced() {
        let root = identity_tempdir();
        let paths = IdentityPaths::under(root.path());
        let (original, _) = load_or_create_device_key(&paths).unwrap();
        let result = std::process::Command::new("icacls.exe")
            .arg(&paths.device_privkey)
            .args(["/grant", "*S-1-1-0:(R)"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        for result in [
            read_signing_key(&paths.device_privkey),
            read_device_key(&paths.device_privkey),
            load_or_create_device_key(&paths).map(|(key, _)| key),
        ] {
            assert!(matches!(result, Err(PersistError::Io(error))
                if error.kind() == io::ErrorKind::PermissionDenied));
        }
        assert_eq!(
            write_device_key(&paths.device_privkey, &SigningKey::from_bytes(&[9; 32]))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied,
        );
        assert_eq!(
            fs::read(&paths.device_privkey).unwrap(),
            original.to_bytes()
        );
    }

    #[test]
    fn explicit_public_key_path_outside_managed_layout_round_trips() {
        let tmp = identity_tempdir();
        let path = tmp.path().join("pubkey.bin");
        let key = [7u8; 32];
        write_public_key(&path, &key).unwrap();
        let back = read_public_key(&path).unwrap();
        assert_eq!(key, back);
    }

    #[cfg(unix)]
    #[test]
    fn authorization_artifacts_never_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = identity_tempdir();
        let identity_root = tmp.path().join("identity-root");
        let user_identity = identity_root.join("user-identity");
        fs::create_dir_all(&user_identity).unwrap();
        let real = user_identity.join("pubkey.bin");
        let link = user_identity.join("linked-pubkey.bin");
        fs::write(&real, [7_u8; 32]).unwrap();
        symlink(&real, &link).unwrap();
        assert!(matches!(read_public_key(&link), Err(PersistError::Io(_))));
        let real_parent = identity_root.join("real-parent");
        fs::create_dir_all(&real_parent).unwrap();
        fs::write(real_parent.join("pubkey.bin"), [9_u8; 32]).unwrap();
        let linked_parent = user_identity.join("linked-parent");
        symlink(&real_parent, &linked_parent).unwrap();
        assert!(matches!(
            read_public_key(&linked_parent.join("pubkey.bin")),
            Err(PersistError::Io(_))
        ));

        write_public_key(&link, &[8_u8; 32]).unwrap();
        assert_eq!(fs::read(&real).unwrap(), [7_u8; 32]);
        assert_eq!(read_public_key(&link).unwrap(), [8_u8; 32]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn authorization_artifact_under_distro_home_symlink_is_readable() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let real_home = tmp.path().join("var/home/alice");
        let identity = real_home.join(".local/share/tilo/user-identity");
        fs::create_dir_all(&identity).unwrap();
        symlink(tmp.path().join("var/home"), tmp.path().join("home")).unwrap();
        let path = tmp
            .path()
            .join("home/alice/.local/share/tilo/user-identity/pubkey.bin");
        fs::write(&path, [11_u8; 32]).unwrap();

        assert_eq!(read_public_key(&path).unwrap(), [11_u8; 32]);
    }

    #[cfg(unix)]
    #[test]
    fn signing_key_round_trip_enforces_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = identity_tempdir();
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
        let tmp = identity_tempdir();
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

    #[cfg(unix)]
    #[test]
    fn signing_key_never_follows_a_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = identity_tempdir();
        let real = tmp.path().join("real.key");
        let link = tmp.path().join("linked.key");
        let (sk, _pk) = crate::UserIdentity::generate("Alice", 1);
        write_signing_key(&real, &sk).unwrap();
        symlink(&real, &link).unwrap();

        match read_signing_key(&link).unwrap_err() {
            PersistError::Io(_) => {}
            other => panic!("expected symlink refusal, got {other:?}"),
        }

        let linked_parent = tmp.path().join("linked-parent");
        symlink(tmp.path(), &linked_parent).unwrap();
        match read_signing_key(&linked_parent.join("real.key")).unwrap_err() {
            PersistError::Io(_) => {}
            other => panic!("expected parent-symlink refusal, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn signing_key_rejects_an_inode_with_multiple_names() {
        let tmp = identity_tempdir();
        let path = tmp.path().join("private.key");
        let alias = tmp.path().join("unexpected-alias.key");
        let sk = SigningKey::from_bytes(&[9_u8; 32]);
        write_signing_key(&path, &sk).unwrap();
        fs::hard_link(&path, &alias).unwrap();

        match read_signing_key(&path).unwrap_err() {
            PersistError::InsecureLinkCount { links, .. } => assert_eq!(links, 2),
            other => panic!("expected hard-link refusal, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn secret_key_owner_policies_are_distinct() {
        assert!(secret_key_owner_is_accepted(SecretOwner::Any, 1001, 1000));
        assert!(secret_key_owner_is_accepted(
            SecretOwner::Reader,
            1000,
            1000
        ));
        assert!(!secret_key_owner_is_accepted(SecretOwner::Reader, 0, 1000));
        assert!(secret_key_owner_is_accepted(
            SecretOwner::RootOrReader,
            0,
            1000
        ));
        assert!(secret_key_owner_is_accepted(
            SecretOwner::RootOrReader,
            1000,
            1000
        ));
        assert!(!secret_key_owner_is_accepted(
            SecretOwner::RootOrReader,
            1001,
            1000
        ));
    }

    #[test]
    fn wrong_length_key_rejected() {
        let tmp = identity_tempdir();
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

        let tmp = identity_tempdir();
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
        let paths = IdentityPaths::under("/var/lib/tilo");
        let alice = paths.device_cert_for_user(&[1; 32]);
        let bob = paths.device_cert_for_user(&[2; 32]);
        assert_ne!(alice, bob);
        assert!(alice.starts_with("/var/lib/tilo/device/certs"));
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
        let tmp = identity_tempdir();
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

    #[cfg(unix)]
    #[test]
    fn device_key_rejects_an_inode_with_multiple_names() {
        let tmp = identity_tempdir();
        let paths = IdentityPaths::under(tmp.path());
        let (_, outcome) = load_or_create_device_key(&paths).unwrap();
        assert_eq!(outcome, DeviceKeyOutcome::Created);
        let alias = tmp.path().join("device-key-alias.bin");
        fs::hard_link(&paths.device_privkey, &alias).unwrap();

        match read_device_key(&paths.device_privkey).unwrap_err() {
            PersistError::InsecureLinkCount { links, .. } => assert_eq!(links, 2),
            other => panic!("expected hard-link refusal, got {other:?}"),
        }
    }

    /// The relaxation is scoped to the device key. A user-scoped secret
    /// still has to be owner-only — the group that reads the device key
    /// has no business reading the user's own signing key.
    #[cfg(unix)]
    #[test]
    fn user_scoped_secret_still_refuses_group_read() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = identity_tempdir();
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
        let tmp = identity_tempdir();
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(&paths.device_privkey).unwrap().nlink(), 1);
        }
    }

    #[test]
    fn simultaneous_device_key_creation_uses_one_persisted_key() {
        let tmp = identity_tempdir();
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
        let tmp = identity_tempdir();
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
        let tmp = identity_tempdir();
        let paths = IdentityPaths::under(tmp.path());
        let seed = [23u8; 32];

        let adopted = adopt_device_key(&paths, &seed).unwrap();
        assert_eq!(adopted.to_bytes(), seed);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(&paths.device_privkey).unwrap().nlink(), 1);
        }
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
        let tmp = identity_tempdir();
        let paths = IdentityPaths::under(tmp.path());
        paths.ensure_directories().unwrap();
        assert!(tmp.path().join("user-identity").is_dir());
        assert!(tmp.path().join("device-private").is_dir());
        assert!(tmp.path().join("device").is_dir());
    }
}
