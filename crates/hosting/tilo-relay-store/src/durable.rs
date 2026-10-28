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
use tilo_lock::MutexRecover as _;

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
        use std::sync::{Mutex, OnceLock};
        static SYNCED: OnceLock<Mutex<HashMap<PathBuf, [i64; 7]>>> = OnceLock::new();
        let synced = SYNCED.get_or_init(Mutex::default);
        // Taken before the barrier: a change racing it leaves a stale stamp.
        let before = stamp(path)?;
        if synced.lock_recover().get(path) == Some(&before) {
            return Ok(());
        }
        crate::fsync(&fs::File::open(path)?)?;
        synced.lock_recover().insert(path.to_owned(), before);
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
/// The entry costs the atomic write's own barrier over its parent once, and a
/// writer that finds the parent's entries unchanged since a barrier issues
/// none of its own: a lineage's history of selector writes is one round of
/// barriers plus a `stat` each, however long the history grows.
///
/// # Writers of one directory
///
/// A writer stamps the parent's entries before it writes. A writer that finds
/// a barrier issued for that stamp needs none; one whose stamp has no barrier
/// issues it, and one that finds another writer's stamp in flight waits for
/// that round and stamps the entries again. Only a stamp a barrier was issued
/// over is ever relied on, so a write that lands between another's write and
/// its barrier is still covered by nothing and issues its own.
#[cfg(unix)]
pub fn durable_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let before = entries_before(path);
    let result = crate::atomic_write(path, bytes)
        .map_err(StoreError::from)
        .and_then(|()| match &before {
            Some((parent, stamp)) => cover_entries(parent, stamp),
            None => Ok(()),
        })
        .and_then(|()| {
            // `native::atomic` syncs the parent after its rename, and Apple's
            // fsync leaves the drive's cache unflushed, so there the barrier
            // is repeated as a full sync before destructive retirement relies
            // on it.
            #[cfg(target_vendor = "apple")]
            if let Some(parent) = path.parent() {
                sync_dir(parent)?;
            }
            #[cfg(any(test, feature = "test-support"))]
            if let Some(parent) = path.parent() {
                crate::test_barrier::check(parent, "sync-dir")?;
            }
            Ok(())
        });
    if result.is_err()
        && let Some((_, stamp)) = &before
    {
        abandon(path, stamp);
    }
    result
}

/// [`durable_write`] where directory stamps are unavailable: every write
/// issues a barrier over its parent.
#[cfg(not(unix))]
pub fn durable_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    crate::atomic_write(path, bytes)?;
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

/// Make the entry of the file `path` durable, as a durable write that has
/// already published it leaves it for its caller: the entries of `path`'s
/// parent, joined with everyone who stamped them before the file was made.
///
/// A caller that made `path` and then calls this once holds its entry as
/// safely as [`durable_write`] holds its own. A caller that goes away before
/// it returns leaves a round for the next caller of either to issue.
#[cfg(unix)]
pub fn retire_entries(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    // The entry is there: the stamp now is the one a barrier over it covers.
    let Ok(stamp) = stamp(parent) else {
        return;
    };
    // A caller that cannot issue the barrier leaves its entry as the one it
    // found under; a later barrier over these entries covers it.
    drop(cover_entries(parent, &stamp));
}

/// A directory whose entries a barrier covers, and the barrier that did it.
#[cfg(unix)]
struct Covered {
    stamp: [i64; 7],
    barrier: std::sync::Arc<Barrier>,
}

/// One round of barriers over a directory's entries.
#[cfg(unix)]
#[derive(Default)]
struct Barrier {
    issued: std::sync::Mutex<bool>,
    done: std::sync::Condvar,
}

#[cfg(unix)]
fn covered() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Covered>> {
    static COVERED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, Covered>>,
    > = std::sync::OnceLock::new();
    COVERED.get_or_init(std::sync::Mutex::default)
}

/// A lock a holder that panicked still tells the truth through: a barrier
/// either was issued or was not.
#[cfg(unix)]
fn held<T>(result: std::sync::LockResult<T>) -> T {
    result.unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The stamp of the entries of the directory holding `path`, as they are
/// before a write into it: `None` where stamps cannot be read, and every
/// write then issues a barrier of its own.
#[cfg(unix)]
fn entries_before(path: &Path) -> Option<(PathBuf, [i64; 7])> {
    let parent = path.parent()?;
    Some((parent.to_path_buf(), stamp(parent).ok()?))
}

/// Make the entries of the directory `parent` durable, before returning:
/// issue the round over the ones `before` names, or wait for the round a
/// writer that found the same entries is issuing.
#[cfg(unix)]
fn cover_entries(parent: &Path, before: &[i64; 7]) -> Result<(), StoreError> {
    let mut before = *before;
    loop {
        let mut state = covered().lock_recover();
        match state.get(parent) {
            // The entries this caller found are the ones that round covered.
            Some(current) if current.stamp == before => return Ok(()),
            Some(current) => {
                let barrier = std::sync::Arc::clone(&current.barrier);
                drop(state);
                wait_issued(&barrier);
                // A round covers what it found, not what this caller found
                // before it: look again, and issue one over those entries.
                before = stamp(parent)?;
                continue;
            }
            None => {
                let barrier = std::sync::Arc::new(Barrier::default());
                state.insert(
                    parent.to_path_buf(),
                    Covered {
                        stamp: before,
                        barrier: std::sync::Arc::clone(&barrier),
                    },
                );
                drop(state);
                return issue(parent, &barrier, &before, &before);
            }
        }
    }
}

/// Issue the round `barrier` over the entries a caller stamped `stamped`
/// and found `observed`, and let whoever waits for it look.
///
/// What the round made durable is the entries it was issued over: if a write
/// changed them meanwhile, no round covers them, and the next caller issues
/// its own.
#[cfg(unix)]
fn issue(
    parent: &Path,
    barrier: &std::sync::Arc<Barrier>,
    observed: &[i64; 7],
    stamped: &[i64; 7],
) -> Result<(), StoreError> {
    let result = sync_dir(parent);
    {
        let mut state = covered().lock_recover();
        if let Some(current) = state.get_mut(parent)
            && std::sync::Arc::ptr_eq(&current.barrier, barrier)
        {
            let unchanged = current.stamp == *stamped && result.is_ok();
            if unchanged {
                current.stamp = *observed;
            } else {
                state.remove(parent);
            }
        }
    }
    *held(barrier.issued.lock()) = true;
    barrier.done.notify_all();
    result
}

#[cfg(unix)]
fn wait_issued(barrier: &Barrier) {
    let mut issued = held(barrier.issued.lock());
    while !*issued {
        issued = held(barrier.done.wait(issued));
    }
}

/// Give up the round claimed for the entries a write into `path` was about
/// to change: a write that failed left no entry for it to cover, and one
/// that succeeded changed the entries it was issued over.
#[cfg(unix)]
fn abandon(path: &Path, before: &[i64; 7]) {
    let Some(parent) = path.parent() else {
        return;
    };
    let mut state = covered().lock_recover();
    if let Some(current) = state.get(parent)
        && current.stamp == *before
        && !*held(current.barrier.issued.lock())
    {
        state.remove(parent);
    }
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn cost(work: impl FnOnce()) -> u64 {
        let before = crate::thread_fsyncs();
        work();
        crate::thread_fsyncs() - before
    }

    /// A commit: make a generation, make its entry durable, name it.
    fn commit(lineage: &Path, name: &str) -> u64 {
        let current = lineage.join("CURRENT");
        cost(|| {
            fs::create_dir(lineage.join(name)).unwrap();
            retire_entries(&lineage);
            durable_write(&current, name.as_bytes()).unwrap();
        })
    }

    #[test]
    fn a_selector_naming_unchanged_entries_costs_no_second_barrier() {
        let tmp = tempfile::tempdir().unwrap();
        let lineage = tmp.path().join("lineage");
        fs::create_dir(&lineage).unwrap();
        commit(&lineage, "00000000000000000000000000000001");
        commit(&lineage, "00000000000000000000000000000002");
        // Every commit makes one entry and replaces one: a lineage's history
        // of selectors does not make the rounds of barriers grow.
        let third = commit(&lineage, "00000000000000000000000000000003");
        let fourth = commit(&lineage, "00000000000000000000000000000004");
        assert_eq!(third, fourth);
        let tenth = (5..=10)
            .map(|count| commit(&lineage, &format!("{count:032x}")))
            .max()
            .unwrap();
        assert_eq!(tenth, fourth, "a longer history of selectors costs no more");
    }

    #[test]
    fn a_commit_that_makes_no_entry_costs_one_round() {
        let tmp = tempfile::tempdir().unwrap();
        let lineage = tmp.path().join("lineage");
        fs::create_dir(&lineage).unwrap();
        let current = lineage.join("CURRENT");
        // Nothing made since the last round: the selector is the only write.
        let first = cost(|| durable_write(&current, b"00000000000000000000000000000001").unwrap());
        let second = cost(|| durable_write(&current, b"00000000000000000000000000000002").unwrap());
        assert_eq!(first, second);
    }

    #[test]
    fn a_writer_that_finds_a_barrier_issued_needs_none() {
        let tmp = tempfile::tempdir().unwrap();
        let lineage = tmp.path().join("lineage");
        fs::create_dir(&lineage).unwrap();
        let (parent, stamp) = entries_before(&lineage.join("CURRENT")).unwrap();
        cover_entries(&parent, &stamp).unwrap();
        // The entries are still the ones that round was issued over.
        assert_eq!(cost(|| cover_entries(&parent, &stamp).unwrap()), 0);
        // An entry made since is not covered: the round the last writer
        // issued covers the entries it found, and this one issues its own.
        fs::write(lineage.join("new"), b"x").unwrap();
        assert_eq!(cost(|| cover_entries(&parent, &stamp).unwrap()), 1);
        // What that round made durable is what the next caller finds.
        let (_, now) = entries_before(&lineage.join("CURRENT")).unwrap();
        assert_eq!(cost(|| cover_entries(&parent, &now).unwrap()), 0);
    }

    #[test]
    fn a_retired_entry_is_covered_by_the_next_round() {
        let tmp = tempfile::tempdir().unwrap();
        let lineage = tmp.path().join("lineage");
        fs::create_dir(&lineage).unwrap();
        // The staging path makes its entries and retires them once: a stage
        // costs one round, not one per entry.
        let staged = cost(|| {
            fs::create_dir(lineage.join("stage")).unwrap();
            fs::write(lineage.join("stage").join("checkpoint.delta"), b"x").unwrap();
            fs::write(lineage.join("selection"), b"x").unwrap();
            retire_entries(&lineage);
        });
        assert_eq!(staged, 1);
        // The selector after it finds those entries covered.
        let selecting = cost(|| {
            assert_eq!(cost(|| retire_entries(&lineage)), 0);
            durable_write(&lineage.join("CURRENT"), b"00000000000000000000000000000001").unwrap();
        });
        assert_eq!(selecting, selecting.min(cost(|| {
            durable_write(&lineage.join("CURRENT"), b"00000000000000000000000000000002").unwrap();
        })));
    }

    #[test]
    fn a_failed_write_gives_up_the_round_it_claimed() {
        let tmp = tempfile::tempdir().unwrap();
        let lineage = tmp.path().join("lineage");
        fs::create_dir(&lineage).unwrap();
        let (parent, stamp) = entries_before(&lineage.join("CURRENT")).unwrap();
        cover_entries(&parent, &stamp).unwrap();
        // A write that failed and left no entry leaves the entries as the
        // round it found under covers them.
        abandon(&lineage.join("CURRENT"), &stamp);
        assert_eq!(cost(|| cover_entries(&parent, &stamp).unwrap()), 0);
    }
}
