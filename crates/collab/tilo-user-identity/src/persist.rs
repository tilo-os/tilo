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
use crate::cert::{
    DeviceCapabilities, DeviceCert, device_cert_signing_digest, verify_device_cert_at,
};

const MAX_DEVICE_CERT_BYTES: usize = 64 * 1024;

#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::unnecessary_wraps,
        reason = "only Linux can fail to disable dumps; callers share one signature"
    )
)]
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

/// Distributions and platforms put ordinary directories behind symlinks they
/// own: Fedora Atomic exposes /home as a symlink to /var/home, and macOS puts
/// /tmp and /var (so every `$TMPDIR`) behind /private. Resolving an absolute
/// path while rejecting every symlink would refuse a perfectly normal login
/// before it reaches Tilo's identity tree. Managed `IdentityPaths` are
/// therefore anchored at the parent of the nearest identity directory, and
/// the remainder is resolved beneath that canonical directory, which keeps
/// symlink rejection inside Tilo's storage. Explicit CLI paths have no such
/// managed boundary and keep the strict absolute-path behavior.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn managed_root(path: &Path) -> Option<(&Path, &Path)> {
    path.ancestors()
        .zip(path.ancestors().skip(1))
        .find_map(|(child, parent)| {
            let name = child.file_name()?.to_str()?;
            if !matches!(name, "user-identity" | "device-private" | "device") {
                return None;
            }
            Some((parent, path.strip_prefix(parent).ok()?))
        })
}

#[cfg(target_os = "linux")]
fn open_regular_without_symlinks(path: &Path) -> io::Result<fs::File> {
    let (root, candidate, beneath) = if let Some((root, relative)) = managed_root(path) {
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
        (Some(root), candidate, true)
    } else {
        let candidate = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "identity artifact path contains an embedded NUL byte",
            )
        })?;
        (None, candidate, false)
    };
    crate::nofollow::open(
        root.as_ref(),
        &candidate,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK,
        0,
        beneath,
    )
}

/// Darwin rejects symlinks in every component of the path it resolves
/// (`O_NOFOLLOW_ANY`), matching Linux openat2 `RESOLVE_NO_SYMLINKS`; managed
/// paths resolve beneath their canonical root descriptor (see
/// [`managed_root`]).
#[cfg(target_os = "macos")]
fn open_regular_without_symlinks(path: &Path) -> io::Result<fs::File> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    let (root, candidate) = match managed_root(path) {
        Some((root, relative)) => (Some(fs::File::open(fs::canonicalize(root)?)?), relative),
        None => (None, path),
    };
    let candidate = CString::new(candidate.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity artifact path contains an embedded NUL byte",
        )
    })?;
    let directory = root
        .as_ref()
        .map_or(libc::AT_FDCWD, std::os::fd::AsRawFd::as_raw_fd);
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW_ANY;
    // SAFETY: `candidate` is NUL-terminated and `directory` is AT_FDCWD or a
    // descriptor `root` keeps open for this call.
    let descriptor = unsafe { libc::openat(directory, candidate.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned this owned descriptor and no other File owns it.
    Ok(unsafe { fs::File::from_raw_fd(descriptor) })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open_regular_without_symlinks(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
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
    /// Where the device key itself is kept.
    pub device_key_custody: DeviceKeyCustody,
}

/// Where a layout keeps this device's private key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceKeyCustody {
    /// A group-private file at [`IdentityPaths::device_privkey`]: the Tilo OS
    /// layout, where the root filesystem daemon and the session share it.
    #[default]
    File,
    /// The user's login Keychain. A key file an earlier build left at
    /// [`IdentityPaths::device_privkey`] moves into it once, on first use.
    #[cfg(target_os = "macos")]
    Keychain,
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
            device_key_custody: DeviceKeyCustody::File,
        }
    }

    /// This layout with its device key kept in `custody`.
    #[must_use]
    pub fn with_device_key_custody(mut self, custody: DeviceKeyCustody) -> Self {
        self.device_key_custody = custody;
        self
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
        if let Some(parent) = self
            .device_privkey
            .parent()
            .filter(|_| self.device_key_custody == DeviceKeyCustody::File)
        {
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
            .join(format!("{}.bin", hex::encode(user_public_key)))
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

/// Atomically write a 32-byte Ed25519 public key to disk with
/// [`PUBLIC_FILE_MODE`] — public keys are non-secret. Atomic
/// replacement avoids both partial trust roots and following a final symlink.
///
/// # Errors
///
/// Returns an error if writing to `path` fails.
pub fn write_public_key(path: &Path, pubkey: &[u8; 32]) -> io::Result<()> {
    path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    write_public_file(path, pubkey)
}

/// The directories public identity files live in get this mode whatever the
/// writer's umask, keeping a setgid bit inherited from their parent. The
/// machine's group-writable device tree must also allow later login users
/// in that same trust group to publish their own root-signed certificates.
const PUBLIC_DIR_MODE: u32 = 0o755;

fn create_public_dir(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(path)?;
    // The umask applies to a mode given at creation; set it afterwards.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let inherited = fs::metadata(path)?.permissions().mode() & 0o7000;
        let parent_mode = path
            .parent()
            .map(fs::metadata)
            .transpose()?
            .map(|metadata| metadata.permissions().mode() & 0o7777);
        let mode = if parent_mode == Some(0o2770) {
            0o770
        } else {
            PUBLIC_DIR_MODE
        };
        fs::set_permissions(path, fs::Permissions::from_mode(inherited | mode))?;
    }
    Ok(())
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
        let accepted_owner = match owner_policy {
            SecretOwner::Any => None,
            SecretOwner::Reader => Some("uid"),
            SecretOwner::RootOrReader => Some("root or uid"),
        };
        if let Some(accepted_owner) = accepted_owner {
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
                    expected: format!("{accepted_owner} {reader}"),
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
    tilo_files::native::atomic::write_with_mode(path, seed.as_ref(), 0o600)
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

/// Mode of the public identity files: the person's public key and the device
/// certificates. Pinned rather than left to the writer's umask: a root-run
/// enrollment under a strict umask would otherwise leave them unreadable to
/// the session that verifies them.
pub const PUBLIC_FILE_MODE: u32 = 0o644;

/// Atomically replace a public identity file with [`PUBLIC_FILE_MODE`].
fn write_public_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        tilo_files::native::atomic::write_with_mode(path, bytes, PUBLIC_FILE_MODE)
    }
    #[cfg(not(unix))]
    {
        tilo_files::native::atomic::write(path, bytes)
    }
}

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

/// Read this device's key from wherever `paths` keeps it.
///
/// # Errors
///
/// As [`read_device_key`] for a key file. A Keychain that holds no key, or
/// refuses or fails to answer, is an error: a missing device key is never
/// replaced here.
pub fn load_device_key(paths: &IdentityPaths) -> Result<SigningKey, PersistError> {
    match paths.device_key_custody {
        DeviceKeyCustody::File => read_device_key(&paths.device_privkey),
        #[cfg(target_os = "macos")]
        DeviceKeyCustody::Keychain => crate::keychain::load(paths)?.ok_or_else(|| {
            PersistError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "the login Keychain holds no Tilo device key",
            ))
        }),
    }
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
    tilo_files::native::atomic::write_with_mode(path, seed.as_ref(), DEVICE_KEY_MODE)
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
    #[cfg(target_os = "macos")]
    if paths.device_key_custody == DeviceKeyCustody::Keychain {
        return crate::keychain::load_or_create(paths);
    }
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

/// Whether the `renameat2` that just failed did so because the filesystem
/// has no `RENAME_NOREPLACE` (gVisor's gofer mounts answer `EINVAL`).
#[cfg(target_os = "linux")]
fn no_replace_rename_unsupported() -> bool {
    matches!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::EINVAL | libc::ENOSYS)
    )
}

/// Claim `destination` with a hard link, which fails on an existing name as
/// `RENAME_NOREPLACE` would, then drop the source name. Both names are in the
/// same private directory for the moment between. Returns 0 or -1 with
/// `errno` set, like the syscall it stands in for.
#[cfg(target_os = "linux")]
fn link_claim(source: &CString, destination: &CString) -> libc::c_long {
    // SAFETY: both pointers refer to live NUL-terminated path buffers.
    let linked = unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            0,
        )
    };
    if linked != 0 {
        return -1;
    }
    // SAFETY: `source` is a live NUL-terminated path buffer.
    unsafe { libc::unlink(source.as_ptr()) };
    0
}

/// Publish a complete device key exactly once.
///
/// `tilo_files::native::atomic::write_with_mode` is intentionally not used here: its final
/// rename correctly replaces a file for ordinary updates, but device-key
/// creation must never replace a key another first-login process just made.
/// The candidate is fully written and synced under a unique same-directory
/// temporary name, then Linux `renameat2(RENAME_NOREPLACE)` atomically claims
/// the final name only if it is still absent. Unlike a hard-link claim, the
/// private key is never briefly reachable through two names; a filesystem
/// without `RENAME_NOREPLACE` falls back to that claim. A caller that
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
    #[cfg(target_os = "linux")]
    let result = if result != 0 && no_replace_rename_unsupported() {
        link_claim(&source, &destination)
    } else {
        result
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
            tilo_files::native::atomic::sync_directory(parent)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(PersistError::Io(error)),
    }
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
    if paths.device_key_custody != DeviceKeyCustody::File {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "only a device key file adopts an earlier subsystem key",
        )));
    }
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
    let bytes = read_regular_bounded(path, MAX_DEVICE_CERT_BYTES)?;
    let cert: DeviceCert = crate::canonical::from_canonical_bytes(&bytes)?;
    Ok(cert)
}

/// Write a signed device cert to disk. Public metadata, with
/// [`PUBLIC_FILE_MODE`].
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
    create_public_dir(parent)?;
    write_public_file(path, &bytes)?;
    Ok(())
}

/// Certify a new device for a person whose root key stays on the setup machine.
///
/// `sign` is the person's existing root capability. It receives the device
/// certificate digest and returns the signature. This function never receives
/// or writes the root secret. It writes the person's public key, a freshly
/// generated device key, and the signed certificate under `paths`, both as the
/// default and the identity-specific certificate used for authorization. An
/// existing device key is left untouched.
///
/// # Errors
///
/// Returns [`PersistError::Io`] when the label is empty or longer than 64
/// bytes, the device key is already present, or signing or writing fails.
pub fn provision_joined_device(
    paths: &IdentityPaths,
    user_public_key: [u8; 32],
    sign: impl FnOnce(&[u8]) -> Result<[u8; 64], String>,
    label: &str,
    now_ns: u64,
) -> Result<DeviceCert, PersistError> {
    if label.is_empty() || label.len() > 64 {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "device label must be 1 to 64 bytes",
        )));
    }
    if paths.device_privkey.exists() || paths.device_cert.exists() {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "target already has a device identity",
        )));
    }
    paths.ensure_directories()?;
    let device = SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut cert = DeviceCert {
        user_public_key,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.to_string(),
        capabilities: DeviceCapabilities {
            can_own_workspaces: true,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: true,
            foreign: false,
            can_administer_devices: false,
        },
        issued_at_ns: now_ns,
        expires_at_ns: None,
        signature: [0; 64],
    };
    let digest = device_cert_signing_digest(&cert).map_err(|error| {
        PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("device certificate digest failed: {error}"),
        ))
    })?;
    cert.signature = sign(&digest).map_err(|error| {
        PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("device certificate was not signed: {error}"),
        ))
    })?;
    verify_device_cert_at(&cert, now_ns).map_err(|error| {
        PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("device certificate did not verify: {error}"),
        ))
    })?;
    write_public_key(&paths.user_pubkey, &cert.user_public_key)?;
    if !publish_device_key_no_clobber(&paths.device_privkey, &device)? {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "target already has a device identity",
        )));
    }
    write_device_cert(&paths.device_cert_for_user(&cert.user_public_key), &cert)?;
    write_device_cert(&paths.device_cert, &cert)?;
    if paths.user_privkey.exists() {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "setup must not place the human root secret on the target",
        )));
    }
    Ok(cert)
}

/// Install a root-signed certificate for this device's existing key, as a
/// device that joined without the root: the user public key and the
/// certificate (default and per-user). The root never comes here. Refuses a
/// certificate for another key, one that does not verify at `now_ns`, one
/// that admits devices, and a second identity once one is installed; the
/// same identity may install a renewed certificate.
///
/// # Errors
///
/// [`PersistError::Io`] with `InvalidData` for a certificate this device
/// must not take, `AlreadyExists` for another identity, or a write failure.
pub fn install_device_cert(
    paths: &IdentityPaths,
    cert: &DeviceCert,
    now_ns: u64,
) -> Result<(), PersistError> {
    let refused = |kind, message: String| PersistError::Io(io::Error::new(kind, message));
    if paths.user_privkey.exists() {
        return Err(refused(
            io::ErrorKind::AlreadyExists,
            "this device holds a user root".into(),
        ));
    }
    let device = read_device_key(&paths.device_privkey)?;
    if cert.device_public_key != device.verifying_key().to_bytes() {
        return Err(refused(
            io::ErrorKind::InvalidData,
            "the certificate is for another device key".into(),
        ));
    }
    verify_device_cert_at(cert, now_ns).map_err(|error| {
        refused(
            io::ErrorKind::InvalidData,
            format!("the certificate did not verify: {error}"),
        )
    })?;
    if cert.capabilities.can_admit_devices {
        return Err(refused(
            io::ErrorKind::InvalidData,
            "a device without the root does not admit devices".into(),
        ));
    }
    if paths.user_pubkey.is_file() && read_public_key(&paths.user_pubkey)? != cert.user_public_key {
        return Err(refused(
            io::ErrorKind::AlreadyExists,
            "this device already belongs to another identity".into(),
        ));
    }
    if let Some(parent) = paths.user_pubkey.parent() {
        fs::create_dir_all(parent)?;
    }
    write_public_key(&paths.user_pubkey, &cert.user_public_key)?;
    write_device_cert(&paths.device_cert_for_user(&cert.user_public_key), cert)?;
    write_device_cert(&paths.device_cert, cert)?;
    Ok(())
}

/// Whether `paths` already holds a device enrollment and no human root: one
/// made by a setup machine, or a nearby pairing approved from another device.
///
/// Missing files are `Ok(false)`, so ordinary first boot still creates an
/// identity. Present but inconsistent or unreadable files are an error: first
/// boot must not replace them with a new person.
///
/// # Errors
///
/// Returns [`PersistError`] when a present identity file cannot be read or the
/// certificate does not match its keys.
pub fn joined_device_is_ready(paths: &IdentityPaths, now_ns: u64) -> Result<bool, PersistError> {
    if paths.user_privkey.exists() {
        return Ok(false);
    }
    if !(paths.user_pubkey.is_file() && paths.device_cert.is_file()) {
        return Ok(false);
    }
    match fs::metadata(&paths.device_privkey) {
        Ok(metadata) if metadata.is_file() => {}
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(PersistError::Io(io::Error::new(
                error.kind(),
                format!(
                    "enrolled device key at {} is not accessible: {error}",
                    paths.device_privkey.display()
                ),
            )));
        }
        _ => return Ok(false),
    }
    let pubkey = read_public_key(&paths.user_pubkey)?;
    let device = read_device_key(&paths.device_privkey)?;
    let cert = read_device_cert(&paths.device_cert)?;
    verify_device_cert_at(&cert, now_ns).map_err(|error| {
        PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("enrolled device certificate did not verify: {error}"),
        ))
    })?;
    // A primary device can admit further devices. That is an ordinary
    // machine, not one the imager already enrolled, even when its root
    // secret lives in the identity record rather than
    // user-identity/privkey.bin, so first-boot provisioning must still run
    // for each OS user.
    if cert.capabilities.can_admit_devices {
        return Ok(false);
    }
    if cert.user_public_key != pubkey || cert.device_public_key != device.verifying_key().to_bytes()
    {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "enrolled device certificate does not match its keys",
        )));
    }
    Ok(true)
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

    #[test]
    fn joined_device_is_certified_without_copying_the_human_root() {
        let root = SigningKey::generate(&mut UnwrapErr(SysRng));
        let dir = identity_tempdir();
        let paths = IdentityPaths::under(dir.path());
        let cert = provision_joined_device(
            &paths,
            root.verifying_key().to_bytes(),
            |digest| Ok(root.sign(digest).to_bytes()),
            "orange-pi",
            1_000,
        )
        .unwrap();

        assert_eq!(cert.user_public_key, root.verifying_key().to_bytes());
        assert_ne!(cert.device_public_key, cert.user_public_key);
        assert!(cert.capabilities.can_read_all_workspaces);
        assert!(!cert.capabilities.can_admit_devices);
        verify_device_cert_at(&cert, 1_000).unwrap();
        assert_eq!(read_device_cert(&paths.device_cert).unwrap(), cert);
        assert_eq!(
            read_device_cert(&paths.device_cert_for_user(&cert.user_public_key)).unwrap(),
            cert
        );
        assert_eq!(
            read_device_key(&paths.device_privkey)
                .unwrap()
                .verifying_key()
                .to_bytes(),
            cert.device_public_key
        );
        assert!(!paths.user_privkey.exists());
        assert!(
            provision_joined_device(
                &paths,
                root.verifying_key().to_bytes(),
                |digest| Ok(root.sign(digest).to_bytes()),
                "orange-pi",
                2_000,
            )
            .unwrap_err()
            .to_string()
            .contains("already has a device identity")
        );
    }

    /// A device whose key was made first takes the certificate a root
    /// signed for it elsewhere, and is then a ready joined device.
    #[test]
    fn a_certificate_signed_elsewhere_installs_for_the_existing_key() {
        let dir = identity_tempdir();
        let paths = IdentityPaths::under(dir.path());
        let (device, _) = load_or_create_device_key(&paths).unwrap();
        let root = SigningKey::from_bytes(&[3; 32]);
        let cert = |root: &SigningKey, device: [u8; 32], capabilities| {
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device,
                device_label: "node".into(),
                capabilities,
                issued_at_ns: 1_000,
                expires_at_ns: Some(10_000),
                signature: [0; 64],
            };
            crate::sign_device_cert(&mut cert, root).unwrap();
            cert
        };
        let own = device.verifying_key().to_bytes();
        let node = crate::cloud_enrollment::host_capabilities;
        let refused = |result: Result<(), PersistError>| result.unwrap_err().to_string();

        assert!(
            refused(install_device_cert(
                &paths,
                &cert(&root, [9; 32], node()),
                2_000
            ))
            .contains("another device key")
        );
        assert!(
            refused(install_device_cert(
                &paths,
                &cert(&root, own, node()),
                20_000
            ))
            .contains("did not verify")
        );
        let primary = DeviceCapabilities::primary();
        assert!(
            refused(install_device_cert(
                &paths,
                &cert(&root, own, primary),
                2_000
            ))
            .contains("does not admit")
        );
        assert!(!joined_device_is_ready(&paths, 2_000).unwrap());

        let installed = cert(&root, own, node());
        install_device_cert(&paths, &installed, 2_000).unwrap();
        assert!(joined_device_is_ready(&paths, 2_000).unwrap());
        assert_eq!(read_device_cert(&paths.device_cert).unwrap(), installed);
        assert_eq!(
            read_device_cert(&paths.device_cert_for_user(&installed.user_public_key)).unwrap(),
            installed
        );
        // A renewal from the same root replaces it; another root does not.
        install_device_cert(&paths, &cert(&root, own, node()), 3_000).unwrap();
        let stranger = SigningKey::from_bytes(&[4; 32]);
        assert!(
            refused(install_device_cert(
                &paths,
                &cert(&stranger, own, node()),
                2_000
            ))
            .contains("another identity")
        );
    }

    /// A device paired from a nearby device holds an ordinary member
    /// certificate: it does not read every workspace, and is still a ready
    /// joined device rather than one that needs a human root.
    #[test]
    fn a_nearby_paired_member_is_a_ready_joined_device() {
        let dir = identity_tempdir();
        let paths = IdentityPaths::under(dir.path());
        let (device, _) = load_or_create_device_key(&paths).unwrap();
        let root = SigningKey::from_bytes(&[5; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1_000,
            expires_at_ns: Some(10_000),
            signature: [0; 64],
        };
        crate::sign_device_cert(&mut cert, &root).unwrap();
        install_device_cert(&paths, &cert, 2_000).unwrap();

        assert!(joined_device_is_ready(&paths, 2_000).unwrap());
    }

    /// An enrollment whose device key the session cannot reach is reported,
    /// not mistaken for a machine without one.
    #[cfg(unix)]
    #[test]
    fn an_unreachable_device_key_is_an_error_not_a_fresh_machine() {
        use std::os::unix::fs::PermissionsExt as _;

        // SAFETY: geteuid takes no arguments and has no failure mode.
        if unsafe { libc::geteuid() } == 0 {
            // Root reaches the key whatever its directory's mode.
            return;
        }
        let dir = identity_tempdir();
        let paths = IdentityPaths::under(dir.path());
        let (device, _) = load_or_create_device_key(&paths).unwrap();
        let root = SigningKey::from_bytes(&[6; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1_000,
            expires_at_ns: Some(10_000),
            signature: [0; 64],
        };
        crate::sign_device_cert(&mut cert, &root).unwrap();
        install_device_cert(&paths, &cert, 2_000).unwrap();
        let key_dir = paths.device_privkey.parent().unwrap().to_path_buf();
        fs::set_permissions(&key_dir, fs::Permissions::from_mode(0o000)).unwrap();

        let result = joined_device_is_ready(&paths, 2_000);
        fs::set_permissions(&key_dir, fs::Permissions::from_mode(0o700)).unwrap();

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("is not accessible")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn secret_operations_make_the_process_non_dumpable() {
        protect_secret_process().expect("disable identity core dumps");
        // SAFETY: PR_GET_DUMPABLE consumes scalar arguments only.
        assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) }, 0);
    }

    /// Replacing a signing key keeps it private; a round trip enforces 0600; world-readable,
    /// symlinked or multiply-named key files are rejected; secret-key owner policies are distinct; a
    /// wrong-length key is rejected.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn signing_keys_are_private_single_named_files_of_the_right_length() {
        // Phase: replacing_a_signing_key_preserves_private_readability.
        {
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

        // Phase: signing_key_round_trip_enforces_0600.
        #[cfg(unix)]
        {
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

        // Phase: signing_key_rejects_world_readable.
        #[cfg(unix)]
        {
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

        // Phase: signing_key_never_follows_a_symlink.
        #[cfg(unix)]
        {
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

        // Phase: signing_key_rejects_an_inode_with_multiple_names.
        #[cfg(unix)]
        {
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

        // Phase: secret_key_owner_policies_are_distinct.
        #[cfg(unix)]
        {
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

        // Phase: wrong_length_key_rejected.
        {
            let tmp = identity_tempdir();
            let path = tmp.path().join("pubkey.bin");
            fs::write(&path, [0u8; 16]).unwrap();
            match read_public_key(&path).unwrap_err() {
                PersistError::WrongKeyLength(16) => {}
                other => panic!("expected WrongKeyLength(16), got {other:?}"),
            }
        }
    }

    /// A public Windows key is neither read nor silently replaced; an explicit public-key path
    /// outside the managed layout round-trips; authorization artifacts never follow symlinks, except
    /// under a distro home symlink where they are readable.
    #[test]
    fn public_keys_and_authorization_artifacts_are_read_only_from_trusted_paths() {
        // Phase: public_windows_key_is_neither_read_nor_silently_replaced.
        #[cfg(windows)]
        {
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

        // Phase: explicit_public_key_path_outside_managed_layout_round_trips.
        {
            let tmp = identity_tempdir();
            let path = tmp.path().join("pubkey.bin");
            let key = [7u8; 32];
            write_public_key(&path, &key).unwrap();
            let back = read_public_key(&path).unwrap();
            assert_eq!(key, back);
        }

        // Phase: authorization_artifacts_never_follow_symlinks.
        #[cfg(unix)]
        {
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

        // Phase: authorization_artifact_under_distro_home_symlink_is_readable.
        #[cfg(target_os = "linux")]
        {
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
    }

    /// `ensure_directories` creates the expected layout, certificates round-trip, and identity-specific
    /// cert paths do not collide.
    #[test]
    fn the_identity_layout_holds_per_identity_certificates() {
        // Phase: ensure_directories_creates_expected_layout.
        {
            let tmp = identity_tempdir();
            let paths = IdentityPaths::under(tmp.path());
            paths.ensure_directories().unwrap();
            assert!(tmp.path().join("user-identity").is_dir());
            assert!(tmp.path().join("device-private").is_dir());
            assert!(tmp.path().join("device").is_dir());
        }

        // Phase: cert_round_trip.
        {
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

        // Phase: identity_specific_cert_paths_do_not_collide.
        {
            let paths = IdentityPaths::under("/var/lib/tilo");
            let alice = paths.device_cert_for_user(&[1; 32]);
            let bob = paths.device_cert_for_user(&[2; 32]);
            assert_ne!(alice, bob);
            assert!(alice.starts_with("/var/lib/tilo/device/certs"));
        }
    }

    /// The device key permits the trust group and nobody else and rejects a multiply-named inode; a
    /// user-scoped secret still refuses group read; the device key is created once then reloaded,
    /// simultaneous creation persists one key, an unreadable key raises instead of rotating, and
    /// adopting a key seeds a new device but never replaces one.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn device_keys_are_group_private_created_once_and_never_rotated_silently() {
        // Phase: device_key_permits_the_trust_group_and_nobody_else.
        // The device key is deliberately group-readable — two components
        // running as two uids present the one device identity — but a user
        // outside that group must not be able to read it, and that is
        // enforced on every read rather than trusted to whoever set the
        // mode.
        #[cfg(unix)]
        {
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

        // Phase: device_key_rejects_an_inode_with_multiple_names.
        #[cfg(unix)]
        {
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

        // Phase: user_scoped_secret_still_refuses_group_read.
        // The relaxation is scoped to the device key. A user-scoped secret
        // still has to be owner-only — the group that reads the device key
        // has no business reading the user's own signing key.
        #[cfg(unix)]
        {
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

        // Phase: device_key_is_created_once_then_reloaded.
        {
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

        // Phase: simultaneous_device_key_creation_uses_one_persisted_key.
        {
            let tmp = identity_tempdir();
            let paths = IdentityPaths::under(tmp.path());
            let start = Arc::new(Barrier::new(3));

            let first_paths = paths.clone();
            let first_start = Arc::clone(&start);
            let first = std::thread::spawn(move || {
                first_start.wait();
                load_or_create_device_key(&first_paths)
                    .map(|(key, outcome)| (key.to_bytes(), outcome))
            });
            let second_paths = paths.clone();
            let second_start = Arc::clone(&start);
            let second = std::thread::spawn(move || {
                second_start.wait();
                load_or_create_device_key(&second_paths)
                    .map(|(key, outcome)| (key.to_bytes(), outcome))
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

        // Phase: unreadable_device_key_raises_instead_of_rotating.
        // The defining property: an unreadable key file must raise, not
        // be replaced. A device that silently re-keys drops every grant
        // written against its old identity and reports nothing.
        {
            let tmp = identity_tempdir();
            let paths = IdentityPaths::under(tmp.path());
            paths.ensure_directories().unwrap();
            fs::write(&paths.device_privkey, b"not a key").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&paths.device_privkey, fs::Permissions::from_mode(0o600))
                    .unwrap();
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

        // Phase: adopt_device_key_seeds_a_new_device_but_never_replaces_one.
        {
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
    }

    #[cfg(unix)]
    #[test]
    fn public_certificates_preserve_the_device_directory_trust_group() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(dir.path());
        paths.ensure_directories().unwrap();
        let device_dir = paths.device_cert.parent().unwrap();
        fs::set_permissions(device_dir, fs::Permissions::from_mode(0o2770)).unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [3; 32],
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        crate::sign_device_cert(&mut cert, &root).unwrap();
        let cert_path = paths.device_cert_for_user(&cert.user_public_key);
        write_device_cert(&cert_path, &cert).unwrap();
        let metadata = fs::metadata(cert_path.parent().unwrap()).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o2770);
        assert_eq!(metadata.gid(), fs::metadata(device_dir).unwrap().gid());
        assert_eq!(read_device_cert(&cert_path).unwrap(), cert);
        assert!(!paths.device_privkey.exists());
    }

    #[cfg(unix)]
    #[test]
    fn public_identity_files_are_readable_whatever_the_writers_umask() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: test-local umask, restored before the assertions.
        let previous = unsafe { libc::umask(0o077) };
        let pubkey = dir.path().join("user-identity/pubkey.bin");
        fs::create_dir_all(pubkey.parent().unwrap()).unwrap();
        write_public_key(&pubkey, &[7; 32]).unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [3; 32],
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        crate::sign_device_cert(&mut cert, &root).unwrap();
        let cert_path = dir.path().join("device/certs/one.bin");
        write_device_cert(&cert_path, &cert).unwrap();
        unsafe { libc::umask(previous) };
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&pubkey), 0o644);
        assert_eq!(mode(&cert_path), 0o644);
        assert_eq!(mode(cert_path.parent().unwrap()), 0o755);
    }
}
