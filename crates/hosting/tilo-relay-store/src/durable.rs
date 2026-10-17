//! Counted durability barriers and retained generations.
//!
//! A lineage directory holds immutable generations and a `CURRENT` file
//! naming the active one. Writes become visible only through a synced
//! rename of `CURRENT`, and [`GenerationGc`] removes only records it
//! recognises from generations `CURRENT` no longer names.
use crate::StoreError;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

fn invalid(message: &str) -> StoreError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

/// `fsync` of the directory `path`, counted in [`crate::fsyncs`].
///
/// # Errors
///
/// Returns an error when the directory cannot be opened or synced.
pub fn sync_dir(path: &Path) -> Result<(), StoreError> {
    #[cfg(any(test, feature = "test-support"))]
    crate::test_barrier::check(path, "sync-dir")?;
    crate::fsync(&fs::File::open(path)?)?;
    Ok(())
}

/// An entry's identity, size and change times: writing a file, or adding,
/// removing or renaming a directory's entries, changes them.
#[cfg(unix)]
///
/// # Errors
///
/// Returns an error when `path` cannot be inspected.
pub fn stamp(path: &Path) -> io::Result<[i64; 7]> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = fs::metadata(path)?;
    Ok([
        i64::from_ne_bytes(metadata.dev().to_ne_bytes()),
        i64::from_ne_bytes(metadata.ino().to_ne_bytes()),
        i64::from_ne_bytes(metadata.size().to_ne_bytes()),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    ])
}

/// [`sync_dir`], unless this process synced `path` before and its entries
/// have not changed since: a barrier every commit repeats over the same
/// ancestors then costs a `stat` instead of an fsync.
///
/// # Errors
///
/// Returns an error when the directory cannot be inspected or synced.
pub fn sync_dir_once(path: &Path) -> Result<(), StoreError> {
    #[cfg(any(test, feature = "test-support"))]
    crate::test_barrier::check(path, "sync-dir")?;
    sync_once(path)
}

/// `fsync` of the file `path`, unless this process synced the same inode
/// before and it has not been written since. Readers and collection quanta
/// re-establish the durability of an unchanged selection this way.
///
/// # Errors
///
/// Returns an error when the file cannot be inspected or synced.
pub fn sync_file_once(path: &Path) -> Result<(), StoreError> {
    #[cfg(any(test, feature = "test-support"))]
    crate::test_barrier::check(path, "sync-file")?;
    sync_once(path)
}

fn sync_once(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock, PoisonError};
        static SYNCED: OnceLock<Mutex<HashMap<PathBuf, [i64; 7]>>> = OnceLock::new();
        let synced = SYNCED.get_or_init(Mutex::default);
        // Taken before the barrier: a change racing it leaves a stale stamp.
        let before = stamp(path)?;
        if synced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            == Some(&before)
        {
            return Ok(());
        }
        crate::fsync(&fs::File::open(path)?)?;
        synced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_owned(), before);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        crate::fsync(&fs::File::open(path)?)?;
        Ok(())
    }
}

/// Create the directory `path` if it is missing, and make its entry in
/// its parent durable.
///
/// # Errors
///
/// Returns an error when the directory cannot be created or its parent
/// synced.
pub fn create_dir(path: &Path) -> Result<(), StoreError> {
    // Every commit passes here for directories that exist: a stat, rather
    // than a mkdir that fails with EEXIST and a stat.
    if !path.is_dir() {
        match fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
            Err(error) => return Err(error.into()),
        }
    }
    // Also sync existing directories: a previous attempt may have returned
    // after creation but before the parent directory reached stable storage.
    if let Some(parent) = path.parent() {
        sync_dir_once(parent)?;
    }
    Ok(())
}

/// Atomically replace `path` with `bytes`, durably: the file, and its
/// entry in its parent directory.
///
/// # Errors
///
/// Returns an error when the write or a barrier fails.
pub fn durable_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    crate::atomic_write(path, bytes)?;
    // native::atomic fsyncs the parent and fails when that fails. Apple's fsync
    // leaves the drive's cache unflushed, so there the barrier is repeated
    // with F_FULLFSYNC before destructive retirement relies on it.
    #[cfg(any(test, feature = "test-support", target_vendor = "apple"))]
    if let Some(parent) = path.parent() {
        #[cfg(target_vendor = "apple")]
        sync_dir(parent)?;
        #[cfg(any(test, feature = "test-support"))]
        #[cfg(not(target_vendor = "apple"))]
        crate::test_barrier::check(parent, "sync-dir")?;
    }
    Ok(())
}

/// Whether `name` is a generation directory's name: 32 hex digits.
pub fn generation_name(name: &str) -> bool {
    name.len() == 32 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The CURRENT that will select generation `name`, written beside the
/// lineage's CURRENT before it is renamed over it.
pub const SELECTION_PREFIX: &str = ".CURRENT.";

/// The generation a selection [`SELECTION_PREFIX`] file names.
pub fn selection_generation(name: &str) -> Option<&str> {
    name.strip_prefix(SELECTION_PREFIX)
        .filter(|generation| generation_name(generation))
}

/// The generation `lineage`'s `CURRENT` selects, `None` before its first.
///
/// # Errors
///
/// Returns an error when `CURRENT` cannot be read, names no valid
/// generation, or names one that is missing.
pub fn current(lineage: &Path) -> Result<Option<PathBuf>, StoreError> {
    use std::io::Read;
    let mut name = String::new();
    match fs::File::open(lineage.join("CURRENT")) {
        Ok(file) => {
            file.take(33).read_to_string(&mut name)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    if !generation_name(&name) {
        return Err(invalid("invalid retained generation"));
    }
    let generation = lineage.join(name);
    if !fs::symlink_metadata(&generation)?.is_dir() {
        return Err(invalid("missing retained generation"));
    }
    Ok(Some(generation))
}

/// Bounded incremental GC; no recursion and no authority over unknown content.
pub struct GenerationGc {
    lineage: PathBuf,
    entries: crate::bounded::DirectoryCursor,
    child: Option<(PathBuf, crate::bounded::DirectoryCursor)>,
    /// The next quantum's barrier already holds: the caller has just
    /// written CURRENT with [`durable_write`], which synced it and the
    /// lineage after the rename.
    barrier_held: bool,
    /// This collection removed something, which its last quantum syncs.
    removed: bool,
}
impl GenerationGc {
    /// A collection of the generations `lineage`'s `CURRENT` no longer names.
    ///
    /// # Errors
    ///
    /// Returns an error when the lineage cannot be listed.
    pub fn new(lineage: PathBuf) -> Result<Self, StoreError> {
        Ok(Self {
            entries: crate::bounded::DirectoryCursor::open(&lineage)?,
            lineage,
            child: None,
            barrier_held: false,
            removed: false,
        })
    }
    /// Collect after this process just wrote the lineage's CURRENT with
    /// [`durable_write`]: the first quantum needs no barrier of its own.
    ///
    /// # Errors
    ///
    /// Returns an error when the lineage cannot be listed.
    pub fn after_commit(lineage: PathBuf) -> Result<Self, StoreError> {
        Ok(Self {
            barrier_held: true,
            ..Self::new(lineage)?
        })
    }
    /// One quantum of collection.
    ///
    /// # Errors
    ///
    /// Returns an error when `CURRENT` is missing or a barrier, listing or
    /// removal fails.
    pub fn step(
        &mut self,
        limit: crate::bounded::WorkLimit,
    ) -> Result<crate::bounded::Step<()>, StoreError> {
        use crate::bounded::Step;
        let limit = limit.checked()?;
        // Reestablish Task 1's mandatory barrier after reopen and before every quantum.
        if !std::mem::take(&mut self.barrier_held) {
            sync_file_once(&self.lineage.join("CURRENT"))?;
            sync_dir_once(&self.lineage)?;
        }
        let active = current(&self.lineage)?.ok_or_else(|| invalid("missing CURRENT during GC"))?;
        for _ in 0..limit.record_entries() {
            if let Some((path, entries)) = &mut self.child {
                if *path == active {
                    self.child = None;
                    continue;
                }
                if let Some(entry) = entries.peek()? {
                    let name = entry.file_name();
                    if (known_record(&name)
                        || (self
                            .lineage
                            .file_name()
                            .is_some_and(|v| v == crate::shell::DIRECTORY)
                            && (name == "basis.manifest"
                                || name == "live.manifest"
                                || name == "gc.manifest"
                                || name
                                    .to_str()
                                    .and_then(|s| s.strip_prefix("gc-"))
                                    .is_some_and(|s| known_record(std::ffi::OsStr::new(s))))))
                        && entry.file_type()?.is_file()
                    {
                        fs::remove_file(entry.path())?;
                        self.removed = true;
                    }
                    entries.advance();
                } else {
                    match fs::remove_dir(&*path) {
                        Ok(()) => self.removed = true,
                        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {}
                        Err(e) => return Err(e.into()),
                    }
                    self.child = None;
                }
            } else if let Some(entry) = self.entries.peek()? {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if entry.path() != active && entry.file_type()?.is_dir() && generation_name(&name) {
                    self.child = Some((
                        entry.path(),
                        crate::bounded::DirectoryCursor::open(&entry.path())?,
                    ));
                } else if selection_generation(&name).is_some() && entry.file_type()?.is_file() {
                    // A build that failed before its commit left it.
                    fs::remove_file(entry.path())?;
                    self.removed = true;
                }
                self.entries.advance();
            } else {
                // A collection that removed nothing left the lineage as its
                // barrier found it.
                if self.removed {
                    sync_dir_once(&self.lineage)?;
                }
                return Ok(Step::Done(()));
            }
        }
        Ok(Step::More)
    }
}
fn known_record(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        let name = name.strip_suffix(".continuity-v1").unwrap_or(name);
        name == "checkpoint.delta"
            || (name.strip_suffix(".delta").is_some_and(|prefix| {
                matches!(prefix.len(), 8 | 16) && prefix.bytes().all(|b| b.is_ascii_hexdigit())
            }))
    })
}
