//! Content in a local directory, sharded one level on the first two hex digits of the address so
//! no directory accumulates everything the machine has ever stored.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::memory::check_new_item;
use crate::{
    Collected, ContentBackend, ContentError, ContentLimits, ContentReader, Stored, Verifying,
    valid_hash,
};

/// Content in `<root>/<hash[..2]>/<hash>`. Writes stream into a temporary file beside the target
/// and are renamed into place once complete and hashed, so a reader never sees a partial item
/// under an address that promises whole content; reads verify as they stream.
pub struct DirBackend {
    root: PathBuf,
    /// Directories consulted, read-only, when content is not under `root` — where an earlier
    /// layout kept it.
    also_read: Vec<PathBuf>,
    limits: ContentLimits,
    /// Whether an item is flushed to disk before it is published under its address.
    durable: bool,
    state: Mutex<Index>,
}

#[derive(Default)]
struct Index {
    bytes: usize,
    entries: BTreeMap<String, usize>,
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
        }
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

    fn load_index(&self, index: &mut Index) -> Result<(), ContentError> {
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

    fn intact(path: &Path, hash: &str, limit: usize) -> bool {
        let Ok(file) = std::fs::File::open(path) else {
            return false;
        };
        if file
            .metadata()
            .ok()
            .and_then(|meta| usize::try_from(meta.len()).ok())
            .is_none_or(|len| len > limit)
        {
            return false;
        }
        let mut reader = Verifying::new(file, hash);
        std::io::copy(&mut reader, &mut std::io::sink()).is_ok()
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
            if Self::intact(&path, &hash, usize::MAX) {
                return Ok(());
            }
            let mut index = self
                .state
                .lock()
                .map_err(|_| ContentError::Io("content store lock poisoned".into()))?;
            let root_lock = self.lock_root_for_write()?;
            // Rescan under the root lock: the cached index is not authority for admission, since
            // another process may have written since this backend last looked.
            self.load_index(&mut index)?;
            if let Some(old_len) = index.entries.remove(&hash) {
                // A file under this name that failed verification above is not retained content;
                // it must not keep consuming quota. The rename below replaces it.
                index.bytes = index.bytes.saturating_sub(old_len);
            }
            check_new_item(&self.limits, index.bytes, index.entries.len(), len)?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::rename(&tmp, &path)?;
            index.bytes = index.bytes.saturating_add(len);
            index.entries.insert(hash.clone(), len);
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
            .is_some_and(|path| Self::intact(&path, hash, self.limits.max_item_bytes))
    }

    fn collect_unreferenced(&self, live: &BTreeSet<String>) -> Result<Collected, ContentError> {
        let mut index = self
            .state
            .lock()
            .map_err(|_| ContentError::Io("content store lock poisoned".into()))?;
        let root_lock = self.lock_root_for_write()?;
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
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            index.entries.remove(&hash);
            index.bytes = index.bytes.saturating_sub(len);
            collected.bytes = collected
                .bytes
                .saturating_add(u64::try_from(len).unwrap_or(u64::MAX));
            collected.hashes.push(hash);
        }
        drop(root_lock);
        drop(index);
        Ok(collected)
    }
}
