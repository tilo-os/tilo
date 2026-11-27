//! On-disk persistence for user + device identity material.
//!
//! Tilo lays out identity files under `/var/lib/tilo/`:
//!
//! ```text
//! /var/lib/tilo/
//! ├── user-identity/
//! │   └── pubkey.bin      # 32-byte Ed25519 public key (present on every device)
//! ├── device-private/
//! │   └── keypair.bin     # 32-byte Ed25519 secret seed (per-device, always present)
//! └── device/
//!     └── certs/          # one certificate per person this device is certified by
//! ```
//!
//! All files are raw byte blobs; the on-disk format stays simple so it
//! can be inspected with `hexdump` and does not depend
//! on `serde_json` at boot.
//!
//! Secret files must not be reachable by anyone else, and the mode is checked
//! at read time — a permissive secret file returns
//! [`PersistError::InsecurePermissions`] rather than being read.
//! The person's root is never here: it lives in their identity store.
//! `keypair.bin` is the device key, owner-only ([`DEVICE_KEY_MODE`]) in a
//! directory only the daemon's account may enter: one process writes as the
//! device, its daemon, which takes the key with [`take_device_key`], and a
//! second one that takes it is refused ([`PersistError::KeyInUse`]). Every
//! other program of the computer asks the daemon to sign with it.

use getrandom::{SysRng, rand_core::UnwrapErr};
#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::fs;
#[cfg(not(windows))]
use std::io::Write;
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd as _;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::SigningKey;
use thiserror::Error;
use tilo_files::native::migration::Format;
use zeroize::Zeroizing;

use tilo_user_identity::CanonicalError;
use tilo_user_identity::cert::{
    DeviceCert, DeviceCertError, verify_device_cert, verify_device_cert_at,
};

mod one_writer;

pub use one_writer::{DeviceKeyLock, device_public_key, take_device_key, taken_device_key};

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

/// Apply canonical process protection before held-file private ownership reads.
///
/// # Errors
/// Returns the native failure when dump/ptrace protection cannot be established.
pub fn protect_private_ownership() -> io::Result<()> {
    protect_secret_process()
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
/// Wraps the paths so callers can inject a test root
/// (`tempdir`) without having to hand-plumb every path through.
#[derive(Clone, Debug)]
pub struct IdentityPaths {
    /// `<root>/user-identity/pubkey.bin`
    pub user_pubkey: std::path::PathBuf,
    /// `<root>/device-private/keypair.bin`
    pub device_privkey: std::path::PathBuf,
    /// `<root>/device`: the certificate of each person this device is certified by lives under
    /// its `certs/` directory, one per person.
    pub device_dir: std::path::PathBuf,
    /// Where the device key itself is kept.
    pub device_key_ownership: DeviceKeyOwnership,
}

/// Where a layout keeps this device's private key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceKeyOwnership {
    /// A file at [`IdentityPaths::device_privkey`]: the Tilo OS layout, where
    /// the device's daemon keeps the key owner-only and every other program
    /// asks it to sign.
    #[default]
    File,
    /// The platform's secret store, under this device's own app namespace: the
    /// login Keychain on macOS, and on Android the app's store, whose key the
    /// Android Keystore wraps. Only the user's own runtime reads the key there.
    #[cfg(any(
        target_os = "macos",
        target_os = "android",
        all(test, target_os = "linux")
    ))]
    PlatformStore,
}

/// Where a machine keeps its own identity tree.
pub const SYSTEM_IDENTITY_ROOT: &str = "/var/lib/tilo";

/// Where this device's key and certificates live, for every program of this account: the session
/// that proves for the device, its setup, `tilo`, and the shell runtime that proves to it. They
/// must all read the same key, or a proof made with one proves nothing to a checker holding
/// another.
///
/// `TILO_IDENTITY_ROOT` when it names one; else the machine's tree, [`SYSTEM_IDENTITY_ROOT`], when
/// it holds an identity (`device/`); else this account's own data directory
/// (`$XDG_DATA_HOME/tilo`), as on a computer running Tilo beside another operating system.
#[must_use]
pub fn collaboration_identity_root() -> PathBuf {
    if let Some(root) = std::env::var_os("TILO_IDENTITY_ROOT").filter(|value| !value.is_empty()) {
        return PathBuf::from(root);
    }
    held_root(
        Path::new(SYSTEM_IDENTITY_ROOT),
        tilo_paths::data_dir().as_deref(),
    )
}

/// [`collaboration_identity_root`] without the environment: `system` when it holds an identity,
/// else the account's own `data` directory, else `system` (where an identity will be made).
#[must_use]
pub fn held_root(system: &Path, data: Option<&Path>) -> PathBuf {
    if system.join("device").is_dir() {
        return system.to_path_buf();
    }
    data.filter(|dir| dir.is_absolute())
        .map_or_else(|| system.to_path_buf(), |dir| dir.join("tilo"))
}

impl IdentityPaths {
    /// Standard layout under `/var/lib/tilo`.
    pub fn default_system() -> Self {
        Self::under(SYSTEM_IDENTITY_ROOT)
    }

    /// Custom root — the boot path uses `/var/lib/tilo`, tests use
    /// a `tempdir`.
    pub fn under(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            user_pubkey: root.join("user-identity/pubkey.bin"),
            device_privkey: root.join("device-private/keypair.bin"),
            device_dir: root.join("device"),
            device_key_ownership: DeviceKeyOwnership::File,
        }
    }

    /// This layout with its device key kept in `ownership`.
    #[must_use]
    pub fn with_device_key_ownership(mut self, ownership: DeviceKeyOwnership) -> Self {
        self.device_key_ownership = ownership;
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
            tilo_files::native::private::directory(parent)?;
            #[cfg(not(windows))]
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self
            .device_privkey
            .parent()
            .filter(|_| self.device_key_ownership == DeviceKeyOwnership::File)
        {
            #[cfg(windows)]
            tilo_files::native::private::directory(parent)?;
            #[cfg(not(windows))]
            fs::create_dir_all(parent)?;
        }
        fs::create_dir_all(self.certs_dir())?;
        Ok(())
    }

    /// `<root>/device/certs`: one certificate per person this device is certified by.
    #[must_use]
    pub fn certs_dir(&self) -> std::path::PathBuf {
        self.device_dir.join("certs")
    }

    /// Certificate issued to this device by `user_public_key`.
    ///
    /// A device has one keypair but may act for different local identities in
    /// different workspaces. Keying certificates by their issuing user root
    /// preserves that one-device invariant without crossing identities.
    #[must_use]
    pub fn device_cert_for_user(&self, user_public_key: &[u8; 32]) -> std::path::PathBuf {
        self.certs_dir()
            .join(format!("{}.bin", hex::encode(user_public_key)))
    }

    /// The certificate this device holds for the person it belongs to, the owner named in
    /// `user-identity/pubkey.bin`: `<root>/device/certs/<owner>.bin`. `None` when no person is
    /// claimed here or no certificate of theirs is filed.
    ///
    /// A program that needs one certificate for the machine, not one identity's, reads this.
    ///
    /// # Errors
    ///
    /// Returns an error when the owner's key or certificate is present but cannot be read.
    pub fn owner_cert(&self) -> Result<Option<std::path::PathBuf>, PersistError> {
        let Some((_, path)) = self.owner_and_cert()? else {
            return Ok(None);
        };
        Ok(path.is_file().then_some(path))
    }

    /// The person this device belongs to, named in `user-identity/pubkey.bin`, and where their
    /// certificate is filed ([`Self::owner_cert`]); `None` when no person is claimed here, and an
    /// error when the owner's key is present but cannot be read.
    pub fn owner_and_cert(&self) -> Result<Option<([u8; 32], std::path::PathBuf)>, PersistError> {
        if !self.user_pubkey.is_file() {
            return Ok(None);
        }
        let owner = read_public_key(&self.user_pubkey)?;
        let path = self.device_cert_for_user(&owner);
        Ok(Some((owner, path)))
    }

    /// Where a build before one certificate per person kept the owner's certificate,
    /// `<root>/device/cert.bin`. Nothing reads it: [`settle_device_certificate`] moves it to the
    /// owner's place and removes it.
    #[must_use]
    pub fn system_device_cert(&self) -> std::path::PathBuf {
        self.device_dir.join("cert.bin")
    }
}

/// Read the certificate this device holds for the person it belongs to
/// ([`IdentityPaths::owner_cert`]).
///
/// # Errors
///
/// Returns an error when the owner's key or certificate is present but cannot be read.
pub fn read_owner_cert(paths: &IdentityPaths) -> Result<Option<DeviceCert>, PersistError> {
    Ok(read_owner_cert_and_person(paths)?.map(|(_, cert)| cert))
}

/// Read the person this device belongs to and the certificate it holds for them
/// ([`IdentityPaths::owner_cert`]); an error when the owner's key or certificate is present but
/// cannot be read.
pub fn read_owner_cert_and_person(
    paths: &IdentityPaths,
) -> Result<Option<([u8; 32], DeviceCert)>, PersistError> {
    let Some((owner, path)) = paths.owner_and_cert()? else {
        return Ok(None);
    };
    match read_device_cert(&path) {
        Ok(cert) => Ok(Some((owner, cert))),
        Err(PersistError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Settle this device's certificates on the one layout it reads: a certificate the device's owner
/// holds at the pre-per-person path [`IdentityPaths::system_device_cert`] is moved to
/// `certs/<owner>.bin`, unless the owner's place already holds one, and the previous file is
/// removed. Idempotent: a machine already on the one layout is left as it is.
///
/// Answers whether it moved or removed anything. Runs where a machine's identity is prepared at
/// boot, so a device imaged before this layout keeps serving its owner without a second copy.
///
/// # Errors
///
/// Returns an error when the owner's key cannot be read, or the owner's certificate cannot be
/// moved or removed.
pub fn settle_device_certificate(paths: &IdentityPaths) -> Result<bool, PersistError> {
    let system_cert = paths.system_device_cert();
    match fs::symlink_metadata(&system_cert) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    let Some((_, filed)) = paths.owner_and_cert()? else {
        return Ok(false);
    };
    if filed.exists() {
        fs::remove_file(&system_cert)?;
    } else {
        create_public_dir(&paths.certs_dir())?;
        fs::rename(&system_cert, &filed)?;
    }
    Ok(true)
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
    #[error("secret file {path} has permissions {mode:o}; a private key must be owner-only (0600)")]
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
    /// Another process writes as this device: it took the device's key
    /// ([`take_device_key`]), and one device signs from one process.
    #[error(
        "this device's key is already in use by {}: one process writes as the device",
        one_writer::in_use_by(*pid, program.as_deref())
    )]
    KeyInUse {
        /// The process that took the key, when the system says.
        pid: Option<u32>,
        /// The program it runs, when the system says.
        program: Option<String>,
    },
    /// This process signs as the device without having taken its key: only the device's daemon
    /// does ([`take_device_key`]); another program asks it.
    #[error(
        "the device service isn't running in this process: only the device's daemon signs as the device"
    )]
    NotTheDaemon,
}

impl PersistError {
    /// The I/O failure this refusal is, when it is one: what a caller inspects to tell a certificate
    /// refused only for the clock apart from a broken one
    /// ([`CertificateClockError::in_io`](tilo_user_identity::CertificateClockError::in_io)).
    #[must_use]
    pub fn io(&self) -> Option<&io::Error> {
        match self {
            Self::Io(error) => Some(error),
            Self::InsecurePermissions { .. }
            | Self::InsecureLinkCount { .. }
            | Self::InsecureOwner { .. }
            | Self::WrongKeyLength(_)
            | Self::Canonical(_)
            | Self::KeyInUse { .. }
            | Self::NotTheDaemon => None,
        }
    }

    /// The refusal for a device that holds no certificate of its person:
    /// [`io::ErrorKind::NotFound`], in the words a person reads.
    #[must_use]
    pub fn no_owner_cert() -> Self {
        Self::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "this device holds no certificate of its person",
        ))
    }
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
pub(crate) fn read_regular_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, PersistError> {
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
pub fn write_public_key(path: &Path, pubkey: &[u8; 32]) -> io::Result<()> {
    path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    write_public_file(path, pubkey)
}

/// Most revocations one device's list holds.
pub const MAX_REVOCATIONS: usize = 4096;
/// Largest revocation list file this reads.
pub const MAX_REVOCATIONS_BYTES: u64 = 4 * 1024 * 1024;

/// Failures reading or extending the signed revocations beside the person's
/// public key.
#[derive(Debug, Error)]
pub enum RevocationsError {
    /// The list could not be read or replaced.
    #[error("revocation list {0} could not be read or replaced: {1}")]
    Io(String, #[source] io::Error),
    /// The file is not the vector of signed entries this writes.
    #[error("revocation list {0} could not be decoded: {1}")]
    Decode(String, #[source] postcard::Error),
    /// An entry was not one the person's root signed.
    #[error("revocation list {0} holds an entry that does not verify: {1}")]
    Entry(String, #[source] tilo_user_identity::RevocationError),
    /// The list holds its bound already.
    #[error("revocation list {0} is full")]
    Full(String),
}

/// Where this device keeps the revocations it verified: beside the person's
/// public key, where the filesystem daemon, the server and every other reader
/// of the identity root find them.
#[must_use]
pub fn revocations_path(paths: &IdentityPaths) -> std::path::PathBuf {
    paths
        .user_pubkey
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("revocations.bin")
}

/// The signed revocations beside the person's public key, verified. A list
/// that is not there is empty; one that does not decode or hold a root-signed
/// entry is an error rather than an empty list, so corrupt trust state never
/// reads as "nothing is revoked".
///
/// # Errors
///
/// As [`RevocationsError`]: the file cannot be read, is not a list of at most
/// [`MAX_REVOCATIONS`] entries, or an entry's signature does not verify.
pub fn read_revocations(
    paths: &IdentityPaths,
) -> Result<tilo_user_identity::RevocationList, RevocationsError> {
    let path = revocations_path(paths);
    let name = path.display().to_string();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(tilo_user_identity::RevocationList::new());
        }
        Err(error) => return Err(RevocationsError::Io(name, error)),
    };
    if bytes.len() as u64 > MAX_REVOCATIONS_BYTES {
        return Err(RevocationsError::Io(
            name,
            io::Error::new(io::ErrorKind::InvalidData, "the list is too large"),
        ));
    }
    let entries: Vec<tilo_user_identity::RevocationEntry> = postcard::from_bytes(&bytes)
        .map_err(|error| RevocationsError::Decode(name.clone(), error))?;
    let mut list = tilo_user_identity::RevocationList::new();
    for entry in entries {
        list.insert(entry)
            .map_err(|error| RevocationsError::Entry(name.clone(), error))?;
    }
    Ok(list)
}

/// Add `entry` to the revocations beside the person's public key: verified,
/// kept once, ordered by root and device, and replacing the file atomically.
///
/// # Errors
///
/// As [`read_revocations`], or [`RevocationsError::Full`] when the list holds
/// [`MAX_REVOCATIONS`] already.
pub fn append_revocation(
    paths: &IdentityPaths,
    entry: &tilo_user_identity::RevocationEntry,
) -> Result<(), RevocationsError> {
    let path = revocations_path(paths);
    let name = path.display().to_string();
    let mut list = read_revocations(paths)?;
    if list.len() >= MAX_REVOCATIONS {
        return Err(RevocationsError::Full(name));
    }
    list.insert(entry.clone())
        .map_err(|error| RevocationsError::Entry(name.clone(), error))?;
    let mut entries: Vec<tilo_user_identity::RevocationEntry> = list.iter().cloned().collect();
    entries.sort_by_key(|entry| (entry.user_public_key, entry.revoked_device_public_key));
    let bytes = postcard::to_allocvec(&entries)
        .map_err(|error| RevocationsError::Decode(name.clone(), error))?;
    tilo_files::native::atomic::write_with_mode(&path, &bytes, 0o600)
        .map_err(|error| RevocationsError::Io(name, error))
}

/// The directories public identity files live in get this mode whatever the
/// writer's umask, keeping a setgid bit inherited from their parent. The
/// machine's group-writable device tree must also allow later login users
/// in that same trust group to publish their own root-signed certificates.
#[cfg(unix)]
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
    let file = tilo_files::native::private::read(path)?;
    #[cfg(not(windows))]
    let file = open_regular_without_symlinks(path).map_err(|error| {
        PersistError::Io(io::Error::new(
            error.kind(),
            format!("cannot open secret key {}: {error}", path.display()),
        ))
    })?;
    signing_key_from_open_file(&file, path, forbidden_mode, owner_policy)
}

/// Read a device key from an already admitted Unix inode using the same key
/// policy as [`read_device_key`]. The caller retains namespace provenance;
/// this function never opens a path or generates a replacement key.
///
/// # Errors
/// Refuses nonregular, insecure, foreign-owned, multiply-linked or wrong-size keys.
#[cfg(unix)]
pub fn read_device_key_from_file(file: &fs::File) -> Result<SigningKey, PersistError> {
    protect_secret_process()?;
    signing_key_from_open_file(
        file,
        Path::new("<held device key>"),
        0o077,
        SecretOwner::RootOrReader,
    )
}

fn signing_key_from_open_file(
    file: &fs::File,
    path: &Path,
    forbidden_mode: u32,
    owner_policy: SecretOwner,
) -> Result<SigningKey, PersistError> {
    admit_secret_file(file, path, forbidden_mode, owner_policy)?;

    // Read one byte beyond the only valid size. A held reader must not depend
    // on a descriptor's shared mutable cursor, including aliases of this fd.
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::fs::FileExt as _;
        let mut bytes = Zeroizing::new(vec![0u8; 33]);
        let mut read = 0;
        while read < bytes.len() {
            let offset = u64::try_from(read)
                .map_err(|_| io::Error::other("key read offset exceeds its bound"))?;
            let remaining = bytes
                .get_mut(read..)
                .ok_or_else(|| io::Error::other("key reader exceeded its buffer"))?;
            let found = file.read_at(remaining, offset)?;
            if found == 0 {
                break;
            }
            read = read
                .checked_add(found)
                .ok_or_else(|| io::Error::other("key read length exceeds its bound"))?;
        }
        bytes.truncate(read);
        bytes
    };
    #[cfg(not(unix))]
    let bytes = {
        let mut bytes = Zeroizing::new(Vec::with_capacity(33));
        file.take(33).read_to_end(&mut bytes)?;
        bytes
    };
    if bytes.len() != 32 {
        return Err(PersistError::WrongKeyLength(bytes.len()));
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    seed.copy_from_slice(&bytes);
    Ok(SigningKey::from_bytes(&seed))
}

/// Read a private record of at most `limit` bytes under the device key's
/// policy: a regular, singly linked file owned by root or the reader that
/// nobody outside its owner can read or write.
///
/// # Errors
///
/// As [`read_device_key`] for the file's admission, or
/// [`PersistError::Io`] with `InvalidData` when it holds more than `limit`.
pub fn read_private_record(path: &Path, limit: usize) -> Result<Zeroizing<Vec<u8>>, PersistError> {
    protect_secret_process()?;
    let file = open_regular_without_symlinks(path).map_err(|error| {
        PersistError::Io(io::Error::new(
            error.kind(),
            format!("cannot open private record {}: {error}", path.display()),
        ))
    })?;
    admit_secret_file(&file, path, 0o077, SecretOwner::RootOrReader)?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(limit.saturating_add(1)));
    (&file)
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("private record {} exceeds {limit} bytes", path.display()),
        )));
    }
    Ok(bytes)
}

/// The checks every secret file passes before a byte of it is read: a
/// regular file whose mode has none of `forbidden_mode`, owned as
/// `owner_policy` accepts, with a single name.
fn admit_secret_file(
    file: &fs::File,
    path: &Path,
    forbidden_mode: u32,
    owner_policy: SecretOwner,
) -> Result<(), PersistError> {
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
            // The device key is the daemon's own file, and a store a test injects
            // may be the caller's. A third account may never substitute the
            // signing identity merely by matching its mode.
            let reader = rustix::process::geteuid().as_raw();
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
    Ok(())
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
        // Root reads a key of any owner: the key is the daemon account's, and
        // the root units that must read it (the re-key at boot) are not the
        // substitution this refuses, which is another account's.
        SecretOwner::RootOrReader => owner == 0 || reader == 0 || owner == reader,
    }
}

/// Write a 32-byte Ed25519 signing key to disk with mode 0600.
pub fn write_signing_key(path: &Path, sk: &SigningKey) -> io::Result<()> {
    protect_secret_process()?;
    // A partial write must leave the previous secret intact, and the mode is
    // set on the temp file so the key is never briefly world-readable at its
    // final path.
    path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let seed = Zeroizing::new(sk.to_bytes());
    #[cfg(windows)]
    return tilo_files::native::private::write_atomic(path, seed.as_ref());
    #[cfg(not(windows))]
    tilo_files::native::atomic::write_with_mode(path, seed.as_ref(), 0o600)
}

/// Mode the device key is stored with: owner read/write, nothing for anyone
/// else.
///
/// Only the device's daemon reads the key. It runs as [`DEVICE_DAEMON_ACCOUNT`]
/// on every computer, never as root, so the key is that account's and nobody
/// else's; the directory is that account's alone, so a program of any other
/// account cannot even reach the file. Every other program signs through the daemon
/// (`devices#sign`) and never opens the key.
pub const DEVICE_KEY_MODE: u32 = 0o600;

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

/// Read the device key, requiring that nobody but its owner can access it.
///
/// # Errors
///
/// Returns [`PersistError::InsecurePermissions`] if the file is readable or
/// writable by anyone but its owner (a group-readable key is refused),
/// [`PersistError::InsecureOwner`] if it is owned by neither root nor the
/// reader (root, which may always read it, is never refused),
/// [`PersistError::Io`] if it cannot be read, or
/// [`PersistError::WrongKeyLength`] if it is not exactly 32 bytes.
pub fn read_device_key(path: &Path) -> Result<SigningKey, PersistError> {
    // The process that writes as the device answers the key it took, and opens the file once.
    if let Some(key) = one_writer::taken_here(path) {
        return Ok(key);
    }
    let key = signing_key_from_file(path, 0o077, SecretOwner::RootOrReader)?;
    // Reading the key made this process non-dumpable, so the person's Secret
    // Service can no longer read its executable to know who is calling. The
    // process proves it holds the device key instead.
    #[cfg(target_os = "linux")]
    tilo_secrets::arm_device_proof(Zeroizing::new(key.to_bytes()).as_ref());
    Ok(key)
}

/// Read this device's key from wherever `paths` keeps it.
///
/// # Errors
///
/// As [`read_device_key`] for a key file. A platform store that holds no key,
/// or refuses or fails to answer, is an error: a missing device key is never
/// replaced here.
pub fn load_device_key(paths: &IdentityPaths) -> Result<SigningKey, PersistError> {
    match paths.device_key_ownership {
        DeviceKeyOwnership::File => read_device_key(&paths.device_privkey),
        #[cfg(any(
            target_os = "macos",
            target_os = "android",
            all(test, target_os = "linux")
        ))]
        DeviceKeyOwnership::PlatformStore => crate::platform_store::load()?.ok_or_else(|| {
            PersistError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "the {} holds no Tilo device key",
                    crate::platform_store::NAME
                ),
            ))
        }),
    }
}

/// Whether `paths` keeps a device key at all, without reading it.
///
/// # Errors
///
/// When the key file's directory, or the platform store, cannot answer.
pub fn holds_device_key(paths: &IdentityPaths) -> Result<bool, PersistError> {
    match paths.device_key_ownership {
        DeviceKeyOwnership::File => match fs::symlink_metadata(&paths.device_privkey) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        },
        #[cfg(any(
            target_os = "macos",
            target_os = "android",
            all(test, target_os = "linux")
        ))]
        DeviceKeyOwnership::PlatformStore => Ok(crate::platform_store::load()?.is_some()),
    }
}

/// Make `sk` this device's key wherever `paths` keeps it, replacing the one
/// there: only a re-key ([`crate::branch`]) does this.
///
/// # Errors
///
/// When the key cannot be written, or the platform store does not keep it.
pub fn replace_device_key(paths: &IdentityPaths, sk: &SigningKey) -> Result<(), PersistError> {
    match paths.device_key_ownership {
        DeviceKeyOwnership::File => Ok(write_device_key(&paths.device_privkey, sk)?),
        #[cfg(any(
            target_os = "macos",
            target_os = "android",
            all(test, target_os = "linux")
        ))]
        DeviceKeyOwnership::PlatformStore => crate::platform_store::replace(sk),
    }
}

/// Write the device key with [`DEVICE_KEY_MODE`], atomically.
pub fn write_device_key(path: &Path, sk: &SigningKey) -> io::Result<()> {
    protect_secret_process()?;
    one_writer::forget(path);
    path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let seed = Zeroizing::new(sk.to_bytes());
    #[cfg(windows)]
    return tilo_files::native::private::write_atomic(path, seed.as_ref());
    #[cfg(not(windows))]
    tilo_files::native::atomic::write_with_mode(path, seed.as_ref(), DEVICE_KEY_MODE)
}

/// The account the device's daemon runs as on every computer, the one process that reads the
/// machine's key.
pub const DEVICE_DAEMON_ACCOUNT: &str = "tilo";

/// The uid and primary gid of [`DEVICE_DAEMON_ACCOUNT`].
///
/// # Errors
///
/// This computer has no such account.
#[cfg(unix)]
pub fn device_daemon_owner() -> io::Result<(u32, u32)> {
    let name = std::ffi::CString::new(DEVICE_DAEMON_ACCOUNT).map_err(io::Error::other)?;
    // SAFETY: an all-zero passwd is a valid out-buffer for getpwnam_r.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0_u8; 16 * 1024];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: a NUL-terminated name, and out-pointers to the entry, its string buffer of the
    // length given, and the result, all live for the call.
    let rc = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            &raw mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut found,
        )
    };
    if rc != 0 || found.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("this computer has no `{DEVICE_DAEMON_ACCOUNT}` account for its device daemon"),
        ));
    }
    Ok((entry.pw_uid, entry.pw_gid))
}

/// No computer without Unix accounts keeps its device key in a file a daemon account owns.
///
/// # Errors
///
/// Always.
#[cfg(not(unix))]
pub fn device_daemon_owner() -> io::Result<(u32, u32)> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Give the key at `path` and its directory to `owner` (uid, gid): the directory `0700`, the key
/// [`DEVICE_KEY_MODE`].
///
/// `owner` is named, never read off the directory: whoever made or copied the directory last
/// (an installer, a re-key, another service of an earlier image) can have left an owner the
/// daemon isn't, and settling on it would hand the key to that account. Run every boot, so an
/// install from any earlier state settles on the one state the daemon reads. As root this needs
/// `CAP_CHOWN` and `CAP_FOWNER`, and `CAP_DAC_OVERRIDE` to reach the key inside the directory.
///
/// # Errors
///
/// The key or its directory cannot be read, the key is not a regular file, or either cannot be
/// given its owner and mode.
pub fn enforce_device_key_owner_and_mode(path: &Path, owner: (u32, u32)) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let directory = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the device key {} has no directory", path.display()),
            )
        })?;
        let key = fs::symlink_metadata(path)?;
        if !key.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the device key {} is not a regular file", path.display()),
            ));
        }
        for (path, metadata, mode) in [
            (directory, fs::metadata(directory)?, 0o700),
            (path, key, DEVICE_KEY_MODE),
        ] {
            if (metadata.uid(), metadata.gid()) != owner {
                std::os::unix::fs::chown(path, Some(owner.0), Some(owner.1))?;
            }
            if metadata.permissions().mode() & 0o777 != mode {
                fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (path, owner);
    Ok(())
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
    #[cfg(any(
        target_os = "macos",
        target_os = "android",
        all(test, target_os = "linux")
    ))]
    if paths.device_key_ownership == DeviceKeyOwnership::PlatformStore {
        return crate::platform_store::load_or_create();
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

// Keep platform refusal in the claim operation so every outcome reaches the
// caller's temporary-file cleanup without relying on a diverging result value.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn claim_identity_name(source: &Path, destination: &Path) -> io::Result<()> {
    match tilo_files::native::rename_file_noreplace(source, destination) {
        // Once the fallback hard link succeeds, the destination holds the key
        // even if unlinking the temporary name then fails.
        Ok(
            tilo_files::native::FileRename::Moved
            | tilo_files::native::FileRename::SourceUnlinkFailed(_),
        ) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn claim_identity_name(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace identity publication is unsupported on this platform",
    ))
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
    if path.as_os_str().as_bytes().contains(&0) {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "device-key path contains a NUL byte",
        )));
    }
    // Validate the caller-controlled final path before persisting a temporary
    // secret. Otherwise an invalid NUL-containing basename would strand the
    // complete candidate in the shared identity directory.
    // Reject intermediate links before placing any secret in the directory.
    let directory = open_regular_without_symlinks(parent)?;
    if !directory.metadata()?.is_dir() {
        return Err(io::Error::other("identity parent is not a directory").into());
    }
    let temp = create_device_key_claim_temp(parent, bytes, mode)?;
    let claim = claim_identity_name(&temp, path);
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
    Ok(tilo_files::native::private::create_atomic(
        path,
        seed.as_ref(),
    )?)
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
    if paths.device_key_ownership != DeviceKeyOwnership::File {
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
    let stored = tilo_user_identity::cert::binary_layout_of(&bytes)?;
    let bytes = DEVICE_CERT
        .bring_up(path, bytes, stored)
        .map_err(|error| PersistError::Io(io::Error::other(error.to_string())))?;
    let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&bytes)?;
    Ok(cert)
}

/// A device certificate file (`device/cert.bin`, `device/certs/*`): the
/// certificate's canonical binary encoding, whose layout names its version
/// ([`tilo_user_identity::cert::binary_layout_of`]). A later layout brings the files of
/// this one up through a step here, and the old file stays beside it. The
/// bytes each version wrote are kept in `golden/device-certificate/`.
pub const DEVICE_CERT: Format = Format {
    name: "device certificate",
    // The identity crate owns the layout number ([`tilo_user_identity::cert::BINARY_LAYOUT`]);
    // a test beside the golden bytes keeps this in step with it.
    version: 4,
    oldest: 1,
    steps: &[
        layout_one_is_layout_two,
        layout_two_is_layout_three,
        layout_three_is_layout_four,
    ],
};

/// Layout 2 only added a certificate that reaches one workspace alone: what layout 1 wrote is
/// the same certificate in layout 2, byte for byte, and its signature covers those bytes. So
/// [`tilo_user_identity::cert::binary_layout_of`] names such a file layout 2 already, and this
/// step leaves the bytes as they are.
#[allow(
    clippy::unnecessary_wraps,
    reason = "a migration step's signature, which every step has"
)]
fn layout_one_is_layout_two(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    Ok(bytes)
}

/// Layout 3 only added a placed device's certificate, confined to the records the host that
/// placed it named, which no device's identity files keep: what layout 2 wrote is the same
/// certificate in layout 3, byte for byte, so this step too leaves the bytes as they are.
#[allow(
    clippy::unnecessary_wraps,
    reason = "a migration step's signature, which every step has"
)]
fn layout_two_is_layout_three(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    Ok(bytes)
}

/// Layout 4 only added an agent's grant that names its agent by its id: what layout 3 wrote is
/// the same certificate in layout 4, byte for byte (an agent's key it named by the hash of its id
/// included), and its signature covers those bytes. So this step too leaves them as they are.
#[allow(
    clippy::unnecessary_wraps,
    reason = "a migration step's signature, which every step has"
)]
fn layout_three_is_layout_four(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    Ok(bytes)
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
    let bytes = tilo_user_identity::canonical::canonical_bytes(cert)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    create_public_dir(parent)?;
    write_public_file(path, &bytes)?;
    Ok(())
}

/// A certificate that did not verify, as the refusal it is. One refused only because the wall clock
/// reads outside its validity interval keeps that reading
/// ([`CertificateClockError`](tilo_user_identity::CertificateClockError)), so a caller can wait for the clock to
/// reach it instead of ending without an identity — a session in a resumed VM, whose clock starts
/// at the instant its template was snapshotted.
fn refused_verification(cert: &DeviceCert, now_ns: u64, error: &DeviceCertError) -> PersistError {
    PersistError::Io(
        tilo_user_identity::CertificateClockError::of(cert, now_ns, error).map_or_else(
            || {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the certificate did not verify: {error}"),
                )
            },
            tilo_user_identity::CertificateClockError::into_io,
        ),
    )
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
    let device = device_public_key(paths)?;
    if cert.device_public_key != device {
        return Err(refused(
            io::ErrorKind::InvalidData,
            "the certificate is for another device key".into(),
        ));
    }
    verify_device_cert_at(cert, now_ns)
        .map_err(|error| refused_verification(cert, now_ns, &error))?;
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
    Ok(())
}

/// Whether `paths` holds a device enrollment, whether or
/// not its certificate is inside its validity right now: a certificate
/// that ran out is still this device's, and is the one a renewal replaces.
///
/// This is [`joined_device_is_ready`] without the clock's verdict, for a
/// caller that must renew a lapsed certificate before asking whether the
/// device is usable now. Every other check — the keys, the certificate's
/// signature and that it names this device's key — is the same.
///
/// # Errors
///
/// Returns [`PersistError`] when a present identity file cannot be read, the
/// device key is not accessible, or the certificate does not match its keys.
pub fn joined_device_is_enrolled(paths: &IdentityPaths) -> Result<bool, PersistError> {
    let Some((pubkey, cert)) = read_owner_cert_and_person(paths)? else {
        return Ok(false);
    };
    // The key is where `paths` keeps it: a phone's is in the platform store, never in the file.
    let held = match paths.device_key_ownership {
        DeviceKeyOwnership::File => match fs::metadata(&paths.device_privkey) {
            Ok(metadata) => metadata.is_file(),
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                return Err(PersistError::Io(io::Error::new(
                    error.kind(),
                    format!(
                        "enrolled device key at {} is not accessible: {error}",
                        paths.device_privkey.display()
                    ),
                )));
            }
            Err(_) => false,
        },
        #[cfg(any(
            target_os = "macos",
            target_os = "android",
            all(test, target_os = "linux")
        ))]
        DeviceKeyOwnership::PlatformStore => holds_device_key(paths)?,
    };
    if !held {
        return Ok(false);
    }
    let device = device_public_key(paths)?;
    // A primary device can admit further devices. That is an ordinary
    // machine, not one the imager already enrolled, so first-boot
    // provisioning must still run
    // for each OS user: it verifies the certificate, and renews one that
    // is due before it is ever refused for having expired.
    if cert.capabilities.can_admit_devices {
        return Ok(false);
    }
    verify_device_cert(&cert).map_err(|error| {
        PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("enrolled device certificate did not verify: {error}"),
        ))
    })?;
    if cert.user_public_key != pubkey || cert.device_public_key != device {
        return Err(PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "enrolled device certificate does not match its keys",
        )));
    }
    Ok(true)
}

/// Whether `paths` already holds a device enrollment: one
/// made by a setup machine, or a nearby pairing approved from another device.
///
/// Missing files are `Ok(false)`, so ordinary first boot still creates an
/// identity. Present but inconsistent or unreadable files are an error: first
/// boot must not replace them with a new person.
///
/// # Errors
///
/// Returns [`PersistError`] when a present identity file cannot be read, the
/// certificate is refused for the clock's reading alone (as a
/// [`tilo_user_identity::CertificateClockError`] carried in the error), or it does not
/// match its keys.
pub fn joined_device_is_ready(paths: &IdentityPaths, now_ns: u64) -> Result<bool, PersistError> {
    if !joined_device_is_enrolled(paths)? {
        return Ok(false);
    }
    let Some(cert) = read_owner_cert(paths)? else {
        return Ok(false);
    };
    verify_device_cert_at(&cert, now_ns).map_err(|error| {
        // Refused for the clock's reading alone, the certificate still
        // proves the enrolment: say so in a form callers can tell apart.
        PersistError::Io(
            tilo_user_identity::CertificateClockError::of(&cert, now_ns, &error).map_or_else(
                || {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("enrolled device certificate did not verify: {error}"),
                    )
                },
                tilo_user_identity::CertificateClockError::into_io,
            ),
        )
    })?;
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
    #[cfg(windows)]
    use tilo_test_env::SpawnUnderEnvLock as _;
    use tilo_user_identity::DeviceCapabilities;

    fn identity_tempdir() -> tempfile::TempDir {
        // macOS exposes /var via a symlink; security fixtures must start
        // from the actual directory before adding their deliberate links.
        tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
    }
    use ed25519_dalek::Signer;
    use std::sync::{Arc, Barrier};

    /// The identity root every program of the account reads the device key and certificates from
    /// is the machine's only when the machine holds an identity: a tree with no `device/` is
    /// where an identity would be made on that machine, not the one this account has, so the
    /// account's own data directory answers instead. Without one either, the machine's tree does.
    #[test]
    fn the_identity_root_is_the_machine_tree_only_when_it_holds_an_identity() {
        let fixture = identity_tempdir();
        let system = fixture.path().join("var/lib/tilo");
        let data = fixture.path().join("home/ada/.local/share");
        std::fs::create_dir_all(&system).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        assert_eq!(held_root(&system, Some(&data)), data.join("tilo"));
        std::fs::create_dir_all(system.join("device")).unwrap();
        assert_eq!(held_root(&system, Some(&data)), system);
        assert_eq!(
            held_root(&fixture.path().join("absent"), Some(&data)),
            data.join("tilo")
        );
        assert_eq!(
            held_root(&fixture.path().join("absent"), None),
            fixture.path().join("absent"),
            "with nowhere of the account's own, an identity is made in the machine's tree"
        );
        assert_eq!(
            held_root(&fixture.path().join("absent"), Some(Path::new("relative"))),
            fixture.path().join("absent"),
            "a relative data directory names no place an identity could be"
        );
    }

    /// `device/cert.bin` as `tilo login --new-identity` wrote it before the
    /// host and read-all grants were retired: a member certificate whose
    /// retired flags are clear. Every version's bytes are kept beside this
    /// crate and never change; a layout change gives the format a new
    /// version, its own kept bytes and the step that brings these up.
    fn kept_v1() -> Vec<u8> {
        let text = include_str!("../golden/device-certificate/v1.hex");
        hex::decode(text.split_whitespace().collect::<String>()).unwrap()
    }

    /// `device/cert.bin` of a workspace's VM: a device certificate that reaches one workspace
    /// alone, which only layout 2 writes.
    fn kept_v2() -> Vec<u8> {
        let text = include_str!("../golden/device-certificate/v2.hex");
        hex::decode(text.split_whitespace().collect::<String>()).unwrap()
    }

    /// A placed device's certificate, confined to two records, as a host issues it under its
    /// person's placement grant, which one of their devices signed: only layout 3 writes it.
    fn kept_v3() -> Vec<u8> {
        let text = include_str!("../golden/device-certificate/v3.hex");
        hex::decode(text.split_whitespace().collect::<String>()).unwrap()
    }

    /// The records the golden placed device's certificate names.
    fn golden_placed_records() -> tilo_user_identity::PlacedRecords {
        tilo_user_identity::PlacedRecords::new([0x11; 16], [[0x33; 32], [0x44; 32]]).unwrap()
    }

    #[test]
    fn a_placed_device_certificate_reads_verifies_and_writes_as_it_was() {
        let kept = kept_v3();
        assert_eq!(
            tilo_user_identity::cert::binary_layout_of(&kept).unwrap(),
            tilo_user_identity::cert::BINARY_LAYOUT
        );
        let dir = identity_tempdir();
        let path = dir.path().join("cert.bin");
        let read = std::cell::RefCell::new(None);
        tilo_files::native::migration::assert_golden_bytes(
            DEVICE_CERT.name,
            &kept,
            |bytes| {
                fs::write(&path, bytes).unwrap();
                let cert = read_device_cert(&path).unwrap();
                verify_device_cert(&cert).unwrap();
                assert_eq!(
                    cert.capabilities,
                    DeviceCapabilities::placed(golden_placed_records())
                );
                let grant = cert.issuer.as_deref().expect("issued under a grant");
                assert!(grant.capabilities.placing.is_some(), "a placement grant");
                assert!(
                    grant.issuer.is_some(),
                    "which a device of the person's signed"
                );
                assert_eq!(fs::read(&path).unwrap(), bytes);
                *read.borrow_mut() = Some(cert);
            },
            || {
                let written = dir.path().join("written.bin");
                write_device_cert(&written, read.borrow().as_ref().unwrap()).unwrap();
                fs::read(written).unwrap()
            },
        );
    }

    #[test]
    fn the_step_from_layout_two_leaves_its_certificate_as_it_was() {
        let migrated = DEVICE_CERT.migrate(kept_v2(), 2).unwrap();
        assert_eq!(migrated, kept_v2());
        let cert: DeviceCert =
            tilo_user_identity::canonical::from_canonical_bytes(&migrated).unwrap();
        verify_device_cert(&cert).unwrap();
    }

    #[test]
    fn the_step_from_layout_three_leaves_its_certificate_as_it_was() {
        tilo_files::native::migration::assert_step_reads_previous(
            &DEVICE_CERT,
            &kept_v3(),
            |migrated| {
                assert_eq!(migrated, kept_v3().as_slice());
                let cert: DeviceCert =
                    tilo_user_identity::canonical::from_canonical_bytes(migrated).unwrap();
                verify_device_cert(&cert).unwrap();
            },
        );
    }

    /// An agent's key as its person's root grants it in layout 4: the agent by its id, two
    /// workspaces and one app, for an hour.
    fn kept_v4() -> Vec<u8> {
        let text = include_str!("../golden/device-certificate/v4.hex");
        hex::decode(text.split_whitespace().collect::<String>()).unwrap()
    }

    /// The grant the layout 4 golden certificate carries.
    fn golden_agent_grant() -> tilo_user_identity::AgentScope {
        tilo_user_identity::AgentScope::Agent(tilo_user_identity::AgentGrant {
            agent: "claude-code-0a1b2c3d4e".to_owned(),
            workspaces: vec![[0x11; 16], [0x22; 16]],
            apps: vec!["notes".to_owned()],
        })
    }

    #[test]
    fn an_agents_grant_reads_verifies_and_writes_as_it_was() {
        let kept = kept_v4();
        assert_eq!(
            tilo_user_identity::cert::binary_layout_of(&kept).unwrap(),
            tilo_user_identity::cert::BINARY_LAYOUT
        );
        assert_eq!(
            DEVICE_CERT.version,
            tilo_user_identity::cert::BINARY_LAYOUT,
            "the declared version is the one the identity crate writes"
        );
        let dir = identity_tempdir();
        let path = dir.path().join("cert.bin");
        let read = std::cell::RefCell::new(None);
        tilo_files::native::migration::assert_golden_bytes(
            DEVICE_CERT.name,
            &kept,
            |bytes| {
                fs::write(&path, bytes).unwrap();
                let cert = read_device_cert(&path).unwrap();
                verify_device_cert_at(&cert, 1_000_001).unwrap();
                assert_eq!(
                    cert.capabilities,
                    DeviceCapabilities::agent_grant(golden_agent_grant())
                );
                assert!(
                    cert.capabilities
                        .agent
                        .as_ref()
                        .unwrap()
                        .is_agent("claude-code-0a1b2c3d4e")
                );
                assert_eq!(fs::read(&path).unwrap(), bytes);
                *read.borrow_mut() = Some(cert);
            },
            || {
                let written = dir.path().join("written.bin");
                write_device_cert(&written, read.borrow().as_ref().unwrap()).unwrap();
                fs::read(written).unwrap()
            },
        );
    }

    /// What a build of layout 3 does with an agent's grant of layout 4 after a device rolls back:
    /// it names the certificate a later layout than its own and refuses the file with that, never
    /// reading it as a certificate it knows.
    #[test]
    fn a_build_of_layout_three_refuses_an_agents_grant_with_why() {
        // The format as a build of layout 3 declared it.
        let layout_three = Format {
            version: 3,
            steps: &[layout_one_is_layout_two, layout_two_is_layout_three],
            ..DEVICE_CERT
        };
        let kept = kept_v4();
        // Layout 3 knew first capabilities bytes up to its placement grant's marker (6), and
        // named any byte above them a later layout.
        let (_, capabilities) =
            postcard::take_from_bytes::<([u8; 32], [u8; 32], String)>(&kept).unwrap();
        let first = capabilities[0];
        assert!(
            first > 6,
            "layout 3 names this certificate a later layout: {first}"
        );
        let stored = 3 + 1;
        let dir = identity_tempdir();
        let path = dir.path().join("cert.bin");
        fs::write(&path, &kept).unwrap();
        let refused = layout_three
            .bring_up(&path, kept.clone(), stored)
            .unwrap_err()
            .to_string();
        assert_eq!(
            refused,
            "device certificate version 4 was written by a newer build than this one (3)"
        );
        assert_eq!(fs::read(&path).unwrap(), kept, "the file stays as it was");
    }

    #[test]
    fn a_confined_device_certificate_reads_verifies_and_writes_as_it_was() {
        let kept = kept_v2();
        // Layout 3 reads every certificate layout 2 wrote as it is: the file is current.
        assert_eq!(
            tilo_user_identity::cert::binary_layout_of(&kept).unwrap(),
            tilo_user_identity::cert::BINARY_LAYOUT
        );
        let dir = identity_tempdir();
        let path = dir.path().join("cert.bin");
        let read = std::cell::RefCell::new(None);
        tilo_files::native::migration::assert_golden_bytes(
            DEVICE_CERT.name,
            &kept,
            |bytes| {
                fs::write(&path, bytes).unwrap();
                let cert = read_device_cert(&path).unwrap();
                verify_device_cert(&cert).unwrap();
                assert_eq!(
                    cert.capabilities,
                    DeviceCapabilities::workspace_device([0x11; 16])
                );
                assert_eq!(
                    fs::read(&path).unwrap(),
                    bytes,
                    "a current file is read as it is"
                );
                *read.borrow_mut() = Some(cert);
            },
            || {
                let written = dir.path().join("written.bin");
                write_device_cert(&written, read.borrow().as_ref().unwrap()).unwrap();
                fs::read(written).unwrap()
            },
        );
    }

    #[test]
    fn the_steps_from_layout_one_leave_its_certificate_as_it_was() {
        let migrated = DEVICE_CERT.migrate(kept_v1(), 1).unwrap();
        assert_eq!(migrated, kept_v1());
        let cert: DeviceCert =
            tilo_user_identity::canonical::from_canonical_bytes(&migrated).unwrap();
        verify_device_cert(&cert).unwrap();
    }

    #[test]
    fn a_device_certificate_an_older_build_wrote_reads_verifies_and_writes_as_it_was() {
        let kept = kept_v1();
        // This layout reads every certificate layout 1 wrote as it is: the file is current.
        assert_eq!(
            tilo_user_identity::cert::binary_layout_of(&kept).unwrap(),
            tilo_user_identity::cert::BINARY_LAYOUT
        );
        assert_eq!(
            DEVICE_CERT.version,
            tilo_user_identity::cert::BINARY_LAYOUT,
            "the declared version is the one the identity crate writes"
        );
        let dir = identity_tempdir();
        let paths = IdentityPaths::under(dir.path());
        paths.ensure_directories().unwrap();
        let cert_path = paths.certs_dir().join("layout-one.bin");
        let read = std::cell::RefCell::new(None);
        tilo_files::native::migration::assert_golden_bytes(
            DEVICE_CERT.name,
            &kept,
            |bytes| {
                fs::write(&cert_path, bytes).unwrap();
                let cert = read_device_cert(&cert_path).unwrap();
                verify_device_cert(&cert).unwrap();
                assert_eq!(cert.device_label, "tilo on laptop");
                assert_eq!(cert.capabilities, DeviceCapabilities::member());
                assert_eq!(cert.expires_at_ns, None);
                assert_eq!(
                    fs::read(&cert_path).unwrap(),
                    bytes,
                    "a current file is read as it is"
                );
                *read.borrow_mut() = Some(cert);
            },
            || {
                let written = dir.path().join("written.bin");
                write_device_cert(&written, read.borrow().as_ref().unwrap()).unwrap();
                fs::read(written).unwrap()
            },
        );
    }

    /// A file in a layout later than this build's is refused and left as it
    /// is, not read as if it were this one.
    #[test]
    fn a_device_certificate_in_a_later_layout_is_refused_and_left_alone() {
        let dir = identity_tempdir();
        let path = dir.path().join("cert.bin");
        let mut later = kept_v1();
        later[32 + 32 + 1 + "tilo on laptop".len()] = 9;
        fs::write(&path, &later).unwrap();
        let refused = read_device_cert(&path).unwrap_err().to_string();
        assert!(refused.contains("newer build"), "{refused}");
        assert_eq!(fs::read(&path).unwrap(), later);
    }

    #[cfg(not(windows))]
    #[test]
    fn failed_identity_publication_removes_the_private_candidate() {
        let tmp = identity_tempdir();
        let destination = tmp.path().join("identity");
        fs::create_dir(&destination).unwrap();

        let result = publish_identity_bytes_no_clobber(&destination, &[7; 32], DEVICE_KEY_MODE);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(matches!(result, Ok(false)), "{result:?}");
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        assert!(matches!(
            result,
            Err(PersistError::Io(ref error)) if error.kind() == io::ErrorKind::Unsupported
        ));

        let remaining: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(remaining, vec![destination.clone()]);
        assert!(destination.is_dir());
    }

    tilo_test_env::scenario! {
        #[cfg(unix)]
        fn held_device_key_reader_keeps_its_cursor_and_refuses_wrong_sizes;

        phase held_device_key_reader_preserves_cursor_and_canonical_inode_policy;
        {
            use std::io::{Seek as _, SeekFrom};
            use std::os::unix::fs::PermissionsExt as _;
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("device-key");
            let key = SigningKey::from_bytes(&[31; 32]);
            write_device_key(&path, &key).unwrap();
            let file = fs::File::open(&path).unwrap();
            let mut alias = file.try_clone().unwrap();
            alias.seek(SeekFrom::End(0)).unwrap();
            assert_eq!(
                read_device_key_from_file(&file).unwrap().verifying_key(),
                key.verifying_key()
            );
            assert_eq!(alias.stream_position().unwrap(), 32);
            assert_eq!(
                read_device_key(&path).unwrap().verifying_key(),
                key.verifying_key()
            );
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(matches!(
                read_device_key_from_file(&file),
                Err(PersistError::InsecurePermissions { .. })
            ));
            fs::set_permissions(&path, fs::Permissions::from_mode(DEVICE_KEY_MODE)).unwrap();
            fs::hard_link(&path, root.path().join("alias")).unwrap();
            assert!(matches!(
                read_device_key_from_file(&file),
                Err(PersistError::InsecureLinkCount { .. })
            ));
        }

        phase held_device_key_reader_refuses_short_and_oversized_actual_files;
        {
            use std::os::unix::fs::PermissionsExt as _;
            let root = tempfile::tempdir().unwrap();
            for (name, length) in [("short", 31), ("long", 4096)] {
                let path = root.path().join(name);
                fs::write(&path, vec![0u8; length]).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(DEVICE_KEY_MODE)).unwrap();
                let file = fs::File::open(path).unwrap();
                assert!(matches!(
                    read_device_key_from_file(&file),
                    Err(PersistError::WrongKeyLength(actual)) if actual == length.min(33)
                ));
            }
        }
    }

    tilo_test_env::scenario! {
        fn a_joined_device_is_ready_with_its_own_certificate_and_no_human_root;

        phase joined_device_is_certified_without_copying_the_human_root;
        {
            let root = SigningKey::generate(&mut UnwrapErr(SysRng));
            let dir = identity_tempdir();
            let paths = IdentityPaths::under(dir.path());
            let (device, _) = load_or_create_device_key(&paths).unwrap();
            // This process stands in for the device's daemon, which alone answers which key it is.
            let _taken = take_device_key(&paths).unwrap();
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "orange-pi".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1_000,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
            install_device_cert(&paths, &cert, 1_000).unwrap();

            assert_eq!(cert.user_public_key, root.verifying_key().to_bytes());
            assert_ne!(cert.device_public_key, cert.user_public_key);
            assert!(cert.capabilities.can_own_workspaces);
            assert!(!cert.capabilities.can_admit_devices);
            verify_device_cert_at(&cert, 1_000).unwrap();
            assert_eq!(read_owner_cert(&paths).unwrap(), Some(cert.clone()));
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
        }

        phase a_certificate_signed_elsewhere_installs_for_the_existing_key;
        // A device whose key was made first takes the certificate a root
        // signed for it elsewhere, and is then a ready joined device.
        {
            let dir = identity_tempdir();
            let paths = IdentityPaths::under(dir.path());
            let (device, _) = load_or_create_device_key(&paths).unwrap();
            let _taken = take_device_key(&paths).expect("this process signs as the device");
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
                    issuer: None,
                };
                tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
                cert
            };
            let own = device.verifying_key().to_bytes();
            let node = tilo_user_identity::DeviceCapabilities::member;
            let refused = |result: Result<(), PersistError>| result.unwrap_err().to_string();

            assert!(
                refused(install_device_cert(
                    &paths,
                    &cert(&root, [9; 32], node()),
                    2_000
                ))
                .contains("another device key")
            );
            // A clock outside the certificate's interval is refused as that and says so: expired
            // says no clock reaches it, and a clock that reads before its issuance says when it
            // does — what a session resumed from a snapshot waits for instead of ending without an
            // identity.
            let late = install_device_cert(&paths, &cert(&root, own, node()), 20_000).unwrap_err();
            let error = late.io().unwrap();
            assert!(
                tilo_user_identity::CertificateClockError::in_io(error)
                    .is_some_and(|clock| clock.valid_at_ns().is_none()),
                "an expired certificate is a refusal for the clock alone"
            );
            let early = install_device_cert(&paths, &cert(&root, own, node()), 500).unwrap_err();
            let clock = tilo_user_identity::CertificateClockError::in_io(early.io().unwrap())
                .expect("a refusal for the clock alone keeps its reading");
            assert_eq!(clock.valid_at_ns(), Some(1_000));
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

        phase a_nearby_paired_member_is_a_ready_joined_device;
        // A device paired from a nearby device holds an ordinary member
        // certificate: it does not read every workspace, and is still a ready
        // joined device rather than one that needs a human root.
        {
            let dir = identity_tempdir();
            let paths = IdentityPaths::under(dir.path());
            let (device, _) = load_or_create_device_key(&paths).unwrap();
            let _taken = take_device_key(&paths).expect("this process signs as the device");
            let root = SigningKey::from_bytes(&[5; 32]);
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "laptop".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1_000,
                expires_at_ns: Some(10_000),
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
            install_device_cert(&paths, &cert, 2_000).unwrap();

            assert!(joined_device_is_ready(&paths, 2_000).unwrap());
        }
    }

    /// An enrollment whose device key the session cannot reach is reported,
    /// not mistaken for a machine without one.
    #[cfg(unix)]
    #[test]
    fn an_unreachable_device_key_is_an_error_not_a_fresh_machine() {
        use std::os::unix::fs::PermissionsExt as _;

        #[cfg(target_os = "linux")]
        tilo_test_env::require_capability!(
            NonRoot,
            tilo_test_env::non_root_identity(),
            "an unreachable key directory is only unreachable to an ordinary account"
        );
        // The identity probes are Linux-only; elsewhere root still reads the directory.
        #[cfg(not(target_os = "linux"))]
        if rustix::process::geteuid().as_raw() == 0 {
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
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        // This process signs as the device, as its daemon does: the certificate names the key it
        // holds, and the check below still probes the key file for the machine's enrollment.
        let _taken = take_device_key(&paths).expect("this process signs as the device");
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

    tilo_test_env::scenario! {
        /// Replacing a signing key keeps it private; a round trip enforces 0600; world-readable,
        /// symlinked or multiply-named key files are rejected; secret-key owner policies are distinct; a
        /// wrong-length key is rejected.
        fn signing_keys_are_private_single_named_files_of_the_right_length;

        phase replacing_a_signing_key_preserves_private_readability;
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

        phase signing_key_round_trip_enforces_0600;
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let tmp = identity_tempdir();
                let path = tmp.path().join("keypair.bin");
                let (sk, _pk) = tilo_user_identity::UserIdentity::generate("Alice", 1);
                write_signing_key(&path, &sk).unwrap();

                // Written file should be 0600.
                let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600);

                let loaded = read_signing_key(&path).unwrap();
                // Signing with either should give the same signature.
                let msg = b"round-trip";
                assert_eq!(sk.sign(msg).to_bytes(), loaded.sign(msg).to_bytes());
            }
        }

        phase signing_key_rejects_world_readable;
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let tmp = identity_tempdir();
                let path = tmp.path().join("keypair.bin");
                let (sk, _pk) = tilo_user_identity::UserIdentity::generate("Alice", 1);
                write_signing_key(&path, &sk).unwrap();
                // Loosen to 0644.
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                match read_signing_key(&path).unwrap_err() {
                    PersistError::InsecurePermissions { mode, .. } => assert_eq!(mode, 0o644),
                    other => panic!("expected InsecurePermissions, got {other:?}"),
                }
            }
        }

        phase signing_key_never_follows_a_symlink;
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;

                let tmp = identity_tempdir();
                let real = tmp.path().join("real.key");
                let link = tmp.path().join("linked.key");
                let (sk, _pk) = tilo_user_identity::UserIdentity::generate("Alice", 1);
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
        }

        phase signing_key_rejects_an_inode_with_multiple_names;
        {
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
        }

        phase secret_key_owner_policies_are_distinct;
        {
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
        }

        phase wrong_length_key_rejected;
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

    tilo_test_env::scenario! {
        /// A public Windows key is neither read nor silently replaced; an explicit public-key path
        /// outside the managed layout round-trips; authorization artifacts never follow symlinks, except
        /// under a distro home symlink where they are readable.
        fn public_keys_and_authorization_artifacts_are_read_only_from_trusted_paths;

        phase public_windows_key_is_neither_read_nor_silently_replaced;
        {
            #[cfg(windows)]
            {
                let root = identity_tempdir();
                let paths = IdentityPaths::under(root.path());
                let (original, _) = load_or_create_device_key(&paths).unwrap();
                let result = std::process::Command::new("icacls.exe")
                    .arg(&paths.device_privkey)
                    .args(["/grant", "*S-1-1-0:(R)"])
                    .output_locked()
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
        }

        phase explicit_public_key_path_outside_managed_layout_round_trips;
        {
            let tmp = identity_tempdir();
            let path = tmp.path().join("pubkey.bin");
            let key = [7u8; 32];
            write_public_key(&path, &key).unwrap();
            let back = read_public_key(&path).unwrap();
            assert_eq!(key, back);
        }

        phase authorization_artifacts_never_follow_symlinks;
        {
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
        }

        phase authorization_artifact_under_distro_home_symlink_is_readable;
        {
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
    }

    tilo_test_env::scenario! {
        /// `ensure_directories` creates the expected layout, certificates round-trip, and identity-specific
        /// cert paths do not collide.
        fn the_identity_layout_holds_per_identity_certificates;

        phase ensure_directories_creates_expected_layout;
        {
            let tmp = identity_tempdir();
            let paths = IdentityPaths::under(tmp.path());
            paths.ensure_directories().unwrap();
            assert!(tmp.path().join("user-identity").is_dir());
            assert!(tmp.path().join("device-private").is_dir());
            assert!(tmp.path().join("device").is_dir());
        }

        phase cert_round_trip;
        {
            use tilo_user_identity::cert::{DeviceCapabilities, sign_device_cert};
            use tilo_user_identity::{DeviceIdentity, UserIdentity};

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
                issuer: None,
            };
            sign_device_cert(&mut cert, &root_sk).unwrap();
            write_device_cert(&path, &cert).unwrap();
            let back = read_device_cert(&path).unwrap();
            assert_eq!(cert, back);
        }

        phase identity_specific_cert_paths_do_not_collide;
        {
            let paths = IdentityPaths::under("/var/lib/tilo");
            let alice = paths.device_cert_for_user(&[1; 32]);
            let bob = paths.device_cert_for_user(&[2; 32]);
            assert_ne!(alice, bob);
            assert!(alice.starts_with("/var/lib/tilo/device/certs"));
        }
    }

    tilo_test_env::scenario! {
        /// The device key is owner-only and refuses a group-readable one, and rejects a
        /// multiply-named inode; its creation writes it so, a user-scoped secret still refuses
        /// group read; the device key is created once then reloaded, simultaneous creation
        /// persists one key, an unreadable key raises instead of rotating, and adopting a key
        /// seeds a new device but never replaces one.
        fn device_keys_are_owner_only_created_once_and_never_rotated_silently;

        phase device_key_is_owner_only_and_refuses_a_group_readable_key;
        // Only the device's daemon reads the key, as the account that owns its
        // directory. A key anyone else can reach — its group included — is
        // refused on every read rather than trusted to whoever set the mode.
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let tmp = identity_tempdir();
                let paths = IdentityPaths::under(tmp.path());
                let (sk, _) = load_or_create_device_key(&paths).unwrap();
                let path = &paths.device_privkey;

                let written = fs::metadata(path).unwrap().permissions().mode() & 0o777;
                assert_eq!(written, DEVICE_KEY_MODE);
                assert_eq!(written, 0o600, "owner read and write only");

                // Owner-only: accepted.
                assert_eq!(read_device_key(path).unwrap().to_bytes(), sk.to_bytes());

                // A group read: refused, whether read by path or from an opened file.
                fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();
                match read_device_key(path) {
                    Err(PersistError::InsecurePermissions { mode, .. }) => assert_eq!(mode, 0o640),
                    other => panic!("a group-readable device key must be refused, got {other:?}"),
                }
                let file = fs::File::open(path).unwrap();
                assert!(matches!(
                    read_device_key_from_file(&file),
                    Err(PersistError::InsecurePermissions { .. })
                ));

                // Any bit for the group or for `other`: refused.
                for mode in [0o644u32, 0o604, 0o601, 0o666, 0o660, 0o620] {
                    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
                    match read_device_key(path) {
                        Err(PersistError::InsecurePermissions { .. }) => {}
                        other => panic!("mode {mode:o} must be refused, got {other:?}"),
                    }
                }

                // Giving the key and its directory back their state, as the machine's boot does,
                // makes it readable again.
                let directory = path.parent().unwrap();
                fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
                let me = (
                    rustix::process::geteuid().as_raw(),
                    rustix::process::getegid().as_raw(),
                );
                enforce_device_key_owner_and_mode(path, me).unwrap();
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    DEVICE_KEY_MODE
                );
                assert_eq!(
                    fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                    0o700
                );
                assert_eq!(read_device_key(path).unwrap().to_bytes(), sk.to_bytes());
            }
        }

        phase device_key_rejects_an_inode_with_multiple_names;
        {
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
        }

        phase user_scoped_secret_refuses_group_read;
        // A person's own signing key is owner-only too: a group has no business
        // reading it either.
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let tmp = identity_tempdir();
                let path = tmp.path().join("user-secret.bin");
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
        }

        phase device_key_is_created_once_then_reloaded;
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

        phase simultaneous_device_key_creation_uses_one_persisted_key;
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

        phase unreadable_device_key_raises_instead_of_rotating;
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

        phase adopt_device_key_seeds_a_new_device_but_never_replaces_one;
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
        // The installer leaves the device directory the trust group's, and tmpfiles gives the
        // certificate directory beside it the same group and setgid bit.
        let device_dir = &paths.device_dir;
        fs::set_permissions(device_dir, fs::Permissions::from_mode(0o2770)).unwrap();
        let certs = paths.certs_dir();
        fs::set_permissions(&certs, fs::Permissions::from_mode(0o2770)).unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [3; 32],
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let cert_path = paths.device_cert_for_user(&cert.user_public_key);
        write_device_cert(&cert_path, &cert).unwrap();
        let metadata = fs::metadata(&certs).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o2770);
        assert_eq!(metadata.gid(), fs::metadata(device_dir).unwrap().gid());
        assert_eq!(read_device_cert(&cert_path).unwrap(), cert);
        assert!(!paths.device_privkey.exists());
    }

    #[test]
    fn a_previous_system_certificate_settles_beside_its_persons_and_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(dir.path());
        paths.ensure_directories().unwrap();
        let root = SigningKey::from_bytes(&[9; 32]);
        let person = root.verifying_key().to_bytes();
        write_public_key(&paths.user_pubkey, &person).unwrap();
        let mut cert = DeviceCert {
            user_public_key: person,
            device_public_key: [3; 32],
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let system_cert = paths.system_device_cert();
        write_device_cert(&system_cert, &cert).unwrap();

        assert!(settle_device_certificate(&paths).unwrap());
        assert!(!system_cert.exists(), "the second copy is gone");
        let filed = paths.device_cert_for_user(&person);
        assert_eq!(read_device_cert(&filed).unwrap(), cert);

        assert!(
            !settle_device_certificate(&paths).unwrap(),
            "settled already"
        );
        assert_eq!(read_device_cert(&filed).unwrap(), cert);
    }

    #[test]
    fn a_previous_system_certificate_leaves_a_certificate_already_filed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(dir.path());
        paths.ensure_directories().unwrap();
        let root = SigningKey::from_bytes(&[9; 32]);
        let person = root.verifying_key().to_bytes();
        write_public_key(&paths.user_pubkey, &person).unwrap();
        let cert = |label: &str, seed: [u8; 32], root: &SigningKey| {
            let mut cert = DeviceCert {
                user_public_key: person,
                device_public_key: seed,
                device_label: label.into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
            cert
        };
        let filed_cert = cert("filed", [4; 32], &root);
        let previous_cert = cert("previous", [5; 32], &root);
        let filed = paths.device_cert_for_user(&person);
        write_device_cert(&filed, &filed_cert).unwrap();
        write_device_cert(&paths.system_device_cert(), &previous_cert).unwrap();

        assert!(settle_device_certificate(&paths).unwrap());
        assert!(!paths.system_device_cert().exists());
        assert_eq!(read_device_cert(&filed).unwrap(), filed_cert);
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
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let cert_path = dir.path().join("device/certs/one.bin");
        write_device_cert(&cert_path, &cert).unwrap();
        unsafe { libc::umask(previous) };
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&pubkey), 0o644);
        assert_eq!(mode(&cert_path), 0o644);
        assert_eq!(mode(cert_path.parent().unwrap()), 0o755);
    }
}
