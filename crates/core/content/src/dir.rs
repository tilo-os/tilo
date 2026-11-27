//! Content in a local directory, sharded one level on the first two hex digits of the address so
//! no directory accumulates everything the machine has ever stored.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::memory::check_new_item;
use crate::{
    Collected, ContentBackend, ContentError, ContentLimits, ContentReader, ContentState, Stored,
    Verifying, valid_hash,
};

/// Content in `<root>/<hash[..2]>/<hash>`. Writes stream into a temporary file beside the target
/// and are renamed into place once complete and hashed, so a reader never sees a partial item
/// under an address that promises whole content; reads verify as they stream.
///
/// A durable store (the default) publishes an item behind a barrier: the item is flushed before
/// the rename, and the shard directory after it — and the root too when the write created the
/// shard — so an item `put` has returned for survives a power loss under its address.
/// [`DirBackend::regenerable`] stores skip every flush.
///
/// [`ContentBackend::has`] hashes a file once per file identity (inode, length, timestamps) per
/// backend: a file replaced or rewritten since is verified again. The quota total is kept in
/// memory between writes and rescanned only when another writer has written since (every writer
/// bumps `<root>/.content-store.gen` under the root lock), or before refusing a write, since files
/// removed behind the store's back make the kept total over-count.
pub struct DirBackend {
    root: PathBuf,
    /// Directories consulted, read-only, when content is not under `root` — where an earlier
    /// layout kept it.
    also_read: Vec<PathBuf>,
    limits: ContentLimits,
    durable: bool,
    state: Mutex<Index>,
    verified: Mutex<HashMap<PathBuf, FileSig>>,
    #[cfg(test)]
    probes: Probes,
}

#[derive(Default)]
struct Index {
    bytes: usize,
    entries: BTreeMap<String, usize>,
    /// The root's write generation this index was last brought up to date with, or `None` when it
    /// must be rescanned before it is trusted.
    generation: Option<u64>,
}

/// How many items of one batch are received at once: each waits on the disk,
/// so a folder of small files flushes a few in parallel.
const PUT_MANY_WORKERS: usize = 8;

/// Bumped under the root write lock by every writer after it changes the store, so a backend
/// whose kept index names the current generation knows nobody else has written since. Read and
/// written only under that lock; a missing or unreadable file means "rescan".
const GENERATION_FILE: &str = ".content-store.gen";

/// What identifies one version of a file without reading it: a file renamed over the item, or
/// rewritten in place, differs in at least one of these.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileSig {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    ctime_ns: i128,
}

impl FileSig {
    fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            dev: meta.dev(),
            #[cfg(unix)]
            ino: meta.ino(),
            #[cfg(unix)]
            ctime_ns: i128::from(meta.ctime())
                .saturating_mul(1_000_000_000)
                .saturating_add(i128::from(meta.ctime_nsec())),
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct Probes {
    pub(crate) dir_syncs: AtomicUsize,
    pub(crate) file_syncs: AtomicUsize,
}

/// Held around every mutation and the quota scan before it. Separate from [`Index`]: two backends
/// (or two processes) on one root must not each admit a write from a stale local total.
type RootWriteLock = advisory_lock::FileLock;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl DirBackend {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_limits(root, ContentLimits::default())
    }

    pub fn with_limits(root: impl Into<PathBuf>, limits: ContentLimits) -> Self {
        Self {
            root: root.into(),
            also_read: Vec::new(),
            limits,
            durable: true,
            state: Mutex::new(Index::default()),
            verified: Mutex::new(HashMap::new()),
            #[cfg(test)]
            probes: Probes::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn probes(&self) -> &Probes {
        &self.probes
    }

    #[must_use]
    pub fn reading_also(mut self, dir: impl Into<PathBuf>) -> Self {
        self.also_read.push(dir.into());
        self
    }

    /// Hold content its owner can recreate, such as a cache: writes are not flushed to disk, so
    /// a write never waits for the filesystem to flush every other dirty page first. After a
    /// crash an item may be missing or torn; reads verify it against its address, so a torn item
    /// reads as absent.
    #[must_use]
    pub fn regenerable(mut self) -> Self {
        self.durable = false;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn limits(&self) -> ContentLimits {
        self.limits
    }

    fn path_in(root: &Path, hash: &str) -> Option<PathBuf> {
        // The address reaches here from documents and peers: anything but plain hex is refused, since
        // a key with a separator in it would otherwise name a file outside the store.
        if !valid_hash(hash) {
            return None;
        }
        let hash = hash.to_ascii_lowercase();
        let prefix = hash.get(..2)?;
        Some(root.join(prefix).join(&hash))
    }

    fn existing(&self, hash: &str) -> Option<PathBuf> {
        std::iter::once(&self.root)
            .chain(&self.also_read)
            .filter_map(|root| Self::path_in(root, hash))
            .find(|path| path.is_file())
    }

    fn lock_root_for_write(&self) -> Result<RootWriteLock, ContentError> {
        std::fs::create_dir_all(&self.root)?;
        Ok(advisory_lock::FileLock::open(
            &self.root.join(".content-store.lock"),
            advisory_lock::LockMode::Exclusive,
        )?)
    }

    /// The root's write generation. Call only under the root write lock.
    fn read_generation(&self) -> Option<u64> {
        std::fs::read_to_string(self.root.join(GENERATION_FILE))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Record that this backend changed the store, whose generation was `current` (as read under
    /// the root write lock still held), and bring `index` — already updated for the change — to
    /// the new generation.
    fn bump_generation(&self, index: &mut Index, current: Option<u64>) {
        let next = current.map_or_else(
            || tilo_clock::try_unix_now_ns().unwrap_or(1),
            |current| current.wrapping_add(1),
        );
        let path = self.root.join(GENERATION_FILE);
        if std::fs::write(&path, next.to_string()).is_ok() {
            index.generation = Some(next);
        } else {
            // Without a readable generation every backend rescans, which stays correct. A
            // generation left as it was would let another backend trust an index missing this
            // write.
            let _ = std::fs::remove_file(&path);
            index.generation = None;
        }
    }

    /// Bring `index` up to date for a write: trust it when it names the root's current
    /// generation, otherwise rescan. Call only under the root write lock. Returns the generation
    /// read and whether a scan ran.
    fn refresh_index(&self, index: &mut Index) -> Result<(Option<u64>, bool), ContentError> {
        let current = self.read_generation();
        if current.is_some() && index.generation == current {
            return Ok((current, false));
        }
        self.load_index(index)?;
        index.generation = current;
        Ok((current, true))
    }

    /// Flush `dir`'s entries to disk, for a durable store; a regenerable one never waits on it.
    fn sync_dir(&self, dir: &Path) -> Result<(), ContentError> {
        if !self.durable {
            return Ok(());
        }
        #[cfg(test)]
        self.probes.dir_syncs.fetch_add(1, Ordering::Relaxed);
        tilo_files::native::atomic::sync_directory(dir)?;
        Ok(())
    }

    fn load_index(&self, index: &mut Index) -> Result<(), ContentError> {
        index.generation = None;
        let shards = match std::fs::read_dir(&self.root) {
            Ok(shards) => shards,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut scanned = 0usize;
        let mut entries = BTreeMap::new();
        let mut total = 0usize;
        let mut count = || -> Result<(), ContentError> {
            scanned = scanned.saturating_add(1);
            if scanned > self.limits.max_scan_entries {
                return Err(ContentError::ScanLimitExceeded {
                    limit: self.limits.max_scan_entries,
                });
            }
            Ok(())
        };
        for shard in shards {
            let shard = shard?;
            count()?;
            let name = shard.file_name();
            let Some(prefix) = name.to_str() else {
                continue;
            };
            if prefix.len() != 2
                || !prefix.bytes().all(|b| b.is_ascii_hexdigit())
                || !shard.file_type().is_ok_and(|kind| kind.is_dir())
            {
                continue;
            }
            for item in std::fs::read_dir(shard.path())? {
                let item = item?;
                count()?;
                let name = item.file_name();
                let Some(hash) = name.to_str().filter(|hash| {
                    valid_hash(hash) && !hash.bytes().any(|b| b.is_ascii_uppercase())
                }) else {
                    continue;
                };
                if !item.file_type().is_ok_and(|kind| kind.is_file()) {
                    continue;
                }
                let len = usize::try_from(item.metadata()?.len()).map_err(|_| {
                    ContentError::TotalQuotaExceeded {
                        attempted: usize::MAX,
                        limit: self.limits.max_total_bytes,
                    }
                })?;
                total = total.saturating_add(len);
                entries.insert(hash.to_string(), len);
            }
        }
        index.bytes = total;
        index.entries = entries;
        Ok(())
    }

    /// Receive `reader` into a temporary file under the root, answering its
    /// temporary file, hash and length. `sync` flushes it before answering.
    fn receive(
        &self,
        reader: &mut dyn Read,
        sync: bool,
    ) -> Result<(PathBuf, String, usize), ContentError> {
        std::fs::create_dir_all(&self.root)?;
        let serial = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self
            .root
            .join(format!(".incoming-{}-{serial}", std::process::id()));
        let result = (|| -> Result<(String, usize), ContentError> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            let mut hasher = blake3::Hasher::new();
            let mut len = 0usize;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                };
                len = len.saturating_add(n);
                if len > self.limits.max_item_bytes {
                    return Err(ContentError::ItemTooLarge {
                        actual: len,
                        limit: self.limits.max_item_bytes,
                    });
                }
                let chunk = buf.get(..n).unwrap_or_default();
                hasher.update(chunk);
                file.write_all(chunk)?;
            }
            if sync {
                file.sync_all()?;
                #[cfg(test)]
                self.probes.file_syncs.fetch_add(1, Ordering::Relaxed);
            }
            Ok((hasher.finalize().to_hex().to_string(), len))
        })();
        match result {
            Ok((hash, len)) => Ok((tmp, hash, len)),
            Err(error) => {
                let _ = std::fs::remove_file(&tmp);
                Err(error)
            }
        }
    }

    /// Whether `path` holds the content at `hash`, no larger than `limit`. A file this backend has
    /// already found intact, and which has not changed since, is not read again.
    fn intact(&self, path: &Path, hash: &str, limit: usize) -> bool {
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        if !meta.is_file()
            || usize::try_from(meta.len())
                .ok()
                .is_none_or(|len| len > limit)
        {
            return false;
        }
        if self
            .verified
            .lock()
            .is_ok_and(|verified| verified.get(path) == Some(&FileSig::of(&meta)))
        {
            return true;
        }
        let Ok(file) = std::fs::File::open(path) else {
            return false;
        };
        // The identity of the file actually hashed, taken before reading it: a change while it
        // is read leaves a different identity behind, which the next call verifies afresh.
        let Ok(meta) = file.metadata() else {
            return false;
        };
        if usize::try_from(meta.len())
            .ok()
            .is_none_or(|len| len > limit)
        {
            return false;
        }
        let mut reader = Verifying::new(file, hash);
        if std::io::copy(&mut reader, &mut std::io::sink()).is_err() {
            return false;
        }
        self.remember_intact(path.to_path_buf(), FileSig::of(&meta));
        true
    }

    fn remember_intact(&self, path: PathBuf, sig: FileSig) {
        if let Ok(mut verified) = self.verified.lock() {
            verified.insert(path, sig);
        }
    }

    fn forget_intact(&self, path: &Path) {
        if let Ok(mut verified) = self.verified.lock() {
            verified.remove(path);
        }
    }

    /// Admit the received items of [`ContentBackend::put_many`] under one
    /// lock: the index, its generation and each shard directory are written
    /// once for the whole set instead of once per item.
    fn admit_received(
        &self,
        outcomes: &mut [Option<Result<Stored, ContentError>>],
        received: &[(usize, PathBuf, String, usize)],
        flush: BatchFlush,
    ) -> Result<(), String> {
        if received.is_empty() {
            return Ok(());
        }
        let mut index = self
            .state
            .lock()
            .map_err(|_| "content store lock poisoned".to_string())?;
        let root_lock = self
            .lock_root_for_write()
            .map_err(|error| error.to_string())?;
        let (generation, _) = self
            .refresh_index(&mut index)
            .map_err(|error| error.to_string())?;
        let mut shards: BTreeSet<PathBuf> = BTreeSet::new();
        let mut wrote = false;
        for (at, tmp, hash, len) in received {
            let stored = Stored {
                hash: hash.clone(),
                len: u64::try_from(*len).unwrap_or(u64::MAX),
            };
            let Some(path) = Self::path_in(&self.root, hash) else {
                let _ = std::fs::remove_file(tmp);
                note(outcomes, *at, Err(ContentError::InvalidHash(hash.clone())));
                continue;
            };
            if self.intact(&path, hash, usize::MAX) {
                self.written_now(&path);
                let _ = std::fs::remove_file(tmp);
                note(outcomes, *at, Ok(stored));
                continue;
            }
            if check_admission(&self.limits, &index, hash, *len).is_err() {
                // Files removed behind the store's back make the kept total
                // over-count: look again before refusing on it.
                self.load_index(&mut index)
                    .map_err(|error| error.to_string())?;
                if let Err(refusal) = check_admission(&self.limits, &index, hash, *len) {
                    let _ = std::fs::remove_file(tmp);
                    note(outcomes, *at, Err(refusal));
                    continue;
                }
            }
            let Some(dir) = path.parent() else {
                let _ = std::fs::remove_file(tmp);
                note(outcomes, *at, Err(ContentError::InvalidHash(hash.clone())));
                continue;
            };
            let dir = dir.to_path_buf();
            if !dir.is_dir() {
                if let Err(error) = std::fs::create_dir_all(&dir) {
                    let _ = std::fs::remove_file(tmp);
                    note(outcomes, *at, Err(error.into()));
                    continue;
                }
                if flush == BatchFlush::Each {
                    let _ = self.sync_dir(&self.root);
                }
            }
            if let Err(error) = std::fs::rename(tmp, &path) {
                let _ = std::fs::remove_file(tmp);
                note(outcomes, *at, Err(error.into()));
                continue;
            }
            if let Some(old_len) = index.entries.insert(hash.clone(), *len) {
                index.bytes = index.bytes.saturating_sub(old_len);
            }
            index.bytes = index.bytes.saturating_add(*len);
            match std::fs::metadata(&path) {
                Ok(meta) => self.remember_intact(path.clone(), FileSig::of(&meta)),
                Err(_) => self.forget_intact(&path),
            }
            shards.insert(dir);
            wrote = true;
            note(outcomes, *at, Ok(stored));
        }
        if wrote {
            self.bump_generation(&mut index, generation);
        }
        drop(root_lock);
        drop(index);
        if flush == BatchFlush::Each {
            self.sync_dirs(shards);
        }
        Ok(())
    }

    /// Flush everything this backend wrote under its root in one call: the batch's items, their
    /// renames and their directories. The platform flushes a filesystem; elsewhere the items are
    /// flushed one by one instead ([`BatchFlush::Each`]).
    #[cfg(target_os = "linux")]
    fn sync_store(&self) -> Result<(), String> {
        let root = std::fs::File::open(&self.root).map_err(|error| error.to_string())?;
        rustix::fs::syncfs(&root).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Flush the shard directories a batch was renamed into: a few at once,
    /// since a batch spreads over many shards and each flush waits on the
    /// disk.
    fn sync_dirs(&self, shards: BTreeSet<PathBuf>) {
        if shards.is_empty() || !self.durable {
            return;
        }
        let dirs: Vec<PathBuf> = shards.into_iter().collect();
        let workers = PUT_MANY_WORKERS.min(dirs.len()).max(1);
        let per = dirs.len().div_ceil(workers);
        std::thread::scope(|scope| {
            for chunk in dirs.chunks(per) {
                scope.spawn(move || {
                    for dir in chunk {
                        let _ = self.sync_dir(dir);
                    }
                });
            }
        });
    }
}

/// How a batch's items reach the disk.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BatchFlush {
    /// The platform flushes the whole filesystem once, after the batch is in place: nothing is
    /// answered for until it returns.
    Whole,
    /// Each item is flushed as it is received, and the directories it was renamed into after:
    /// where the platform has no filesystem-wide flush.
    Each,
    /// Every flush is skipped: a store that can regenerate what it keeps.
    None,
}

/// Mark item `index`'s outcome of [`DirBackend::admit_received`].
fn note(
    outcomes: &mut [Option<Result<Stored, ContentError>>],
    index: usize,
    result: Result<Stored, ContentError>,
) {
    if let Some(outcome) = outcomes.get_mut(index) {
        *outcome = Some(result);
    }
}

impl ContentBackend for DirBackend {
    fn put(&self, reader: &mut dyn Read, _mime: &str) -> Result<Stored, ContentError> {
        // Flushed only once it is known to be new: content already here, as most of what a
        // warm start puts again, costs no wait on the disk.
        let (tmp, hash, len) = self.receive(reader, false)?;
        let stored = Stored {
            hash: hash.clone(),
            len: u64::try_from(len).unwrap_or(u64::MAX),
        };
        let admit = || -> Result<(), ContentError> {
            let Some(path) = Self::path_in(&self.root, &hash) else {
                return Err(ContentError::InvalidHash(hash.clone()));
            };
            let mut index = self
                .state
                .lock()
                .map_err(|_| ContentError::Io("content store lock poisoned".into()))?;
            let root_lock = self.lock_root_for_write()?;
            // Content already here is an idempotent write, even if a later configuration tightened
            // the limits. It is written again just now, as far as a collection that spares what
            // was written lately goes: under the lock, so no collection removes it in between.
            if self.intact(&path, &hash, usize::MAX) {
                self.written_now(&path);
                return Ok(());
            }
            // The kept index is authority for admission only while no other writer has written
            // since: every writer bumps the generation under this lock.
            let (generation, scanned) = self.refresh_index(&mut index)?;
            if let Err(refusal) = check_admission(&self.limits, &index, &hash, len) {
                // Files removed behind the store's back make a kept total over-count: look again
                // before refusing on it.
                if scanned {
                    return Err(refusal);
                }
                self.load_index(&mut index)?;
                index.generation = generation;
                check_admission(&self.limits, &index, &hash, len)?;
            }
            let Some(dir) = path.parent() else {
                return Err(ContentError::InvalidHash(hash.clone()));
            };
            let created_shard = !dir.is_dir();
            std::fs::create_dir_all(dir)?;
            if created_shard {
                self.sync_dir(&self.root)?;
            }
            if self.durable {
                // Write access, which some platforms need to flush a file.
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&tmp)?
                    .sync_all()?;
                #[cfg(test)]
                self.probes.file_syncs.fetch_add(1, Ordering::Relaxed);
            }
            std::fs::rename(&tmp, &path)?;
            // A file under this name that failed verification above was not retained content;
            // the rename replaced it, and with it the quota it held.
            if let Some(old_len) = index.entries.insert(hash.clone(), len) {
                index.bytes = index.bytes.saturating_sub(old_len);
            }
            index.bytes = index.bytes.saturating_add(len);
            self.bump_generation(&mut index, generation);
            match std::fs::metadata(&path) {
                Ok(meta) => self.remember_intact(path.clone(), FileSig::of(&meta)),
                Err(_) => self.forget_intact(&path),
            }
            self.sync_dir(dir)?;
            drop(root_lock);
            drop(index);
            Ok(())
        };
        let result = admit();
        let _ = std::fs::remove_file(&tmp);
        result.map(|()| stored)
    }

    /// Store `items` together: each is received and flushed as [`Self::put`]
    /// does, and the index, its generation and each shard directory are
    /// written once for the whole set instead of once per item.
    fn put_many(&self, items: &[(&[u8], &str)]) -> Vec<Result<Stored, ContentError>> {
        let started = tilo_clock::now();
        let mut outcomes: Vec<Option<Result<Stored, ContentError>>> =
            (0..items.len()).map(|_| None).collect();
        let mut received: Vec<(usize, PathBuf, String, usize)> = Vec::new();
        // Where the platform flushes a filesystem, the batch's items are written whole and
        // flushed together after they are renamed; elsewhere each item waits on its own flush.
        let flush = if self.durable {
            if cfg!(target_os = "linux") {
                BatchFlush::Whole
            } else {
                BatchFlush::Each
            }
        } else {
            BatchFlush::None
        };
        // The items are written a few at a time: each waits on the disk.
        let workers = PUT_MANY_WORKERS.min(items.len()).max(1);
        let per = items.len().div_ceil(workers);
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for (part, chunk) in items.chunks(per).enumerate() {
                let start = part.saturating_mul(per);
                handles.push(scope.spawn(move || {
                    let mut done = Vec::new();
                    for (offset, (bytes, _mime)) in chunk.iter().enumerate() {
                        let index = start.saturating_add(offset);
                        let entry = self
                            .receive(&mut &bytes[..], flush == BatchFlush::Each)
                            .map_err(|error| (index, error));
                        done.push((index, entry));
                    }
                    done
                }));
            }
            for handle in handles {
                let Ok(done) = handle.join() else {
                    continue;
                };
                for (index, entry) in done {
                    match entry {
                        Ok((tmp, hash, len)) => received.push((index, tmp, hash, len)),
                        Err((index, error)) => note(&mut outcomes, index, Err(error)),
                    }
                }
            }
        });
        let received_ms = started.elapsed().as_millis();
        let admitted = self.admit_received(&mut outcomes, &received, flush);
        let admitted = if admitted.is_ok() && flush == BatchFlush::Whole {
            // Nothing is answered for until the store's filesystem has written the batch: its
            // items, their renames and their directories.
            #[cfg(target_os = "linux")]
            {
                self.sync_store()
            }
            #[cfg(not(target_os = "linux"))]
            {
                Ok(())
            }
        } else {
            admitted
        };
        tracing::debug!(
            items = items.len(),
            received_ms,
            admitted_ms = started.elapsed().as_millis().saturating_sub(received_ms),
            total_ms = started.elapsed().as_millis(),
            "content: a batch was written"
        );
        match admitted {
            Ok(()) => {}
            Err(why) => {
                for (index, tmp, _, _) in &received {
                    if outcomes.get(*index).is_some_and(Option::is_none) {
                        note(&mut outcomes, *index, Err(ContentError::Io(why.clone())));
                    }
                    let _ = std::fs::remove_file(tmp);
                }
            }
        }
        outcomes
            .into_iter()
            .map(|outcome| {
                outcome.unwrap_or_else(|| Err(ContentError::Io("no item was received".into())))
            })
            .collect()
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        if !valid_hash(hash) {
            return Err(ContentError::InvalidHash(hash.to_string()));
        }
        let Some(path) = self.existing(hash) else {
            return Ok(None);
        };
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let declared = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
        // Never stream an arbitrarily large file merely because a peer named its address.
        if declared > self.limits.max_item_bytes {
            return Ok(None);
        }
        let cap = u64::try_from(self.limits.max_item_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        Ok(Some(Box::new(Verifying::new(file.take(cap), hash))))
    }

    fn open_at(&self, hash: &str, offset: u64) -> Result<Option<ContentReader>, ContentError> {
        use std::io::{Seek as _, SeekFrom};
        if !valid_hash(hash) {
            return Err(ContentError::InvalidHash(hash.to_string()));
        }
        let Some(path) = self.existing(hash) else {
            return Ok(None);
        };
        let mut file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        file.seek(SeekFrom::Start(offset))?;
        Ok(Some(Box::new(file)))
    }

    fn has(&self, hash: &str) -> bool {
        self.existing(hash)
            .is_some_and(|path| self.intact(&path, hash, self.limits.max_item_bytes))
    }

    fn size(&self, hash: &str) -> Option<u64> {
        let path = Self::path_in(&self.root, hash)?;
        std::fs::metadata(path).ok().map(|metadata| metadata.len())
    }

    fn state(&self, hash: &str) -> ContentState {
        if self.has(hash) {
            ContentState::Resolved
        } else {
            ContentState::Unavailable
        }
    }

    fn collect_unreferenced(&self, live: &BTreeSet<String>) -> Result<Collected, ContentError> {
        self.remove_where(|hash| !live.contains(hash))
    }

    fn release(&self, hash: &str) -> Result<Collected, ContentError> {
        self.remove_where(|kept| kept == hash)
    }

    fn max_item_bytes(&self) -> usize {
        self.limits.max_item_bytes
    }

    fn spill(&self) -> std::io::Result<Option<Box<dyn crate::SpillFile>>> {
        Ok(Some(Box::new(Spill::open(&self.root)?)))
    }
}

impl DirBackend {
    pub(crate) fn written_before(&self, hash: &str, before: SystemTime) -> bool {
        Self::path_in(&self.root, hash)
            .and_then(|path| std::fs::metadata(path).ok()?.modified().ok())
            .is_some_and(|written| written < before)
    }

    /// Remove every item not in `live` that was last written before `before`: what a store
    /// collects when an item may be referenced only after it is written (an upload a record
    /// names once it is kept). Writing an item that is already here counts as writing it.
    ///
    /// # Errors
    ///
    /// The store could not be read or changed.
    pub fn collect_unreferenced_before(
        &self,
        live: &BTreeSet<String>,
        before: SystemTime,
    ) -> Result<Collected, ContentError> {
        self.remove_where(|hash| !live.contains(hash) && self.written_before(hash, before))
    }

    fn written_now(&self, path: &Path) {
        let touched = std::fs::File::options()
            .append(true)
            .open(path)
            .and_then(|file| {
                file.set_modified(tilo_clock::to_std(tilo_clock::wall()))?;
                file.metadata()
            });
        match touched {
            Ok(meta) => self.remember_intact(path.to_path_buf(), FileSig::of(&meta)),
            Err(_) => self.forget_intact(path),
        }
    }

    fn remove_where(&self, gone: impl Fn(&str) -> bool) -> Result<Collected, ContentError> {
        let mut index = self
            .state
            .lock()
            .map_err(|_| ContentError::Io("content store lock poisoned".into()))?;
        let root_lock = self.lock_root_for_write()?;
        let generation = self.read_generation();
        self.load_index(&mut index)?;
        let stale: Vec<(String, usize)> = index
            .entries
            .iter()
            .filter(|(hash, _)| gone(hash))
            .map(|(hash, len)| (hash.clone(), *len))
            .collect();
        let mut collected = Collected::default();
        for (hash, len) in stale {
            let Some(path) = Self::path_in(&self.root, &hash) else {
                continue;
            };
            self.forget_intact(&path);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    // What was removed before the failure is a change other writers must see.
                    self.bump_generation(&mut index, generation);
                    return Err(error.into());
                }
            }
            index.entries.remove(&hash);
            index.bytes = index.bytes.saturating_sub(len);
            collected.bytes = collected
                .bytes
                .saturating_add(u64::try_from(len).unwrap_or(u64::MAX));
            collected.hashes.push(hash);
        }
        self.bump_generation(&mut index, generation);
        drop(root_lock);
        drop(index);
        Ok(collected)
    }
}

/// Admit `len` new bytes under `hash` against `index`'s total. A file already under the name that
/// failed verification is not retained content and must not count against the write that
/// replaces it.
fn check_admission(
    limits: &ContentLimits,
    index: &Index,
    hash: &str,
    len: usize,
) -> Result<(), ContentError> {
    let (bytes, entries) = match index.entries.get(hash) {
        Some(old_len) => (
            index.bytes.saturating_sub(*old_len),
            index.entries.len().saturating_sub(1),
        ),
        None => (index.bytes, index.entries.len()),
    };
    check_new_item(limits, bytes, entries, len)
}

static SPILL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The file the plaintext of an item larger than memory waits in while it is hashed and sealed:
/// beside the store's own content, never in a system temporary directory, which a device may
/// hold in RAM. Removed when the write ends, however it ends.
pub(crate) struct Spill {
    file: std::fs::File,
    path: PathBuf,
}

impl Spill {
    pub(crate) fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let serial = SPILL_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(".sealing-{}-{serial}", std::process::id()));
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self { file, path })
    }
}

impl std::io::Read for Spill {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl std::io::Write for Spill {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl std::io::Seek for Spill {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.file, position)
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
