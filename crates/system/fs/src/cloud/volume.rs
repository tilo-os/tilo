//! One mounted cloud account: metadata, hydration, write-back and sync.
//!
//! [`CloudVolume`] is the engine behind a `~/Remote/<account>` mount. It is
//! independent of FUSE: the kernel adapter ([`super::fuse`]) and the tests
//! call the same inode-level operations.
//!
//! - **Metadata** is the account's [`Tree`], filled from the provider's
//!   change feed or by walking listings, and persisted, so the folder lists
//!   instantly and offline after a restart.
//! - **Reads** hydrate on demand: the missing ranges of the requested
//!   window (widened for sequential readers) are fetched with ranged,
//!   revision-pinned reads and written into a [`FileArea`]: the file-backed,
//!   range-resident, budgeted cache the storage daemon's content store keeps
//!   its content in, keyed here by remote revision. A read never pulls a
//!   whole large file unless the reader asks for all of it.
//! - **Writes** go to a staged copy outside the cache. When the last writer
//!   closes, an upload is queued in the persisted outbox and sent in the
//!   background with the revision the edit started from as its precondition.
//!   Creates, folder creation, renames and deletes queue the same way, so
//!   everything works offline and resumes after a restart.
//! - **Conflicts** keep both versions with the storage daemon's conflict
//!   copy name (`<name>.sync-conflict-<hash>`): the remote version keeps the
//!   name, the local edit is uploaded next to it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use cloud_source::tree::Tree;
use cloud_source::{
    Change, ChangeFeed, CloudSource, Cursor, Error, Item, ItemId, ItemKind, Notify, Revision,
    UploadTarget,
};
use serde::{Deserialize, Serialize};

use crate::store::area::{AreaKey, Residency, hash_file};
use crate::store::{FileArea, Retention};

/// The cache key of one remote revision of one item.
type RevKey = AreaKey;

fn rev_key(id: &ItemId, revision: Option<&Revision>) -> RevKey {
    let mut hasher = blake3::Hasher::new();
    hasher.update(id.as_str().as_bytes());
    hasher.update(&[0]);
    hasher.update(revision.map_or("", |r| r.as_str()).as_bytes());
    *hasher.finalize().as_bytes()
}

/// The account's content cache under `dir`, holding at most `budget`
/// resident bytes.
fn open_content(dir: &Path, budget: u64) -> io::Result<FileArea> {
    // Leftovers of the cache layout before the content store held it; a
    // cache, so they are dropped rather than converted.
    for stale in ["partial", "blobs"] {
        let _ = fs::remove_dir_all(dir.join(stale));
    }
    let _ = fs::remove_file(dir.join("index.json"));
    FileArea::open(dir, budget)
}

/// Reads up to `len` bytes at `offset` of the file at `path`.
fn read_file_at(path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    let len = usize::try_from(size.saturating_sub(offset))
        .unwrap_or(usize::MAX)
        .min(len);
    let mut out = vec![0_u8; len];
    let mut done = 0_usize;
    while done < len {
        let read = file.read_at(
            out.get_mut(done..).unwrap_or_default(),
            offset.saturating_add(done as u64),
        )?;
        if read == 0 {
            break;
        }
        done = done.saturating_add(read);
    }
    out.truncate(done);
    Ok(out)
}

pub type Errno = i32;

/// Inode of the account root.
pub const ROOT_INO: u64 = 1;
/// Fetch granularity: missing ranges are rounded out to this.
const GRANULE: u64 = 1 << 20;
/// Largest read-ahead window for a sequential reader.
const READAHEAD_MAX: u64 = 16 << 20;
/// Prefix of ids of items created locally and not uploaded yet. NUL never
/// occurs in a provider id or a file name.
const LOCAL_PREFIX: &str = "\u{0}local:";

/// Account health as the volume sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Syncing,
    Healthy,
    Offline,
    NeedsAuthentication,
    Error,
}

impl Health {
    fn of(error: &Error) -> Self {
        match error {
            Error::Offline | Error::RateLimited { .. } | Error::Io(_) => Self::Offline,
            Error::Unauthorized => Self::NeedsAuthentication,
            Error::Status(status) if *status >= 500 => Self::Offline,
            _ => Self::Error,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Syncing => "syncing",
            Self::Healthy => "healthy",
            Self::Offline => "offline",
            Self::NeedsAuthentication => "needs-authentication",
            Self::Error => "error",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "syncing" => Self::Syncing,
            "healthy" => Self::Healthy,
            "offline" => Self::Offline,
            "needs-authentication" => Self::NeedsAuthentication,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

/// What `stat` shows for an inode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attr {
    pub ino: u64,
    pub kind: Kind,
    pub size: u64,
    pub mtime: SystemTime,
    pub perm: u16,
}

pub struct VolumeConfig {
    /// Private per-account directory: metadata, outbox, staged edits and
    /// the content cache.
    pub dir: PathBuf,
    /// Resident content bytes the cache may hold.
    pub cache_budget: u64,
    /// How often a provider without change notification is asked for
    /// changes while idle.
    pub poll_interval: Duration,
}

/// A queued local change, executed in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Op {
    Upload(ItemId),
    Mkdir(ItemId),
    Move(ItemId),
    Delete {
        id: ItemId,
        revision: Option<Revision>,
    },
}

impl Op {
    fn id(&self) -> &ItemId {
        match self {
            Self::Upload(id) | Self::Mkdir(id) | Self::Move(id) | Self::Delete { id, .. } => id,
        }
    }

    fn id_mut(&mut self) -> &mut ItemId {
        match self {
            Self::Upload(id) | Self::Mkdir(id) | Self::Move(id) | Self::Delete { id, .. } => id,
        }
    }
}

/// Local state of an item with a staged edit.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Local {
    /// File name in the staging directory holding the current content.
    staged: Option<String>,
    /// The remote revision the edit started from (the upload precondition).
    base: Option<Revision>,
    /// Changed since the last upload.
    dirty: bool,
    /// Bumped by every write, to tell whether an upload sent the latest.
    #[serde(skip)]
    generation: u64,
    #[serde(skip)]
    writers: u32,
}

struct Handle {
    id: ItemId,
    write: bool,
    seq_end: u64,
    window: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct TreeFile {
    items: Vec<Item>,
    cursor: Option<Cursor>,
}

#[derive(Default, Serialize, Deserialize)]
struct LocalFile {
    items: Vec<Item>,
    local: Vec<(ItemId, Local)>,
    outbox: Vec<Op>,
}

struct Inner {
    tree: Tree,
    cursor: Option<Cursor>,
    ino_by_id: HashMap<ItemId, u64>,
    id_by_ino: HashMap<u64, ItemId>,
    next_ino: u64,
    local: HashMap<ItemId, Local>,
    outbox: VecDeque<Op>,
    handles: HashMap<u64, Handle>,
    next_fh: u64,
    /// Folders listed at least once in this process (poll walks).
    listed: HashSet<ItemId>,
}

impl Inner {
    fn ino(&mut self, id: &ItemId) -> u64 {
        if let Some(ino) = self.ino_by_id.get(id) {
            return *ino;
        }
        let ino = self.next_ino;
        self.next_ino = self.next_ino.saturating_add(1);
        self.ino_by_id.insert(id.clone(), ino);
        self.id_by_ino.insert(ino, id.clone());
        ino
    }

    fn id(&self, ino: u64) -> Result<ItemId, Errno> {
        self.id_by_ino.get(&ino).cloned().ok_or(libc::ENOENT)
    }

    fn pending(&self, id: &ItemId) -> bool {
        self.outbox.iter().any(|op| op.id() == id)
            || self
                .local
                .get(id)
                .is_some_and(|local| local.dirty || local.writers > 0)
    }

    fn queue(&mut self, op: Op) {
        if !self.outbox.contains(&op) {
            self.outbox.push_back(op);
        }
    }
}

fn is_local(id: &ItemId) -> bool {
    id.as_str().starts_with(LOCAL_PREFIX)
}

fn native_payload(item: &Item) -> Option<Vec<u8>> {
    let ItemKind::Native(document) = &item.kind else {
        return None;
    };
    let body = serde_json::json!({
        "id": item.id.as_str(),
        "mime": document.mime,
        "url": document.web_url,
        "exports": document.exports.iter().map(|f| &f.extension).collect::<Vec<_>>(),
    });
    let mut bytes = serde_json::to_vec_pretty(&body).unwrap_or_default();
    bytes.push(b'\n');
    Some(bytes)
}

fn errno_of(error: &Error) -> Errno {
    match error {
        Error::NotFound => libc::ENOENT,
        Error::Forbidden => libc::EACCES,
        Error::InsufficientStorage => libc::ENOSPC,
        Error::TooLarge => libc::EFBIG,
        _ => libc::EIO,
    }
}

fn io_errno(error: &io::Error) -> Errno {
    error.raw_os_error().unwrap_or(libc::EIO)
}

/// A mounted cloud account.
pub struct CloudVolume {
    source: Arc<dyn CloudSource>,
    dir: PathBuf,
    staging: PathBuf,
    /// Cached remote content; released at shutdown, so the account's
    /// directory can be opened again.
    content: Mutex<Option<Arc<FileArea>>>,
    poll_interval: Duration,
    inner: Mutex<Inner>,
    /// Serialises fetches of one revision so concurrent readers of the same
    /// range download it once.
    flights: Mutex<HashMap<RevKey, Arc<Mutex<()>>>>,
    /// Serialises outbox execution.
    uploading: Mutex<()>,
    health: Mutex<Option<Health>>,
    quota: Mutex<Option<cloud_source::Quota>>,
    on_health: Box<dyn Fn(Health) + Send + Sync>,
    wake: Condvar,
    /// Bumped whenever a local change is queued.
    queued: Mutex<u64>,
    stop: AtomicBool,
}

impl CloudVolume {
    /// Open the volume for `source`, restoring persisted metadata and
    /// queued changes from `config.dir`.
    pub fn open(
        source: Arc<dyn CloudSource>,
        config: VolumeConfig,
        on_health: Box<dyn Fn(Health) + Send + Sync>,
    ) -> io::Result<Arc<Self>> {
        let staging = config.dir.join("staging");
        fs::create_dir_all(&staging)?;
        let content = Mutex::new(Some(Arc::new(open_content(
            &config.dir.join("content"),
            config.cache_budget,
        )?)));
        let root = source.root();
        let mut tree = Tree::new(root.clone());
        let saved: TreeFile = read_json(&config.dir.join("tree.json")).unwrap_or_default();
        for item in saved.items {
            tree.upsert(item);
        }
        let local_file: LocalFile = read_json(&config.dir.join("local.json")).unwrap_or_default();
        for item in local_file.items {
            tree.upsert(item);
        }
        let mut inner = Inner {
            tree,
            cursor: saved.cursor,
            ino_by_id: HashMap::new(),
            id_by_ino: HashMap::new(),
            next_ino: ROOT_INO.saturating_add(1),
            local: local_file.local.into_iter().collect(),
            outbox: local_file.outbox.into_iter().collect(),
            handles: HashMap::new(),
            next_fh: 1,
            listed: HashSet::new(),
        };
        inner.ino_by_id.insert(root.clone(), ROOT_INO);
        inner.id_by_ino.insert(ROOT_INO, root);
        // An edit whose writer was still open when the process stopped has
        // no queued upload yet; nobody will close it now.
        let unsent: Vec<ItemId> = inner
            .local
            .iter()
            .filter(|(_, local)| local.dirty && local.staged.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for id in unsent {
            inner.queue(Op::Upload(id));
        }
        Ok(Arc::new(Self {
            source,
            dir: config.dir,
            staging,
            content,
            poll_interval: config.poll_interval,
            inner: Mutex::new(inner),
            flights: Mutex::new(HashMap::new()),
            uploading: Mutex::new(()),
            health: Mutex::new(None),
            quota: Mutex::new(None),
            on_health,
            wake: Condvar::new(),
            queued: Mutex::new(0),
            stop: AtomicBool::new(false),
        }))
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn source(&self) -> &Arc<dyn CloudSource> {
        &self.source
    }

    fn report(&self, health: Health) {
        let changed = {
            let mut current = self
                .health
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let changed = *current != Some(health);
            *current = Some(health);
            changed
        };
        if changed {
            (self.on_health)(health);
        }
    }

    /// The account's storage quota as of the last sync.
    pub fn quota(&self) -> Option<cloud_source::Quota> {
        *self
            .quota
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn health(&self) -> Option<Health> {
        *self
            .health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn staged_path(&self, name: &str) -> PathBuf {
        self.staging.join(name)
    }

    // ---- persistence -------------------------------------------------

    fn save_tree(&self) -> io::Result<()> {
        let bytes = {
            let inner = self.inner();
            let file = TreeFile {
                items: inner
                    .tree
                    .items()
                    .filter(|item| !is_local(&item.id) && !inner.pending(&item.id))
                    .cloned()
                    .collect(),
                cursor: inner.cursor.clone(),
            };
            serde_json::to_vec(&file).map_err(io::Error::other)?
        };
        tilo_files::native::atomic::write(&self.dir.join("tree.json"), &bytes)?;
        self.content().map_or(Ok(()), |content| content.flush())
    }

    /// The content cache, until the volume shuts down.
    fn content(&self) -> Option<Arc<FileArea>> {
        self.content
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn save_local(&self) -> io::Result<()> {
        let bytes = {
            let inner = self.inner();
            let file = LocalFile {
                items: inner
                    .tree
                    .items()
                    .filter(|item| is_local(&item.id) || inner.pending(&item.id))
                    .cloned()
                    .collect(),
                local: inner
                    .local
                    .iter()
                    .map(|(id, local)| (id.clone(), local.clone()))
                    .collect(),
                outbox: inner.outbox.iter().cloned().collect(),
            };
            serde_json::to_vec(&file).map_err(io::Error::other)?
        };
        tilo_files::native::atomic::write(&self.dir.join("local.json"), &bytes)
    }

    fn persist_local(&self) -> Result<(), Errno> {
        self.save_local().map_err(|e| io_errno(&e))
    }

    // ---- attributes and names ----------------------------------------

    fn attr_of(&self, inner: &mut Inner, id: &ItemId) -> Result<Attr, Errno> {
        let ino = inner.ino(id);
        if id == inner.tree.root() {
            return Ok(Attr {
                ino,
                kind: Kind::Dir,
                size: 0,
                mtime: SystemTime::UNIX_EPOCH,
                perm: 0o755,
            });
        }
        let item = inner.tree.get(id).ok_or(libc::ENOENT)?;
        let mtime = item.modified.unwrap_or(SystemTime::UNIX_EPOCH);
        Ok(match &item.kind {
            ItemKind::Folder => Attr {
                ino,
                kind: Kind::Dir,
                size: 0,
                mtime,
                perm: 0o755,
            },
            ItemKind::Native(_) => Attr {
                ino,
                kind: Kind::File,
                size: native_payload(item).map_or(0, |p| p.len() as u64),
                mtime,
                perm: 0o444,
            },
            ItemKind::File => {
                let staged = inner
                    .local
                    .get(id)
                    .and_then(|local| local.staged.as_deref())
                    .and_then(|name| fs::metadata(self.staged_path(name)).ok());
                match staged {
                    Some(meta) => Attr {
                        ino,
                        kind: Kind::File,
                        size: meta.len(),
                        mtime: meta.modified().unwrap_or(mtime),
                        perm: 0o644,
                    },
                    None => Attr {
                        ino,
                        kind: Kind::File,
                        size: item.size.unwrap_or(0),
                        mtime,
                        perm: 0o644,
                    },
                }
            }
        })
    }

    fn folder_id(inner: &Inner, ino: u64) -> Result<ItemId, Errno> {
        let id = inner.id(ino)?;
        if &id == inner.tree.root() {
            return Ok(id);
        }
        match inner.tree.get(&id).map(|item| &item.kind) {
            Some(ItemKind::Folder) => Ok(id),
            Some(_) => Err(libc::ENOTDIR),
            None => Err(libc::ENOENT),
        }
    }

    pub fn getattr(&self, ino: u64) -> Result<Attr, Errno> {
        let mut inner = self.inner();
        let id = inner.id(ino)?;
        self.attr_of(&mut inner, &id)
    }

    pub fn lookup(&self, parent: u64, name: &str) -> Result<Attr, Errno> {
        let mut inner = self.inner();
        let folder = Self::folder_id(&inner, parent)?;
        let id = inner
            .tree
            .child(&folder, name)
            .map(|item| item.id.clone())
            .ok_or(libc::ENOENT)?;
        self.attr_of(&mut inner, &id)
    }

    #[allow(
        clippy::needless_collect,
        reason = "the listing borrows the tree; attributes need it mutably"
    )]
    pub fn readdir(&self, ino: u64) -> Result<Vec<(String, Attr)>, Errno> {
        let mut inner = self.inner();
        let folder = Self::folder_id(&inner, ino)?;
        let children: Vec<(String, ItemId)> = inner
            .tree
            .children(&folder)
            .into_iter()
            .map(|(name, item)| (name, item.id.clone()))
            .collect();
        children
            .into_iter()
            .map(|(name, id)| Ok((name, self.attr_of(&mut inner, &id)?)))
            .collect()
    }

    /// The inode at a `/`-separated path below the root (tools and tests).
    pub fn resolve(&self, path: &str) -> Result<u64, Errno> {
        let mut ino = ROOT_INO;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            ino = self.lookup(ino, part)?.ino;
        }
        Ok(ino)
    }

    // ---- reads ---------------------------------------------------------

    fn flight(&self, key: &RevKey) -> Arc<Mutex<()>> {
        let mut flights = self
            .flights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(flights.entry(*key).or_default())
    }

    /// Make `range` (plus `ahead` bytes after it) of the current revision of
    /// `id` resident; returns where to read it.
    fn hydrate(&self, id: &ItemId, range: Range<u64>, ahead: u64) -> Result<PathBuf, Errno> {
        let mut refreshed = false;
        loop {
            let (revision, size) = {
                let inner = self.inner();
                let item = inner.tree.get(id).ok_or(libc::ENOENT)?;
                (item.revision.clone(), item.size.unwrap_or(0))
            };
            let key = rev_key(id, revision.as_ref());
            let want = range.start..range.end.saturating_add(ahead).min(size);
            let files = self.content().ok_or(libc::EIO)?;
            if let Residency::Complete(path) = files.residency(&key, size, &range)
                && (want.end <= range.end
                    || files
                        .entry(&key)
                        .is_some_and(|e| e.size == size && e.complete()))
            {
                return Ok(path);
            }
            let flight = self.flight(&key);
            let _guard = flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let missing = match files.residency(&key, size, &want) {
                Residency::Complete(path) => return Ok(path),
                Residency::Missing(missing) => missing,
            };
            match self.fetch(id, &key, size, revision.as_ref(), &missing) {
                Ok(()) => {}
                Err(Error::Conflict { .. }) if !refreshed => {
                    // The file changed remotely since it was listed: take
                    // the new metadata and read that version.
                    refreshed = true;
                    let fresh = self.source.stat(id).map_err(|e| errno_of(&e))?;
                    self.apply_upsert(fresh);
                    continue;
                }
                Err(Error::NotFound) => {
                    self.inner().tree.remove(id);
                    return Err(libc::ENOENT);
                }
                Err(error) => {
                    if !matches!(error, Error::Conflict { .. }) {
                        self.report(Health::of(&error));
                    }
                    // What was asked for may still be resident even if the
                    // read-ahead failed.
                    if let Residency::Complete(path) = files.residency(&key, size, &range) {
                        return Ok(path);
                    }
                    return Err(errno_of(&error));
                }
            }
            if let Residency::Complete(path) = files.residency(&key, size, &range) {
                return Ok(path);
            }
            return Err(libc::EIO);
        }
    }

    fn fetch(
        &self,
        id: &ItemId,
        key: &RevKey,
        size: u64,
        revision: Option<&Revision>,
        missing: &[Range<u64>],
    ) -> Result<(), Error> {
        for range in missing {
            let start = range
                .start
                .checked_div(GRANULE)
                .unwrap_or(0)
                .saturating_mul(GRANULE);
            let end = range
                .end
                .div_ceil(GRANULE)
                .saturating_mul(GRANULE)
                .min(size);
            let mut download = self.source.read(id, Some(start..end), revision)?;
            let got = download.range.clone();
            let content = self
                .content()
                .ok_or_else(|| Error::Io(io::Error::other("the volume is shut down")))?;
            content.fill_from(
                key,
                size,
                got.start,
                &mut download.body,
                got.end.saturating_sub(got.start),
            )?;
        }
        Ok(())
    }

    /// Read through an open handle.
    pub fn read(&self, fh: u64, offset: u64, len: u32) -> Result<Vec<u8>, Errno> {
        let (id, ahead) = {
            let mut inner = self.inner();
            let handle = inner.handles.get_mut(&fh).ok_or(libc::EBADF)?;
            let end = offset.saturating_add(u64::from(len));
            handle.window = if offset == handle.seq_end && offset > 0 {
                handle
                    .window
                    .saturating_mul(2)
                    .clamp(GRANULE, READAHEAD_MAX)
            } else {
                0
            };
            handle.seq_end = end;
            (handle.id.clone(), handle.window)
        };
        self.read_id(&id, offset, len, ahead)
    }

    fn read_id(&self, id: &ItemId, offset: u64, len: u32, ahead: u64) -> Result<Vec<u8>, Errno> {
        let (payload, staged, size) = {
            let inner = self.inner();
            let item = inner.tree.get(id).ok_or(libc::ENOENT)?;
            if item.is_folder() {
                return Err(libc::EISDIR);
            }
            let staged = inner
                .local
                .get(id)
                .and_then(|l| l.staged.clone())
                .map(|name| self.staged_path(&name));
            (native_payload(item), staged, item.size.unwrap_or(0))
        };
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        if let Some(payload) = payload {
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(payload.len());
            let end = start.saturating_add(len).min(payload.len());
            return Ok(payload.get(start..end).unwrap_or_default().to_vec());
        }
        if let Some(path) = staged {
            return read_file_at(&path, offset, len).map_err(|e| io_errno(&e));
        }
        if offset >= size {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(len as u64).min(size);
        let path = self.hydrate(id, offset..end, ahead)?;
        read_file_at(&path, offset, len).map_err(|e| io_errno(&e))
    }

    /// Read without a handle (tools and tests).
    pub fn read_ino(&self, ino: u64, offset: u64, len: u32) -> Result<Vec<u8>, Errno> {
        let id = self.inner().id(ino)?;
        self.read_id(&id, offset, len, 0)
    }

    // ---- writes --------------------------------------------------------

    /// Give `id` a staged copy of its current content (or an empty one).
    fn ensure_staged(&self, id: &ItemId, truncate: bool) -> Result<(), Errno> {
        let (has, native, size) = {
            let inner = self.inner();
            let item = inner.tree.get(id).ok_or(libc::ENOENT)?;
            (
                inner.local.get(id).is_some_and(|l| l.staged.is_some()),
                matches!(item.kind, ItemKind::Native(_)),
                item.size.unwrap_or(0),
            )
        };
        if native {
            return Err(libc::EACCES);
        }
        if has {
            if truncate {
                let name = self.inner().local.get(id).and_then(|l| l.staged.clone());
                if let Some(name) = name {
                    File::options()
                        .write(true)
                        .open(self.staged_path(&name))
                        .and_then(|f| f.set_len(0))
                        .map_err(|e| io_errno(&e))?;
                }
            }
            return Ok(());
        }
        let name = uuid::Uuid::new_v4().simple().to_string();
        let path = self.staged_path(&name);
        if truncate || size == 0 {
            File::create(&path).map_err(|e| io_errno(&e))?;
        } else {
            let source = self.hydrate(id, 0..size, 0)?;
            fs::copy(&source, &path).map_err(|e| io_errno(&e))?;
        }
        let mut inner = self.inner();
        let base = inner.tree.get(id).and_then(|item| item.revision.clone());
        let local = inner.local.entry(id.clone()).or_default();
        if local.staged.is_some() {
            drop(inner);
            let _ = fs::remove_file(&path);
            return Ok(());
        }
        local.staged = Some(name);
        if local.base.is_none() {
            local.base = base;
        }
        Ok(())
    }

    pub fn open_file(&self, ino: u64, write: bool, truncate: bool) -> Result<u64, Errno> {
        let id = {
            let inner = self.inner();
            let id = inner.id(ino)?;
            let item = inner.tree.get(&id).ok_or(libc::ENOENT)?;
            if item.is_folder() {
                return Err(libc::EISDIR);
            }
            id
        };
        if write {
            self.ensure_staged(&id, truncate)?;
        }
        let mut inner = self.inner();
        if write {
            let local = inner.local.entry(id.clone()).or_default();
            local.writers = local.writers.saturating_add(1);
            if truncate {
                local.dirty = true;
                local.generation = local.generation.saturating_add(1);
            }
        }
        let fh = inner.next_fh;
        inner.next_fh = inner.next_fh.saturating_add(1);
        inner.handles.insert(
            fh,
            Handle {
                id,
                write,
                seq_end: 0,
                window: 0,
            },
        );
        Ok(fh)
    }

    pub fn write(&self, fh: u64, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        let path = {
            let mut inner = self.inner();
            let handle = inner.handles.get(&fh).ok_or(libc::EBADF)?;
            if !handle.write {
                return Err(libc::EBADF);
            }
            let id = handle.id.clone();
            let local = inner.local.get_mut(&id).ok_or(libc::EIO)?;
            local.dirty = true;
            local.generation = local.generation.saturating_add(1);
            let name = local.staged.clone().ok_or(libc::EIO)?;
            self.staged_path(&name)
        };
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|e| io_errno(&e))?;
        file.write_all_at(data, offset).map_err(|e| io_errno(&e))?;
        Ok(data.len())
    }

    pub fn release(&self, fh: u64) -> Result<(), Errno> {
        let queued = {
            let mut inner = self.inner();
            let Some(handle) = inner.handles.remove(&fh) else {
                return Ok(());
            };
            if !handle.write {
                return Ok(());
            }
            let Some(local) = inner.local.get_mut(&handle.id) else {
                return Ok(());
            };
            local.writers = local.writers.saturating_sub(1);
            let upload = local.writers == 0 && local.dirty;
            if upload {
                inner.queue(Op::Upload(handle.id));
            }
            upload
        };
        if queued {
            self.persist_local()?;
            self.kick();
        }
        Ok(())
    }

    pub fn truncate(&self, ino: u64, size: u64) -> Result<Attr, Errno> {
        let id = self.inner().id(ino)?;
        self.ensure_staged(&id, size == 0)?;
        let mut inner = self.inner();
        let local = inner.local.get_mut(&id).ok_or(libc::EIO)?;
        let name = local.staged.clone().ok_or(libc::EIO)?;
        File::options()
            .write(true)
            .open(self.staged_path(&name))
            .and_then(|f| f.set_len(size))
            .map_err(|e| io_errno(&e))?;
        local.dirty = true;
        local.generation = local.generation.saturating_add(1);
        if local.writers == 0 {
            inner.queue(Op::Upload(id.clone()));
        }
        let attr = self.attr_of(&mut inner, &id)?;
        drop(inner);
        self.persist_local()?;
        self.kick();
        Ok(attr)
    }

    fn new_local(parent: &ItemId, name: &str, kind: ItemKind) -> Item {
        Item {
            id: ItemId::new(format!("{LOCAL_PREFIX}{}", uuid::Uuid::new_v4().simple())),
            parents: vec![parent.clone()],
            name: name.to_owned(),
            kind,
            size: Some(0),
            modified: Some(SystemTime::now()),
            revision: None,
            hashes: Vec::new(),
            mime: None,
        }
    }

    fn check_new_name(inner: &Inner, folder: &ItemId, name: &str) -> Result<(), Errno> {
        if name.is_empty() || name.contains('/') || name.contains('\0') {
            return Err(libc::EINVAL);
        }
        if inner.tree.child(folder, name).is_some() {
            return Err(libc::EEXIST);
        }
        Ok(())
    }

    pub fn create(&self, parent: u64, name: &str) -> Result<(Attr, u64), Errno> {
        let staged = uuid::Uuid::new_v4().simple().to_string();
        File::create(self.staged_path(&staged)).map_err(|e| io_errno(&e))?;
        let mut inner = self.inner();
        let folder = Self::folder_id(&inner, parent)?;
        Self::check_new_name(&inner, &folder, name)?;
        let item = Self::new_local(&folder, name, ItemKind::File);
        let id = item.id.clone();
        inner.tree.upsert(item);
        inner.local.insert(
            id.clone(),
            Local {
                staged: Some(staged),
                base: None,
                dirty: true,
                generation: 1,
                writers: 1,
            },
        );
        let fh = inner.next_fh;
        inner.next_fh = inner.next_fh.saturating_add(1);
        inner.handles.insert(
            fh,
            Handle {
                id: id.clone(),
                write: true,
                seq_end: 0,
                window: 0,
            },
        );
        let attr = self.attr_of(&mut inner, &id)?;
        drop(inner);
        self.persist_local()?;
        Ok((attr, fh))
    }

    pub fn mkdir(&self, parent: u64, name: &str) -> Result<Attr, Errno> {
        let mut inner = self.inner();
        let folder = Self::folder_id(&inner, parent)?;
        Self::check_new_name(&inner, &folder, name)?;
        let mut item = Self::new_local(&folder, name, ItemKind::Folder);
        item.size = None;
        let id = item.id.clone();
        inner.tree.upsert(item);
        inner.queue(Op::Mkdir(id.clone()));
        let attr = self.attr_of(&mut inner, &id)?;
        drop(inner);
        self.persist_local()?;
        self.kick();
        Ok(attr)
    }

    /// Remove `id` locally and queue its remote deletion.
    fn remove_locally(&self, inner: &mut Inner, id: &ItemId) {
        let mut doomed = vec![id.clone()];
        let mut index = 0;
        while let Some(current) = doomed.get(index).cloned() {
            for (_, child) in inner.tree.children(&current) {
                doomed.push(child.id.clone());
            }
            index = index.saturating_add(1);
        }
        for victim in &doomed {
            inner.outbox.retain(|op| match op {
                Op::Delete { .. } => true,
                other => other.id() != victim,
            });
            if let Some(local) = inner.local.remove(victim)
                && let Some(name) = local.staged
            {
                let _ = fs::remove_file(self.staged_path(&name));
            }
        }
        if !is_local(id) {
            let revision = inner
                .tree
                .get(id)
                .filter(|item| item.is_file())
                .and_then(|item| item.revision.clone());
            inner.queue(Op::Delete {
                id: id.clone(),
                revision,
            });
        }
        inner.tree.remove_recursive(id);
    }

    fn remove(&self, parent: u64, name: &str, dir: bool) -> Result<(), Errno> {
        let mut inner = self.inner();
        let folder = Self::folder_id(&inner, parent)?;
        let item = inner
            .tree
            .child(&folder, name)
            .cloned()
            .ok_or(libc::ENOENT)?;
        match (dir, item.is_folder()) {
            (true, false) => return Err(libc::ENOTDIR),
            (false, true) => return Err(libc::EISDIR),
            (true, true) if !inner.tree.children(&item.id).is_empty() => {
                return Err(libc::ENOTEMPTY);
            }
            _ => {}
        }
        self.remove_locally(&mut inner, &item.id);
        drop(inner);
        self.persist_local()?;
        self.kick();
        Ok(())
    }

    pub fn unlink(&self, parent: u64, name: &str) -> Result<(), Errno> {
        self.remove(parent, name, false)
    }

    pub fn rmdir(&self, parent: u64, name: &str) -> Result<(), Errno> {
        self.remove(parent, name, true)
    }

    pub fn rename(
        &self,
        parent: u64,
        name: &str,
        new_parent: u64,
        new_name: &str,
    ) -> Result<(), Errno> {
        let (source, target, from, to) = {
            let inner = self.inner();
            let from = Self::folder_id(&inner, parent)?;
            let to = Self::folder_id(&inner, new_parent)?;
            let source = inner.tree.child(&from, name).cloned().ok_or(libc::ENOENT)?;
            let target = inner.tree.child(&to, new_name).cloned();
            (source, target, from, to)
        };
        if new_name.is_empty() || new_name.contains('/') || new_name.contains('\0') {
            return Err(libc::EINVAL);
        }
        if let Some(target) = &target {
            if target.id == source.id {
                return Ok(());
            }
            match (source.is_folder(), target.is_folder()) {
                (false, true) => return Err(libc::EISDIR),
                (true, false) => return Err(libc::ENOTDIR),
                (true, true) => {
                    if !self.inner().tree.children(&target.id).is_empty() {
                        return Err(libc::ENOTEMPTY);
                    }
                    let mut inner = self.inner();
                    self.remove_locally(&mut inner, &target.id);
                }
                (false, false) => return self.replace(&source, target),
            }
        }
        let mut inner = self.inner();
        let mut moved = source.clone();
        for p in &mut moved.parents {
            if *p == from {
                p.clone_from(&to);
            }
        }
        new_name.clone_into(&mut moved.name);
        inner.tree.upsert(moved);
        if !is_local(&source.id) {
            inner.queue(Op::Move(source.id));
        }
        drop(inner);
        self.persist_local()?;
        self.kick();
        Ok(())
    }

    /// `rename(source, target)` over an existing file: the target keeps
    /// its identity and history and takes the source's content, the way an
    /// editor's atomic save means it.
    fn replace(&self, source: &Item, target: &Item) -> Result<(), Errno> {
        self.ensure_staged(&source.id, false)?;
        let mut inner = self.inner();
        let staged = inner
            .local
            .get_mut(&source.id)
            .and_then(|l| l.staged.take())
            .ok_or(libc::EIO)?;
        let target_base = target.revision.clone();
        let previous = inner.local.remove(&target.id);
        if let Some(old) = previous.as_ref().and_then(|l| l.staged.clone()) {
            let _ = fs::remove_file(self.staged_path(&old));
        }
        let writers = previous.as_ref().map_or(0, |l| l.writers);
        inner.local.insert(
            target.id.clone(),
            Local {
                staged: Some(staged),
                base: previous.and_then(|l| l.base).or(target_base),
                dirty: true,
                generation: 1,
                writers,
            },
        );
        self.remove_locally(&mut inner, &source.id);
        if writers == 0 {
            inner.queue(Op::Upload(target.id.clone()));
        }
        drop(inner);
        self.persist_local()?;
        self.kick();
        Ok(())
    }

    // ---- write-back ----------------------------------------------------

    /// Queued local changes not yet on the server.
    pub fn pending(&self) -> usize {
        self.inner().outbox.len()
    }

    /// Send queued changes until the queue is empty or the server is
    /// unreachable. Returns how many were completed.
    pub fn upload_pending(&self) -> Result<usize, Error> {
        let _serial = self
            .uploading
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut done = 0_usize;
        loop {
            let Some(op) = self.inner().outbox.front().cloned() else {
                return Ok(done);
            };
            if let Err(error) = self.execute(&op) {
                self.report(Health::of(&error));
                return Err(error);
            }
            {
                let mut inner = self.inner();
                if let Some(position) = inner.outbox.iter().position(|o| *o == op) {
                    inner.outbox.remove(position);
                }
            }
            self.save_local()?;
            done = done.saturating_add(1);
        }
    }

    fn execute(&self, op: &Op) -> Result<(), Error> {
        match op {
            Op::Upload(id) => self.upload(id),
            Op::Mkdir(id) => self.push_mkdir(id),
            Op::Move(id) => self.push_move(id),
            Op::Delete { id, revision } => match self.source.delete(id, revision.as_ref()) {
                Ok(()) | Err(Error::NotFound) => Ok(()),
                Err(Error::Conflict { .. }) => {
                    // Changed remotely after the local delete: the newer
                    // content survives.
                    match self.source.stat(id) {
                        Ok(item) => {
                            self.apply_upsert(item);
                            Ok(())
                        }
                        Err(Error::NotFound) => Ok(()),
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            },
        }
    }

    fn parent_ready(item: &Item) -> Option<ItemId> {
        let parent = item.parents.first()?.clone();
        (!is_local(&parent)).then_some(parent)
    }

    fn upload(&self, id: &ItemId) -> Result<(), Error> {
        let (item, staged, base, generation) = {
            let inner = self.inner();
            let Some(item) = inner.tree.get(id).cloned() else {
                return Ok(());
            };
            let Some(local) = inner.local.get(id) else {
                return Ok(());
            };
            if local.writers > 0 {
                // Requeued when the writer closes.
                return Ok(());
            }
            let Some(name) = local.staged.clone() else {
                return Ok(());
            };
            (
                item,
                self.staged_path(&name),
                local.base.clone(),
                local.generation,
            )
        };
        let parent = Self::parent_ready(&item).ok_or(Error::Offline)?;
        let target = if is_local(id) {
            UploadTarget::Create {
                parent: parent.clone(),
                name: item.name.clone(),
            }
        } else {
            UploadTarget::Replace {
                id: id.clone(),
                expect: base,
            }
        };
        match self.send_file(target, &staged) {
            Ok(uploaded) => {
                self.finish_upload(id, &uploaded, &staged, generation);
                Ok(())
            }
            Err(Error::Conflict { .. } | Error::AlreadyExists) => {
                self.keep_both(id, &item, &parent, &staged)
            }
            Err(Error::NotFound) if !is_local(id) => {
                // Deleted remotely while edited here: the edit survives as a
                // conflict copy where the file was.
                self.keep_both(id, &item, &parent, &staged)
            }
            Err(error) => Err(error),
        }
    }

    fn send_file(&self, target: UploadTarget, path: &Path) -> Result<Item, Error> {
        let size = fs::metadata(path)?.len();
        let path = path.to_path_buf();
        cloud_source::upload(self.source.as_ref(), target, size, move |offset| {
            let mut file = File::open(&path)?;
            file.seek(SeekFrom::Start(offset))?;
            Ok(Box::new(file))
        })
    }

    fn finish_upload(&self, id: &ItemId, uploaded: &Item, staged: &Path, generation: u64) {
        let adopt = {
            let mut inner = self.inner();
            Self::remap(&mut inner, id, uploaded.clone());
            let unchanged = inner
                .local
                .get(&uploaded.id)
                .is_some_and(|l| l.generation == generation && l.writers == 0);
            if unchanged {
                inner.local.remove(&uploaded.id);
            } else if let Some(local) = inner.local.get_mut(&uploaded.id) {
                // Written again while uploading: the next upload starts
                // from what was just sent.
                local.base.clone_from(&uploaded.revision);
                if local.writers == 0 {
                    inner.queue(Op::Upload(uploaded.id.clone()));
                }
            }
            unchanged
        };
        if adopt {
            let key = rev_key(&uploaded.id, uploaded.revision.as_ref());
            if let Some(content) = self.content() {
                let _ = content.adopt_file(&key, staged, Retention::Cached);
            }
        }
        self.report(Health::Healthy);
    }

    /// Upload the local content next to the remote version under the
    /// conflict-copy name; the original name shows the remote version.
    fn keep_both(
        &self,
        id: &ItemId,
        item: &Item,
        parent: &ItemId,
        staged: &Path,
    ) -> Result<(), Error> {
        let bytes = hash_file(staged)?;
        let copy_name = String::from_utf8_lossy(
            crate::conflict::conflict_name(item.name.as_bytes(), &iroh_blobs::Hash::from(bytes))
                .as_bytes(),
        )
        .into_owned();
        let copy = match self.send_file(
            UploadTarget::Create {
                parent: parent.clone(),
                name: copy_name.clone(),
            },
            staged,
        ) {
            Ok(copy) => copy,
            // The same bytes were already kept under this name.
            Err(Error::AlreadyExists) => self
                .source
                .list(parent)?
                .into_iter()
                .find(|candidate| candidate.name == copy_name)
                .ok_or(Error::NotFound)?,
            Err(error) => return Err(error),
        };
        let remote = if is_local(id) {
            None
        } else {
            match self.source.stat(id) {
                Ok(remote) => Some(remote),
                Err(Error::NotFound) => None,
                Err(error) => return Err(error),
            }
        };
        {
            let mut inner = self.inner();
            inner.local.remove(id);
            match remote {
                Some(remote) => {
                    inner.tree.upsert(remote);
                }
                None => {
                    inner.tree.remove(id);
                }
            }
            inner.tree.upsert(copy.clone());
        }
        let key = rev_key(&copy.id, copy.revision.as_ref());
        if let Some(content) = self.content() {
            let _ = content.adopt_file(&key, staged, Retention::Cached);
        }
        if is_local(id) {
            // The name belongs to someone else's new file now; list it.
            if let Ok(children) = self.source.list(parent) {
                for child in children {
                    self.apply_upsert(child);
                }
            }
        }
        Ok(())
    }

    fn push_mkdir(&self, id: &ItemId) -> Result<(), Error> {
        let Some(item) = self.inner().tree.get(id).cloned() else {
            return Ok(());
        };
        let parent = Self::parent_ready(&item).ok_or(Error::Offline)?;
        let created = match self.source.create_folder(&parent, &item.name) {
            Ok(created) => created,
            // Someone made the same folder: merge into it.
            Err(Error::AlreadyExists) => self
                .source
                .list(&parent)?
                .into_iter()
                .find(|c| c.name == item.name && c.is_folder())
                .ok_or(Error::AlreadyExists)?,
            Err(error) => return Err(error),
        };
        let mut inner = self.inner();
        Self::remap(&mut inner, id, created);
        Ok(())
    }

    fn push_move(&self, id: &ItemId) -> Result<(), Error> {
        let Some(item) = self.inner().tree.get(id).cloned() else {
            return Ok(());
        };
        let parent = Self::parent_ready(&item).ok_or(Error::Offline)?;
        match self.source.move_item(id, &parent, &item.name, None) {
            Ok(moved) => {
                if moved.id != *id && moved.is_folder() {
                    self.remap_subtree(id, moved)?;
                } else {
                    let mut inner = self.inner();
                    Self::remap(&mut inner, id, moved);
                }
                Ok(())
            }
            // Gone remotely, or the name is taken there: the next sync
            // shows the server's state.
            Err(Error::NotFound | Error::AlreadyExists) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Replace `old` by the server's `item` everywhere: tree, inode, staged
    /// state, queued changes, open handles and children's parent links.
    fn remap(inner: &mut Inner, old: &ItemId, item: Item) {
        let new = item.id.clone();
        if *old == new {
            let local = inner.local.get(old).cloned();
            let mut item = item;
            if local.as_ref().is_some_and(|l| l.staged.is_some()) {
                // Keep showing the local bytes' size until they settle.
                item.size = inner.tree.get(old).and_then(|i| i.size).or(item.size);
            }
            inner.tree.upsert(item);
            return;
        }
        let children: Vec<Item> = inner
            .tree
            .children(old)
            .into_iter()
            .map(|(_, child)| child.clone())
            .collect();
        inner.tree.remove(old);
        inner.tree.upsert(item);
        for mut child in children {
            for parent in &mut child.parents {
                if parent == old {
                    parent.clone_from(&new);
                }
            }
            inner.tree.upsert(child);
        }
        if let Some(ino) = inner.ino_by_id.remove(old) {
            inner.ino_by_id.insert(new.clone(), ino);
            inner.id_by_ino.insert(ino, new.clone());
        }
        if let Some(local) = inner.local.remove(old) {
            inner.local.insert(new.clone(), local);
        }
        for op in &mut inner.outbox {
            if op.id() == old {
                op.id_mut().clone_from(&new);
            }
        }
        for handle in inner.handles.values_mut() {
            if handle.id == *old {
                handle.id.clone_from(&new);
            }
        }
    }

    /// A folder moved on a provider whose ids are paths: every descendant
    /// got a new id too. Match them by name against fresh listings.
    fn remap_subtree(&self, old: &ItemId, moved: Item) -> Result<(), Error> {
        let mut pending = vec![(old.clone(), moved)];
        while let Some((old_id, new_item)) = pending.pop() {
            let old_children: Vec<Item> = {
                let inner = self.inner();
                inner
                    .tree
                    .children(&old_id)
                    .into_iter()
                    .map(|(_, c)| c.clone())
                    .collect()
            };
            let new_id = new_item.id.clone();
            {
                let mut inner = self.inner();
                Self::remap(&mut inner, &old_id, new_item);
            }
            if old_children.iter().all(|c| is_local(&c.id)) {
                continue;
            }
            let fresh = self.source.list(&new_id)?;
            for child in old_children.into_iter().filter(|c| !is_local(&c.id)) {
                if let Some(found) = fresh.iter().find(|f| f.name == child.name) {
                    pending.push((child.id.clone(), found.clone()));
                }
            }
        }
        Ok(())
    }

    // ---- remote → local --------------------------------------------------

    fn apply_upsert(&self, item: Item) {
        let mut inner = self.inner();
        if inner.pending(&item.id) {
            return;
        }
        if let Some(old) = inner.tree.get(&item.id)
            && old.revision != item.revision
            && let Some(content) = self.content()
        {
            let _ = content.remove(&rev_key(&old.id, old.revision.as_ref()));
        }
        inner.tree.upsert(item);
    }

    fn apply_remove(&self, id: &ItemId) {
        let mut inner = self.inner();
        if inner.pending(id) {
            return;
        }
        for item in inner.tree.remove_recursive(id) {
            if let Some(content) = self.content() {
                let _ = content.remove(&rev_key(&item.id, item.revision.as_ref()));
            }
        }
    }

    /// Pull remote changes once: the cursor feed, or a walk of listings.
    pub fn sync_once(&self) -> Result<(), Error> {
        let result = match self.source.capabilities().change_feed {
            ChangeFeed::Cursor => self.sync_feed(),
            ChangeFeed::Poll => {
                let root = self.source.root();
                self.walk(&root)
            }
        };
        if result.is_ok() && self.source.capabilities().quota {
            let quota = self.source.quota().ok();
            *self
                .quota
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = quota;
        }
        match &result {
            Ok(()) => self.report(Health::Healthy),
            Err(error) => self.report(Health::of(error)),
        }
        self.save_tree()?;
        result
    }

    fn sync_feed(&self) -> Result<(), Error> {
        let mut cursor = self.inner().cursor.clone();
        let mut seen: Option<HashSet<ItemId>> = None;
        loop {
            let page = match self.source.changes(cursor.as_ref()) {
                Ok(page) => page,
                Err(Error::CursorExpired) if cursor.is_some() => {
                    cursor = None;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if page.reset {
                seen = Some(HashSet::new());
            }
            for change in page.changes {
                match change {
                    Change::Upsert(item) => {
                        if let Some(seen) = seen.as_mut() {
                            seen.insert(item.id.clone());
                        }
                        self.apply_upsert(item);
                    }
                    Change::Remove(id) => self.apply_remove(&id),
                }
            }
            cursor = Some(page.cursor);
            if !page.has_more {
                break;
            }
        }
        if let Some(seen) = seen {
            let stale: Vec<ItemId> = self
                .inner()
                .tree
                .items()
                .filter(|item| !is_local(&item.id) && !seen.contains(&item.id))
                .map(|item| item.id.clone())
                .collect();
            for id in stale {
                self.apply_remove(&id);
            }
        }
        self.inner().cursor = cursor;
        Ok(())
    }

    fn walk(&self, root: &ItemId) -> Result<(), Error> {
        let recursive = self.source.capabilities().recursive_folder_revisions;
        let mut pending = vec![root.clone()];
        while let Some(folder) = pending.pop() {
            let listing = self.source.list(&folder)?;
            let listed: HashSet<ItemId> = listing.iter().map(|i| i.id.clone()).collect();
            let known: Vec<ItemId> = self
                .inner()
                .tree
                .children(&folder)
                .into_iter()
                .map(|(_, item)| item.id.clone())
                .filter(|id| !is_local(id))
                .collect();
            for id in known.iter().filter(|id| !listed.contains(*id)) {
                self.apply_remove(id);
            }
            for item in listing {
                if item.is_folder() {
                    let unchanged = {
                        let inner = self.inner();
                        recursive
                            && inner.listed.contains(&item.id)
                            && inner
                                .tree
                                .get(&item.id)
                                .is_some_and(|old| old.revision == item.revision)
                    };
                    if !unchanged {
                        pending.push(item.id.clone());
                    }
                }
                self.apply_upsert(item);
            }
            self.inner().listed.insert(folder);
        }
        Ok(())
    }

    // ---- background work -------------------------------------------------

    /// Wake the uploader (a change was queued).
    pub fn kick(&self) {
        let mut queued = self
            .queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *queued = queued.wrapping_add(1);
        drop(queued);
        self.wake.notify_all();
    }

    /// Wait up to `timeout` for a change queued after `seen` (or a stop);
    /// updates `seen`.
    fn wait_queued(&self, seen: &mut u64, timeout: Duration) {
        let queued = self
            .queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (queued, _) = self
            .wake
            .wait_timeout_while(queued, timeout, |current| {
                *current == *seen && !self.stop.load(Ordering::SeqCst)
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *seen = *queued;
    }

    /// Wait up to `timeout` or until stopped.
    fn wait_stop(&self, timeout: Duration) {
        let queued = self
            .queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = self
            .wake
            .wait_timeout_while(queued, timeout, |_| !self.stop.load(Ordering::SeqCst))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }

    /// Run sync and write-back on background threads until
    /// [`Self::shutdown`].
    pub fn spawn_workers(self: &Arc<Self>) -> Vec<std::thread::JoinHandle<()>> {
        let syncer = Arc::clone(self);
        let uploader = Arc::clone(self);
        vec![
            std::thread::spawn(move || syncer.sync_loop()),
            std::thread::spawn(move || uploader.upload_loop()),
        ]
    }

    fn sync_loop(&self) {
        let mut backoff = self.poll_interval;
        while !self.stop.load(Ordering::SeqCst) {
            match self.sync_once() {
                Ok(()) => backoff = self.poll_interval,
                Err(error) => {
                    tracing::info!(%error, "cloud sync failed; retrying");
                    backoff = backoff.saturating_mul(2).min(Duration::from_secs(300));
                }
            }
            if self.stop.load(Ordering::SeqCst) {
                return;
            }
            let notify = self.source.capabilities().notify;
            if notify == Notify::None || self.health() != Some(Health::Healthy) {
                self.wait_stop(backoff);
            } else {
                let cursor = self.inner().cursor.clone();
                if let Err(error) = self.source.wait(cursor.as_ref(), Duration::from_secs(90)) {
                    tracing::info!(%error, "change notification failed");
                    self.wait_stop(backoff);
                }
            }
        }
    }

    fn upload_loop(&self) {
        let mut backoff = Duration::from_secs(2);
        let mut seen = *self
            .queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !self.stop.load(Ordering::SeqCst) {
            match self.upload_pending() {
                Ok(_) => {
                    backoff = Duration::from_secs(2);
                    self.wait_queued(&mut seen, Duration::from_secs(3600));
                }
                Err(error) => {
                    tracing::info!(%error, "cloud upload deferred");
                    self.wait_queued(&mut seen, backoff);
                    backoff = backoff.saturating_mul(2).min(Duration::from_secs(300));
                }
            }
        }
    }

    /// Stop the workers and persist state.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.kick();
        let _ = self.save_local();
        let _ = self.save_tree();
        // Dropping the last handle flushes and closes the cache.
        self.content
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}
