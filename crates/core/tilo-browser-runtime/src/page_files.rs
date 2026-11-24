//! A store's files on a page: held in memory and committed later, in order.
//!
//! A page cannot wait for its database while it answers, so a
//! [`PageBackend`] answers every change at once from the files it holds in
//! memory and commits what changed afterwards, one commit at a time: a
//! commit holds every change made before it, and the next starts only once
//! it completed, so what is durable is always every change up to some
//! moment. A file is kept as parts of [`PART`] bytes and a commit writes only
//! the parts that changed, so appending to a long file writes its last part,
//! not the file. An [`Appender::sync`] fails until a completed commit holds
//! the bytes it is asked for, and [`Backend::durable`] commits what changed
//! and resolves once that is durable.
//!
//! The commit itself is the [`Committer`]'s: the page hands it to a worker
//! that writes it to `IndexedDB` (`web/peer-files-worker.mjs`), packed as one
//! buffer ([`pack_file`]) so the page thread copies it once and moves it,
//! rather than handing the database each part. A commit of many files is
//! packed a slice at a time, the page running between slices, and what
//! changed meanwhile joins it at the end: the page never waits a frame on a
//! commit, and a commit still holds every file as it stood at one moment.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use tilo_peer_state::store::{Appender, Backend, Durable, MemoryBackend, ReadAt};

/// What a panic left behind a lock is still the files: nothing here leaves them half changed.
trait Recover<T> {
    fn lock_recover(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> Recover<T> for Mutex<T> {
    fn lock_recover(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The bytes of one stored part of a file; its last part may be shorter.
pub(crate) const PART: usize = 64 * 1024;

/// How many files, or bytes, a commit packs before it lets the page run:
/// well under a frame of the page's time.
const SLICE_FILES: usize = 256;
const SLICE_BYTES: usize = 1 << 20;

/// Writes a packed commit where the files are kept.
pub(crate) trait Committer: collab_types::signer::MaybeSendSync + 'static {
    /// Write `commit` ([`pack`]) in one transaction, resolving once it is durable.
    fn commit(&self, commit: Vec<u8>) -> Durable;

    /// Run `commit` on its own, after the change that asked for it.
    fn later(&self, commit: Durable);

    /// Resolve once the page has run what waits for it.
    fn pause(&self) -> Durable;
}

/// A store's files held in memory, committed through a [`Committer`].
pub(crate) struct PageBackend<C> {
    files: MemoryBackend,
    pending: Mutex<Pending>,
    /// One commit at a time: the next waits for the one before to complete.
    turn: tokio::sync::Mutex<()>,
    committer: C,
    this: Weak<Self>,
}

#[derive(Default)]
struct Pending {
    /// The files changed since the commit that took them, each from the
    /// lowest byte a change since then touched.
    changed: HashMap<PathBuf, u64>,
    /// How many of each file's first bytes are durable.
    committed: HashMap<PathBuf, u64>,
    /// Whether a commit of what changed is on its way.
    scheduled: bool,
}

impl<C> std::fmt::Debug for PageBackend<C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PageBackend")
            .field("changed", &self.pending.lock_recover().changed.len())
            .finish_non_exhaustive()
    }
}

impl<C: Committer> PageBackend<C> {
    /// The files `rows` ([`unpack`]) hold, every one durable, committed
    /// through `committer` as they change.
    pub(crate) fn open(rows: &[u8], committer: C) -> io::Result<Arc<Self>> {
        let files = unpack(rows)?;
        let durable = files
            .iter()
            .map(|(path, bytes)| (path.clone(), bytes.len() as u64))
            .collect();
        Ok(Arc::new_cyclic(|this| Self {
            files: MemoryBackend::holding(files),
            pending: Mutex::new(Pending {
                committed: durable,
                ..Pending::default()
            }),
            turn: tokio::sync::Mutex::new(()),
            committer,
            this: this.clone(),
        }))
    }

    /// Whether every change so far is durable.
    pub(crate) fn settled(&self) -> bool {
        self.pending.lock_recover().changed.is_empty()
    }

    /// `path` changed from byte `from` on; a commit follows on its own.
    fn changed(&self, path: &Path, from: u64) {
        let mut pending = self.pending.lock_recover();
        let low = pending.changed.entry(path.to_path_buf()).or_insert(from);
        *low = (*low).min(from);
        if let Some(committed) = pending.committed.get_mut(path) {
            *committed = (*committed).min(from);
        }
        let schedule = !std::mem::replace(&mut pending.scheduled, true);
        drop(pending);
        if schedule && let Some(this) = self.this.upgrade() {
            self.committer.later(Box::pin(async move {
                this.pending.lock_recover().scheduled = false;
                this.commit().await
            }));
        }
    }

    /// Commit everything changed so far, after the commit before it.
    async fn commit(self: Arc<Self>) -> io::Result<()> {
        let _turn = self.turn.lock().await;
        let mut taken = std::mem::take(&mut self.pending.lock_recover().changed);
        if taken.is_empty() {
            return Ok(());
        }
        let mut packing = Packing::default();
        for (path, from) in &taken {
            if self.pack(&mut packing, path, *from) {
                self.pause().await;
            }
        }
        // What changed while the page ran joins the commit, packed with no
        // pause, so the commit holds every file as it stands now; a file
        // packed again is written again from where it changed.
        let caught_up = std::mem::take(&mut self.pending.lock_recover().changed);
        for (path, from) in caught_up {
            self.pack(&mut packing, &path, from);
            let low = taken.entry(path).or_insert(from);
            *low = (*low).min(from);
        }
        let done = self.committer.commit(packing.commit).await;
        // What follows is a slice at a time too: a change meanwhile is
        // recorded in `changed`, which each file's account reads.
        let mut booked = 0_usize;
        if let Err(error) = done {
            // Changed still: the next commit writes them.
            for (path, from) in taken {
                self.restore(path, from);
                booked = booked.saturating_add(1);
                if booked.is_multiple_of(SLICE_FILES) {
                    self.pause().await;
                }
            }
            return Err(error);
        }
        for (path, len) in packing.lengths {
            self.account(path, len);
            booked = booked.saturating_add(1);
            if booked.is_multiple_of(SLICE_FILES) {
                self.pause().await;
            }
        }
        Ok(())
    }

    /// `path` changed from `from` on still, after a commit that failed.
    fn restore(&self, path: PathBuf, from: u64) {
        self.pending
            .lock_recover()
            .changed
            .entry(path)
            .and_modify(|low| *low = (*low).min(from))
            .or_insert(from);
    }

    /// Account for `path` committed at `len` bytes, or gone (`None`).
    fn account(&self, path: PathBuf, len: Option<u64>) {
        let mut pending = self.pending.lock_recover();
        if let Some(len) = len {
            // A change since the commit took the file keeps what it
            // changed from durable no further than where it changed.
            let durable = pending
                .changed
                .get(&path)
                .map_or(len, |since| len.min(*since));
            pending.committed.insert(path, durable);
        } else {
            // Gone, and no part of it stored.
            pending.committed.remove(&path);
        }
    }

    /// Let the page run. A page that cannot be let run is not waited on:
    /// the commit goes on.
    async fn pause(&self) {
        self.committer.pause().await.unwrap_or_default();
    }

    /// Pack `path` from byte `from` into the commit; whether the slice is
    /// full and the page should run before the next.
    fn pack(&self, packing: &mut Packing, path: &Path, from: u64) -> bool {
        let packed = packing.commit.len();
        let len = self.files.with_file(path, |bytes| {
            pack_file(&mut packing.commit, path, from, Some(bytes));
            bytes.len() as u64
        });
        // A file made and gone between two commits was never stored: there
        // is nothing to let go of, unless this commit packed it already.
        if len.is_none()
            && (packing.lengths.get(path).is_some_and(Option::is_some)
                || self.pending.lock_recover().committed.contains_key(path))
        {
            pack_file(&mut packing.commit, path, 0, None);
        }
        packing.lengths.insert(path.to_path_buf(), len);
        packing.files = packing.files.saturating_add(1);
        packing.bytes = packing
            .bytes
            .saturating_add(packing.commit.len().saturating_sub(packed));
        let full = packing.files >= SLICE_FILES || packing.bytes >= SLICE_BYTES;
        if full {
            (packing.files, packing.bytes) = (0, 0);
        }
        full
    }

    fn whole(&self, path: &Path) -> u64 {
        self.files
            .with_file(path, |bytes| bytes.len() as u64)
            .unwrap_or(0)
    }
}

impl<C: Committer> Backend for PageBackend<C> {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files.read(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadAt>> {
        self.files.open_read(path)
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<(PathBuf, bool)>> {
        self.files.list(dir)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.files.is_dir(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.files.is_file(path)
    }

    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.files.create_dir_all(dir)
    }

    fn open_append(&self, path: &Path) -> io::Result<Box<dyn Appender>> {
        let inner = self.files.open_append(path)?;
        let backend = self
            .this
            .upgrade()
            .ok_or_else(|| io::Error::other("the page's files are closed"))?;
        Ok(Box::new(PageAppender {
            inner,
            backend,
            path: path.to_path_buf(),
        }))
    }

    fn create(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.replace(path, bytes)
    }

    fn replace(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.files.replace(path, bytes)?;
        self.changed(path, 0);
        Ok(())
    }

    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.files.truncate(path, len)?;
        self.changed(path, len);
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let moved: Vec<PathBuf> = if self.files.is_file(from) {
            vec![from.to_path_buf()]
        } else {
            self.files.files_under(from)
        };
        self.files.rename(from, to)?;
        for path in moved {
            let target = path
                .strip_prefix(from)
                .map_or_else(|_| to.to_path_buf(), |rest| to.join(rest));
            self.changed(&path, 0);
            self.changed(&target, 0);
        }
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        self.files.remove(path)?;
        self.changed(path, 0);
        Ok(())
    }

    fn remove_all(&self, dir: &Path) -> io::Result<()> {
        let gone = self.files.files_under(dir);
        self.files.remove_all(dir)?;
        for path in gone {
            self.changed(&path, 0);
        }
        Ok(())
    }

    /// Commits are in order and each holds every change before it, so the
    /// commit any later sync waits for holds what `dir` holds now.
    fn sync_dir(&self, _dir: &Path) -> io::Result<()> {
        Ok(())
    }

    fn durable(&self) -> Durable {
        match self.this.upgrade() {
            Some(this) => Box::pin(this.commit()),
            None => Box::pin(std::future::ready(Err(io::Error::other(
                "the page's files are closed",
            )))),
        }
    }
}

/// A commit being packed: its bytes, each file's length in it (`None` when
/// gone), and how much the current slice packed.
#[derive(Default)]
struct Packing {
    commit: Vec<u8>,
    lengths: HashMap<PathBuf, Option<u64>>,
    files: usize,
    bytes: usize,
}

struct PageAppender<C> {
    inner: Box<dyn Appender>,
    backend: Arc<PageBackend<C>>,
    path: PathBuf,
}

impl<C: Committer> Appender for PageAppender<C> {
    fn append(&self, bytes: &[u8]) -> io::Result<()> {
        let at = self.backend.whole(&self.path);
        self.inner.append(bytes)?;
        self.backend.changed(&self.path, at);
        Ok(())
    }

    fn truncate(&self, len: u64) -> io::Result<()> {
        self.inner.truncate(len)?;
        self.backend.changed(&self.path, len);
        Ok(())
    }

    fn sync(&self, through: u64) -> io::Result<()> {
        let committed = self
            .backend
            .pending
            .lock_recover()
            .committed
            .get(&self.path)
            .copied()
            .unwrap_or(0);
        if committed >= through {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "{} holds {committed} durable bytes of the {through} asked for: its commit has not completed",
                self.path.display()
            ),
        ))
    }
}

/// Pack a file's parts from the one holding byte `from` on, and where its
/// parts end, into `commit`: per file its path (`u32` length, UTF-8), the
/// part its stored parts end before (`u64`), how many parts follow (`u32`),
/// then each part's number (`u64`), length (`u32`) and bytes, little-endian.
/// An empty file is one empty part; a file gone (`None`) is none.
fn pack_file(commit: &mut Vec<u8>, path: &Path, from: u64, bytes: Option<&[u8]>) {
    let name = path.to_string_lossy();
    let parts: Vec<&[u8]> = match bytes {
        Some([]) => vec![&[]],
        Some(bytes) => bytes.chunks(PART).collect(),
        None => Vec::new(),
    };
    let count = parts.len();
    let first = usize::try_from(from)
        .ok()
        .and_then(|from| from.checked_div(PART))
        .map_or(count, |first| first.min(count));
    put_u32(commit, name.len());
    commit.extend_from_slice(name.as_bytes());
    commit.extend_from_slice(&u64::try_from(count).unwrap_or(u64::MAX).to_le_bytes());
    put_u32(commit, count.saturating_sub(first));
    for (part, bytes) in parts.into_iter().enumerate().skip(first) {
        commit.extend_from_slice(&u64::try_from(part).unwrap_or(u64::MAX).to_le_bytes());
        put_u32(commit, bytes.len());
        commit.extend_from_slice(bytes);
    }
}

fn put_u32(into: &mut Vec<u8>, value: usize) {
    into.extend_from_slice(&u32::try_from(value).unwrap_or(u32::MAX).to_le_bytes());
}

/// The files of `rows` as the database keeps them: each row its path
/// (`u32` length, UTF-8), part number (`u64`), length (`u32`) and bytes,
/// little-endian, in any order. A file is its parts from the first, every
/// one whole but its last.
///
/// # Errors
///
/// The rows do not read, or a file misses a part.
pub(crate) fn unpack(rows: &[u8]) -> io::Result<Vec<(PathBuf, Vec<u8>)>> {
    let broken = |what: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the page's stored files: {what}"),
        )
    };
    let mut parts: BTreeMap<(String, u64), &[u8]> = BTreeMap::new();
    let mut rest = rows;
    while !rest.is_empty() {
        let (name, after) = take(rest, 4)
            .and_then(|(len, after)| {
                let len = u32::from_le_bytes(len.try_into().ok()?) as usize;
                take(after, len)
            })
            .ok_or_else(|| broken("a row is cut short"))?;
        let name = std::str::from_utf8(name).map_err(|_| broken("a path is not UTF-8"))?;
        let (part, after) = take(after, 8).ok_or_else(|| broken("a row is cut short"))?;
        let part = u64::from_le_bytes(part.try_into().map_err(|_| broken("a row is cut short"))?);
        let (bytes, after) = take(after, 4)
            .and_then(|(len, after)| take(after, u32::from_le_bytes(len.try_into().ok()?) as usize))
            .ok_or_else(|| broken("a row is cut short"))?;
        parts.insert((name.to_owned(), part), bytes);
        rest = after;
    }
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut last: Option<(String, u64, usize)> = None;
    for ((name, part), bytes) in parts {
        let continues = last
            .as_ref()
            .is_some_and(|(previous, _, _)| *previous == name);
        if continues {
            let (_, previous, len) = last.as_ref().map_or((String::new(), 0, 0), Clone::clone);
            if previous.checked_add(1) != Some(part) || len != PART {
                return Err(broken(&format!("{name} misses a part before {part}")));
            }
        } else if part != 0 {
            return Err(broken(&format!("{name} misses its first part")));
        }
        if continues && let Some((_, file)) = files.last_mut() {
            file.extend_from_slice(bytes);
        } else {
            files.push((PathBuf::from(&name), bytes.to_vec()));
        }
        last = Some((name, part, bytes.len()));
    }
    Ok(files)
}

fn take(bytes: &[u8], len: usize) -> Option<(&[u8], &[u8])> {
    (bytes.len() >= len).then(|| bytes.split_at(len))
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;
    use tilo_peer_state::store::{Appetite, Store};

    /// Each part of each file, by path and part number.
    pub(crate) type Rows = BTreeMap<(String, u64), Vec<u8>>;

    /// What the page does while a commit lets it run.
    pub(crate) type Meanwhile = Box<dyn FnOnce() + Send>;

    /// A database for the tests: each row by path and part, written whole
    /// per commit or, when told to fail, not at all. It counts the parts
    /// each commit wrote, and the times a commit let the page run.
    #[derive(Clone, Default)]
    pub(crate) struct Database {
        pub(crate) rows: Arc<Mutex<Rows>>,
        pub(crate) fail: Arc<Mutex<bool>>,
        pub(crate) commits: Arc<Mutex<Vec<usize>>>,
        pub(crate) pauses: Arc<Mutex<usize>>,
        /// What the page does the next time a commit lets it run.
        pub(crate) meanwhile: Arc<Mutex<Option<Meanwhile>>>,
    }

    impl Database {
        /// The rows packed as a page's worker hands them over.
        pub(crate) fn packed(&self) -> Vec<u8> {
            let mut packed = Vec::new();
            for ((name, part), bytes) in self.rows.lock_recover().iter() {
                put_u32(&mut packed, name.len());
                packed.extend_from_slice(name.as_bytes());
                packed.extend_from_slice(&part.to_le_bytes());
                put_u32(&mut packed, bytes.len());
                packed.extend_from_slice(bytes);
            }
            packed
        }

        fn apply(&self, commit: &[u8]) -> io::Result<()> {
            if *self.fail.lock_recover() {
                return Err(io::Error::other("the disk is full"));
            }
            let mut rows = self.rows.lock_recover();
            let mut rest = commit;
            let mut written = 0;
            while !rest.is_empty() {
                let read_u32 =
                    |bytes: &[u8]| u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
                let read_u64 = |bytes: &[u8]| u64::from_le_bytes(bytes[..8].try_into().unwrap());
                let len = read_u32(rest);
                let name = std::str::from_utf8(&rest[4..4 + len]).unwrap().to_owned();
                rest = &rest[4 + len..];
                let end = read_u64(rest);
                let count = read_u32(&rest[8..]);
                rest = &rest[12..];
                rows.retain(|(row, part), _| *row != name || *part < end);
                for _ in 0..count {
                    let part = read_u64(rest);
                    let len = read_u32(&rest[8..]);
                    rows.insert((name.clone(), part), rest[12..12 + len].to_vec());
                    rest = &rest[12 + len..];
                    written += 1;
                }
            }
            drop(rows);
            self.commits.lock_recover().push(written);
            Ok(())
        }
    }

    impl Committer for Database {
        fn commit(&self, commit: Vec<u8>) -> Durable {
            Box::pin(std::future::ready(self.apply(&commit)))
        }

        fn later(&self, _commit: Durable) {}

        fn pause(&self) -> Durable {
            *self.pauses.lock_recover() += 1;
            let meanwhile = self.meanwhile.lock_recover().take();
            if let Some(meanwhile) = meanwhile {
                meanwhile();
            }
            Box::pin(std::future::ready(Ok(())))
        }
    }

    pub(crate) fn now<T>(future: impl std::future::Future<Output = T>) -> T {
        match std::pin::pin!(future)
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("the commit did not complete"),
        }
    }

    pub(crate) fn store(database: &Database) -> (Arc<PageBackend<Database>>, Store) {
        let backend = PageBackend::open(&database.packed(), database.clone()).unwrap();
        let store =
            Store::open_on(Arc::clone(&backend) as Arc<dyn Backend>, Appetite::StoreAll).unwrap();
        (backend, store)
    }

    /// A long file is kept as parts, a commit writes only the parts that
    /// changed, and what was committed reads back whole.
    #[test]
    fn a_commit_writes_only_the_parts_that_changed() {
        let database = Database::default();
        let (backend, store) = store(&database);
        // What a store writes as it opens is committed on its own.
        now(backend.durable()).unwrap();
        let long: Vec<u8> = (0..PART * 3 + 10).map(|at| (at % 251) as u8).collect();
        store.keep_state("peer/workspaces", &long).unwrap();
        now(backend.durable()).unwrap();
        assert_eq!(database.commits.lock_recover().last(), Some(&4));
        let mut longer = long;
        longer.extend_from_slice(b"one more record");
        store.keep_state("peer/workspaces", &longer).unwrap();
        store.keep_state("peer/record", b"record").unwrap();
        now(backend.durable()).unwrap();
        // `keep_state` replaces the file: every part again, and the record.
        assert_eq!(database.commits.lock_recover().last(), Some(&5));
        store.keep_state("peer/workspaces", b"short").unwrap();
        now(backend.durable()).unwrap();
        assert_eq!(
            database
                .rows
                .lock_recover()
                .keys()
                .filter(|(name, _)| name.ends_with("workspaces"))
                .count(),
            1,
            "the parts after a file's end go"
        );
        store.forget_state("peer/record").unwrap();
        now(backend.durable()).unwrap();
        drop(store);
        let (_, reopened) = super::tests::store(&database);
        assert_eq!(
            reopened.state("peer/workspaces").unwrap(),
            Some(b"short".to_vec())
        );
        assert_eq!(reopened.state("peer/record").unwrap(), None);
    }

    /// A commit that failed keeps its changes for the next one, which
    /// writes them; nothing is durable before then.
    #[test]
    fn a_failed_commit_leaves_its_changes_to_the_next() {
        let database = Database::default();
        let (backend, store) = store(&database);
        store.keep_state("page/kept", b"before").unwrap();
        now(backend.durable()).unwrap();
        *database.fail.lock_recover() = true;
        store.keep_state("page/kept", b"after").unwrap();
        assert!(now(backend.durable()).is_err());
        assert!(!backend.settled());
        assert_eq!(
            super::tests::store(&database).1.state("page/kept").unwrap(),
            Some(b"before".to_vec())
        );
        *database.fail.lock_recover() = false;
        now(backend.durable()).unwrap();
        assert!(backend.settled());
        assert_eq!(
            super::tests::store(&database).1.state("page/kept").unwrap(),
            Some(b"after".to_vec())
        );
    }

    /// Ten thousand files changed at once are committed in slices, the page
    /// running between them, and every one is stored; a file made and gone
    /// before a commit costs it nothing, and an empty one stays.
    #[test]
    fn a_large_commit_lets_the_page_run_between_its_slices() {
        let database = Database::default();
        let (backend, store) = store(&database);
        now(backend.durable()).unwrap();
        for n in 0..10_000 {
            store
                .keep_state(&format!("page/record-{n}"), &[7; 96])
                .unwrap();
        }
        store.keep_state("page/empty", b"").unwrap();
        now(backend.durable()).unwrap();
        assert!(*database.pauses.lock_recover() >= 10_000 / SLICE_FILES);
        assert_eq!(
            database.commits.lock_recover().last(),
            Some(&10_001),
            "a part per record, none for what a write left behind"
        );
        drop(store);
        let (_, reopened) = super::tests::store(&database);
        assert_eq!(
            reopened.state("page/record-9999").unwrap(),
            Some(vec![7; 96])
        );
        assert_eq!(reopened.state("page/empty").unwrap(), Some(Vec::new()));
    }

    /// What the page changes while a commit lets it run joins that commit:
    /// files it packed and the page then removed are not stored, nor are
    /// those it removed before they were packed, and a file the page wrote
    /// meanwhile is.
    #[test]
    fn a_commit_holds_what_changed_while_the_page_ran() {
        let database = Database::default();
        let backend = PageBackend::open(&[], database.clone()).unwrap();
        let names: Vec<PathBuf> = (0..SLICE_FILES * 3)
            .map(|n| PathBuf::from(format!("x/{n}")))
            .collect();
        for name in &names {
            backend.create(name, b"made").unwrap();
        }
        let page = Arc::clone(&backend);
        *database.meanwhile.lock_recover() = Some(Box::new(move || {
            for name in &names {
                page.remove(name).unwrap();
            }
            page.create(Path::new("x/late"), b"written meanwhile")
                .unwrap();
        }));
        now(backend.durable()).unwrap();
        assert!(backend.settled());
        assert_eq!(
            database
                .rows
                .lock_recover()
                .keys()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>(),
            ["x/late"],
            "the commit holds the files as they stood when it was handed over"
        );
        let reopened = PageBackend::open(&database.packed(), Database::default()).unwrap();
        assert_eq!(
            reopened.read(Path::new("x/late")).unwrap(),
            b"written meanwhile"
        );
    }

    #[test]
    fn stored_files_that_miss_a_part_are_refused() {
        let database = Database::default();
        database
            .rows
            .lock_recover()
            .insert(("state/x".into(), 1), vec![1]);
        assert!(PageBackend::open(&database.packed(), database.clone()).is_err());
        database
            .rows
            .lock_recover()
            .insert(("state/x".into(), 0), vec![1]);
        assert!(
            PageBackend::open(&database.packed(), database).is_err(),
            "a short part before another"
        );
    }
}
