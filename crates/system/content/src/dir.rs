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
    /// Whether an item is flushed to disk before it is published under its address.
    durable: bool,
    state: Mutex<Index>,
    /// Items this backend has hashed and found intact, by the identity their file had then.
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

/// Counts of the expensive operations, so tests can assert they do not happen.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Probes {
    /// Whole-file hashes of an item already in the store.
    pub(crate) verifications: AtomicUsize,
    pub(crate) scans: AtomicUsize,
    /// Directory flushes.
    pub(crate) dir_syncs: AtomicUsize,
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

    /// Also read content from `dir` (same layout) when it is not under this backend's root.
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

    /// The file holding `hash`, under the root or a read-only directory, if one is there.
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
        // From an unknown generation, start somewhere no other backend can have kept: the clock.
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
        #[cfg(test)]
        self.probes.scans.fetch_add(1, Ordering::Relaxed);
        // Whatever happens below, the kept index is no longer known to match any generation.
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
                // Canonical lower-case names only: no aliases, and a deterministic index order.
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

    /// Copy `reader` into a fresh temporary file under the root, hashing as it goes.
    fn receive(&self, reader: &mut dyn Read) -> Result<(PathBuf, String, usize), ContentError> {
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
            if self.durable {
                file.sync_all()?;
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
        #[cfg(test)]
        self.probes.verifications.fetch_add(1, Ordering::Relaxed);
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
}

impl ContentBackend for DirBackend {
    fn put(&self, reader: &mut dyn Read, _mime: &str) -> Result<Stored, ContentError> {
        let (tmp, hash, len) = self.receive(reader)?;
        let stored = Stored {
            hash: hash.clone(),
            len: u64::try_from(len).unwrap_or(u64::MAX),
        };
        let admit = || -> Result<(), ContentError> {
            let Some(path) = Self::path_in(&self.root, &hash) else {
                return Err(ContentError::InvalidHash(hash.clone()));
            };
            // Content already here is an idempotent write, even if a later configuration tightened
            // the limits.
            if self.intact(&path, &hash, usize::MAX) {
                return Ok(());
            }
            let mut index = self
                .state
                .lock()
                .map_err(|_| ContentError::Io("content store lock poisoned".into()))?;
            let root_lock = self.lock_root_for_write()?;
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
            std::fs::rename(&tmp, &path)?;
            // A file under this name that failed verification above was not retained content;
            // the rename replaced it, and with it the quota it held.
            if let Some(old_len) = index.entries.insert(hash.clone(), len) {
                index.bytes = index.bytes.saturating_sub(old_len);
            }
            index.bytes = index.bytes.saturating_add(len);
            self.bump_generation(&mut index, generation);
            // Hashed while it streamed in: `has` need not read it again.
            match std::fs::metadata(&path) {
                Ok(meta) => self.remember_intact(path.clone(), FileSig::of(&meta)),
                Err(_) => self.forget_intact(&path),
            }
            // The rename is durable only once the directory holding the new name is.
            self.sync_dir(dir)?;
            drop(root_lock);
            drop(index);
            Ok(())
        };
        let result = admit();
        // After a successful rename the temporary name is gone and this is a no-op.
        let _ = std::fs::remove_file(&tmp);
        result.map(|()| stored)
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

    fn has(&self, hash: &str) -> bool {
        self.existing(hash)
            .is_some_and(|path| self.intact(&path, hash, self.limits.max_item_bytes))
    }

    fn state(&self, hash: &str) -> ContentState {
        if self.has(hash) {
            ContentState::Resolved
        } else {
            ContentState::Unavailable
        }
    }

    fn collect_unreferenced(&self, live: &BTreeSet<String>) -> Result<Collected, ContentError> {
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
            .filter(|(hash, _)| !live.contains(*hash))
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
