//! Durable, bounded storage for exact-device Nearby transfers.
//!
//! Sources are copied into this private spool before an offer is sent. This
//! makes a resumed transfer immutable across source edits and reboots. The
//! wire protocol refers only to random IDs and portable display names.
//!
//! This is the one byte-transfer primitive between a person's devices. A
//! Nearby send delivers into a destination the receiver chose; a media
//! handoff ([`TransferKind::MediaHandoff`]) delivers into the store's private,
//! leased media cache and never into Downloads or a Files destination.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use serde::{Deserialize, Serialize};

pub const STORE_SCHEMA_VERSION: u16 = 1;
pub const MAX_TRANSFERS: usize = 64;
pub const MAX_TRANSFER_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const MAX_FOLDER_ENTRIES: usize = 100_000;
pub const TRANSFER_LIFETIME_MS: u64 = 7 * 24 * 60 * 60 * 1000;
pub const CHECKPOINT_BYTES: u64 = 8 * 1024 * 1024;
/// Verified media-handoff objects kept for players, apart from the spool quota.
pub const MAX_MEDIA_CACHE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// Free space every staged or incoming object must leave on the spool's filesystem.
pub const MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// How long a player that resolved a cached media object keeps it alive.
pub const MEDIA_LEASE: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);
/// Completed media handoffs a receiver remembers before evicting the oldest.
pub const MAX_CACHED_MEDIA: usize = 64;

#[cfg(target_os = "linux")]
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
#[cfg(target_os = "linux")]
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
#[cfg(target_os = "linux")]
const RESOLVE_BENEATH: u64 = 0x08;

#[cfg(target_os = "linux")]
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// An already-open source capability rooted beneath one live Files grant.
/// Deferred staging owns the descriptor and never reopens the app-provided
/// absolute path.
pub struct SecureNearbySource {
    file: File,
    name: String,
}

/// An already-open writable destination capability. Installation remains
/// anchored to this descriptor even if the granted directory is renamed.
pub struct SecureNearbyDestination {
    directory: File,
    display_root: PathBuf,
}

impl SecureNearbyDestination {
    /// Retain an exact directory beneath an existing grant, with the same
    /// no-follow relative walk used by source admission.
    #[cfg(target_os = "linux")]
    pub(crate) fn open_beneath(grant: &Path, requested: &Path) -> Result<Self, String> {
        let source = SecureNearbySource::open_beneath(grant, requested)?;
        if !source.metadata()?.is_dir() {
            return Err("destination is not a directory".into());
        }
        Ok(Self {
            directory: source.into_file(),
            display_root: requested.to_path_buf(),
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn metadata(&self) -> Result<fs::Metadata, String> {
        self.directory.metadata().map_err(display)
    }

    /// Retain the leaf itself, including a symlink as data. Only one validated
    /// name is accepted; directory walking remains the existing beneath walk.
    #[cfg(target_os = "linux")]
    pub(crate) fn entry(&self, name: &std::ffi::OsStr) -> Result<File, String> {
        let path = Path::new(name);
        validate_relative(path)?;
        if path.components().count() != 1 {
            return Err("entry is not one leaf".into());
        }
        let name = CString::new(name.as_bytes()).map_err(display)?;
        // SAFETY: owned directory and a live NUL-terminated single name; a
        // successful openat returns a new owned no-follow metadata descriptor.
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        // SAFETY: successful openat transferred this descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Create a private child under this retained directory, without reopening
    /// its display path. Existing children are never adopted.
    #[cfg(target_os = "linux")]
    pub(crate) fn private_child(&self, name: &std::ffi::OsStr) -> Result<Self, String> {
        let path = Path::new(name);
        validate_relative(path)?;
        if path.components().count() != 1 {
            return Err("private child is not one leaf".into());
        }
        let encoded = CString::new(name.as_bytes()).map_err(display)?;
        // SAFETY: retained directory and a terminated single-component name;
        // mkdirat creates exclusively with owner-only access.
        if unsafe { libc::mkdirat(self.directory.as_raw_fd(), encoded.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        let directory = self.entry(name)?;
        if !directory.metadata().map_err(display)?.is_dir() {
            return Err("private child changed during creation".into());
        }
        Ok(Self {
            directory,
            display_root: self.display_root.join(name),
        })
    }

    /// Open one approved writable Files grant without following a final link.
    ///
    /// # Errors
    ///
    /// Returns a message when the grant cannot be resolved or opened, or is
    /// not a directory.
    pub fn open_grant(grant: &Path) -> Result<Self, String> {
        let display_root = fs::canonicalize(grant).map_err(display)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(&display_root)
            .map_err(display)?;
        if !directory.metadata().map_err(display)?.is_dir() {
            return Err(crate::shell::i18n::t("nearby-error-destination-not-folder"));
        }
        Ok(Self {
            directory,
            display_root,
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn directory_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd as _;
        self.directory.as_raw_fd()
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn anchored_root(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()))
    }

    pub(crate) fn install_inline(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        #[cfg(target_os = "macos")]
        {
            if bytes.is_empty() || u64::try_from(bytes.len()).map_err(display)? > MAX_TRANSFER_BYTES
            {
                return Err(crate::shell::i18n::t("nearby-error-too-large"));
            }
            return install_capability_file(&self.directory, name, &mut io::Cursor::new(bytes))
                .map(|name| self.display_root.join(name));
        }
        #[cfg(not(target_os = "macos"))]
        {
            let anchored = install_bytes_create_new(&self.anchored_root(), name, bytes)?;
            Ok(self.display_root.join(
                anchored
                    .file_name()
                    .ok_or_else(|| crate::shell::i18n::t("nearby-error-destination-name"))?,
            ))
        }
    }
}

impl SecureNearbySource {
    /// Open `requested` beneath one exact granted file/directory without
    /// following any symlink in the relative walk.
    ///
    /// # Errors
    /// Returns an error when the grant/source is unavailable, outside the
    /// grant, contains symlinks, or is not a regular file/directory.
    pub fn open_beneath(grant: &Path, requested: &Path) -> Result<Self, String> {
        let name = portable_name(requested)?;
        let canonical_grant = fs::canonicalize(grant).map_err(display)?;
        let metadata = fs::metadata(&canonical_grant).map_err(display)?;
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&canonical_grant)
            .map_err(display)?;
        let file = if metadata.is_dir() {
            let relative = requested
                .strip_prefix(grant)
                .map_err(|_| crate::shell::i18n::t("nearby-error-outside-grant"))?;
            if relative.as_os_str().is_empty() {
                root
            } else {
                validate_relative(relative)?;
                openat2_beneath(&root, relative, false)?
            }
        } else if metadata.is_file() && requested == grant {
            root
        } else {
            return Err(crate::shell::i18n::t("nearby-error-outside-grant"));
        };
        let source_metadata = file.metadata().map_err(display)?;
        if !source_metadata.is_file() && !source_metadata.is_dir() {
            return Err(crate::shell::i18n::t("nearby-error-one-item"));
        }
        Ok(Self { file, name })
    }

    pub(crate) fn metadata(&self) -> Result<fs::Metadata, String> {
        self.file.metadata().map_err(display)
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn into_file(self) -> File {
        self.file
    }
}

#[cfg(target_os = "linux")]
fn openat2_beneath(root: &File, relative: &Path, directory: bool) -> Result<File, String> {
    validate_relative(relative)?;
    let path = CString::new(relative.as_os_str().as_bytes())
        .map_err(|_| "Nearby Send path contains NUL".to_string())?;
    let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    let how = OpenHow {
        flags: u64::try_from(flags).map_err(display)?,
        mode: 0,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
    };
    // SAFETY: `path` and `how` are valid for the duration of the syscall; a
    // successful return is a new owned descriptor.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            path.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let fd = i32::try_from(fd).map_err(display)?;
    // SAFETY: `openat2` returned a fresh descriptor owned by this function.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// What one transfer carries. Postcard encodes the variant index on the wire,
/// so variants are only ever appended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferKind {
    File,
    FolderArchive,
    /// The local file behind a media handoff. The receiver's accepted
    /// `ResumeMedia` authorises it instead of a Files consent, and it is
    /// delivered only into the private media cache. Neither side keeps it
    /// across a restart: the authorising action is session-local.
    MediaHandoff,
}

impl TransferKind {
    /// The continuity switch that governs transfers of this kind.
    #[must_use]
    pub const fn capability(self) -> crate::shell::continuity_policy::ContinuityCapability {
        match self {
            Self::File | Self::FolderArchive => {
                crate::shell::continuity_policy::ContinuityCapability::FileTransfer
            }
            Self::MediaHandoff => crate::shell::continuity_policy::ContinuityCapability::Media,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub id: [u8; 16],
    pub target: String,
    pub name: String,
    pub kind: TransferKind,
    pub total_bytes: u64,
    pub content_hash: [u8; 32],
    pub created_unix_ms: u64,
    pub expires_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEntry {
    pub id: [u8; 16],
    pub source: String,
    pub request_id: [u8; 16],
    pub name: String,
    pub kind: TransferKind,
    pub total_bytes: u64,
    pub content_hash: [u8; 32],
    pub committed_offset: u64,
    pub created_unix_ms: u64,
    pub expires_unix_ms: u64,
}

/// What an exact source offers for one incoming object: the manifest a new
/// inbox entry is created from, or that a resume must match exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingManifest {
    pub id: [u8; 16],
    pub source: String,
    pub request_id: [u8; 16],
    pub name: String,
    pub kind: TransferKind,
    pub total_bytes: u64,
    pub content_hash: [u8; 32],
}

#[derive(Default, Serialize, Deserialize)]
struct StoreIndex {
    version: u16,
    outbox: Vec<OutboxEntry>,
    inbox: Vec<InboxEntry>,
}

pub struct NearbyTransferStore {
    root: PathBuf,
    index: StoreIndex,
    /// Running hash of each incoming prefix written this session, so finishing
    /// a large object does not re-read it on the receive path.
    hashers: BTreeMap<[u8; 16], (u64, blake3::Hasher)>,
    media: MediaCache,
}

/// Session-local bindings from an exact sender and transfer to a verified
/// cached object, plus the player leases that keep an object alive after its
/// binding is gone. Objects are content addressed, so bindings may share one.
#[derive(Default)]
struct MediaCache {
    completed: BTreeMap<(String, [u8; 16]), PathBuf>,
    completed_order: VecDeque<(String, [u8; 16])>,
    leased_until: BTreeMap<PathBuf, std::time::Instant>,
}

impl MediaCache {
    fn expire_leases(&mut self) {
        let now = std::time::Instant::now();
        self.leased_until.retain(|_, until| *until > now);
    }

    fn protected_paths(&mut self) -> BTreeSet<PathBuf> {
        self.expire_leases();
        self.completed
            .values()
            .cloned()
            .chain(self.leased_until.keys().cloned())
            .collect()
    }

    /// Remove `path` once no binding and no live lease references it.
    fn release_object(&mut self, path: &Path) {
        self.expire_leases();
        let referenced = self.completed.values().any(|candidate| candidate == path)
            || self.leased_until.contains_key(path);
        if !referenced {
            let _ = fs::remove_file(path);
            if let Some(parent) = path.parent() {
                let _ = fs::remove_dir(parent);
            }
        }
    }

    fn unbind(&mut self, key: &(String, [u8; 16])) -> bool {
        let Some(path) = self.completed.remove(key) else {
            return false;
        };
        self.completed_order.retain(|candidate| candidate != key);
        self.release_object(&path);
        true
    }

    fn evict_oldest_completed(&mut self) -> bool {
        let oldest = loop {
            if let Some(candidate) = self.completed_order.pop_front() {
                if self.completed.contains_key(&candidate) {
                    break Some(candidate);
                }
                continue;
            }
            break self.completed.keys().next().cloned();
        };
        oldest.is_some_and(|oldest| self.unbind(&oldest))
    }

    fn bind(&mut self, key: (String, [u8; 16]), path: PathBuf) {
        if let Some(previous) = self.completed.insert(key.clone(), path) {
            self.completed_order.retain(|candidate| candidate != &key);
            if self.completed.get(&key) != Some(&previous) {
                self.release_object(&previous);
            }
        }
        self.completed_order.push_back(key);
        while self.completed.len() > MAX_CACHED_MEDIA {
            if !self.evict_oldest_completed() {
                break;
            }
        }
    }
}

/// One staging request: the identity and display name an outbox entry gets.
struct Staging {
    id: [u8; 16],
    name: String,
    target: String,
    now_unix_ms: u64,
    media: bool,
}

impl NearbyTransferStore {
    /// Open and repair a private transfer spool. Uncheckpointed crash tails are
    /// truncated before a resume offset is advertised.
    ///
    /// # Errors
    /// Returns an error for unsafe storage, invalid metadata, or failed repair.
    pub fn open(root: PathBuf, now_unix_ms: u64) -> Result<Self, String> {
        ensure_private_dir(&root)?;
        ensure_private_dir(&root.join("outbox"))?;
        ensure_private_dir(&root.join("inbox"))?;
        let index_path = root.join("index.json");
        let mut index = if index_path.exists() {
            let bytes = fs::read(&index_path).map_err(display)?;
            serde_json::from_slice::<StoreIndex>(&bytes).map_err(display)?
        } else {
            StoreIndex {
                version: STORE_SCHEMA_VERSION,
                ..StoreIndex::default()
            }
        };
        if index.version != STORE_SCHEMA_VERSION {
            return Err("unsupported nearby-transfer index version".into());
        }
        validate_index(&index)?;
        index
            .outbox
            .retain(|entry| entry.expires_unix_ms > now_unix_ms);
        index
            .inbox
            .retain(|entry| entry.expires_unix_ms > now_unix_ms);
        // A media handoff is authorised by one session's `ResumeMedia`; after a
        // restart neither side can resume it, so its bytes are only waste.
        index
            .outbox
            .retain(|entry| entry.kind != TransferKind::MediaHandoff);
        index
            .inbox
            .retain(|entry| entry.kind != TransferKind::MediaHandoff);
        ensure_private_dir(&root.join("media"))?;
        let mut store = Self {
            root,
            index,
            hashers: BTreeMap::new(),
            media: MediaCache::default(),
        };
        store.repair_files()?;
        store.persist()?;
        let _ = prune_media_objects(&store.media_root(), &BTreeSet::new(), MAX_MEDIA_CACHE_BYTES);
        Ok(store)
    }

    pub fn outbox(&self) -> &[OutboxEntry] {
        &self.index.outbox
    }
    pub fn inbox(&self) -> &[InboxEntry] {
        &self.index.inbox
    }

    pub fn object_path(&self, id: [u8; 16]) -> PathBuf {
        self.root
            .join("outbox")
            .join(format!("{}.object", id_hex(id)))
    }

    pub fn partial_path(&self, id: [u8; 16]) -> PathBuf {
        self.root.join("inbox").join(format!("{}.part", id_hex(id)))
    }

    /// Private root of verified media-handoff objects, `<hash>/<name>` beneath.
    pub fn media_root(&self) -> PathBuf {
        self.root.join("media")
    }

    /// Stage a source under a new random identity.
    ///
    /// # Errors
    /// Returns an error for unsafe sources, quota exhaustion, or failed persistence.
    pub fn stage(
        &mut self,
        source: &Path,
        target: String,
        now_unix_ms: u64,
    ) -> Result<OutboxEntry, String> {
        let id = loop {
            let candidate = random_id();
            if !self.index.outbox.iter().any(|entry| entry.id == candidate)
                && !self.index.inbox.iter().any(|entry| entry.id == candidate)
                && !self.object_path(candidate).exists()
                && !self.partial_path(candidate).exists()
            {
                break candidate;
            }
        };
        self.stage_with_id(source, target, now_unix_ms, id)
    }

    /// Stage a source under a caller-issued identity.
    ///
    /// # Errors
    /// Returns an error for an invalid/in-use ID, unsafe source, quota, or I/O failure.
    pub fn stage_with_id(
        &mut self,
        source: &Path,
        target: String,
        now_unix_ms: u64,
        id: [u8; 16],
    ) -> Result<OutboxEntry, String> {
        let name = portable_name(source)?;
        self.stage_path(
            source,
            Staging {
                id,
                name,
                target,
                now_unix_ms,
                media: false,
            },
        )
    }

    /// Stage the regular file behind a media handoff under a caller-issued
    /// identity and portable display name.
    ///
    /// # Errors
    /// Returns an error for an invalid/in-use ID or name, a source that is not
    /// one regular file, quota or free-space exhaustion, or I/O failure.
    pub fn stage_media_with_id(
        &mut self,
        source: &Path,
        name: String,
        target: String,
        now_unix_ms: u64,
        id: [u8; 16],
    ) -> Result<OutboxEntry, String> {
        self.stage_path(
            source,
            Staging {
                id,
                name,
                target,
                now_unix_ms,
                media: true,
            },
        )
    }

    fn stage_path(&mut self, source: &Path, staging: Staging) -> Result<OutboxEntry, String> {
        let Staging {
            id,
            name,
            target,
            now_unix_ms,
            media,
        } = staging;
        self.ensure_capacity(0)?;
        if !super::device_fabric::valid_nearby_file_name(&name) {
            return Err(crate::shell::i18n::t("nearby-error-name-not-portable"));
        }
        if id == [0; 16]
            || self.index.outbox.iter().any(|entry| entry.id == id)
            || self.index.inbox.iter().any(|entry| entry.id == id)
            || self.object_path(id).exists()
            || self.partial_path(id).exists()
        {
            return Err("nearby transfer id is already in use".into());
        }
        let object = self.object_path(id);
        let metadata = fs::symlink_metadata(source).map_err(display)?;
        let staged = if metadata.file_type().is_file() {
            if metadata.len() == 0 || metadata.len() > MAX_TRANSFER_BYTES {
                return Err(crate::shell::i18n::t("nearby-error-too-large"));
            }
            self.ensure_capacity(metadata.len())?;
            self.ensure_free_space(metadata.len())?;
            let kind = if media {
                TransferKind::MediaHandoff
            } else {
                TransferKind::File
            };
            copy_regular(source, &object).map(|()| kind)
        } else if media {
            return Err(crate::shell::i18n::t("nearby-error-not-regular"));
        } else if metadata.file_type().is_dir() {
            archive_folder(source, &object).map(|()| TransferKind::FolderArchive)
        } else {
            return Err("nearby transfer source must be a regular file or folder".into());
        };
        let kind = match staged {
            Ok(kind) => kind,
            Err(error) => {
                let _ = fs::remove_file(&object);
                return Err(error);
            }
        };
        let (total_bytes, content_hash) = hash_file(&object)?;
        if total_bytes == 0 || total_bytes > MAX_TRANSFER_BYTES {
            let _ = fs::remove_file(&object);
            return Err(crate::shell::i18n::t("nearby-error-too-large"));
        }
        if let Err(error) = self.ensure_capacity(total_bytes) {
            let _ = fs::remove_file(&object);
            return Err(error);
        }
        let entry = OutboxEntry {
            id,
            target,
            name,
            kind,
            total_bytes,
            content_hash,
            created_unix_ms: now_unix_ms,
            expires_unix_ms: now_unix_ms.saturating_add(TRANSFER_LIFETIME_MS),
        };
        self.index.outbox.push(entry.clone());
        if let Err(error) = self.persist() {
            self.index.outbox.pop();
            let _ = fs::remove_file(object);
            return Err(error);
        }
        Ok(entry)
    }

    /// Stage from an already-open Files grant capability. No app-controlled
    /// absolute path is reopened, including on the deferred worker path.
    ///
    /// # Errors
    /// Returns an error for an invalid/in-use ID, unsafe directory contents,
    /// quota exhaustion, or I/O failure.
    pub fn stage_secure_with_id(
        &mut self,
        source: SecureNearbySource,
        target: String,
        now_unix_ms: u64,
        id: [u8; 16],
    ) -> Result<OutboxEntry, String> {
        self.ensure_capacity(0)?;
        if id == [0; 16]
            || self.index.outbox.iter().any(|entry| entry.id == id)
            || self.index.inbox.iter().any(|entry| entry.id == id)
            || self.object_path(id).exists()
            || self.partial_path(id).exists()
        {
            return Err("nearby transfer id is already in use".into());
        }
        let metadata = source.metadata()?;
        let object = self.object_path(id);
        let name = source.name().to_string();
        let staged = if metadata.is_file() {
            if metadata.len() == 0 || metadata.len() > MAX_TRANSFER_BYTES {
                return Err(crate::shell::i18n::t("nearby-error-too-large"));
            }
            self.ensure_capacity(metadata.len())?;
            self.ensure_free_space(metadata.len())?;
            copy_open_regular(source.into_file(), &object).map(|()| TransferKind::File)
        } else if metadata.is_dir() {
            archive_open_folder(&source.into_file(), &object).map(|()| TransferKind::FolderArchive)
        } else {
            return Err("nearby transfer source must be a regular file or folder".into());
        };
        let kind = match staged {
            Ok(kind) => kind,
            Err(error) => {
                let _ = fs::remove_file(&object);
                return Err(error);
            }
        };
        let (total_bytes, content_hash) = hash_file(&object)?;
        if total_bytes == 0 || total_bytes > MAX_TRANSFER_BYTES {
            let _ = fs::remove_file(&object);
            return Err(crate::shell::i18n::t("nearby-error-too-large"));
        }
        if let Err(error) = self.ensure_capacity(total_bytes) {
            let _ = fs::remove_file(&object);
            return Err(error);
        }
        let entry = OutboxEntry {
            id,
            target,
            name,
            kind,
            total_bytes,
            content_hash,
            created_unix_ms: now_unix_ms,
            expires_unix_ms: now_unix_ms.saturating_add(TRANSFER_LIFETIME_MS),
        };
        self.index.outbox.push(entry.clone());
        if let Err(error) = self.persist() {
            self.index.outbox.pop();
            let _ = fs::remove_file(object);
            return Err(error);
        }
        Ok(entry)
    }

    /// Create or resume one exact-source, exact-request incoming object.
    ///
    /// # Errors
    /// Returns an error for invalid/mismatched manifests, quota, or durable I/O failure.
    pub fn begin_incoming(
        &mut self,
        manifest: IncomingManifest,
        now_unix_ms: u64,
    ) -> Result<(u64, [u8; 32]), String> {
        let IncomingManifest {
            id,
            source,
            request_id,
            name,
            kind,
            total_bytes,
            content_hash,
        } = manifest;
        validate_manifest(id, &source, &name, total_bytes)?;
        if let Some(existing) = self.index.inbox.iter().find(|entry| entry.id == id) {
            if existing.source != source
                || existing.request_id != request_id
                || existing.name != name
                || existing.kind != kind
                || existing.total_bytes != total_bytes
                || existing.content_hash != content_hash
            {
                return Err("resume manifest does not match the staged transfer".into());
            }
            let committed = existing.committed_offset;
            let hash = self.seed_hasher(id, committed)?;
            return Ok((committed, hash));
        }
        self.ensure_capacity(total_bytes)?;
        if kind == TransferKind::MediaHandoff {
            self.reserve_media_cache(total_bytes)?;
        }
        self.ensure_free_space(total_bytes)?;
        let part = self.partial_path(id);
        create_private_new(&part)?.sync_all().map_err(display)?;
        self.hashers.insert(id, (0, blake3::Hasher::new()));
        self.index.inbox.push(InboxEntry {
            id,
            source,
            request_id,
            name,
            kind,
            total_bytes,
            content_hash,
            committed_offset: 0,
            created_unix_ms: now_unix_ms,
            expires_unix_ms: now_unix_ms.saturating_add(TRANSFER_LIFETIME_MS),
        });
        self.persist()?;
        Ok((0, *blake3::hash(&[]).as_bytes()))
    }

    /// Accept a fresh, explicitly consented offer for the same immutable object.
    /// A sender restart/retry changes the request ID, not the certified source
    /// or manifest. Ordinary `begin_incoming` remains exact-request bound.
    ///
    /// # Errors
    /// Returns an error for a changed source/manifest or durable I/O failure.
    pub(crate) fn accept_incoming_offer(
        &mut self,
        manifest: IncomingManifest,
        now_unix_ms: u64,
    ) -> Result<(u64, [u8; 32]), String> {
        let Some(existing) = self
            .index
            .inbox
            .iter()
            .find(|entry| entry.id == manifest.id)
            .cloned()
        else {
            return self.begin_incoming(manifest, now_unix_ms);
        };
        if manifest.request_id == [0; 16] || !same_incoming_object(&existing, &manifest) {
            return Err("resume manifest does not match the staged transfer".into());
        }
        let path = self.partial_path(existing.id);
        let partial = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(display)?;
        partial
            .set_len(existing.committed_offset)
            .map_err(display)?;
        partial.sync_data().map_err(display)?;
        let hash = self.seed_hasher(existing.id, existing.committed_offset)?;
        if let Some(entry) = self
            .index
            .inbox
            .iter_mut()
            .find(|entry| entry.id == existing.id)
        {
            entry.request_id = manifest.request_id;
        }
        if let Err(error) = self.persist() {
            if let Some(entry) = self
                .index
                .inbox
                .iter_mut()
                .find(|entry| entry.id == existing.id)
            {
                entry.request_id = existing.request_id;
            }
            return Err(error);
        }
        Ok((existing.committed_offset, hash))
    }

    /// Discard a retained checkpoint when its freshly re-offered object is
    /// declined. A different peer or manifest cannot discard the old receive.
    ///
    /// # Errors
    /// Returns an error when matching retained bytes cannot be removed.
    pub(crate) fn decline_incoming_offer(
        &mut self,
        manifest: &IncomingManifest,
    ) -> Result<bool, String> {
        let Some(existing) = self
            .index
            .inbox
            .iter()
            .find(|entry| entry.id == manifest.id)
        else {
            return Ok(false);
        };
        if !same_incoming_object(existing, manifest) {
            return Err("resume manifest does not match the staged transfer".into());
        }
        self.cancel_incoming(manifest.id)?;
        Ok(true)
    }

    /// Append one in-order chunk and durably checkpoint bounded prefixes.
    ///
    /// # Errors
    /// Returns an error for an unknown ID, invalid ordering/size, or failed I/O.
    pub fn append(&mut self, id: [u8; 16], offset: u64, bytes: &[u8]) -> Result<u64, String> {
        let entry = self
            .index
            .inbox
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(|| "unknown nearby transfer".to_string())?;
        if bytes.is_empty()
            || offset > entry.total_bytes
            || u64::try_from(bytes.len())
                .map_err(display)?
                .saturating_add(offset)
                > entry.total_bytes
        {
            return Err("invalid nearby transfer chunk".into());
        }
        let path = self.partial_path(id);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(display)?;
        if file.metadata().map_err(display)?.len() != offset {
            return Err("nearby transfer chunk is out of order".into());
        }
        file.seek(SeekFrom::Start(offset)).map_err(display)?;
        if let Err(error) = file.write_all(bytes) {
            self.hashers.remove(&id);
            return Err(error.to_string());
        }
        let next = offset.saturating_add(u64::try_from(bytes.len()).map_err(display)?);
        match self.hashers.get_mut(&id) {
            Some((covered, running)) if *covered == offset => {
                running.update(bytes);
                *covered = next;
            }
            _ => {
                self.hashers.remove(&id);
            }
        }
        if next == entry.total_bytes
            || next.saturating_sub(entry.committed_offset) >= CHECKPOINT_BYTES
        {
            file.sync_data().map_err(display)?;
            if let Some(stored) = self.index.inbox.iter_mut().find(|stored| stored.id == id) {
                stored.committed_offset = next;
            }
            self.persist()?;
        }
        Ok(next)
    }

    /// Verify and collision-safely install a completed object.
    ///
    /// # Errors
    /// Returns an error on integrity failure, unsafe extraction, collision, or I/O failure.
    pub fn finish(&mut self, id: [u8; 16], downloads: &Path) -> Result<PathBuf, String> {
        let entry = self
            .index
            .inbox
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(|| "unknown nearby transfer".to_string())?;
        if entry.kind == TransferKind::MediaHandoff {
            return Err(MEDIA_CACHE_ONLY.into());
        }
        let part = self.partial_path(id);
        self.verify_incoming(&entry)?;
        ensure_private_dir(downloads)?;
        let installed = match entry.kind {
            TransferKind::File => install_file_create_new(&part, downloads, &entry.name)?,
            TransferKind::FolderArchive => {
                extract_archive_create_new(&part, downloads, &entry.name)?
            }
            TransferKind::MediaHandoff => return Err(MEDIA_CACHE_ONLY.into()),
        };
        self.cancel_incoming(id)?;
        Ok(installed)
    }

    /// Verify a completed media handoff and move it into the private media
    /// cache, bound to its exact sender and transfer. The object never enters
    /// Downloads or a Files destination.
    ///
    /// # Errors
    /// Returns an error for an unknown or non-media transfer, an integrity
    /// failure (the transfer is then discarded), or failed I/O.
    pub fn finish_to_cache(&mut self, id: [u8; 16]) -> Result<PathBuf, String> {
        let entry = self
            .index
            .inbox
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(|| "unknown nearby transfer".to_string())?;
        if entry.kind != TransferKind::MediaHandoff {
            return Err("only a media handoff is delivered to the media cache".into());
        }
        self.verify_incoming(&entry)?;
        let object_dir = self.media_root().join(hex::encode(entry.content_hash));
        let installed = ensure_private_dir(&object_dir).and_then(|()| {
            let object = object_dir.join(&entry.name);
            // The directory names the verified content, so replacing an
            // existing copy only swaps identical bytes; a player holding the
            // old descriptor keeps reading its inode.
            fs::rename(self.partial_path(id), &object)
                .map(|()| object)
                .map_err(display)
        });
        let object = match installed {
            Ok(object) => object,
            Err(error) => {
                self.cancel_incoming(id)?;
                return Err(error);
            }
        };
        self.cancel_incoming(id)?;
        self.media.bind((entry.source, id), object.clone());
        Ok(object)
    }

    /// Resolve one verified media handoff from this exact sender to its cached
    /// object and lease it to the player that is about to open it.
    pub fn cached_media(&mut self, source: &str, id: [u8; 16]) -> Option<PathBuf> {
        let path = self
            .media
            .completed
            .get(&(source.to_string(), id))
            .cloned()?;
        let now = std::time::Instant::now();
        self.media
            .leased_until
            .insert(path.clone(), now.checked_add(MEDIA_LEASE).unwrap_or(now));
        Some(path)
    }

    /// Revoke one exact sender/transfer media binding and any partial bytes it
    /// created. A verified object stays while another binding or a live player
    /// lease still references it.
    ///
    /// # Errors
    /// Returns an error when partial bytes cannot be removed.
    pub fn release_media(&mut self, source: &str, id: [u8; 16]) -> Result<bool, String> {
        let incoming = self.index.inbox.iter().any(|entry| {
            entry.id == id && entry.source == source && entry.kind == TransferKind::MediaHandoff
        });
        if incoming {
            self.cancel_incoming(id)?;
        }
        Ok(self.media.unbind(&(source.to_string(), id)) || incoming)
    }

    /// Verify and install through an already-open destination capability.
    ///
    /// # Errors
    ///
    /// Returns a message when the transfer is unknown, its staged bytes cannot
    /// be read, fail the integrity check (the transfer is then discarded), or
    /// cannot be installed into `destination`.
    pub fn finish_secure(
        &mut self,
        id: [u8; 16],
        destination: &SecureNearbyDestination,
    ) -> Result<PathBuf, String> {
        let entry = self
            .index
            .inbox
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(|| "unknown nearby transfer".to_string())?;
        if entry.kind == TransferKind::MediaHandoff {
            return Err(MEDIA_CACHE_ONLY.into());
        }
        let part = self.partial_path(id);
        self.verify_incoming(&entry)?;
        #[cfg(target_os = "macos")]
        {
            let installed = match entry.kind {
                TransferKind::File => {
                    let mut input = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&part)
                        .map_err(display)?;
                    install_capability_file(&destination.directory, &entry.name, &mut input)?
                }
                TransferKind::FolderArchive => {
                    extract_capability_archive(&part, &destination.directory, &entry.name)?
                }
                TransferKind::MediaHandoff => return Err(MEDIA_CACHE_ONLY.into()),
            };
            self.cancel_incoming(id)?;
            return Ok(destination.display_root.join(installed));
        }
        #[cfg(not(target_os = "macos"))]
        {
            let root = destination.anchored_root();
            let installed = match entry.kind {
                TransferKind::File => install_file_create_new(&part, &root, &entry.name)?,
                TransferKind::FolderArchive => {
                    extract_archive_create_new(&part, &root, &entry.name)?
                }
                TransferKind::MediaHandoff => return Err(MEDIA_CACHE_ONLY.into()),
            };
            self.cancel_incoming(id)?;
            Ok(destination.display_root.join(
                installed
                    .file_name()
                    .ok_or_else(|| crate::shell::i18n::t("nearby-error-destination-name"))?,
            ))
        }
    }

    /// Remove outgoing metadata and staged bytes.
    ///
    /// # Errors
    /// Returns an error when cleanup cannot be persisted.
    pub fn cancel_outgoing(&mut self, id: [u8; 16]) -> Result<(), String> {
        self.index.outbox.retain(|entry| entry.id != id);
        remove_if_exists(&self.object_path(id))?;
        self.persist()
    }

    /// Truncate a mismatched incoming prefix to zero.
    ///
    /// # Errors
    /// Returns an error for an unknown ID or failed truncation/persistence.
    pub fn restart_incoming(&mut self, id: [u8; 16]) -> Result<[u8; 32], String> {
        let entry = self
            .index
            .inbox
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| "unknown nearby transfer".to_string())?;
        let path = self.root.join("inbox").join(format!("{}.part", id_hex(id)));
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(display)?;
        file.set_len(0).map_err(display)?;
        file.sync_all().map_err(display)?;
        entry.committed_offset = 0;
        self.hashers.insert(id, (0, blake3::Hasher::new()));
        self.persist()?;
        Ok(*blake3::hash(&[]).as_bytes())
    }

    /// Remove incoming metadata and partial bytes.
    ///
    /// # Errors
    /// Returns an error when cleanup cannot be persisted.
    pub fn cancel_incoming(&mut self, id: [u8; 16]) -> Result<(), String> {
        self.index.inbox.retain(|entry| entry.id != id);
        self.hashers.remove(&id);
        remove_if_exists(&self.partial_path(id))?;
        self.persist()
    }

    /// Hash the committed prefix of `id` and, when nothing follows it on disk,
    /// keep the running hash so the finish need not re-read those bytes.
    fn seed_hasher(&mut self, id: [u8; 16], committed: u64) -> Result<[u8; 32], String> {
        let path = self.partial_path(id);
        let hasher = prefix_hasher(&path, committed)?;
        let hash = *hasher.finalize().as_bytes();
        let on_disk = fs::symlink_metadata(&path).map_err(display)?.len();
        if on_disk == committed {
            self.hashers.insert(id, (committed, hasher));
        } else {
            self.hashers.remove(&id);
        }
        Ok(hash)
    }

    /// Check a completed incoming object's size and blake3 hash before any
    /// delivery. A mismatch discards the untrusted bytes.
    fn verify_incoming(&mut self, entry: &InboxEntry) -> Result<(), String> {
        let part = self.partial_path(entry.id);
        let on_disk = fs::symlink_metadata(&part).map_err(display)?.len();
        let (len, hash) = match self.hashers.remove(&entry.id) {
            Some((covered, running)) if covered == on_disk => {
                (covered, *running.finalize().as_bytes())
            }
            _ => hash_file(&part)?,
        };
        if len != entry.total_bytes || hash != entry.content_hash {
            self.cancel_incoming(entry.id)?;
            return Err("nearby transfer integrity check failed".into());
        }
        Ok(())
    }

    /// Make room in the media cache for one more incoming object, evicting the
    /// oldest objects no binding or player lease protects.
    fn reserve_media_cache(&mut self, candidate_bytes: u64) -> Result<(), String> {
        let reserved = self
            .index
            .inbox
            .iter()
            .filter(|entry| entry.kind == TransferKind::MediaHandoff)
            .fold(0_u64, |total, entry| {
                total.saturating_add(entry.total_bytes)
            });
        let budget = MAX_MEDIA_CACHE_BYTES
            .checked_sub(reserved)
            .and_then(|remaining| remaining.checked_sub(candidate_bytes))
            .ok_or_else(|| crate::shell::i18n::t("nearby-error-quota-full"))?;
        let protected = self.media.protected_paths();
        if prune_media_objects(&self.media_root(), &protected, budget) > budget {
            return Err(crate::shell::i18n::t("nearby-error-quota-full"));
        }
        Ok(())
    }

    /// Refuse an object whose bytes, with every unfinished receive, would
    /// leave less than [`MIN_FREE_BYTES`] on the spool's filesystem.
    fn ensure_free_space(&self, candidate_bytes: u64) -> Result<(), String> {
        let pending = self.index.inbox.iter().fold(0_u64, |total, entry| {
            total.saturating_add(entry.total_bytes.saturating_sub(entry.committed_offset))
        });
        let required = pending
            .saturating_add(candidate_bytes)
            .saturating_add(MIN_FREE_BYTES);
        if available_bytes(&self.root).is_none_or(|available| available < required) {
            return Err(crate::shell::i18n::t("nearby-error-disk-full"));
        }
        Ok(())
    }

    fn ensure_capacity(&self, candidate_bytes: u64) -> Result<(), String> {
        if self
            .index
            .outbox
            .len()
            .saturating_add(self.index.inbox.len())
            >= MAX_TRANSFERS
        {
            return Err(crate::shell::i18n::t("nearby-error-queue-full"));
        }
        let bytes = self
            .index
            .outbox
            .iter()
            .map(|e| e.total_bytes)
            .sum::<u64>()
            .checked_add(self.index.inbox.iter().map(|e| e.total_bytes).sum())
            .and_then(|bytes| bytes.checked_add(candidate_bytes))
            .ok_or_else(|| crate::shell::i18n::t("nearby-error-quota-full"))?;
        if bytes > MAX_TOTAL_BYTES {
            return Err(crate::shell::i18n::t("nearby-error-quota-full"));
        }
        Ok(())
    }

    fn repair_files(&mut self) -> Result<(), String> {
        let outbox_root = self.root.join("outbox");
        self.index.outbox.retain(|entry| {
            fs::symlink_metadata(outbox_root.join(format!("{}.object", id_hex(entry.id))))
                .is_ok_and(|m| m.file_type().is_file() && m.len() == entry.total_bytes)
        });
        let out_ids = self
            .index
            .outbox
            .iter()
            .map(|e| id_hex(e.id))
            .collect::<BTreeSet<_>>();
        let root = self.root.clone();
        clean_orphans(&root.join("outbox"), &out_ids, ".object")?;
        let inbox_root = self.root.join("inbox");
        self.index.inbox.retain(|entry| {
            fs::symlink_metadata(inbox_root.join(format!("{}.part", id_hex(entry.id)))).is_ok_and(
                |metadata| {
                    metadata.file_type().is_file() && metadata.len() >= entry.committed_offset
                },
            )
        });
        for entry in &self.index.inbox {
            let part = self.partial_path(entry.id);
            let file = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&part)
                .map_err(display)?;
            if file.metadata().map_err(display)?.len() < entry.committed_offset {
                return Err("nearby transfer checkpoint exceeds partial file".into());
            }
            file.set_len(entry.committed_offset).map_err(display)?;
        }
        let in_ids = self
            .index
            .inbox
            .iter()
            .map(|e| id_hex(e.id))
            .collect::<BTreeSet<_>>();
        clean_orphans(&root.join("inbox"), &in_ids, ".part")
    }

    fn persist(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec(&self.index).map_err(display)?;
        if bytes.len() > 256 * 1024 {
            return Err("nearby transfer index is too large".into());
        }
        tilo_files::native::atomic::write_with_mode(&self.root.join("index.json"), &bytes, 0o600)
            .map_err(display)
    }
}

const MEDIA_CACHE_ONLY: &str = "a media handoff is delivered only to the private media cache";

#[derive(Debug)]
struct MediaCacheObject {
    path: PathBuf,
    bytes: u64,
    modified: std::time::SystemTime,
}

/// Regular files directly beneath `<media>/<hash>/`. Symlinks and anything
/// else are never followed or counted.
fn media_cache_objects(root: &Path) -> Vec<MediaCacheObject> {
    let mut objects = Vec::new();
    let Ok(hash_dirs) = fs::read_dir(root) else {
        return objects;
    };
    for hash_dir in hash_dirs.flatten() {
        let Ok(hash_metadata) = fs::symlink_metadata(hash_dir.path()) else {
            continue;
        };
        if !hash_metadata.file_type().is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(hash_dir.path()) else {
            continue;
        };
        for file in files.flatten() {
            let Ok(metadata) = fs::symlink_metadata(file.path()) else {
                continue;
            };
            if metadata.file_type().is_file() {
                objects.push(MediaCacheObject {
                    path: file.path(),
                    bytes: metadata.len(),
                    modified: metadata
                        .modified()
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                });
            }
        }
    }
    objects
}

/// Remove the oldest unprotected media objects until at most `target_bytes`
/// remain, and return what remains.
fn prune_media_objects(root: &Path, protected: &BTreeSet<PathBuf>, target_bytes: u64) -> u64 {
    let mut objects = media_cache_objects(root);
    let mut retained = objects
        .iter()
        .fold(0_u64, |total, object| total.saturating_add(object.bytes));
    objects.sort_by_key(|object| object.modified);
    for object in objects {
        if retained <= target_bytes {
            break;
        }
        if protected.contains(&object.path) || fs::remove_file(&object.path).is_err() {
            continue;
        }
        retained = retained.saturating_sub(object.bytes);
        if let Some(parent) = object.path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
    retained
}

#[allow(
    clippy::unnecessary_cast,
    reason = "statvfs block counts are u64 on Linux but narrower on other Unix targets"
)]
fn available_bytes(path: &Path) -> Option<u64> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `info` is valid for one write.
    if unsafe { libc::statvfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs returned success, so it initialised `info`.
    let info = unsafe { info.assume_init() };
    (info.f_bavail as u64).checked_mul(info.f_frsize as u64)
}

fn same_incoming_object(entry: &InboxEntry, manifest: &IncomingManifest) -> bool {
    entry.id == manifest.id
        && entry.source == manifest.source
        && entry.name == manifest.name
        && entry.kind == manifest.kind
        && entry.total_bytes == manifest.total_bytes
        && entry.content_hash == manifest.content_hash
}

fn validate_index(index: &StoreIndex) -> Result<(), String> {
    if index.outbox.len().saturating_add(index.inbox.len()) > MAX_TRANSFERS {
        return Err(crate::shell::i18n::t("nearby-error-too-many"));
    }
    let mut ids = BTreeSet::new();
    let mut claimed_bytes = 0_u64;
    for entry in &index.outbox {
        validate_manifest(entry.id, &entry.target, &entry.name, entry.total_bytes)?;
        claimed_bytes = claimed_bytes
            .checked_add(entry.total_bytes)
            .ok_or_else(|| "nearby transfer quota is invalid".to_string())?;
        if !ids.insert(entry.id) {
            return Err("duplicate nearby transfer id".into());
        }
    }
    for entry in &index.inbox {
        validate_manifest(entry.id, &entry.source, &entry.name, entry.total_bytes)?;
        claimed_bytes = claimed_bytes
            .checked_add(entry.total_bytes)
            .ok_or_else(|| "nearby transfer quota is invalid".to_string())?;
        if entry.request_id == [0; 16]
            || entry.committed_offset > entry.total_bytes
            || !ids.insert(entry.id)
        {
            return Err("invalid nearby inbox entry".into());
        }
    }
    if claimed_bytes > MAX_TOTAL_BYTES {
        return Err("nearby transfer quota is invalid".into());
    }
    Ok(())
}

fn validate_manifest(id: [u8; 16], peer: &str, name: &str, total: u64) -> Result<(), String> {
    if id == [0; 16]
        || peer.is_empty()
        || peer.len() > 256
        || !super::device_fabric::valid_nearby_file_name(name)
        || total == 0
        || total > MAX_TRANSFER_BYTES
    {
        return Err("invalid nearby transfer manifest".into());
    }
    Ok(())
}

fn copy_regular(source: &Path, destination: &Path) -> Result<(), String> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)
        .map_err(display)?;
    if !input.metadata().map_err(display)?.is_file() {
        return Err(crate::shell::i18n::t("nearby-error-not-regular"));
    }
    let mut output = create_private_new(destination)?;
    io::copy(&mut input, &mut output).map_err(display)?;
    output.sync_all().map_err(display)
}

fn archive_folder(source: &Path, destination: &Path) -> Result<(), String> {
    let output = create_private_new(destination)?;
    let mut archive = tar::Builder::new(output);
    archive.mode(tar::HeaderMode::Deterministic);
    let mut entries = walkdir::WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(display)?;
    entries.sort_by(|a, b| {
        a.path()
            .strip_prefix(source)
            .unwrap_or_else(|_| a.path())
            .cmp(b.path().strip_prefix(source).unwrap_or_else(|_| b.path()))
    });
    if entries.len() > MAX_FOLDER_ENTRIES {
        return Err(crate::shell::i18n::t("nearby-error-folder-too-many"));
    }
    let mut source_bytes = 0_u64;
    for entry in entries.into_iter().skip(1) {
        let relative = entry.path().strip_prefix(source).map_err(display)?;
        validate_relative(relative)?;
        let file_type = entry.file_type();
        if file_type.is_symlink() || (!file_type.is_dir() && !file_type.is_file()) {
            return Err(crate::shell::i18n::t("nearby-error-folder-special"));
        }
        let mut header = tar::Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_mode(if file_type.is_dir() { 0o700 } else { 0o600 });
        if file_type.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_cksum();
            archive
                .append_data(&mut header, relative, io::empty())
                .map_err(display)?;
        } else {
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(entry.path())
                .map_err(display)?;
            let size = file.metadata().map_err(display)?.len();
            source_bytes = source_bytes
                .checked_add(size)
                .ok_or_else(|| crate::shell::i18n::t("nearby-error-folder-too-large"))?;
            if source_bytes > MAX_TRANSFER_BYTES {
                return Err(crate::shell::i18n::t("nearby-error-folder-too-large"));
            }
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(size);
            header.set_cksum();
            archive
                .append_data(&mut header, relative, &mut file)
                .map_err(display)?;
        }
    }
    let output = archive.into_inner().map_err(display)?;
    output.sync_all().map_err(display)
}

fn copy_open_regular(mut source: File, destination: &Path) -> Result<(), String> {
    source.seek(SeekFrom::Start(0)).map_err(display)?;
    let metadata = source.metadata().map_err(display)?;
    if !metadata.is_file() {
        return Err("nearby transfer source changed type".into());
    }
    let mut output = create_private_new(destination)?;
    let copied = io::copy(&mut source, &mut output).map_err(display)?;
    if copied != metadata.len() {
        return Err(crate::shell::i18n::t("nearby-error-source-changed"));
    }
    output.sync_all().map_err(display)
}

fn archive_open_folder(source: &File, destination: &Path) -> Result<(), String> {
    if !source.metadata().map_err(display)?.is_dir() {
        return Err("nearby transfer source changed type".into());
    }
    #[cfg(target_os = "macos")]
    let entries = capability_entries(&source)?;
    #[cfg(not(target_os = "macos"))]
    let entries = {
        let root = PathBuf::from(format!("/proc/self/fd/{}", source.as_raw_fd()));
        let mut entries = walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .skip(1)
            .map(|entry| {
                let entry = entry.map_err(display)?;
                let relative = entry
                    .path()
                    .strip_prefix(&root)
                    .map_err(display)?
                    .to_owned();
                Ok((relative, entry.file_type().is_dir()))
            })
            .collect::<Result<Vec<_>, String>>()?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        if entries.len() >= MAX_FOLDER_ENTRIES {
            return Err(crate::shell::i18n::t("nearby-error-folder-too-many"));
        }
        entries
    };
    let output = create_private_new(destination)?;
    let mut archive = tar::Builder::new(output);
    archive.mode(tar::HeaderMode::Deterministic);
    let mut source_bytes = 0_u64;
    for (relative, is_directory) in entries {
        let relative = relative.as_path();
        validate_relative(relative)?;
        let mut opened = openat2_beneath(source, relative, is_directory)?;
        let metadata = opened.metadata().map_err(display)?;
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(crate::shell::i18n::t("nearby-error-folder-special"));
        }
        let mut header = tar::Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_mode(if metadata.is_dir() { 0o700 } else { 0o600 });
        if metadata.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_cksum();
            archive
                .append_data(&mut header, relative, io::empty())
                .map_err(display)?;
        } else {
            let size = metadata.len();
            source_bytes = source_bytes
                .checked_add(size)
                .ok_or_else(|| crate::shell::i18n::t("nearby-error-folder-too-large"))?;
            if source_bytes > MAX_TRANSFER_BYTES {
                return Err(crate::shell::i18n::t("nearby-error-folder-too-large"));
            }
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(size);
            header.set_cksum();
            archive
                .append_data(&mut header, relative, &mut opened)
                .map_err(display)?;
        }
    }
    let output = archive.into_inner().map_err(display)?;
    output.sync_all().map_err(display)
}

fn extract_archive_create_new(
    archive_path: &Path,
    root: &Path,
    name: &str,
) -> Result<PathBuf, String> {
    let destination = root.join(format!(".{}.partial", id_hex(random_id())));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&destination)
        .map_err(display)?;
    let result = (|| {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(archive_path)
            .map_err(display)?;
        let mut archive = tar::Archive::new(file);
        let mut count = 0usize;
        for item in archive.entries().map_err(display)? {
            count = count.saturating_add(1);
            if count > MAX_FOLDER_ENTRIES {
                return Err("archive contains too many entries".into());
            }
            let mut item = item.map_err(display)?;
            let relative = item.path().map_err(display)?.into_owned();
            validate_relative(&relative)?;
            let target = destination.join(&relative);
            let kind = item.header().entry_type();
            if kind.is_dir() {
                ensure_private_dir(&target)?;
            } else if kind.is_file() {
                if let Some(parent) = target.parent() {
                    ensure_private_dir(parent)?;
                }
                let mut output = create_private_new(&target)?;
                io::copy(&mut item, &mut output).map_err(display)?;
                output.sync_all().map_err(display)?;
            } else {
                return Err("archive contains a link or special entry".into());
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&destination);
        return Err(error);
    }
    for suffix in 0..1000 {
        let published = root.join(collision_name(name, suffix));
        match fs::DirBuilder::new().mode(0o700).create(&published) {
            Ok(()) => match fs::rename(&destination, &published) {
                Ok(()) => return Ok(published),
                Err(error) => {
                    // Only remove our still-empty reservation. If another
                    // process populated it, leave its data untouched.
                    let _ = fs::remove_dir(&published);
                    if error.kind() != io::ErrorKind::AlreadyExists
                        && error.raw_os_error() != Some(libc::ENOTEMPTY)
                    {
                        let _ = fs::remove_dir_all(&destination);
                        return Err(error.to_string());
                    }
                }
            },
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                let _ = fs::remove_dir_all(&destination);
                return Err(error.to_string());
            }
        }
    }
    let _ = fs::remove_dir_all(&destination);
    Err("too many nearby transfer name collisions".into())
}

fn install_file_create_new(source: &Path, root: &Path, name: &str) -> Result<PathBuf, String> {
    let destination = unique_destination(root, name)?;
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)
        .map_err(display)?;
    let mut output = create_private_new(&destination)?;
    io::copy(&mut input, &mut output).map_err(display)?;
    output.sync_all().map_err(display)?;
    Ok(destination)
}

fn install_bytes_create_new(root: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    if bytes.is_empty() || u64::try_from(bytes.len()).map_err(display)? > MAX_TRANSFER_BYTES {
        return Err(crate::shell::i18n::t("nearby-error-too-large"));
    }
    let destination = unique_destination(root, name)?;
    let mut output = create_private_new(&destination)?;
    output.write_all(bytes).map_err(display)?;
    output.sync_all().map_err(display)?;
    Ok(destination)
}

fn unique_destination(root: &Path, name: &str) -> Result<PathBuf, String> {
    for suffix in 0..1000 {
        let candidate = root.join(collision_name(name, suffix));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("too many nearby transfer name collisions".into())
}

fn collision_name(name: &str, suffix: usize) -> String {
    if suffix == 0 {
        return name.to_string();
    }
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|v| v.to_str()).unwrap_or("item");
    path.extension().and_then(|v| v.to_str()).map_or_else(
        || format!("{stem} ({suffix})"),
        |ext| format!("{stem} ({suffix}).{ext}"),
    )
}

fn portable_name(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| crate::shell::i18n::t("nearby-error-name-not-portable"))?
        .to_string();
    if !super::device_fabric::valid_nearby_file_name(&name) {
        return Err(crate::shell::i18n::t("nearby-error-name-not-portable"));
    }
    Ok(name)
}

fn validate_relative(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("archive path is unsafe".into());
    }
    Ok(())
}

fn prefix_hasher(path: &Path, limit: u64) -> Result<blake3::Hasher, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(display)?;
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut Read::by_ref(&mut file).take(limit), &mut hasher).map_err(display)?;
    Ok(hasher)
}

fn prefix_hash(path: &Path, limit: u64) -> Result<[u8; 32], String> {
    prefix_hasher(path, limit).map(|hasher| *hasher.finalize().as_bytes())
}

/// Hash a private spool object's prefix without following its final symlink.
///
/// # Errors
/// Returns an error when the object cannot be safely opened or read.
pub fn hash_prefix(path: &Path, limit: u64) -> Result<[u8; 32], String> {
    prefix_hash(path, limit)
}

fn hash_file(path: &Path) -> Result<(u64, [u8; 32]), String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(display)?;
    let mut hasher = blake3::Hasher::new();
    let bytes = io::copy(&mut file, &mut hasher).map_err(display)?;
    Ok((bytes, *hasher.finalize().as_bytes()))
}

fn create_private_new(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(display)
}

fn ensure_private_dir(path: &Path) -> Result<(), String> {
    if path.exists() {
        if !fs::symlink_metadata(path)
            .map_err(display)?
            .file_type()
            .is_dir()
        {
            return Err("nearby transfer directory is unsafe".into());
        }
    } else {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(display)?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(display)
}

fn clean_orphans(dir: &Path, ids: &BTreeSet<String>, suffix: &str) -> Result<(), String> {
    for item in fs::read_dir(dir).map_err(display)? {
        let item = item.map_err(display)?;
        let name = item.file_name().to_string_lossy().into_owned();
        if !name.strip_suffix(suffix).is_some_and(|id| ids.contains(id)) {
            remove_if_exists(&item.path())?;
        }
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn random_id() -> [u8; 16] {
    let mut id = [0; 16];
    UnwrapErr(SysRng).fill_bytes(&mut id);
    id
}
fn id_hex(id: [u8; 16]) -> String {
    hex::encode(id)
}
fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn media_manifest(id: [u8; 16], source: &str, bytes: &[u8]) -> IncomingManifest {
        IncomingManifest {
            id,
            source: source.into(),
            request_id: [0x5e; 16],
            name: "concert.webm".into(),
            kind: TransferKind::MediaHandoff,
            total_bytes: u64::try_from(bytes.len()).unwrap(),
            content_hash: *blake3::hash(bytes).as_bytes(),
        }
    }

    #[test]
    fn media_handoff_object_lands_in_private_cache_with_lease() {
        let tmp = TempDir::new().unwrap();
        let source_file = tmp.path().join("concert.webm");
        let bytes = vec![0x5a; 4096];
        fs::write(&source_file, &bytes).unwrap();
        let mut sender = NearbyTransferStore::open(tmp.path().join("sender"), 10).unwrap();
        let staged = sender
            .stage_media_with_id(
                &source_file,
                "concert.webm".into(),
                "receiver".into(),
                10,
                [0x41; 16],
            )
            .unwrap();
        assert_eq!(staged.kind, TransferKind::MediaHandoff);

        let downloads = tmp.path().join("Downloads");
        let mut receiver = NearbyTransferStore::open(tmp.path().join("receiver"), 10).unwrap();
        let manifest = IncomingManifest {
            id: staged.id,
            source: "sender".into(),
            request_id: [0x42; 16],
            name: staged.name,
            kind: staged.kind,
            total_bytes: staged.total_bytes,
            content_hash: staged.content_hash,
        };
        receiver.begin_incoming(manifest, 10).unwrap();
        receiver.append(staged.id, 0, &bytes).unwrap();
        assert!(
            receiver.finish(staged.id, &downloads).is_err(),
            "a media handoff never installs into Downloads"
        );
        let path = receiver.finish_to_cache(staged.id).unwrap();
        assert!(path.starts_with(receiver.media_root()));
        assert!(!downloads.exists());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(receiver.inbox().is_empty());
        assert!(!receiver.media.leased_until.contains_key(&path));
        let resolved = std::time::Instant::now();
        assert_eq!(
            receiver.cached_media("sender", staged.id),
            Some(path.clone())
        );
        assert!(
            receiver
                .media
                .leased_until
                .get(&path)
                .is_some_and(|until| until.saturating_duration_since(resolved) >= MEDIA_LEASE),
            "resolving the object leases it to the player"
        );
        assert_eq!(receiver.cached_media("other-sender", staged.id), None);
    }

    #[test]
    fn media_handoff_resumes_from_committed_offset() {
        let tmp = TempDir::new().unwrap();
        let half = usize::try_from(CHECKPOINT_BYTES).unwrap();
        let mut bytes = vec![0x11; half];
        bytes.extend(vec![0x22; half]);
        let manifest = media_manifest([0x43; 16], "sender", &bytes);
        let mut store = NearbyTransferStore::open(tmp.path().join("spool"), 10).unwrap();
        assert_eq!(
            store.begin_incoming(manifest.clone(), 10).unwrap(),
            (0, *blake3::hash(&[]).as_bytes())
        );
        store.append(manifest.id, 0, &bytes[..half]).unwrap();
        // The link drops; the sender re-offers the same object.
        let (offset, prefix) = store.begin_incoming(manifest.clone(), 11).unwrap();
        assert_eq!(offset, CHECKPOINT_BYTES);
        assert_eq!(prefix, *blake3::hash(&bytes[..half]).as_bytes());
        let mut retry = manifest.clone();
        retry.request_id = [0x44; 16];
        assert_eq!(
            store.accept_incoming_offer(retry, 12).unwrap(),
            (offset, prefix),
            "a re-offer resumes from the verified prefix, not byte zero"
        );
        store.append(manifest.id, offset, &bytes[half..]).unwrap();
        let path = store.finish_to_cache(manifest.id).unwrap();
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn media_handoff_is_session_local_and_dropped_on_reopen() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("spool");
        let source_file = tmp.path().join("film.mkv");
        fs::write(&source_file, b"film").unwrap();
        let mut store = NearbyTransferStore::open(root.clone(), 10).unwrap();
        let staged = store
            .stage_media_with_id(
                &source_file,
                "film.mkv".into(),
                "peer".into(),
                10,
                [0x45; 16],
            )
            .unwrap();
        let manifest = media_manifest([0x46; 16], "peer", b"partial bytes");
        store.begin_incoming(manifest.clone(), 10).unwrap();
        store.append(manifest.id, 0, b"partial").unwrap();
        let object = store.object_path(staged.id);
        let part = store.partial_path(manifest.id);
        drop(store);
        let reopened = NearbyTransferStore::open(root, 11).unwrap();
        assert!(reopened.outbox().is_empty());
        assert!(reopened.inbox().is_empty());
        assert!(!object.exists());
        assert!(!part.exists());
    }

    #[test]
    fn media_handoff_source_must_be_one_regular_file() {
        let tmp = TempDir::new().unwrap();
        let folder = tmp.path().join("album");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("track.flac"), b"audio").unwrap();
        let mut store = NearbyTransferStore::open(tmp.path().join("spool"), 10).unwrap();
        assert!(
            store
                .stage_media_with_id(&folder, "album".into(), "peer".into(), 10, [0x47; 16])
                .is_err()
        );
        assert!(store.outbox().is_empty());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn media_cache_prunes_leases_and_discards_only_unreferenced_objects() {
        fn media_cache_prunes_only_unprotected_regular_objects() {
            use std::os::unix::fs::symlink;

            let cache = TempDir::new().unwrap();
            let first_dir = cache.path().join("11".repeat(32));
            let second_dir = cache.path().join("22".repeat(32));
            ensure_private_dir(&first_dir).unwrap();
            ensure_private_dir(&second_dir).unwrap();
            let protected_path = first_dir.join("keep.mp4");
            let evictable_path = second_dir.join("old.mp4");
            fs::write(&protected_path, [1_u8; 4]).unwrap();
            fs::write(&evictable_path, [2_u8; 4]).unwrap();
            let outside = cache.path().join("outside");
            fs::write(&outside, b"do not follow").unwrap();
            symlink(&outside, second_dir.join("link.mp4")).unwrap();

            let retained =
                prune_media_objects(cache.path(), &BTreeSet::from([protected_path.clone()]), 4);
            assert_eq!(retained, 4);
            assert!(protected_path.exists());
            assert!(!evictable_path.exists());
            assert_eq!(fs::read(outside).unwrap(), b"do not follow");
        }

        fn player_lease_keeps_evicted_transfer_object_alive() {
            let cache = TempDir::new().unwrap();
            let object_dir = cache.path().join("33".repeat(32));
            ensure_private_dir(&object_dir).unwrap();
            let path = object_dir.join("film.mkv");
            fs::write(&path, b"film").unwrap();
            let key = ("source".to_string(), [0x55; 16]);
            let mut media = MediaCache::default();
            media.completed.insert(key.clone(), path.clone());
            media.completed_order.push_back(key);
            let now = std::time::Instant::now();
            media
                .leased_until
                .insert(path.clone(), now.checked_add(MEDIA_LEASE).unwrap_or(now));

            assert!(media.evict_oldest_completed());
            assert!(path.exists());
            assert!(media.completed.is_empty());
        }

        fn media_directories_are_owner_only() {
            let tmp = TempDir::new().unwrap();
            let store = NearbyTransferStore::open(tmp.path().join("spool"), 10).unwrap();
            assert_eq!(
                fs::metadata(store.media_root())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            let private = tmp.path().join("private");
            ensure_private_dir(&private).unwrap();
            assert_eq!(
                fs::metadata(private).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }

        fn media_release_is_exact_and_preserves_referenced_objects() {
            let tmp = TempDir::new().unwrap();
            let mut store = NearbyTransferStore::open(tmp.path().join("spool"), 10).unwrap();
            let source = "source-device";
            let sibling = "sibling-device";
            let transfer_id = [0x73; 16];
            let manifest = media_manifest(transfer_id, source, b"partial!");
            store.begin_incoming(manifest, 10).unwrap();
            let part_path = store.partial_path(transfer_id);
            assert!(part_path.exists());
            assert!(
                !store.release_media(sibling, transfer_id).unwrap(),
                "another sender cannot discard this receive"
            );
            assert!(part_path.exists());
            assert!(store.release_media(source, transfer_id).unwrap());
            assert!(!part_path.exists());
            assert!(store.inbox().is_empty());

            let object_dir = store.media_root().join("44".repeat(32));
            ensure_private_dir(&object_dir).unwrap();
            let object = object_dir.join("shared.webm");
            fs::write(&object, b"verified").unwrap();
            store
                .media
                .bind((source.to_string(), transfer_id), object.clone());
            store
                .media
                .bind((sibling.to_string(), transfer_id), object.clone());
            let now = std::time::Instant::now();
            store
                .media
                .leased_until
                .insert(object.clone(), now.checked_add(MEDIA_LEASE).unwrap());
            assert!(store.release_media(source, transfer_id).unwrap());
            assert!(
                object.exists(),
                "sibling mapping and player lease protect bytes"
            );
            assert!(store.cached_media(source, transfer_id).is_none());
            assert!(store.cached_media(sibling, transfer_id).is_some());
            store.media.leased_until.clear();
            assert!(store.release_media(sibling, transfer_id).unwrap());
            assert!(!object.exists(), "an unreferenced object is removed");
        }

        media_cache_prunes_only_unprotected_regular_objects();
        player_lease_keeps_evicted_transfer_object_alive();
        media_directories_are_owner_only();
        media_release_is_exact_and_preserves_referenced_objects();
    }

    #[test]
    fn media_ingest_requires_verification_before_it_is_playable() {
        fn media_becomes_visible_only_after_size_and_hash_verification() {
            let tmp = TempDir::new().unwrap();
            let mut store = NearbyTransferStore::open(tmp.path().join("spool"), 10).unwrap();
            let transfer_id = [0x33; 16];
            let chunk = crate::network::actor::PERSONAL_TRANSFER_CHUNK_BYTES;
            let bytes = vec![0x5a; chunk.saturating_add(17)];
            let manifest = media_manifest(transfer_id, "source", &bytes);
            store.begin_incoming(manifest, 10).unwrap();
            assert!(
                store.cached_media("source", transfer_id).is_none(),
                "a manifest is not playable bytes"
            );
            for (index, piece) in bytes.chunks(chunk).enumerate() {
                store
                    .append(
                        transfer_id,
                        u64::try_from(index.saturating_mul(chunk)).unwrap(),
                        piece,
                    )
                    .unwrap();
            }
            assert!(
                store.cached_media("source", transfer_id).is_none(),
                "all chunks still require a verified finish"
            );
            let path = store.finish_to_cache(transfer_id).unwrap();
            assert_eq!(
                store.cached_media("source", transfer_id),
                Some(path.clone())
            );
            assert_eq!(fs::read(path).unwrap(), bytes);
        }

        fn malformed_media_never_leaves_a_playable_cache_entry() {
            let tmp = TempDir::new().unwrap();
            let mut store = NearbyTransferStore::open(tmp.path().join("spool"), 10).unwrap();
            let transfer_id = [0x55; 16];
            let manifest = media_manifest(transfer_id, "source", b"good");
            store.begin_incoming(manifest, 10).unwrap();
            assert!(
                store.append(transfer_id, 1, b"evi").is_err(),
                "an out-of-order chunk is refused"
            );
            store.append(transfer_id, 0, b"evil").unwrap();
            assert!(store.finish_to_cache(transfer_id).is_err());
            assert!(store.cached_media("source", transfer_id).is_none());
            assert_eq!(
                fs::read_dir(tmp.path().join("spool").join("inbox"))
                    .unwrap()
                    .count(),
                0,
                "rejected transfers remove partial bytes"
            );
            assert_eq!(
                media_cache_objects(&store.media_root()).len(),
                0,
                "nothing unverified reaches the cache"
            );
        }

        fn non_media_transfer_never_enters_the_media_cache() {
            let tmp = TempDir::new().unwrap();
            let mut store = NearbyTransferStore::open(tmp.path().join("spool"), 10).unwrap();
            let transfer_id = [0x56; 16];
            let mut manifest = media_manifest(transfer_id, "source", b"file");
            manifest.kind = TransferKind::File;
            store.begin_incoming(manifest, 10).unwrap();
            store.append(transfer_id, 0, b"file").unwrap();
            assert!(store.finish_to_cache(transfer_id).is_err());
            assert!(store.cached_media("source", transfer_id).is_none());
        }

        media_becomes_visible_only_after_size_and_hash_verification();
        malformed_media_never_leaves_a_playable_cache_entry();
        non_media_transfer_never_enters_the_media_cache();
    }

    #[test]
    fn nearby_fresh_consent_rebinds_retry_and_truncates_only_uncommitted_tail() {
        let root = TempDir::new().unwrap();
        let spool = root.path().join("spool");
        let checkpoint = vec![3; usize::try_from(CHECKPOINT_BYTES).unwrap()];
        let manifest = IncomingManifest {
            id: [61; 16],
            source: "certified-peer".into(),
            request_id: [62; 16],
            name: "report.bin".into(),
            kind: TransferKind::File,
            total_bytes: CHECKPOINT_BYTES + 20,
            content_hash: [4; 32],
        };
        let mut store = NearbyTransferStore::open(spool.clone(), 10).unwrap();
        store.begin_incoming(manifest.clone(), 10).unwrap();
        store.append(manifest.id, 0, &checkpoint).unwrap();
        store
            .append(manifest.id, CHECKPOINT_BYTES, b"crash-tail")
            .unwrap();
        let before = store.inbox()[0].clone();
        let mut retry = manifest.clone();
        retry.request_id = [63; 16];
        assert!(
            store.begin_incoming(retry.clone(), 11).is_err(),
            "ordinary resume cannot rotate consent"
        );
        let (offset, hash) = store.accept_incoming_offer(retry.clone(), 11).unwrap();
        assert_eq!(offset, CHECKPOINT_BYTES);
        assert_eq!(hash, *blake3::hash(&checkpoint).as_bytes());
        assert_eq!(
            fs::read(store.partial_path(manifest.id)).unwrap(),
            checkpoint
        );
        assert_eq!(store.inbox()[0].request_id, retry.request_id);
        assert_eq!(store.inbox()[0].expires_unix_ms, before.expires_unix_ms);
        assert!(
            store.begin_incoming(manifest, 11).is_err(),
            "old request is now stale"
        );
        drop(store);
        let mut reopened = NearbyTransferStore::open(spool, 12).unwrap();
        assert_eq!(reopened.begin_incoming(retry, 12).unwrap(), (offset, hash));
    }

    #[test]
    fn nearby_reoffer_cannot_change_peer_or_immutable_manifest() {
        let root = TempDir::new().unwrap();
        let mut store = NearbyTransferStore::open(root.path().join("spool"), 10).unwrap();
        let manifest = IncomingManifest {
            id: [71; 16],
            source: "certified-peer".into(),
            request_id: [72; 16],
            name: "report.bin".into(),
            kind: TransferKind::File,
            total_bytes: 20,
            content_hash: [4; 32],
        };
        store.begin_incoming(manifest.clone(), 10).unwrap();
        store.append(manifest.id, 0, b"uncommitted").unwrap();
        let before = store.inbox()[0].clone();
        let mut retry = manifest;
        retry.request_id = [73; 16];
        let mut wrong_peer = retry.clone();
        wrong_peer.source = "other-peer".into();
        let mut wrong_name = retry.clone();
        wrong_name.name = "other.bin".into();
        let mut wrong_kind = retry.clone();
        wrong_kind.kind = TransferKind::FolderArchive;
        let mut wrong_size = retry.clone();
        wrong_size.total_bytes += 1;
        let mut wrong_hash = retry.clone();
        wrong_hash.content_hash = [5; 32];
        let mut zero_request = retry;
        zero_request.request_id = [0; 16];
        for changed in [
            wrong_peer,
            wrong_name,
            wrong_kind,
            wrong_size,
            wrong_hash,
            zero_request,
        ] {
            assert!(store.accept_incoming_offer(changed, 11).is_err());
            assert_eq!(store.inbox(), std::slice::from_ref(&before));
            assert_eq!(
                fs::read(store.partial_path(before.id)).unwrap(),
                b"uncommitted"
            );
        }
    }

    #[test]
    fn nearby_declining_a_fresh_offer_discards_only_its_matching_checkpoint() {
        let root = TempDir::new().unwrap();
        let spool = root.path().join("spool");
        let mut store = NearbyTransferStore::open(spool.clone(), 10).unwrap();
        let mut offer = IncomingManifest {
            id: [81; 16],
            source: "certified-peer".into(),
            request_id: [82; 16],
            name: "report.bin".into(),
            kind: TransferKind::File,
            total_bytes: 20,
            content_hash: [4; 32],
        };
        store.begin_incoming(offer.clone(), 10).unwrap();
        store.append(offer.id, 0, b"partial").unwrap();
        offer.request_id = [83; 16];
        let mut other_peer = offer.clone();
        other_peer.source = "other-peer".into();
        assert!(store.decline_incoming_offer(&other_peer).is_err());
        assert_eq!(store.inbox().len(), 1);
        assert_eq!(fs::read(store.partial_path(offer.id)).unwrap(), b"partial");
        assert!(store.decline_incoming_offer(&offer).unwrap());
        assert!(store.inbox().is_empty());
        assert!(!store.partial_path(offer.id).exists());
        drop(store);
        assert!(
            NearbyTransferStore::open(spool, 11)
                .unwrap()
                .inbox()
                .is_empty()
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn nearby_staging_is_private_resumable_and_integrity_checked() {
        fn file_staging_is_immutable_and_private() {
            let tmp = TempDir::new().unwrap();
            let source = tmp.path().join("note.txt");
            fs::write(&source, b"original").unwrap();
            let mut store = NearbyTransferStore::open(tmp.path().join("spool"), 10).unwrap();
            let entry = store.stage(&source, "peer".into(), 10).unwrap();
            fs::write(&source, b"changed").unwrap();
            assert_eq!(fs::read(store.object_path(entry.id)).unwrap(), b"original");
            assert_eq!(
                fs::metadata(tmp.path().join("spool/index.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        fn reopen_truncates_crash_tail_and_reports_checkpoint_hash() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path().join("spool");
            let mut store = NearbyTransferStore::open(root.clone(), 10).unwrap();
            let id = [4; 16];
            let checkpoint_len = usize::try_from(CHECKPOINT_BYTES).expect("checkpoint fits usize");
            store
                .begin_incoming(
                    IncomingManifest {
                        id,
                        source: "peer".into(),
                        request_id: [5; 16],
                        name: "x.bin".into(),
                        kind: TransferKind::File,
                        total_bytes: CHECKPOINT_BYTES + 9,
                        content_hash: [8; 32],
                    },
                    10,
                )
                .unwrap();
            store.append(id, 0, &vec![3; checkpoint_len]).unwrap();
            fs::OpenOptions::new()
                .append(true)
                .open(store.partial_path(id))
                .unwrap()
                .write_all(b"tail")
                .unwrap();
            drop(store);
            let mut reopened = NearbyTransferStore::open(root, 11).unwrap();
            assert_eq!(
                fs::metadata(reopened.partial_path(id)).unwrap().len(),
                CHECKPOINT_BYTES
            );
            let (offset, hash) = reopened
                .begin_incoming(
                    IncomingManifest {
                        id,
                        source: "peer".into(),
                        request_id: [5; 16],
                        name: "x.bin".into(),
                        kind: TransferKind::File,
                        total_bytes: CHECKPOINT_BYTES + 9,
                        content_hash: [8; 32],
                    },
                    11,
                )
                .unwrap();
            assert_eq!(offset, CHECKPOINT_BYTES);
            assert_eq!(hash, *blake3::hash(&vec![3; checkpoint_len]).as_bytes());
        }

        fn resume_rejects_manifest_mismatch_and_cancel_cleans_bytes() {
            let tmp = TempDir::new().unwrap();
            let mut store = NearbyTransferStore::open(tmp.path().join("spool"), 10).unwrap();
            let id = [7; 16];
            store
                .begin_incoming(
                    IncomingManifest {
                        id,
                        source: "peer".into(),
                        request_id: [6; 16],
                        name: "x".into(),
                        kind: TransferKind::File,
                        total_bytes: 2,
                        content_hash: [1; 32],
                    },
                    10,
                )
                .unwrap();
            assert!(
                store
                    .begin_incoming(
                        IncomingManifest {
                            id,
                            source: "other".into(),
                            request_id: [6; 16],
                            name: "x".into(),
                            kind: TransferKind::File,
                            total_bytes: 2,
                            content_hash: [1; 32]
                        },
                        10
                    )
                    .is_err()
            );
            store.cancel_incoming(id).unwrap();
            assert!(!store.partial_path(id).exists());
            assert!(store.inbox().is_empty());
        }

        fn integrity_failure_removes_untrusted_partial() {
            let tmp = TempDir::new().unwrap();
            let mut store = NearbyTransferStore::open(tmp.path().join("spool"), 10).unwrap();
            let id = [8; 16];
            store
                .begin_incoming(
                    IncomingManifest {
                        id,
                        source: "peer".into(),
                        request_id: [3; 16],
                        name: "x.bin".into(),
                        kind: TransferKind::File,
                        total_bytes: 3,
                        content_hash: [9; 32],
                    },
                    10,
                )
                .unwrap();
            store.append(id, 0, b"bad").unwrap();
            assert!(store.finish(id, &tmp.path().join("downloads")).is_err());
            assert!(!store.partial_path(id).exists());
            assert!(store.inbox().is_empty());
        }

        fn reopen_expires_metadata_and_spooled_bytes() {
            let tmp = TempDir::new().unwrap();
            let source = tmp.path().join("old.bin");
            fs::write(&source, b"old").unwrap();
            let root = tmp.path().join("spool");
            let mut store = NearbyTransferStore::open(root.clone(), 1).unwrap();
            let entry = store.stage(&source, "peer".into(), 1).unwrap();
            let object = store.object_path(entry.id);
            assert!(object.exists());
            drop(store);
            let reopened = NearbyTransferStore::open(root, TRANSFER_LIFETIME_MS + 2).unwrap();
            assert!(reopened.outbox().is_empty());
            assert!(!object.exists());
        }

        fn quota_reserves_the_candidate_bytes_without_overflow() {
            let tmp = TempDir::new().unwrap();
            let mut store = NearbyTransferStore::open(tmp.path().join("spool"), 10).unwrap();
            store.index.outbox.push(OutboxEntry {
                id: [9; 16],
                target: "peer".into(),
                name: "huge".into(),
                kind: TransferKind::File,
                total_bytes: MAX_TOTAL_BYTES - 1,
                content_hash: [0; 32],
                created_unix_ms: 10,
                expires_unix_ms: 20,
            });
            assert!(store.ensure_capacity(2).is_err());
            assert!(store.ensure_capacity(u64::MAX).is_err());
        }

        file_staging_is_immutable_and_private();
        reopen_truncates_crash_tail_and_reports_checkpoint_hash();
        resume_rejects_manifest_mismatch_and_cancel_cleans_bytes();
        integrity_failure_removes_untrusted_partial();
        reopen_expires_metadata_and_spooled_bytes();
        quota_reserves_the_candidate_bytes_without_overflow();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn nearby_files_and_folders_stay_anchored_and_never_overwrite_or_traverse() {
        fn folder_archive_is_deterministic_rejects_links_and_extracts_create_new() {
            let tmp = TempDir::new().unwrap();
            let source = tmp.path().join("folder");
            fs::create_dir(&source).unwrap();
            fs::write(source.join("a.txt"), b"a").unwrap();
            let mut first = NearbyTransferStore::open(tmp.path().join("one"), 10).unwrap();
            let mut second = NearbyTransferStore::open(tmp.path().join("two"), 10).unwrap();
            let a = first.stage(&source, "peer".into(), 10).unwrap();
            fs::write(source.join("a.txt"), b"a").unwrap();
            let b = second.stage(&source, "peer".into(), 10).unwrap();
            assert_eq!(a.content_hash, b.content_hash);

            std::os::unix::fs::symlink("a.txt", source.join("link")).unwrap();
            assert!(first.stage(&source, "peer".into(), 10).is_err());

            let receive = NearbyTransferStore::open(tmp.path().join("receive"), 10).unwrap();
            let downloads = tmp.path().join("downloads");
            ensure_private_dir(&downloads).unwrap();
            fs::create_dir(downloads.join("folder")).unwrap();
            let installed =
                extract_archive_create_new(&first.object_path(a.id), &downloads, "folder").unwrap();
            assert_eq!(installed.file_name().unwrap(), "folder (1)");
            assert_eq!(fs::read(installed.join("a.txt")).unwrap(), b"a");
            drop(receive);
        }

        fn secure_source_is_anchored_across_intermediate_rename_and_symlink_swap() {
            let tmp = TempDir::new().unwrap();
            let grant = tmp.path().join("grant");
            let folder = grant.join("folder");
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("report.txt"), b"original").unwrap();

            let source = SecureNearbySource::open_beneath(&grant, &folder).unwrap();
            let moved = grant.join("moved");
            fs::rename(&folder, &moved).unwrap();
            std::os::unix::fs::symlink("/etc", &folder).unwrap();

            assert!(
                SecureNearbySource::open_beneath(&grant, &folder.join("passwd")).is_err(),
                "an intermediate symlink swap must not escape the grant"
            );
            let mut store = NearbyTransferStore::open(tmp.path().join("store"), 10).unwrap();
            let staged = store
                .stage_secure_with_id(source, "peer".into(), 10, [9; 16])
                .expect("opened directory capability remains anchored after rename");
            assert_eq!(staged.kind, TransferKind::FolderArchive);
            assert!(store.object_path(staged.id).is_file());
        }

        fn secure_destination_survives_rename_and_never_overwrites() {
            let tmp = TempDir::new().unwrap();
            let destination = tmp.path().join("approved");
            fs::create_dir(&destination).unwrap();
            fs::write(destination.join("report.txt"), b"existing").unwrap();
            let expected_destination = fs::canonicalize(&destination).unwrap();
            let capability = SecureNearbyDestination::open_grant(&destination).unwrap();
            let moved = tmp.path().join("moved");
            fs::rename(&destination, &moved).unwrap();
            std::os::unix::fs::symlink("/etc", &destination).unwrap();

            let installed = capability
                .install_inline("report.txt", b"received")
                .unwrap();
            assert_eq!(installed, expected_destination.join("report (1).txt"));
            assert_eq!(fs::read(moved.join("report.txt")).unwrap(), b"existing");
            assert_eq!(fs::read(moved.join("report (1).txt")).unwrap(), b"received");
        }

        fn secure_folder_round_trip_stays_anchored_and_does_not_replace_an_existing_folder() {
            let tmp = TempDir::new().unwrap();
            let source_path = tmp.path().join("folder");
            fs::create_dir_all(source_path.join("nested")).unwrap();
            fs::write(source_path.join("nested/report.txt"), b"received").unwrap();
            let source = SecureNearbySource::open_beneath(tmp.path(), &source_path).unwrap();
            let mut sender = NearbyTransferStore::open(tmp.path().join("sender"), 10).unwrap();
            let entry = sender
                .stage_secure_with_id(source, "peer".into(), 10, [41; 16])
                .unwrap();
            let bytes = fs::read(sender.object_path(entry.id)).unwrap();
            let approved = tmp.path().join("approved");
            fs::create_dir_all(approved.join("folder")).unwrap();
            fs::write(approved.join("folder/keep.txt"), b"keep").unwrap();
            let expected_destination = fs::canonicalize(&approved).unwrap();
            let destination = SecureNearbyDestination::open_grant(&approved).unwrap();
            let moved = tmp.path().join("moved");
            fs::rename(&approved, &moved).unwrap();
            std::os::unix::fs::symlink(&source_path, &approved).unwrap();
            let mut receiver = NearbyTransferStore::open(tmp.path().join("receiver"), 10).unwrap();
            receiver
                .begin_incoming(
                    IncomingManifest {
                        id: entry.id,
                        source: "peer".into(),
                        request_id: [42; 16],
                        name: entry.name,
                        kind: entry.kind,
                        total_bytes: entry.total_bytes,
                        content_hash: entry.content_hash,
                    },
                    10,
                )
                .unwrap();
            receiver.append(entry.id, 0, &bytes).unwrap();
            let installed = receiver.finish_secure(entry.id, &destination).unwrap();
            assert_eq!(installed, expected_destination.join("folder (1)"));
            assert_eq!(fs::read(moved.join("folder/keep.txt")).unwrap(), b"keep");
            assert_eq!(
                fs::read(moved.join("folder (1)/nested/report.txt")).unwrap(),
                b"received"
            );
            assert!(!source_path.join("folder (1)").exists());
            assert!(!receiver.partial_path(entry.id).exists());
        }

        fn extraction_rejects_parent_traversal() {
            let tmp = TempDir::new().unwrap();
            let archive_path = tmp.path().join("bad.tar");
            let file = File::create(&archive_path).unwrap();
            let mut builder = tar::Builder::new(file);
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o600);
            header.set_cksum();
            // The tar crate itself refuses unsafe names, so corrupt a valid name in-place.
            builder.append_data(&mut header, "aa/x", &b"x"[..]).unwrap();
            builder.finish().unwrap();
            let mut bytes = fs::read(&archive_path).unwrap();
            bytes[..4].copy_from_slice(b"../x");
            fs::write(&archive_path, bytes).unwrap();
            let root = tmp.path().join("out");
            ensure_private_dir(&root).unwrap();
            assert!(extract_archive_create_new(&archive_path, &root, "bad").is_err());
            assert!(!tmp.path().join("x").exists());
        }

        folder_archive_is_deterministic_rejects_links_and_extracts_create_new();
        secure_source_is_anchored_across_intermediate_rename_and_symlink_swap();
        secure_destination_survives_rename_and_never_overwrites();
        secure_folder_round_trip_stays_anchored_and_does_not_replace_an_existing_folder();
        extraction_rejects_parent_traversal();
    }
}

// Walk one component at a time relative to owned directory descriptors. The
// validated path contains neither parent traversal nor absolute components,
// and O_NOFOLLOW rejects a symlink at every step, including the final file.
#[cfg(not(target_os = "linux"))]
fn openat2_beneath(root: &File, relative: &Path, directory: bool) -> Result<File, String> {
    validate_relative(relative)?;
    let mut current = root.try_clone().map_err(display)?;
    let mut parts = relative.components().peekable();
    while let Some(Component::Normal(component)) = parts.next() {
        let name = CString::new(component.as_bytes()).map_err(display)?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if directory || parts.peek().is_some() {
                libc::O_DIRECTORY
            } else {
                0
            };
        // SAFETY: name is NUL terminated and current owns a live directory fd.
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        // SAFETY: openat returned a fresh owned descriptor.
        current = unsafe { File::from_raw_fd(fd) };
    }
    Ok(current)
}

#[cfg(target_os = "macos")]
fn capability_entries(root: &File) -> Result<Vec<(PathBuf, bool)>, String> {
    let mut entries = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let directory = if relative.as_os_str().is_empty() {
            root.try_clone().map_err(display)?
        } else {
            openat2_beneath(root, &relative, true)?
        };
        // fdopendir owns its descriptor; duplicate it without passing ownership
        // of the capability itself. Every directory is enumerated once.
        let fd = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(error.to_string());
        }
        struct DirectoryStream(*mut libc::DIR);
        impl Drop for DirectoryStream {
            fn drop(&mut self) {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let stream = DirectoryStream(stream);
        unsafe {
            libc::rewinddir(stream.0);
        }
        loop {
            // readdir uses errno to distinguish end-of-stream from failure.
            unsafe {
                *libc::__error() = 0;
            }
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(0) {
                    return Err(error.to_string());
                }
                break;
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            let path = relative.join(std::ffi::OsStr::from_bytes(name));
            let file = openat2_beneath(root, &path, false)?;
            let metadata = file.metadata().map_err(display)?;
            if !metadata.is_dir() && !metadata.is_file() {
                return Err("folder contains a link or special file".into());
            }
            if metadata.is_dir() {
                pending.push(path.clone());
            }
            entries.push((path, metadata.is_dir()));
            if entries.len() >= MAX_FOLDER_ENTRIES {
                return Err("folder contains too many entries".into());
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

#[cfg(target_os = "macos")]
fn create_capability_file(root: &File, name: impl AsRef<std::ffi::OsStr>) -> io::Result<File> {
    let name = CString::new(name.as_ref().as_bytes())?;
    // O_EXCL prevents overwriting either existing files or symlinks.
    let fd = unsafe {
        libc::openat(
            root.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "macos")]
fn install_capability_file(
    root: &File,
    name: &str,
    input: &mut impl Read,
) -> Result<String, String> {
    for suffix in 0..1000 {
        let candidate = collision_name(name, suffix);
        match create_capability_file(root, &candidate) {
            Ok(mut output) => {
                io::copy(input, &mut output).map_err(display)?;
                output.sync_all().map_err(display)?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("too many nearby transfer name collisions".into())
}

#[cfg(target_os = "macos")]
fn ensure_capability_directory(root: &File, relative: &Path) -> Result<File, String> {
    if relative.as_os_str().is_empty() {
        return root.try_clone().map_err(display);
    }
    validate_relative(relative)?;
    let mut current = root.try_clone().map_err(display)?;
    for component in relative.components() {
        let Component::Normal(component) = component else {
            unreachable!()
        };
        let name = CString::new(component.as_bytes()).map_err(display)?;
        let result = unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) };
        if result != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
            return Err(io::Error::last_os_error().to_string());
        }
        current = openat2_beneath(&current, Path::new(component), true)?;
    }
    Ok(current)
}

#[cfg(target_os = "macos")]
fn extract_capability_archive(source: &Path, root: &File, name: &str) -> Result<String, String> {
    let temporary = format!(".{}.partial", id_hex(random_id()));
    let temporary_c = CString::new(temporary.as_str()).map_err(display)?;
    // Reserve a private directory exclusively; do not reuse a planted entry.
    if unsafe { libc::mkdirat(root.as_raw_fd(), temporary_c.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let directory = openat2_beneath(root, Path::new(&temporary), true)?;
    let result = (|| {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(source)
            .map_err(display)?;
        let mut archive = tar::Archive::new(file);
        let mut total = 0u64;
        for (index, item) in archive.entries().map_err(display)?.enumerate() {
            if index >= MAX_FOLDER_ENTRIES {
                return Err("archive contains too many entries".into());
            }
            let mut item = item.map_err(display)?;
            let path = item.path().map_err(display)?.into_owned();
            validate_relative(&path)?;
            let kind = item.header().entry_type();
            if kind.is_dir() {
                ensure_capability_directory(&directory, &path)?;
            } else if kind.is_file() {
                total = total
                    .checked_add(item.size())
                    .ok_or_else(|| "archive exceeds size limit".to_string())?;
                if total > MAX_TRANSFER_BYTES {
                    return Err("archive exceeds size limit".into());
                }
                let parent = ensure_capability_directory(
                    &directory,
                    path.parent().unwrap_or(Path::new("")),
                )?;
                let leaf = path
                    .file_name()
                    .ok_or_else(|| "archive path is not portable".to_string())?;
                let mut output = create_capability_file(&parent, leaf).map_err(display)?;
                io::copy(&mut item, &mut output).map_err(display)?;
                output.sync_all().map_err(display)?;
            } else {
                return Err("archive contains a link or special entry".into());
            }
        }
        for suffix in 0..1000 {
            let candidate = collision_name(name, suffix);
            let candidate_c = CString::new(candidate.as_str()).map_err(display)?;
            // RENAME_EXCL atomically publishes without replacing any existing
            // directory, including one created after our collision check.
            if unsafe {
                libc::renameatx_np(
                    root.as_raw_fd(),
                    temporary_c.as_ptr(),
                    root.as_raw_fd(),
                    candidate_c.as_ptr(),
                    libc::RENAME_EXCL,
                )
            } == 0
            {
                return Ok(candidate);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error.to_string());
            }
        }
        Err("too many nearby transfer name collisions".into())
    })();
    if result.is_err() {
        // Enumerate only our private directory, then remove children deepest
        // first through their retained parent capabilities.
        if let Ok(entries) = capability_entries(&directory) {
            for (path, is_dir) in entries.into_iter().rev() {
                if let Ok(parent) = if path.parent() == Some(Path::new("")) {
                    directory.try_clone().map_err(display)
                } else {
                    openat2_beneath(&directory, path.parent().unwrap(), true)
                } {
                    if let Some(leaf) = path
                        .file_name()
                        .and_then(|s| CString::new(s.as_bytes()).ok())
                    {
                        unsafe {
                            libc::unlinkat(
                                parent.as_raw_fd(),
                                leaf.as_ptr(),
                                if is_dir { libc::AT_REMOVEDIR } else { 0 },
                            );
                        }
                    }
                }
            }
        }
        unsafe {
            libc::unlinkat(root.as_raw_fd(), temporary_c.as_ptr(), libc::AT_REMOVEDIR);
        }
    }
    result
}
