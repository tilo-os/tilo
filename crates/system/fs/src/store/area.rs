//! The file-backed half of the content store.
//!
//! Content larger than a few chunks is not split into one blob per chunk.
//! It lives in one file per entry, at the entry's own byte offsets, sparse
//! until its ranges arrive:
//!
//! - an **entry** is keyed by 32 bytes: a content id (the manifest hash of
//!   a file version) or a caller's key (a cloud account's revision);
//! - its **residency** is a [`RangeSet`] of the bytes present, persisted,
//!   so partial content survives restarts and a transfer resumes where it
//!   stopped. A range is recorded only after its bytes are durable;
//! - a **chunk index** maps every chunk hash of a content entry to where it
//!   lives, so a chunk is served, deduplicated and read from the entry's
//!   file without a second copy;
//! - a **retention** class decides eviction: [`Retention::Cached`] entries
//!   are dropped least recently used first when the resident bytes exceed
//!   the byte budget; [`Retention::LocalOnly`] (content only this device
//!   holds) and [`Retention::Pinned`] entries never are.
//!
//! Entries enter by being filled range by range, or whole: a complete file
//! is cloned in (`FICLONE`, sharing extents with the source on a
//! reflink-capable filesystem, a copy elsewhere), or bytes are written.
//! Complete keyed entries with identical bytes share extents the same way.
//!
//! Nothing here verifies content: callers write only verified bytes, and
//! the store re-verifies chunks against their hash before serving them.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use fs_content::RangeSet;
use redb::{Database, MultimapTableDefinition, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

const ENTRIES: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("area_entries");
/// Chunk hash → `key ‖ offset (u64 LE) ‖ len (u32 LE)`.
const CHUNKS: MultimapTableDefinition<[u8; 32], [u8; 44]> =
    MultimapTableDefinition::new("area_chunks");
/// Entry key → its chunk index values, for removing them with the entry.
const MEMBERS: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("area_members");

/// Open file handles kept for reads and fills.
const OPEN_FILES: usize = 256;
/// Buffer for filling from a reader or copying without a kernel copy.
const COPY_BUFFER: usize = 1024 * 1024;
/// Newly resident bytes of one entry that may be recorded in memory only.
/// A crash loses at most this much of a transfer's progress, and it is
/// what one sync and one record cover.
const SYNC_BATCH: u64 = 256 * 1024 * 1024;

pub type AreaKey = [u8; 32];

/// How an entry may be evicted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Retention {
    /// A copy of content held elsewhere: evicted first, least recently used
    /// first, when the budget is exceeded.
    Cached,
    /// Content this device may be the only holder of. Never evicted by the
    /// budget.
    LocalOnly,
    /// Kept offline on request. Never evicted by the budget.
    Pinned,
}

/// Where one chunk's bytes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkLocation {
    pub key: AreaKey,
    pub offset: u64,
    pub len: u32,
}

impl ChunkLocation {
    fn encode(&self) -> [u8; 44] {
        let mut out = [0_u8; 44];
        let (key, rest) = out.split_at_mut(32);
        key.copy_from_slice(&self.key);
        let (offset, len) = rest.split_at_mut(8);
        offset.copy_from_slice(&self.offset.to_le_bytes());
        len.copy_from_slice(&self.len.to_le_bytes());
        out
    }

    fn decode(raw: &[u8; 44]) -> Self {
        let (key, rest) = raw.split_at(32);
        let (offset, len) = rest.split_at(8);
        Self {
            key: key.try_into().unwrap_or([0; 32]),
            offset: u64::from_le_bytes(offset.try_into().unwrap_or([0; 8])),
            len: u32::from_le_bytes(len.try_into().unwrap_or([0; 4])),
        }
    }

    pub const fn range(&self) -> Range<u64> {
        self.offset..self.offset.saturating_add(self.len as u64)
    }
}

/// What the area knows about one entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryInfo {
    pub size: u64,
    pub resident: u64,
    pub retention: Retention,
}

impl EntryInfo {
    pub const fn complete(&self) -> bool {
        self.resident >= self.size
    }
}

/// Where a read of an entry can be served from.
#[derive(Debug, PartialEq, Eq)]
pub enum Residency {
    /// Every requested byte is in the file at this path.
    Complete(PathBuf),
    /// These ranges of the request are missing.
    Missing(Vec<Range<u64>>),
}

#[derive(Serialize, Deserialize)]
struct Stored {
    size: u64,
    resident: Vec<(u64, u64)>,
    retention: Retention,
    last_use: u64,
    /// BLAKE3 of the complete bytes of a keyed entry, for sharing extents
    /// between entries with the same bytes.
    digest: Option<[u8; 32]>,
}

struct Entry {
    size: u64,
    resident: RangeSet,
    retention: Retention,
    last_use: u64,
    digest: Option<[u8; 32]>,
    /// Bytes recorded resident in memory since the last durable record.
    unsynced: u64,
    /// What the last durable record holds.
    durable: RangeSet,
    /// Changes whenever the entry's file is created anew, so a write into
    /// an earlier file is never recorded against the new one.
    generation: u64,
}

impl Entry {
    fn stored(&self) -> Stored {
        self.stored_with(&self.resident)
    }

    fn stored_with(&self, resident: &RangeSet) -> Stored {
        Stored {
            size: self.size,
            resident: resident.iter().map(|r| (r.start, r.end)).collect(),
            retention: self.retention,
            last_use: self.last_use,
            digest: self.digest,
        }
    }

    fn info(&self) -> EntryInfo {
        EntryInfo {
            size: self.size,
            resident: self.resident.total(),
            retention: self.retention,
        }
    }
}

#[derive(Default)]
struct State {
    entries: HashMap<AreaKey, Entry>,
    handles: HashMap<AreaKey, Arc<File>>,
    clock: u64,
    generations: u64,
    /// Complete keyed entries by the BLAKE3 of their bytes.
    digests: HashMap<[u8; 32], AreaKey>,
}

impl State {
    fn next_generation(&mut self) -> u64 {
        self.generations = self.generations.saturating_add(1);
        self.generations
    }

    fn tick(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(1);
        self.clock
    }
}

pub struct FileArea {
    data: PathBuf,
    /// The area's index; `None` once [`FileArea::close`] released it.
    db: std::sync::RwLock<Option<Arc<Database>>>,
    state: Mutex<State>,
    /// Serialises persistence so an older snapshot of an entry never
    /// overwrites a newer one.
    commit: Mutex<()>,
    /// Serialises creating and replacing entries: two creators of one entry
    /// must not both create its file, or the later would truncate bytes the
    /// earlier already wrote and recorded.
    create: Mutex<()>,
    /// Distinguishes the staging files of concurrent adoptions.
    staging: AtomicU64,
    budget: AtomicU64,
    /// Whether data is synced and the index is on disk. An ephemeral area
    /// (an in-memory content store's) is gone with its process, so syncing
    /// it would only cost a disk flush per write.
    durable: bool,
}

fn hex(key: &AreaKey) -> String {
    hex::encode(key)
}

fn db_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn range_set(ranges: &[(u64, u64)]) -> RangeSet {
    let mut set = RangeSet::new();
    for (start, end) in ranges {
        set.insert(*start..*end);
    }
    set
}

impl FileArea {
    /// Makes `file`'s data durable, in a durable area.
    fn sync(&self, file: &File) -> io::Result<()> {
        if self.durable {
            file.sync_data()
        } else {
            Ok(())
        }
    }

    /// The area's index database, unless the area was closed.
    fn database(&self) -> io::Result<Arc<Database>> {
        self.db
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| io::Error::other("the content area is closed"))
    }

    /// Releases the area's index database, so that the same directory can
    /// be opened again in this process while handles to this area remain.
    /// Every later operation on this area fails.
    pub fn close(&self) {
        self.db
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    /// Opens (or creates) the area under `dir`, holding at most `budget`
    /// resident bytes of cached entries.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory or its database cannot be opened.
    pub fn open(dir: &Path, budget: u64) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let db = Database::create(dir.join("area.redb")).map_err(db_error)?;
        Self::open_with(dir, db, budget, true)
    }

    /// An area whose files live under `dir` but whose index is in memory
    /// and whose data is never synced: for a store that does not outlive
    /// its process.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be created.
    pub fn open_ephemeral(dir: &Path, budget: u64) -> io::Result<Self> {
        let db = Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .map_err(db_error)?;
        Self::open_with(dir, db, budget, false)
    }

    fn open_with(dir: &Path, db: Database, budget: u64, durable: bool) -> io::Result<Self> {
        let data = dir.join("data");
        std::fs::create_dir_all(&data)?;
        {
            let txn = db.begin_write().map_err(db_error)?;
            txn.open_table(ENTRIES).map_err(db_error)?;
            txn.open_multimap_table(CHUNKS).map_err(db_error)?;
            txn.open_table(MEMBERS).map_err(db_error)?;
            txn.commit().map_err(db_error)?;
        }
        let area = Self {
            data,
            db: std::sync::RwLock::new(Some(Arc::new(db))),
            state: Mutex::new(State::default()),
            commit: Mutex::new(()),
            create: Mutex::new(()),
            staging: AtomicU64::new(0),
            budget: AtomicU64::new(budget),
            durable,
        };
        area.load()?;
        Ok(area)
    }

    /// Reads the persisted entries, dropping those whose file is gone and
    /// files that belong to no entry (left by a crash before the entry was
    /// recorded).
    fn load(&self) -> io::Result<()> {
        let mut entries = HashMap::new();
        let mut clock = 0_u64;
        {
            let txn = self.database()?.begin_read().map_err(db_error)?;
            let table = txn.open_table(ENTRIES).map_err(db_error)?;
            for row in table.iter().map_err(db_error)? {
                let (key, value) = row.map_err(db_error)?;
                let Ok(stored) = postcard::from_bytes::<Stored>(value.value()) else {
                    continue;
                };
                clock = clock.max(stored.last_use);
                entries.insert(
                    key.value(),
                    Entry {
                        size: stored.size,
                        resident: range_set(&stored.resident),
                        retention: stored.retention,
                        last_use: stored.last_use,
                        digest: stored.digest,
                        unsynced: 0,
                        durable: range_set(&stored.resident),
                        generation: 0,
                    },
                );
            }
        }
        let mut gone = Vec::new();
        entries.retain(|key, entry| {
            let present = std::fs::metadata(self.path_of(key)).is_ok_and(|m| m.len() == entry.size);
            if !present {
                gone.push(*key);
            }
            present
        });
        for key in gone {
            self.unpersist(&key)?;
        }
        if let Ok(files) = std::fs::read_dir(&self.data) {
            for file in files.flatten() {
                let name = file.file_name();
                let known = name
                    .to_str()
                    .and_then(parse_hex)
                    .is_some_and(|key| entries.contains_key(&key));
                if !known {
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
        let mut state = self.lock();
        state.digests = entries
            .iter()
            .filter_map(|(key, entry)| entry.digest.map(|d| (d, *key)))
            .collect();
        state.entries = entries;
        state.clock = clock;
        drop(state);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The file holding `key`'s bytes.
    pub fn path_of(&self, key: &AreaKey) -> PathBuf {
        self.data.join(hex(key))
    }

    pub fn budget(&self) -> u64 {
        self.budget.load(Ordering::Relaxed)
    }

    pub fn set_budget(&self, bytes: u64) {
        self.budget.store(bytes, Ordering::Relaxed);
    }

    pub fn entry(&self, key: &AreaKey) -> Option<EntryInfo> {
        self.lock().entries.get(key).map(Entry::info)
    }

    /// Every entry key.
    pub fn keys(&self) -> Vec<AreaKey> {
        self.lock().entries.keys().copied().collect()
    }

    /// Resident bytes of cached entries (what the budget limits) and of all
    /// entries.
    pub fn used(&self) -> (u64, u64) {
        let state = self.lock();
        state.entries.values().fold((0, 0), |(cached, all), e| {
            let resident = e.resident.total();
            (
                if e.retention == Retention::Cached {
                    cached.saturating_add(resident)
                } else {
                    cached
                },
                all.saturating_add(resident),
            )
        })
    }

    /// Parts of `range` of `key` that are not resident (all of it when the
    /// area has no such entry).
    pub fn missing(&self, key: &AreaKey, range: &Range<u64>) -> Vec<Range<u64>> {
        let state = self.lock();
        match state.entries.get(key) {
            Some(entry) => {
                let clipped = range.start.min(entry.size)..range.end.min(entry.size);
                entry.resident.missing(&clipped)
            }
            None if range.is_empty() => Vec::new(),
            None => vec![range.clone()],
        }
    }

    /// Where `range` of `key` (an entry of `size` bytes) can be read, and
    /// marks the entry as used.
    pub fn residency(&self, key: &AreaKey, size: u64, range: &Range<u64>) -> Residency {
        let clipped = range.start.min(size)..range.end.min(size);
        let missing = {
            let mut state = self.lock();
            let now = state.tick();
            match state.entries.get_mut(key) {
                Some(entry) if entry.size == size => {
                    entry.last_use = now;
                    Some(entry.resident.missing(&clipped))
                }
                _ => None,
            }
        };
        match missing {
            Some(missing) if missing.is_empty() => Residency::Complete(self.path_of(key)),
            Some(missing) => Residency::Missing(missing),
            None if size == 0 => match self.ensure(key, 0, Retention::Cached, &[]) {
                Ok(()) => Residency::Complete(self.path_of(key)),
                Err(_) => Residency::Missing(Vec::new()),
            },
            None if clipped.is_empty() => Residency::Missing(Vec::new()),
            None => Residency::Missing(vec![clipped]),
        }
    }

    /// Makes sure `key` has an entry of `size` bytes, creating an empty
    /// (sparse) one if needed. An entry with another size is replaced. A
    /// stricter retention than the entry has is applied. `chunks` indexes
    /// the chunks of a content entry: `(hash, offset, len)`.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be created or the entry cannot
    /// be recorded.
    pub fn ensure(
        &self,
        key: &AreaKey,
        size: u64,
        retention: Retention,
        chunks: &[([u8; 32], u64, u32)],
    ) -> io::Result<()> {
        let _create = self
            .create
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let existing = {
            let mut state = self.lock();
            state.entries.get_mut(key).map(|entry| {
                let same = entry.size == size;
                let stricter = same && retention > entry.retention;
                if stricter {
                    entry.retention = retention;
                }
                (same, stricter)
            })
        };
        match existing {
            Some((true, stricter)) => {
                return if stricter { self.persist(key) } else { Ok(()) };
            }
            Some((false, _)) => self.remove(key)?,
            None => {}
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(self.path_of(key))?;
        file.set_len(size)?;
        {
            let mut state = self.lock();
            let now = state.tick();
            let generation = state.next_generation();
            state.entries.insert(
                *key,
                Entry {
                    size,
                    resident: RangeSet::new(),
                    retention,
                    last_use: now,
                    digest: None,
                    unsynced: 0,
                    durable: RangeSet::new(),
                    generation,
                },
            );
            state.handles.insert(*key, Arc::new(file));
        }
        self.index_chunks(key, chunks)?;
        self.persist(key)
    }

    fn index_chunks(&self, key: &AreaKey, chunks: &[([u8; 32], u64, u32)]) -> io::Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut members = Vec::with_capacity(chunks.len().saturating_mul(76));
        let txn = self.database()?.begin_write().map_err(db_error)?;
        {
            let mut index = txn.open_multimap_table(CHUNKS).map_err(db_error)?;
            for (hash, offset, len) in chunks {
                let location = ChunkLocation {
                    key: *key,
                    offset: *offset,
                    len: *len,
                }
                .encode();
                index.insert(*hash, location).map_err(db_error)?;
                members.extend_from_slice(hash);
                members.extend_from_slice(&location);
            }
            let mut table = txn.open_table(MEMBERS).map_err(db_error)?;
            table.insert(*key, members.as_slice()).map_err(db_error)?;
        }
        txn.commit().map_err(db_error)
    }

    /// Where chunk `hash` is resident, if it is.
    pub fn find_chunk(&self, hash: &[u8; 32]) -> Option<ChunkLocation> {
        let candidates: Vec<ChunkLocation> = {
            let txn = self.database().ok()?.begin_read().ok()?;
            let index = txn.open_multimap_table(CHUNKS).ok()?;
            let values = index.get(*hash).ok()?;
            values
                .filter_map(Result::ok)
                .map(|v| ChunkLocation::decode(&v.value()))
                .collect()
        };
        let state = self.lock();
        candidates.into_iter().find(|c| {
            state
                .entries
                .get(&c.key)
                .is_some_and(|e| e.resident.contains(&c.range()))
        })
    }

    /// The open file of `key`.
    ///
    /// # Errors
    ///
    /// Returns an error when the area has no such entry or its file cannot
    /// be opened.
    pub fn file(&self, key: &AreaKey) -> io::Result<Arc<File>> {
        {
            let state = self.lock();
            if !state.entries.contains_key(key) {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no such entry"));
            }
            if let Some(file) = state.handles.get(key) {
                return Ok(Arc::clone(file));
            }
        }
        let file = Arc::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(self.path_of(key))?,
        );
        let mut state = self.lock();
        if state.handles.len() >= OPEN_FILES
            && let Some(victim) = state.handles.keys().next().copied()
        {
            state.handles.remove(&victim);
        }
        state.handles.insert(*key, Arc::clone(&file));
        drop(state);
        Ok(file)
    }

    /// Writes `bytes` at `offset` of `key` without marking them resident.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry is missing, the write would pass its
    /// end, or the write fails.
    pub fn write_at(&self, key: &AreaKey, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let size = self
            .entry(key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such entry"))?
            .size;
        if offset.saturating_add(bytes.len() as u64) > size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write past the end of the entry",
            ));
        }
        self.file(key)?.write_all_at(bytes, offset)
    }

    /// Records `ranges` of `key` (bytes already written to its file) as
    /// resident. They are readable at once. The record reaches disk after
    /// the file's data has been synced: with `durable` now, otherwise once
    /// [`SYNC_BATCH`] bytes have accumulated or the entry is complete, so a
    /// recorded range is never one whose bytes a crash could lose.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry is missing or the sync or record
    /// fails.
    pub fn mark_resident(
        &self,
        key: &AreaKey,
        ranges: &[Range<u64>],
        durable: bool,
    ) -> io::Result<()> {
        self.mark_resident_in(key, None, ranges, durable)
    }

    /// The current generation of `key`'s file.
    pub fn generation(&self, key: &AreaKey) -> Option<u64> {
        self.lock().entries.get(key).map(|e| e.generation)
    }

    /// The file of `key` together with its generation, for writes that are
    /// recorded later with [`Self::mark_resident_in`].
    ///
    /// # Errors
    ///
    /// Returns an error when the area has no such entry or its file cannot
    /// be opened.
    pub fn file_in_generation(&self, key: &AreaKey) -> io::Result<(Arc<File>, u64)> {
        let generation = || self.lock().entries.get(key).map(|e| e.generation);
        let before = generation();
        let file = self.file(key)?;
        match (before, generation()) {
            (Some(a), Some(b)) if a == b => Ok((file, a)),
            _ => Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "the entry was replaced",
            )),
        }
    }

    /// [`Self::mark_resident`] for bytes written into the file of
    /// `generation`: when the entry has since been replaced (evicted and
    /// created anew), nothing is recorded.
    ///
    /// # Errors
    ///
    /// As [`Self::mark_resident`].
    pub fn mark_resident_in(
        &self,
        key: &AreaKey,
        generation: Option<u64>,
        ranges: &[Range<u64>],
        durable: bool,
    ) -> io::Result<()> {
        if ranges.is_empty() {
            return Ok(());
        }
        let snapshot = {
            let mut guard = self.lock();
            let state = &mut *guard;
            let now = state.tick();
            let entry = state
                .entries
                .get_mut(key)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such entry"))?;
            if generation.is_some_and(|g| g != entry.generation) {
                drop(guard);
                return Ok(());
            }
            let before = entry.resident.total();
            for range in ranges {
                entry
                    .resident
                    .insert(range.start.min(entry.size)..range.end.min(entry.size));
            }
            entry.last_use = now;
            let added = entry.resident.total().saturating_sub(before);
            entry.unsynced = entry.unsynced.saturating_add(added);
            let due =
                durable || entry.unsynced >= SYNC_BATCH || entry.resident.total() >= entry.size;
            let snapshot = due.then(|| {
                entry.unsynced = 0;
                entry.stored()
            });
            drop(guard);
            snapshot
        };
        match snapshot {
            Some(stored) => self.persist_synced(key, &stored),
            None => Ok(()),
        }
    }

    /// Syncs `key`'s data, then records `stored`, a snapshot of its state
    /// taken before the sync: ranges recorded later are not covered by it.
    fn persist_synced(&self, key: &AreaKey, stored: &Stored) -> io::Result<()> {
        self.sync(self.file(key)?.as_ref())?;
        self.persist_stored(key, stored)?;
        let mut state = self.lock();
        if let Some(entry) = state.entries.get_mut(key) {
            let recorded = range_set(&stored.resident);
            if recorded.total() > entry.durable.total() {
                entry.durable = recorded;
            }
        }
        drop(state);
        Ok(())
    }

    /// Reads `len` bytes at `offset` of `key`. Every byte must be resident.
    ///
    /// # Errors
    ///
    /// `NotFound` when part of the range is not resident, or the read's
    /// error.
    pub fn read_at(&self, key: &AreaKey, offset: u64, len: u64) -> io::Result<Vec<u8>> {
        let range = offset..offset.saturating_add(len);
        if !self.missing(key, &range).is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "range is not resident",
            ));
        }
        self.touch(key);
        let size = self.entry(key).map_or(0, |e| e.size);
        let len = len.min(size.saturating_sub(offset));
        let mut out = vec![0_u8; usize::try_from(len).map_err(io::Error::other)?];
        self.file(key)?.read_exact_at(&mut out, offset)?;
        Ok(out)
    }

    /// Marks `key` as used now, for least-recently-used eviction.
    pub fn touch(&self, key: &AreaKey) {
        let mut state = self.lock();
        let now = state.tick();
        if let Some(entry) = state.entries.get_mut(key) {
            entry.last_use = now;
        }
    }

    /// Changes `key`'s retention.
    ///
    /// # Errors
    ///
    /// Returns an error when the change cannot be recorded.
    pub fn set_retention(&self, key: &AreaKey, retention: Retention) -> io::Result<()> {
        let changed = {
            let mut state = self.lock();
            match state.entries.get_mut(key) {
                Some(entry) if entry.retention != retention => {
                    entry.retention = retention;
                    true
                }
                _ => false,
            }
        };
        if changed { self.persist(key) } else { Ok(()) }
    }

    /// Makes `key` a complete entry holding the first `len` bytes of
    /// `source`: cloned where the filesystem shares extents, copied
    /// otherwise. `chunks` are indexed as by [`Self::ensure`].
    ///
    /// # Errors
    ///
    /// Returns an error when the copy or the record fails.
    pub fn adopt_clone(
        &self,
        key: &AreaKey,
        source: &File,
        len: u64,
        retention: Retention,
        chunks: &[([u8; 32], u64, u32)],
    ) -> io::Result<()> {
        let staging = self.staging_path(key);
        let result = (|| {
            let target = OpenOptions::new()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&staging)?;
            clone_or_copy(source, &target, len)?;
            self.sync(&target)?;
            Ok::<_, io::Error>(target)
        })();
        let target = match result {
            Ok(target) => target,
            Err(error) => {
                let _ = std::fs::remove_file(&staging);
                return Err(error);
            }
        };
        self.install(key, &staging, target, len, retention, None, chunks)
    }

    /// Makes `key` a complete entry holding `bytes`.
    ///
    /// # Errors
    ///
    /// Returns an error when the write or the record fails.
    pub fn adopt_bytes(
        &self,
        key: &AreaKey,
        bytes: &[u8],
        retention: Retention,
        chunks: &[([u8; 32], u64, u32)],
    ) -> io::Result<()> {
        let staging = self.staging_path(key);
        let result = (|| {
            let target = OpenOptions::new()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&staging)?;
            target.write_all_at(bytes, 0)?;
            self.sync(&target)?;
            Ok::<_, io::Error>(target)
        })();
        let target = match result {
            Ok(target) => target,
            Err(error) => {
                let _ = std::fs::remove_file(&staging);
                return Err(error);
            }
        };
        self.install(
            key,
            &staging,
            target,
            bytes.len() as u64,
            retention,
            None,
            chunks,
        )
    }

    /// Makes `key` a complete entry by moving the file at `path` in (a copy
    /// when it lives on another filesystem). Returns the BLAKE3 of its
    /// bytes; another complete keyed entry with the same bytes shares its
    /// extents with this one where the filesystem allows.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, moved or recorded.
    pub fn adopt_file(
        &self,
        key: &AreaKey,
        path: &Path,
        retention: Retention,
    ) -> io::Result<[u8; 32]> {
        let digest = hash_file(path)?;
        let staging = self.staging_path(key);
        if std::fs::rename(path, &staging).is_err() {
            std::fs::copy(path, &staging)?;
            std::fs::remove_file(path)?;
        }
        let target = OpenOptions::new().read(true).write(true).open(&staging)?;
        self.sync(&target)?;
        let len = target.metadata()?.len();
        self.share_extents(&digest, &target, len);
        self.install(key, &staging, target, len, retention, Some(digest), &[])?;
        Ok(digest)
    }

    fn staging_path(&self, key: &AreaKey) -> PathBuf {
        let n = self.staging.fetch_add(1, Ordering::Relaxed);
        self.data.join(format!(".{}.{n}.staging", hex(key)))
    }

    fn install(
        &self,
        key: &AreaKey,
        staging: &Path,
        file: File,
        len: u64,
        retention: Retention,
        digest: Option<[u8; 32]>,
        chunks: &[([u8; 32], u64, u32)],
    ) -> io::Result<()> {
        let _create = self
            .create
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.entry(key).is_some() {
            self.remove(key)?;
        }
        std::fs::rename(staging, self.path_of(key))?;
        {
            let mut state = self.lock();
            let now = state.tick();
            let generation = state.next_generation();
            state.entries.insert(
                *key,
                Entry {
                    size: len,
                    resident: RangeSet::full(len),
                    retention,
                    last_use: now,
                    digest,
                    unsynced: 0,
                    durable: RangeSet::full(len),
                    generation,
                },
            );
            state.handles.insert(*key, Arc::new(file));
            if let Some(digest) = digest {
                state.digests.insert(digest, *key);
            }
        }
        self.index_chunks(key, chunks)?;
        self.persist(key)
    }

    /// Replaces `target`'s bytes with a clone of a complete entry holding
    /// the same bytes, so the two share extents. Best effort.
    fn share_extents(&self, digest: &[u8; 32], target: &File, len: u64) {
        let twin = self.lock().digests.get(digest).copied();
        let Some(twin) = twin else { return };
        let Ok(source) = self.file(&twin) else { return };
        if self
            .entry(&twin)
            .is_some_and(|e| e.complete() && e.size == len)
        {
            let _ = reflink(&source, target);
        }
    }

    /// Writes `len` bytes read from `body` at `offset` of `key` (an entry of
    /// `size` bytes, created if needed) and records them resident. A keyed
    /// entry that becomes complete is fingerprinted and shares extents with
    /// an identical one.
    ///
    /// # Errors
    ///
    /// Returns an error when `body` ends early or a write or record fails.
    pub fn fill_from(
        &self,
        key: &AreaKey,
        size: u64,
        offset: u64,
        body: &mut dyn Read,
        len: u64,
    ) -> io::Result<()> {
        self.ensure(key, size, Retention::Cached, &[])?;
        let file = self.file(key)?;
        let end = offset.saturating_add(len).min(size);
        let mut buffer = vec![0_u8; COPY_BUFFER];
        let mut at = offset;
        while at < end {
            let want = usize::try_from(end.saturating_sub(at))
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let chunk = buffer.get_mut(..want).unwrap_or_default();
            let read = body.read(chunk)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "source ended early",
                ));
            }
            file.write_all_at(chunk.get(..read).unwrap_or_default(), at)?;
            at = at.saturating_add(read as u64);
        }
        self.mark_resident(key, std::slice::from_ref(&(offset..end)), false)?;
        if self.entry(key).is_some_and(|e| e.complete()) {
            let digest = hash_file(&self.path_of(key))?;
            self.share_extents(&digest, &file, size);
            {
                let mut state = self.lock();
                if let Some(entry) = state.entries.get_mut(key) {
                    entry.digest = Some(digest);
                }
                state.digests.entry(digest).or_insert(*key);
            }
            self.persist(key)?;
        }
        self.evict_to_budget(&[*key]);
        Ok(())
    }

    /// Copies `range` of `key` into `dest` at the same offsets: a clone of
    /// shared extents where the filesystem allows, a copy elsewhere. Every
    /// byte must be resident.
    ///
    /// # Errors
    ///
    /// `NotFound` when part of the range is not resident, or the copy's
    /// error.
    pub fn copy_to(&self, key: &AreaKey, range: &Range<u64>, dest: &File) -> io::Result<()> {
        if !self.missing(key, range).is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "range is not resident",
            ));
        }
        self.touch(key);
        let source = self.file(key)?;
        copy_range(
            &source,
            range.start,
            dest,
            range.start,
            range.end.saturating_sub(range.start),
        )
    }

    /// Removes `key`, its file and its chunk index entries.
    ///
    /// # Errors
    ///
    /// Returns an error when the removal cannot be recorded.
    pub fn remove(&self, key: &AreaKey) -> io::Result<()> {
        {
            let mut state = self.lock();
            if let Some(entry) = state.entries.remove(key)
                && let Some(digest) = entry.digest
                && state.digests.get(&digest) == Some(key)
            {
                state.digests.remove(&digest);
            }
            state.handles.remove(key);
        }
        let _ = std::fs::remove_file(self.path_of(key));
        self.unpersist(key)
    }

    /// Drops cached entries, least recently used first, until their
    /// resident bytes fit the budget. `keep` are never dropped (entries
    /// being read or filled right now). Returns the bytes freed.
    pub fn evict_to_budget(&self, keep: &[AreaKey]) -> u64 {
        let mut freed = 0_u64;
        loop {
            let victim = {
                let state = self.lock();
                let cached: u64 = state
                    .entries
                    .values()
                    .filter(|e| e.retention == Retention::Cached)
                    .map(|e| e.resident.total())
                    .sum();
                if cached <= self.budget() {
                    return freed;
                }
                state
                    .entries
                    .iter()
                    .filter(|(key, e)| e.retention == Retention::Cached && !keep.contains(key))
                    .min_by_key(|(_, e)| e.last_use)
                    .map(|(key, e)| (*key, e.resident.total()))
            };
            let Some((key, bytes)) = victim else {
                return freed;
            };
            if self.remove(&key).is_err() {
                return freed;
            }
            freed = freed.saturating_add(bytes);
        }
    }

    fn persist(&self, key: &AreaKey) -> io::Result<()> {
        // Ranges not synced yet stay out of the record.
        let stored = self
            .lock()
            .entries
            .get(key)
            .map(|entry| entry.stored_with(&entry.durable));
        match stored {
            Some(stored) => self.persist_stored(key, &stored),
            None => Ok(()),
        }
    }

    fn persist_stored(&self, key: &AreaKey, stored: &Stored) -> io::Result<()> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.lock().entries.contains_key(key) {
            return Ok(());
        }
        let bytes = postcard::to_allocvec(stored).map_err(db_error)?;
        let txn = self.database()?.begin_write().map_err(db_error)?;
        txn.open_table(ENTRIES)
            .map_err(db_error)?
            .insert(*key, bytes.as_slice())
            .map_err(db_error)?;
        txn.commit().map_err(db_error)
    }

    fn unpersist(&self, key: &AreaKey) -> io::Result<()> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let txn = self.database()?.begin_write().map_err(db_error)?;
        {
            txn.open_table(ENTRIES)
                .map_err(db_error)?
                .remove(*key)
                .map_err(db_error)?;
            let mut members = txn.open_table(MEMBERS).map_err(db_error)?;
            let list = members
                .remove(*key)
                .map_err(db_error)?
                .map(|v| v.value().to_vec());
            if let Some(list) = list {
                let mut index = txn.open_multimap_table(CHUNKS).map_err(db_error)?;
                for member in list.chunks_exact(76) {
                    let (hash, location) = member.split_at(32);
                    let (Ok(hash), Ok(location)) =
                        (<[u8; 32]>::try_from(hash), <[u8; 44]>::try_from(location))
                    else {
                        continue;
                    };
                    index.remove(hash, location).map_err(db_error)?;
                }
            }
        }
        txn.commit().map_err(db_error)
    }

    /// Syncs and records every entry's residency and last use, so progress
    /// and eviction order survive a restart.
    ///
    /// # Errors
    ///
    /// Returns an error when a sync or the record fails.
    pub fn flush(&self) -> io::Result<()> {
        let unsynced: Vec<AreaKey> = self
            .lock()
            .entries
            .iter()
            .filter(|(_, e)| e.unsynced > 0)
            .map(|(key, _)| *key)
            .collect();
        let stored_before_sync: Vec<(AreaKey, Stored)> = {
            let mut state = self.lock();
            state
                .entries
                .iter_mut()
                .map(|(key, e)| {
                    e.unsynced = 0;
                    (*key, e.stored())
                })
                .collect()
        };
        for key in &unsynced {
            if let Ok(file) = self.file(key) {
                self.sync(&file)?;
            }
        }
        {
            let mut state = self.lock();
            for (key, stored) in &stored_before_sync {
                if let Some(entry) = state.entries.get_mut(key) {
                    entry.durable = range_set(&stored.resident);
                }
            }
        }
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stored = stored_before_sync;
        let txn = self.database()?.begin_write().map_err(db_error)?;
        {
            let mut table = txn.open_table(ENTRIES).map_err(db_error)?;
            for (key, stored) in stored {
                let bytes = postcard::to_allocvec(&stored).map_err(db_error)?;
                table.insert(key, bytes.as_slice()).map_err(db_error)?;
            }
        }
        txn.commit().map_err(db_error)
    }
}

impl Drop for FileArea {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

fn parse_hex(name: &str) -> Option<AreaKey> {
    let mut key = [0_u8; 32];
    hex::decode_to_slice(name, &mut key).ok()?;
    Some(key)
}

/// BLAKE3 of a file, streamed.
///
/// # Errors
///
/// Returns the read's error.
pub fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(File::open(path)?)?;
    Ok(*hasher.finalize().as_bytes())
}

/// Clones all of `source` into `target` (`FICLONE`).
#[cfg(all(target_os = "linux", feature = "filesystem"))]
fn reflink(source: &File, target: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    /// `_IOW(0x94, 9, int)`.
    const FICLONE: libc::c_ulong = 0x4004_9409;
    // SAFETY: both descriptors are open files; FICLONE takes the source fd
    // as its argument and touches no memory of ours.
    let rc = unsafe { libc::ioctl(target.as_raw_fd(), FICLONE, source.as_raw_fd()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(all(target_os = "linux", feature = "filesystem")))]
fn reflink(_source: &File, _target: &File) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no reflinks here",
    ))
}

/// Makes `target` hold the first `len` bytes of `source`, sharing extents
/// where the filesystem can.
fn clone_or_copy(source: &File, target: &File, len: u64) -> io::Result<()> {
    if source.metadata()?.len() == len && reflink(source, target).is_ok() {
        return Ok(());
    }
    target.set_len(0)?;
    copy_range(source, 0, target, 0, len)?;
    target.set_len(len)
}

/// Copies `len` bytes from `source` at `from` to `dest` at `to`. The kernel
/// clones shared extents where it can (`copy_file_range`).
///
/// # Errors
///
/// Returns the copy's error, or `UnexpectedEof` when `source` is short.
pub fn copy_range(source: &File, from: u64, dest: &File, to: u64, len: u64) -> io::Result<()> {
    #[cfg(all(target_os = "linux", feature = "filesystem"))]
    {
        use std::os::fd::AsRawFd as _;
        let (mut off_in, mut off_out) = (
            libc::loff_t::try_from(from).map_err(io::Error::other)?,
            libc::loff_t::try_from(to).map_err(io::Error::other)?,
        );
        let mut left = len;
        let mut kernel = true;
        while left > 0 && kernel {
            let step = usize::try_from(left).unwrap_or(usize::MAX).min(1 << 30);
            // SAFETY: both descriptors are open; the offsets are ours and
            // the kernel updates them.
            let n = unsafe {
                libc::copy_file_range(
                    source.as_raw_fd(),
                    &raw mut off_in,
                    dest.as_raw_fd(),
                    &raw mut off_out,
                    step,
                    0,
                )
            };
            match usize::try_from(n) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "source is shorter than the range",
                    ));
                }
                Ok(n) => left = left.saturating_sub(n as u64),
                Err(_) => {
                    let error = io::Error::last_os_error();
                    match error.raw_os_error() {
                        Some(libc::EINTR) => {}
                        Some(libc::EXDEV | libc::EINVAL | libc::EOPNOTSUPP | libc::ENOSYS) => {
                            kernel = false;
                        }
                        _ => return Err(error),
                    }
                }
            }
        }
        if left == 0 {
            return Ok(());
        }
        let done = len.saturating_sub(left);
        copy_buffered(
            source,
            from.saturating_add(done),
            dest,
            to.saturating_add(done),
            left,
        )
    }
    #[cfg(not(all(target_os = "linux", feature = "filesystem")))]
    copy_buffered(source, from, dest, to, len)
}

fn copy_buffered(source: &File, from: u64, dest: &File, to: u64, len: u64) -> io::Result<()> {
    let mut buffer = vec![0_u8; COPY_BUFFER];
    let mut done = 0_u64;
    while done < len {
        let want = usize::try_from(len.saturating_sub(done))
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let chunk = buffer.get_mut(..want).unwrap_or_default();
        source.read_exact_at(chunk, from.saturating_add(done))?;
        dest.write_all_at(chunk, to.saturating_add(done))?;
        done = done.saturating_add(want as u64);
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::single_range_in_vec_init,
    reason = "test fixtures record one range at a time"
)]
mod tests {
    use super::*;

    fn key(n: u8) -> AreaKey {
        [n; 32]
    }

    #[test]
    fn ranges_fill_persist_and_resume_after_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..100_u8).collect();
        {
            let area = FileArea::open(dir.path(), u64::MAX).unwrap();
            area.ensure(&key(1), 100, Retention::Cached, &[]).unwrap();
            assert_eq!(area.missing(&key(1), &(0..100)), vec![0..100]);
            area.write_at(&key(1), 10, &bytes[10..20]).unwrap();
            assert_eq!(
                area.missing(&key(1), &(0..30)),
                vec![0..30],
                "written but not recorded"
            );
            area.mark_resident(&key(1), &[10..20], true).unwrap();
            assert_eq!(area.missing(&key(1), &(0..30)), vec![0..10, 20..30]);
            assert!(area.read_at(&key(1), 5, 10).is_err());
            assert_eq!(area.read_at(&key(1), 12, 5).unwrap(), &bytes[12..17]);
        }
        let area = FileArea::open(dir.path(), u64::MAX).unwrap();
        assert_eq!(area.missing(&key(1), &(0..100)), vec![0..10, 20..100]);
        area.fill_from(&key(1), 100, 0, &mut &bytes[..], 100)
            .unwrap();
        assert!(area.entry(&key(1)).unwrap().complete());
        assert_eq!(
            area.residency(&key(1), 100, &(0..100)),
            Residency::Complete(area.path_of(&key(1)))
        );
    }

    #[test]
    fn the_budget_evicts_cached_entries_least_recently_used_first() {
        let dir = tempfile::tempdir().unwrap();
        let area = FileArea::open(dir.path(), 250).unwrap();
        let data = [7_u8; 100];
        area.adopt_bytes(&key(1), &data, Retention::Cached, &[])
            .unwrap();
        area.adopt_bytes(&key(2), &data, Retention::LocalOnly, &[])
            .unwrap();
        area.adopt_bytes(&key(3), &data, Retention::Pinned, &[])
            .unwrap();
        area.adopt_bytes(&key(4), &data, Retention::Cached, &[])
            .unwrap();
        area.touch(&key(1));
        area.fill_from(&key(5), 100, 0, &mut &data[..], 100)
            .unwrap();
        // 300 cached bytes over a 250 budget: key 4 is the least recently
        // used cached entry; local-only and pinned entries stay.
        assert!(area.entry(&key(4)).is_none());
        for kept in [1, 2, 3, 5] {
            assert!(area.entry(&key(kept)).is_some(), "{kept}");
        }
        assert!(!area.path_of(&key(4)).exists());
        area.set_budget(0);
        area.evict_to_budget(&[key(5)]);
        assert!(area.entry(&key(1)).is_none());
        assert!(area.entry(&key(5)).is_some(), "kept while in use");
        assert!(area.entry(&key(2)).is_some() && area.entry(&key(3)).is_some());
    }

    #[test]
    fn chunks_are_found_where_they_are_resident() {
        let dir = tempfile::tempdir().unwrap();
        let area = FileArea::open(dir.path(), u64::MAX).unwrap();
        let chunks = [([9_u8; 32], 0_u64, 40_u32), ([8_u8; 32], 40, 60)];
        area.ensure(&key(1), 100, Retention::Cached, &chunks)
            .unwrap();
        assert!(area.find_chunk(&[9; 32]).is_none(), "not resident yet");
        area.write_at(&key(1), 0, &[1; 40]).unwrap();
        area.mark_resident(&key(1), &[0..40], false).unwrap();
        let found = area.find_chunk(&[9; 32]).unwrap();
        assert_eq!((found.key, found.offset, found.len), (key(1), 0, 40));
        assert!(area.find_chunk(&[8; 32]).is_none());
        area.remove(&key(1)).unwrap();
        assert!(area.find_chunk(&[9; 32]).is_none());
        let reopened = {
            drop(area);
            FileArea::open(dir.path(), u64::MAX).unwrap()
        };
        assert!(reopened.find_chunk(&[9; 32]).is_none());
    }

    #[test]
    fn adopted_files_are_complete_and_copies_land_at_the_same_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let area = FileArea::open(&dir.path().join("area"), u64::MAX).unwrap();
        let source_path = dir.path().join("source");
        let bytes: Vec<u8> = (0..200_000_u32).map(|n| (n % 251) as u8).collect();
        std::fs::write(&source_path, &bytes).unwrap();
        let source = File::open(&source_path).unwrap();
        area.adopt_clone(
            &key(1),
            &source,
            bytes.len() as u64,
            Retention::LocalOnly,
            &[],
        )
        .unwrap();
        assert!(area.entry(&key(1)).unwrap().complete());
        let dest_path = dir.path().join("dest");
        let dest = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&dest_path)
            .unwrap();
        dest.set_len(bytes.len() as u64).unwrap();
        area.copy_to(&key(1), &(4096..150_001), &dest).unwrap();
        let got = std::fs::read(&dest_path).unwrap();
        assert_eq!(&got[4096..150_001], &bytes[4096..150_001]);
        assert!(got[..4096].iter().all(|b| *b == 0));

        let upload = dir.path().join("upload");
        std::fs::write(&upload, &bytes).unwrap();
        let digest = area
            .adopt_file(&key(2), &upload, Retention::Cached)
            .unwrap();
        assert_eq!(digest, *blake3::hash(&bytes).as_bytes());
        assert!(!upload.exists(), "moved, not copied");
        assert_eq!(area.read_at(&key(2), 0, 10).unwrap(), &bytes[..10]);
    }
}
