//! The content store's one physical store.
//!
//! Content larger than a few chunks lives in one file per entry, at the entry's own byte offsets,
//! sparse until its ranges arrive. An entry is keyed by 32 bytes (a content id, a whole blob's hash,
//! or a caller's key such as a cloud account's revision); its residency is a [`RangeSet`] of the bytes
//! present, persisted, so partial content survives restarts and a transfer resumes where it stopped; a
//! chunk index maps every chunk hash to where it lives, so a chunk is served and deduplicated without a
//! second copy; a retention class decides eviction, in which [`Retention::Cached`] and
//! [`Retention::Fetchable`] entries go by budget, least recently used first, and
//! [`Retention::LocalOnly`] and [`Retention::Pinned`] entries never do.
//!
//! Small blobs (manifests, the chunks of small files, thumbnails) are inline: their bytes are rows of
//! the index database, which the budget never evicts. A whole blob too large to inline is a
//! [`Kind::Blob`] entry with its BAO outboard persisted beside it, so peers are served ranges from the
//! outboard rather than by hashing the file again.
//!
//! Nothing here verifies content: callers write only verified bytes, and chunks are re-verified
//! against their hash before being served.

mod commit_log;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use bytes::Bytes;
use fs_content::RangeSet;
use redb::{Database, MultimapTableDefinition, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

use self::commit_log::{CommitLog, Write};

const ENTRIES: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("area_entries");
/// Small blobs, keyed by their own hash, their bytes the value: manifests, the chunks of small
/// files, thumbnails. They are inside the index database rather than files of their own.
const INLINE: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("area_inline");
/// Whole blobs' BAO outboards, post order without a length suffix, keyed by the blob's hash, so a
/// peer's range request is answered from the outboard instead of hashing the file again.
const OUTBOARDS: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("area_outboards");
/// A caller's own names for blobs (document media's per-workspace references, a legacy import's
/// journal), read only by exact name and never scanned or evicted.
const REFERENCE_TAGS: TableDefinition<&str, [u8; 32]> = TableDefinition::new("area_reference_tags");
/// Chunk hash to `key ‖ offset (u64 LE) ‖ len (u32 LE)`.
const CHUNKS: MultimapTableDefinition<[u8; 32], [u8; 44]> =
    MultimapTableDefinition::new("area_chunks");
const MEMBERS: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("area_members");
/// Entry key to where a [`Retention::Fetchable`] entry's bytes are fetched from.
const ORIGINS: TableDefinition<[u8; 32], &str> = TableDefinition::new("area_origins");

const OPEN_FILES: usize = 256;
const COPY_BUFFER: usize = 1024 * 1024;
/// Pages of the index database kept in memory, read and written. Small blobs live inside those pages,
/// so redb's own default (1 GiB) would let the process that keeps a synced folder grow toward a
/// gigabyte of cached pages; the hot blob cache in front of it and the OS page cache serve repeats.
pub const META_CACHE_BYTES: usize = 32 * 1024 * 1024;
/// Newly resident bytes of one entry that may be recorded in memory only: a crash loses at most this
/// much of a transfer's progress.
const SYNC_BATCH: u64 = 256 * 1024 * 1024;
/// How large the log of commits not yet on disk may grow before a commit syncs the database and
/// empties it, when nothing syncs on a window: what opening after a kill applies again.
const LOG_MAX: u64 = 64 * 1024 * 1024;

pub type AreaKey = [u8; 32];

/// What an entry holds: a file version's content, keyed by its content id, or a whole blob imported
/// or fetched by its own hash.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Content,
    Blob,
}

/// What an installed entry holds beyond its bytes: its kind, the BLAKE3 of its complete bytes (for
/// sharing extents between entries with the same content), and the chunk index of a content entry.
struct Shape<'a> {
    kind: Kind,
    digest: Option<[u8; 32]>,
    chunks: &'a [([u8; 32], u64, u32)],
    /// A version's encoded manifest, stored inline in the same commit as the entry's state.
    manifest: Option<&'a [u8]>,
}

/// The most recently used cached entries, which the budget keeps even when an always-on device holds
/// them: the file being read and the ones predicted next.
pub const WORKING_SET_ENTRIES: usize = 8;

/// Names the entries an always-on device is known to hold, asked once per eviction pass.
pub type HeldElsewhere = Arc<dyn Fn() -> HashSet<AreaKey> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Retention {
    /// A copy of content held elsewhere: evicted first, least recently used first, when the budget is
    /// exceeded.
    Cached,
    /// Content this device may be the only holder of. Never evicted by the budget.
    LocalOnly,
    Pinned,
    /// A copy of public content this device can fetch again from where it came from
    /// ([`FileArea::origin`]). Evicted with the cached entries, but only its bytes go: the entry, its
    /// size and its origin stay, so reading it again fetches them.
    Fetchable,
}

impl Retention {
    pub const fn evictable(self) -> bool {
        matches!(self, Self::Cached | Self::Fetchable)
    }

    /// This retention once the entry is kept offline: only what the budget could take changes.
    /// [`Self::LocalOnly`] stays, so a later unpin never makes content only this device holds
    /// evictable.
    #[must_use]
    pub const fn kept_offline(self) -> Self {
        if self.evictable() { Self::Pinned } else { self }
    }

    /// This retention once no pin keeps the entry offline: a pinned copy is a cached one again.
    #[must_use]
    pub const fn released(self) -> Self {
        match self {
            Self::Pinned => Self::Cached,
            other => other,
        }
    }
}

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

#[derive(Debug, PartialEq, Eq)]
pub enum Residency {
    Complete(PathBuf),
    Missing(Vec<Range<u64>>),
}

#[derive(Serialize, Deserialize)]
struct Stored {
    size: u64,
    resident: Vec<(u64, u64)>,
    retention: Retention,
    last_use: u64,
    /// BLAKE3 of the complete bytes of a keyed entry, for sharing extents between entries with the
    /// same bytes.
    digest: Option<[u8; 32]>,
    kind: Kind,
}

struct Entry {
    size: u64,
    resident: RangeSet,
    retention: Retention,
    last_use: u64,
    digest: Option<[u8; 32]>,
    kind: Kind,
    /// Bytes recorded resident in memory since the last durable record.
    unsynced: u64,
    durable: RangeSet,
    /// Changes whenever the entry's file is created anew, so a write into an earlier file is never
    /// recorded against the new one.
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
            kind: self.kind,
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
    db: std::sync::RwLock<Option<Arc<Database>>>,
    state: Mutex<State>,
    /// Serialises persistence so an older snapshot of an entry never overwrites a newer one.
    commit: Mutex<()>,
    /// Serialises creating and replacing entries: two creators of one entry must not both create its
    /// file, or the later would truncate bytes the earlier already wrote and recorded.
    create: Mutex<()>,
    staging: AtomicU64,
    budget: AtomicU64,
    read_bytes: AtomicU64,
    /// Which cached entries the budget gives up first; `None` is plain least-recently-used.
    held_elsewhere: std::sync::RwLock<Option<HeldElsewhere>>,
    /// Whether data is synced and the index is on disk. An ephemeral area is gone with its process, so
    /// syncing it would only cost a disk flush per write.
    durable: bool,
    /// Whether each write's commit waits for the disk; a deferred-durability group clears this and
    /// syncs once at its end.
    sync_writes: AtomicBool,
    /// The commits that did not wait for the disk since the last one that did, so a process killed
    /// before the next sync keeps them; `None` for an ephemeral area. Its lock is held across
    /// each commit, so the log's order is the database's.
    log: Mutex<Option<CommitLog>>,
    /// Entries whose files were written since the last sync of a window, in the order they were
    /// written: the data half of that barrier. Only a store syncing on a window adds to it;
    /// [`Self::sync_written`] fsyncs them before the index that names them.
    written: Mutex<Vec<AreaKey>>,
}

/// Makes `write` in `txn`, answering whether it removed anything ([`Write::RemoveInline`]): what a
/// commit does and what opening does again for each commit the log kept.
fn apply(txn: &redb::WriteTransaction, write: &Write<'_>) -> io::Result<bool> {
    match write {
        Write::Inline(blobs) => {
            let mut table = txn.open_table(INLINE).map_err(db_error)?;
            for (key, bytes) in blobs {
                table.insert(*key, bytes.as_ref()).map_err(db_error)?;
            }
        }
        Write::RemoveInline(key) => {
            let mut table = txn.open_table(INLINE).map_err(db_error)?;
            return Ok(table.remove(*key).map_err(db_error)?.is_some());
        }
        Write::Outboard(key, data) => {
            let mut table = txn.open_table(OUTBOARDS).map_err(db_error)?;
            table.insert(*key, data.as_ref()).map_err(db_error)?;
        }
        Write::Reference(tag, hash) => {
            let mut table = txn.open_table(REFERENCE_TAGS).map_err(db_error)?;
            table.insert(tag.as_ref(), *hash).map_err(db_error)?;
        }
        Write::Chunks(key, chunks) => {
            let mut members = Vec::with_capacity(chunks.len().saturating_mul(76));
            let mut index = txn.open_multimap_table(CHUNKS).map_err(db_error)?;
            for (hash, offset, len) in chunks.iter() {
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
        Write::Origin(key, origin) => {
            txn.open_table(ORIGINS)
                .map_err(db_error)?
                .insert(*key, origin.as_ref())
                .map_err(db_error)?;
        }
        Write::Entries(states) => {
            let mut table = txn.open_table(ENTRIES).map_err(db_error)?;
            for (key, bytes) in states {
                table.insert(*key, bytes.as_ref()).map_err(db_error)?;
            }
        }
        Write::Unpersist(key) => {
            txn.open_table(ENTRIES)
                .map_err(db_error)?
                .remove(*key)
                .map_err(db_error)?;
            txn.open_table(OUTBOARDS)
                .map_err(db_error)?
                .remove(*key)
                .map_err(db_error)?;
            txn.open_table(ORIGINS)
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
        Write::Batch(writes) => {
            let mut removed = false;
            for write in writes {
                removed |= apply(txn, write)?;
            }
            return Ok(removed);
        }
    }
    Ok(false)
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
    fn sync(&self, file: &File) -> io::Result<()> {
        if self.durable {
            file.sync_data()
        } else {
            Ok(())
        }
    }

    fn database(&self) -> io::Result<Arc<Database>> {
        self.db
            .read_recover()
            .clone()
            .ok_or_else(|| io::Error::other("the content area is closed"))
    }

    /// Releases the area's index database, so the same directory can be opened again in this process
    /// while handles to this area remain. Every later operation fails.
    pub fn close(&self) {
        self.db.write_recover().take();
    }

    /// Opens (or creates) the area under `dir`, holding at most `budget` resident bytes of cached
    /// entries.
    pub fn open(dir: &Path, budget: u64) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let db = Database::builder()
            .set_cache_size(META_CACHE_BYTES)
            .create(dir.join("area.redb"))
            .map_err(db_error)?;
        Self::open_with(dir, db, budget, Some(CommitLog::open(dir)?))
    }

    /// An area whose files live under `dir` but whose index is in memory and whose data is never
    /// synced: for a store that does not outlive its process.
    pub fn open_ephemeral(dir: &Path, budget: u64) -> io::Result<Self> {
        let db = Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .map_err(db_error)?;
        Self::open_with(dir, db, budget, None)
    }

    fn open_with(
        dir: &Path,
        db: Database,
        budget: u64,
        log: Option<CommitLog>,
    ) -> io::Result<Self> {
        let durable = log.is_some();
        let data = data_dir(dir);
        std::fs::create_dir_all(&data)?;
        {
            let txn = db.begin_write().map_err(db_error)?;
            txn.open_table(ENTRIES).map_err(db_error)?;
            txn.open_table(INLINE).map_err(db_error)?;
            txn.open_table(OUTBOARDS).map_err(db_error)?;
            txn.open_table(REFERENCE_TAGS).map_err(db_error)?;
            txn.open_multimap_table(CHUNKS).map_err(db_error)?;
            txn.open_table(MEMBERS).map_err(db_error)?;
            txn.open_table(ORIGINS).map_err(db_error)?;
            // What a killed process committed without waiting for the disk goes in again, in the
            // order it was committed, before anything reads the index.
            if let Some(log) = &log {
                for write in log.read()? {
                    apply(&txn, &write)?;
                }
            }
            txn.commit().map_err(db_error)?;
        }
        let mut log = log;
        if let Some(log) = &mut log {
            log.clear()?;
        }
        let area = Self {
            data,
            db: std::sync::RwLock::new(Some(Arc::new(db))),
            state: Mutex::new(State::default()),
            commit: Mutex::new(()),
            create: Mutex::new(()),
            staging: AtomicU64::new(0),
            budget: AtomicU64::new(budget),
            read_bytes: AtomicU64::new(0),
            held_elsewhere: std::sync::RwLock::new(None),
            durable,
            sync_writes: AtomicBool::new(true),
            log: Mutex::new(log),
            written: Mutex::new(Vec::new()),
        };
        area.load()?;
        Ok(area)
    }

    /// Reads the persisted entries, dropping those whose file is gone and files that belong to no
    /// entry (left by a crash before the entry was recorded).
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
                        kind: stored.kind,
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
        self.state.lock_recover()
    }

    /// Commits `write` at the area's current durability, answering whether it removed anything
    /// ([`Write::RemoveInline`]). A commit that waits for the disk empties the log; one that does
    /// not (a deferred-durability group, a store syncing on a window) is added to the log first, so
    /// a process killed before the next sync still holds it, and the group's end or the window
    /// syncs once.
    fn commit_write(&self, write: &Write<'_>) -> io::Result<bool> {
        self.commit_at(write, self.sync_writes.load(Ordering::Relaxed))
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the log's lock is held across the commit, so the log's order is the database's"
    )]
    fn commit_at(&self, write: &Write<'_>, sync: bool) -> io::Result<bool> {
        let mut log = self.log.lock_recover();
        let mut txn = self.database()?.begin_write().map_err(db_error)?;
        let sync = sync || log.as_ref().is_some_and(|log| log.len() >= LOG_MAX);
        if !sync {
            txn.set_durability(redb::Durability::None)
                .map_err(db_error)?;
        }
        let removed = apply(&txn, write)?;
        match log.as_mut() {
            Some(log) if !sync => log.append(write)?,
            _ => {}
        }
        txn.commit().map_err(db_error)?;
        if sync && let Some(log) = log.as_mut() {
            log.clear()?;
        }
        Ok(removed)
    }

    /// Makes the commits made since [`Self::set_sync_writes`]`(false)` durable with one flush, and
    /// empties the log that kept them for a killed process.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the log's lock is held across the commit, so the log's order is the database's"
    )]
    pub fn sync_db(&self) -> io::Result<()> {
        let mut log = self.log.lock_recover();
        let mut txn = self.database()?.begin_write().map_err(db_error)?;
        txn.set_durability(redb::Durability::Immediate)
            .map_err(db_error)?;
        txn.commit().map_err(db_error)?;
        if let Some(log) = log.as_mut() {
            log.clear()?;
        }
        Ok(())
    }

    /// Whether a commit that did not wait for the disk is in the log, not yet synced
    /// ([`Self::sync_db`]).
    pub fn has_unsynced(&self) -> bool {
        self.log
            .lock_recover()
            .as_ref()
            .is_some_and(|log| log.len() > 0)
    }

    /// Whether each commit waits for the disk; see [`Self::sync_db`] for the deferred group's end.
    pub fn set_sync_writes(&self, sync: bool) {
        self.sync_writes.store(sync, Ordering::Relaxed);
    }

    pub fn path_of(&self, key: &AreaKey) -> PathBuf {
        self.data.join(hex::encode(key))
    }

    pub fn budget(&self) -> u64 {
        self.budget.load(Ordering::Relaxed)
    }

    pub fn set_budget(&self, bytes: u64) {
        self.budget.store(bytes, Ordering::Relaxed);
    }

    /// Tell the budget which entries an always-on device holds, so it gives those up before content
    /// that may be harder to fetch. It only orders cached entries: it never makes a local-only or
    /// pinned entry evictable, nor keeps the budget from being met.
    pub fn set_held_elsewhere(&self, held: HeldElsewhere) {
        *self.held_elsewhere.write_recover() = Some(held);
    }

    /// Resident bytes of cached entries, which the budget limits.
    fn cached_bytes(state: &State) -> u64 {
        state
            .entries
            .values()
            .filter(|e| e.retention.evictable())
            .map(|e| e.resident.total())
            .sum()
    }

    pub fn entry(&self, key: &AreaKey) -> Option<EntryInfo> {
        self.lock().entries.get(key).map(Entry::info)
    }

    /// Every entry key.
    pub fn keys(&self) -> Vec<AreaKey> {
        self.lock().entries.keys().copied().collect()
    }

    /// Whether `key` is a whole blob rather than a file version's content.
    pub fn is_blob(&self, key: &AreaKey) -> bool {
        self.lock()
            .entries
            .get(key)
            .is_some_and(|entry| entry.kind == Kind::Blob)
    }

    /// Every whole-blob entry key.
    pub fn blob_keys(&self) -> Vec<AreaKey> {
        self.lock()
            .entries
            .iter()
            .filter(|(_, entry)| entry.kind == Kind::Blob)
            .map(|(key, _)| *key)
            .collect()
    }

    /// Every hash in the chunk index: a chunk is a blob wherever it lives.
    pub fn chunk_hashes(&self) -> Vec<[u8; 32]> {
        let Ok(db) = self.database() else {
            return Vec::new();
        };
        let Ok(txn) = db.begin_read() else {
            return Vec::new();
        };
        let Ok(members) = txn.open_table(MEMBERS) else {
            return Vec::new();
        };
        let mut hashes = Vec::new();
        for row in members.iter().ok().into_iter().flatten() {
            let Ok((_, list)) = row else { continue };
            for member in list.value().chunks_exact(76) {
                if let Some(hash) = member
                    .get(..32)
                    .and_then(|hash| <[u8; 32]>::try_from(hash).ok())
                {
                    hashes.push(hash);
                }
            }
        }
        hashes.sort_unstable();
        hashes.dedup();
        hashes
    }

    /// Stores `blobs` inline under their hashes, in one commit. Inline content is inside the index
    /// database, so the budget never evicts it: a manifest or a chunk an entry needs stays readable
    /// while the entry does.
    pub fn put_inline(&self, blobs: impl IntoIterator<Item = (AreaKey, Bytes)>) -> io::Result<()> {
        let blobs: Vec<_> = blobs.into_iter().collect();
        let write = Write::Inline(
            blobs
                .iter()
                .map(|(key, bytes)| (*key, Cow::Borrowed(bytes.as_ref())))
                .collect(),
        );
        self.commit_write(&write).map(drop)
    }

    /// The inline bytes of `key`.
    pub fn get_inline(&self, key: &AreaKey) -> Option<Vec<u8>> {
        let txn = self.database().ok()?.begin_read().ok()?;
        let table = txn.open_table(INLINE).ok()?;
        table.get(*key).ok()?.map(|bytes| bytes.value().to_vec())
    }

    pub fn has_inline(&self, key: &AreaKey) -> bool {
        let Ok(db) = self.database() else {
            return false;
        };
        let Ok(txn) = db.begin_read() else {
            return false;
        };
        let Ok(table) = txn.open_table(INLINE) else {
            return false;
        };
        table.get(*key).ok().flatten().is_some()
    }

    /// Every inline blob's hash.
    pub fn inline_keys(&self) -> Vec<AreaKey> {
        let Ok(db) = self.database() else {
            return Vec::new();
        };
        let Ok(txn) = db.begin_read() else {
            return Vec::new();
        };
        let Ok(table) = txn.open_table(INLINE) else {
            return Vec::new();
        };
        table
            .iter()
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|row| row.ok().map(|(key, _)| key.value()))
            .collect()
    }

    /// Removes `key`'s inline bytes. Returns whether it held any.
    pub fn remove_inline(&self, key: &AreaKey) -> io::Result<bool> {
        self.commit_write(&Write::RemoveInline(*key))
    }

    /// The persisted BAO outboard of whole blob `key`.
    pub fn outboard(&self, key: &AreaKey) -> Option<Vec<u8>> {
        let txn = self.database().ok()?.begin_read().ok()?;
        let table = txn.open_table(OUTBOARDS).ok()?;
        table.get(*key).ok()?.map(|data| data.value().to_vec())
    }

    fn put_outboard(&self, key: &AreaKey, data: &[u8]) -> io::Result<()> {
        self.commit_write(&Write::Outboard(*key, Cow::Borrowed(data)))
            .map(drop)
    }

    /// The blob `tag` names, if any.
    pub fn reference(&self, tag: &str) -> Option<AreaKey> {
        let txn = self.database().ok()?.begin_read().ok()?;
        let table = txn.open_table(REFERENCE_TAGS).ok()?;
        Some(table.get(tag).ok()??.value())
    }

    /// Makes `tag` a name for blob `hash`.
    pub fn set_reference(&self, tag: &str, hash: &AreaKey) -> io::Result<()> {
        self.commit_write(&Write::Reference(Cow::Borrowed(tag), *hash))
            .map(drop)
    }

    /// `key`'s size and the ranges of it that are resident.
    pub fn resident(&self, key: &AreaKey) -> Option<(u64, Vec<Range<u64>>)> {
        self.lock().entries.get(key).map(|entry| {
            (
                entry.size,
                entry.resident.iter().cloned().collect::<Vec<Range<u64>>>(),
            )
        })
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

    /// Parts of `range` of `key` that are not resident (all of it when the area has no such entry).
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

    /// Where `range` of `key` (an entry of `size` bytes) can be read, marking the entry as used.
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

    /// Makes sure `key` has an entry of `size` bytes, creating an empty sparse one if needed. An entry
    /// with another size is replaced, and a stricter retention than the entry has is applied.
    /// `chunks` indexes the chunks of a content entry as `(hash, offset, len)`.
    pub fn ensure(
        &self,
        key: &AreaKey,
        size: u64,
        retention: Retention,
        chunks: &[([u8; 32], u64, u32)],
    ) -> io::Result<()> {
        let _create = self.create.lock_recover();
        let existing = {
            let mut state = self.lock();
            state.entries.get_mut(key).map(|entry| {
                let same = entry.size == size;
                // Only what the budget could take is held back; nothing asked for here makes an entry
                // evictable.
                let stricter = same && entry.retention.evictable() && !retention.evictable();
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
                    kind: Kind::Content,
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
        let _commit = self.commit.lock_recover();
        self.commit_write(&Write::Chunks(*key, Cow::Borrowed(chunks)))
            .map(drop)
    }

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

    /// Records `ranges` of `key` (bytes already written to its file) as resident. They are readable at
    /// once; the record reaches disk after the file's data is synced, with `durable` now or otherwise
    /// once `SYNC_BATCH` bytes accumulated or the entry is complete, so a recorded range is never one
    /// a crash could lose.
    pub fn mark_resident(
        &self,
        key: &AreaKey,
        ranges: &[Range<u64>],
        durable: bool,
    ) -> io::Result<()> {
        self.mark_resident_in(key, None, ranges, durable)
    }

    pub fn generation(&self, key: &AreaKey) -> Option<u64> {
        self.lock().entries.get(key).map(|e| e.generation)
    }

    /// The file of `key` with its generation, for writes recorded later with
    /// [`Self::mark_resident_in`].
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

    /// [`Self::mark_resident`] for bytes written into the file of `generation`: when the entry has been
    /// replaced since, nothing is recorded.
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

    /// Syncs `key`'s data, then records `stored`, a snapshot taken before the sync: ranges recorded
    /// later are not covered by it.
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
        self.read_bytes.fetch_add(len, Ordering::Relaxed);
        Ok(out)
    }

    pub fn read_bytes(&self) -> u64 {
        self.read_bytes.load(Ordering::Relaxed)
    }

    pub fn touch(&self, key: &AreaKey) {
        let mut state = self.lock();
        let now = state.tick();
        if let Some(entry) = state.entries.get_mut(key) {
            entry.last_use = now;
        }
    }

    /// Changes `key`'s retention.
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

    /// Keeps `key` offline ([`Retention::kept_offline`]), or stops keeping it
    /// ([`Retention::released`]).
    pub fn keep_offline(&self, key: &AreaKey, keep: bool) -> io::Result<()> {
        let Some(current) = self.lock().entries.get(key).map(|entry| entry.retention) else {
            return Ok(());
        };
        let wanted = if keep {
            current.kept_offline()
        } else {
            current.released()
        };
        self.set_retention(key, wanted)
    }

    /// Keeps exactly the entries in `kept` offline: an evictable one among them is pinned, and a pinned
    /// entry outside them (unpinned since, an earlier version of a pinned file, a deleted file) is
    /// released to the budget. Entries only this device holds are never touched. Returns how many
    /// entries changed.
    pub fn keep_offline_only(&self, kept: &HashSet<AreaKey>) -> io::Result<usize> {
        let changed: Vec<AreaKey> = {
            let mut state = self.lock();
            state
                .entries
                .iter_mut()
                .filter_map(|(key, entry)| {
                    let wanted = if kept.contains(key) {
                        entry.retention.kept_offline()
                    } else {
                        entry.retention.released()
                    };
                    (wanted != entry.retention).then(|| {
                        entry.retention = wanted;
                        *key
                    })
                })
                .collect()
        };
        for key in &changed {
            self.persist(key)?;
        }
        Ok(changed.len())
    }

    /// Makes `key` a complete entry holding the first `len` bytes of `source`: cloned where the
    /// filesystem shares extents, copied otherwise. `chunks` are indexed as by [`Self::ensure`].
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
        self.install(
            key,
            &staging,
            target,
            len,
            retention,
            &Shape {
                kind: Kind::Content,
                digest: None,
                chunks,
                manifest: None,
            },
            true,
        )
    }

    pub fn adopt_bytes(
        &self,
        key: &AreaKey,
        bytes: &[u8],
        retention: Retention,
        chunks: &[([u8; 32], u64, u32)],
    ) -> io::Result<()> {
        // A chunk file per version is what a copy of small files writes; syncing each one here
        // made a photo pay four fsyncs, tens of milliseconds each on a common disk. A store
        // syncing on a window leaves them to the barrier instead.
        let windowed = self.durable && !self.sync_writes.load(Ordering::Relaxed);
        let staging = self.staging_path(key);
        let result = (|| {
            let target = OpenOptions::new()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&staging)?;
            target.write_all_at(bytes, 0)?;
            if !windowed {
                self.sync(&target)?;
            }
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
            &Shape {
                kind: Kind::Content,
                digest: None,
                chunks,
                manifest: None,
            },
            !windowed,
        )
    }

    /// Makes `key` a complete entry holding the file at `path` as it already stands, linked in
    /// (no second write) when both are on one filesystem. The caller's file is left as it was:
    /// it still reads the same bytes, which are now the entry's too. `chunks` are indexed as by
    /// [`Self::ensure`], so a peer is served their ranges of this entry, and `manifest`, the
    /// encoded manifest those chunks are named by, is stored inline in the same commit.
    ///
    /// A store syncing on a window leaves the entry to the barrier, as [`Self::adopt_bytes`]
    /// does, so a version a close answered for costs one write and the window's one sync rather
    /// than a write, a copy and an fsync of each piece.
    ///
    /// # Errors
    /// The file could not be linked in (another filesystem is `EXDEV`), or the entry could not be
    /// stored. The caller's file is untouched unless the link succeeded.
    pub fn adopt_version_file(
        &self,
        key: &AreaKey,
        path: &Path,
        len: u64,
        retention: Retention,
        chunks: &[([u8; 32], u64, u32)],
        manifest: &[u8],
    ) -> io::Result<()> {
        let windowed = self.durable && !self.sync_writes.load(Ordering::Relaxed);
        let staging = self.staging_path(key);
        std::fs::hard_link(path, &staging)?;
        let target = match OpenOptions::new().read(true).write(true).open(&staging) {
            Ok(target) => target,
            Err(error) => {
                let _ = std::fs::remove_file(&staging);
                return Err(error);
            }
        };
        if !windowed && let Err(error) = self.sync(&target) {
            let _ = std::fs::remove_file(&staging);
            return Err(error);
        }
        self.install(
            key,
            &staging,
            target,
            len,
            retention,
            &Shape {
                kind: Kind::Content,
                digest: None,
                chunks,
                manifest: Some(manifest),
            },
            !windowed,
        )
    }

    /// Make the entry files written since the last sync durable: the data half of a window
    /// barrier, run before the index that names them is made durable and before the mount
    /// syncs the journals that name the tree, so a name a barrier makes durable never points
    /// at bytes that are not. Costs the writes since the last barrier, not the store's size.
    ///
    /// A process killed before this still holds the bytes (the page cache outlives it), and
    /// the commits that record them are in the log; only a power loss can lose the window.
    ///
    /// Answers whether it also made the index durable: it commits the index durably under the
    /// lock persist takes, so no record committed after this drain can slip into that commit
    /// without its bytes, and the caller need not sync the database again.
    ///
    /// # Errors
    /// An entry's file could not be synced; its key stays queued for the next barrier, and no
    /// record is made durable.
    pub fn sync_written(&self) -> io::Result<bool> {
        if !self.durable {
            self.written.lock_recover().clear();
            return Ok(false);
        }
        if self.written.lock_recover().is_empty() {
            return Ok(false);
        }
        // Persists take this lock, so a record committed after the drain below is one the next
        // barrier covers, never one this barrier makes durable without its bytes. A fetch that
        // fills an entry writes its bytes before it persists, so a range it lands while this
        // fsyncs is either in the bytes below or in a record that waits for the next barrier.
        let _commit = self.commit.lock_recover();
        let pending = std::mem::take(&mut *self.written.lock_recover());
        // A window of a copy closes one small entry per file, and syncing them one after another
        // makes the window cost their count. The filesystem batches the fsyncs of a group into one
        // commit anyway, so a few syncers take the same window in a fraction of the time.
        let syncers = std::thread::available_parallelism()
            .map_or(4, |cores| cores.get().clamp(2, 8))
            .min(pending.len().max(1));
        let per = pending.len().div_ceil(syncers).max(1);
        let (unsynced, first) = std::thread::scope(|scope| {
            let workers: Vec<_> = pending
                .chunks(per)
                .map(|chunk| {
                    scope.spawn(move || {
                        let mut unsynced = Vec::new();
                        let mut first = None;
                        for key in chunk {
                            // An entry removed or replaced since it was written has nothing left
                            // to sync; one replaced before this barrier queued its bytes again
                            // under the same key.
                            if self.entry(key).is_none() {
                                continue;
                            }
                            match self.file(key).and_then(|file| self.sync(&file)).map_err(
                                |error| {
                                    io::Error::new(
                                        error.kind(),
                                        format!("{}: {error}", self.path_of(key).display()),
                                    )
                                },
                            ) {
                                Ok(()) => {}
                                Err(error) => {
                                    unsynced.push(*key);
                                    first.get_or_insert(error);
                                }
                            }
                        }
                        (unsynced, first)
                    })
                })
                .collect();
            let mut unsynced = Vec::new();
            let mut first = None;
            for worker in workers {
                match worker.join() {
                    Ok((failed, error)) => {
                        unsynced.extend(failed);
                        if first.is_none() {
                            first = error;
                        }
                    }
                    Err(_) => {
                        first.get_or_insert_with(|| {
                            io::Error::other("a barrier's sync worker did not finish")
                        });
                    }
                }
            }
            (unsynced, first)
        });
        if let Some(error) = first {
            self.written.lock_recover().extend(unsynced);
            return Err(error);
        }
        // The bytes are on the disk before any record naming them is made durable.
        self.commit_at(&Write::Entries(Vec::new()), true)
            .map(drop)?;
        Ok(true)
    }

    /// Whether a written entry file waits for the next barrier ([`Self::sync_written`]).
    pub fn has_unsynced_data(&self) -> bool {
        !self.written.lock_recover().is_empty()
    }

    /// Makes `key` a complete entry by moving the file at `path` in (a copy when it lives on another
    /// filesystem). Returns the BLAKE3 of its bytes; another complete keyed entry with the same bytes
    /// shares extents with it where the filesystem allows.
    pub fn adopt_file(
        &self,
        key: &AreaKey,
        path: &Path,
        retention: Retention,
    ) -> io::Result<[u8; 32]> {
        let digest = hash_file(path)?;
        self.adopt_hashed_file(key, path, digest, retention)?;
        Ok(digest)
    }

    /// [`Self::adopt_file`] for a caller that already read the bytes' BLAKE3, so the file does not
    /// have to be read twice.
    pub fn adopt_hashed_file(
        &self,
        key: &AreaKey,
        path: &Path,
        digest: [u8; 32],
        retention: Retention,
    ) -> io::Result<()> {
        let staging = self.staging_path(key);
        if std::fs::rename(path, &staging).is_err() {
            std::fs::copy(path, &staging)?;
            std::fs::remove_file(path)?;
        }
        let target = OpenOptions::new().read(true).write(true).open(&staging)?;
        self.sync(&target)?;
        let len = target.metadata()?.len();
        self.share_extents(&digest, &target, len);
        self.install(
            key,
            &staging,
            target,
            len,
            retention,
            &Shape {
                kind: Kind::Content,
                digest: Some(digest),
                chunks: &[],
                manifest: None,
            },
            true,
        )
    }

    /// Makes `key` a complete whole-blob entry holding `bytes`, with `outboard` its BAO outboard.
    pub fn adopt_blob_bytes(&self, key: &AreaKey, bytes: &[u8], outboard: &[u8]) -> io::Result<()> {
        self.adopt_bytes(key, bytes, Retention::LocalOnly, &[])?;
        let mut state = self.lock();
        if let Some(entry) = state.entries.get_mut(key) {
            entry.kind = Kind::Blob;
            entry.digest = Some(*key);
        }
        state.digests.insert(*key, *key);
        drop(state);
        self.persist(key)?;
        self.put_outboard(key, outboard)
    }

    /// Makes `key` a complete whole-blob entry holding the first `len` bytes of `source`, cloned
    /// where the filesystem shares extents and copied otherwise, with `outboard` its BAO outboard.
    pub fn adopt_blob_clone(
        &self,
        key: &AreaKey,
        source: &File,
        len: u64,
        outboard: &[u8],
    ) -> io::Result<()> {
        self.adopt_clone(key, source, len, Retention::LocalOnly, &[])?;
        let mut state = self.lock();
        if let Some(entry) = state.entries.get_mut(key) {
            entry.kind = Kind::Blob;
            entry.digest = Some(*key);
        }
        state.digests.insert(*key, *key);
        drop(state);
        self.persist(key)?;
        self.put_outboard(key, outboard)
    }

    /// A fresh staging file under the area's data directory, for content that is hashed as it
    /// arrives: [`Self::install_staged_blob`] makes it a entry, and a caller that abandons the
    /// import removes the path.
    pub fn staging_file(&self) -> io::Result<(PathBuf, File)> {
        let n = self.staging.fetch_add(1, Ordering::Relaxed);
        let path = self.data.join(format!(".{n}.staging"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)?;
        Ok((path, file))
    }

    /// Makes `staging` (readable in full, `len` bytes long) the whole-blob entry `key`, whose BLAKE3
    /// is `key`, with `outboard` its BAO outboard.
    pub fn install_staged_blob(
        &self,
        key: &AreaKey,
        staging: &Path,
        len: u64,
        outboard: &[u8],
    ) -> io::Result<()> {
        let target = OpenOptions::new().read(true).write(true).open(staging)?;
        self.sync(&target)?;
        self.install(
            key,
            staging,
            target,
            len,
            Retention::LocalOnly,
            &Shape {
                kind: Kind::Blob,
                digest: Some(*key),
                chunks: &[],
                manifest: None,
            },
            true,
        )?;
        self.put_outboard(key, outboard)
    }

    fn staging_path(&self, key: &AreaKey) -> PathBuf {
        let n = self.staging.fetch_add(1, Ordering::Relaxed);
        self.data.join(format!(".{}.{n}.staging", hex::encode(key)))
    }

    fn install(
        &self,
        key: &AreaKey,
        staging: &Path,
        file: File,
        len: u64,
        retention: Retention,
        shape: &Shape<'_>,
        synced: bool,
    ) -> io::Result<()> {
        let (kind, digest, chunks) = (shape.kind, shape.digest, shape.chunks);
        let manifest = shape.manifest;
        let _create = self.create.lock_recover();
        if self.entry(key).is_some() {
            self.remove(key)?;
        }
        std::fs::rename(staging, self.path_of(key))?;
        if !synced {
            // Queued before the record is written: a barrier that makes the record durable
            // has the bytes it names synced, and one that runs between this and the record's
            // commit leaves the record to the next barrier.
            self.written.lock_recover().push(*key);
        }
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
                    kind,
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
        // One commit for everything a version stores: the entry's state, its chunk index, and the
        // manifest naming them, so a closed file costs one transaction rather than three.
        let stored = self
            .lock()
            .entries
            .get(key)
            .map(|entry| entry.stored_with(&entry.durable));
        let Some(stored) = stored else {
            return Ok(());
        };
        let bytes = postcard::to_allocvec(&stored).map_err(db_error)?;
        let mut writes = Vec::with_capacity(3);
        if !chunks.is_empty() {
            writes.push(Write::Chunks(*key, Cow::Borrowed(chunks)));
        }
        if let Some(manifest) = manifest {
            writes.push(Write::Inline(vec![(*key, Cow::Borrowed(manifest))]));
        }
        writes.push(Write::Entries(vec![(*key, Cow::Owned(bytes))]));
        self.commit_write(&Write::Batch(writes)).map(drop)
    }

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

    /// Writes `len` bytes read from `body` at `offset` of `key` (an entry of `size` bytes, created if
    /// needed) and records them resident. A keyed entry that becomes complete is fingerprinted and
    /// shares extents with an identical one.
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

    /// Copies `range` of `key` into `dest` at the same offsets: a clone of shared extents where the
    /// filesystem allows. Every byte must be resident.
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
    /// `adopt_fetched` makes `key` a complete [`Retention::Fetchable`] entry holding `bytes`, fetched
    /// from `origin`, where a read fetches them again once the budget dropped them.
    pub fn adopt_fetched(&self, key: &AreaKey, bytes: &[u8], origin: &str) -> io::Result<()> {
        self.adopt_bytes(key, bytes, Retention::Fetchable, &[])?;
        let _commit = self.commit.lock_recover();
        self.commit_write(&Write::Origin(*key, Cow::Borrowed(origin)))
            .map(drop)
    }

    /// Where the bytes of a [`Retention::Fetchable`] entry are fetched from.
    pub fn origin(&self, key: &AreaKey) -> Option<String> {
        let txn = self.database().ok()?.begin_read().ok()?;
        let table = txn.open_table(ORIGINS).ok()?;
        table
            .get(*key)
            .ok()?
            .map(|origin| origin.value().to_owned())
    }

    /// Drops the bytes of a [`Retention::Fetchable`] entry and keeps the entry, its size and its origin:
    /// reading it again fetches them.
    pub fn drop_bytes(&self, key: &AreaKey) -> io::Result<()> {
        let size = {
            let mut state = self.lock();
            let generation = state.next_generation();
            let Some(entry) = state.entries.get_mut(key) else {
                return Ok(());
            };
            entry.resident = RangeSet::new();
            entry.durable = RangeSet::new();
            entry.unsynced = 0;
            entry.generation = generation;
            let size = entry.size;
            state.handles.remove(key);
            size
        };
        // A sparse file of the entry's size: its blocks are freed, and the entry still loads after a
        // restart.
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(self.path_of(key))?;
        file.set_len(size)?;
        self.persist(key)
    }

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

    /// Drops cached entries until their resident bytes fit the budget: first those an always-on device
    /// holds, outside the [working set](WORKING_SET_ENTRIES), then the rest, least recently used first
    /// within each. A [`Retention::Fetchable`] entry loses only its bytes. `keep` are never dropped.
    /// Returns the bytes freed.
    pub fn evict_to_budget(&self, keep: &[AreaKey]) -> u64 {
        let mut uses: Vec<u64> = {
            let state = self.lock();
            if Self::cached_bytes(&state) <= self.budget() {
                return 0;
            }
            state
                .entries
                .values()
                .filter(|e| e.retention.evictable() && !e.resident.is_empty())
                .map(|e| e.last_use)
                .collect()
        };
        uses.sort_unstable_by(|a, b| b.cmp(a));
        let working_set_since = uses
            .get(WORKING_SET_ENTRIES.saturating_sub(1))
            .copied()
            .unwrap_or(0);
        // Asked outside the area's lock: the answer may take other locks.
        let held = self.held_elsewhere.read_recover().clone();
        let held_elsewhere = held.map(|held| held()).unwrap_or_default();
        let mut freed = 0_u64;
        loop {
            let victim = {
                let state = self.lock();
                if Self::cached_bytes(&state) <= self.budget() {
                    return freed;
                }
                state
                    .entries
                    .iter()
                    .filter(|(key, e)| {
                        e.retention.evictable() && !e.resident.is_empty() && !keep.contains(key)
                    })
                    .min_by_key(|(key, e)| {
                        let give_up_first =
                            e.last_use < working_set_since && held_elsewhere.contains(*key);
                        (!give_up_first, e.last_use)
                    })
                    .map(|(key, e)| (*key, e.resident.total(), e.retention))
            };
            let Some((key, bytes, retention)) = victim else {
                return freed;
            };
            let dropped = if retention == Retention::Fetchable {
                self.drop_bytes(&key)
            } else {
                self.remove(&key)
            };
            if dropped.is_err() {
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
        let _commit = self.commit.lock_recover();
        if !self.lock().entries.contains_key(key) {
            return Ok(());
        }
        let bytes = postcard::to_allocvec(stored).map_err(db_error)?;
        self.commit_write(&Write::Entries(vec![(*key, Cow::Owned(bytes))]))
            .map(drop)
    }

    fn unpersist(&self, key: &AreaKey) -> io::Result<()> {
        let _commit = self.commit.lock_recover();
        self.commit_write(&Write::Unpersist(*key)).map(drop)
    }

    /// Syncs and records every entry's residency and last use, so progress and eviction order survive a
    /// restart.
    pub fn flush(&self) -> io::Result<()> {
        // Bytes a window left to its barrier are made durable here too, so a store that stops
        // before the next barrier still holds everything it accepted.
        let _ = self.sync_written()?;
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
        let _commit = self.commit.lock_recover();
        let mut states = Vec::with_capacity(stored_before_sync.len());
        for (key, stored) in stored_before_sync {
            let bytes = postcard::to_allocvec(&stored).map_err(db_error)?;
            states.push((key, Cow::Owned(bytes)));
        }
        self.commit_at(&Write::Entries(states), true).map(drop)
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

fn data_dir(dir: &Path) -> PathBuf {
    dir.join("data")
}

/// `key`'s whole file from the area under `dir`, read without opening the area: for another process
/// that must not take the area's database lock. `Ok(None)` when there is no regular file for `key` or
/// it is longer than `max_bytes`.
///
/// The bytes are neither verified nor known to be wholly resident (missing ranges of a sparse entry
/// read as zeros): the caller checks them against a hash it trusts.
pub fn read_entry_unlocked(
    dir: &Path,
    key: &AreaKey,
    max_bytes: u64,
) -> io::Result<Option<Vec<u8>>> {
    use std::os::unix::fs::MetadataExt as _;
    let path = data_dir(dir).join(hex::encode(key));
    let listed = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !listed.is_file() || listed.len() > max_bytes {
        return Ok(None);
    }
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // The file read must be the one inspected: an entry replaced in between is simply not read this
    // time.
    let opened = file.metadata()?;
    if (opened.dev(), opened.ino()) != (listed.dev(), listed.ino()) || opened.len() > max_bytes {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).unwrap_or(0));
    file.take(max_bytes).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

pub fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(File::open(path)?)?;
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(all(target_os = "linux", feature = "filesystem"))]
fn reflink(source: &File, target: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    /// `_IOW(0x94, 9, int)`, typed as each C library takes ioctl's request.
    const FICLONE: libc::Ioctl = 0x4004_9409;
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

/// Makes `target` hold the first `len` bytes of `source`, sharing extents where the filesystem can.
fn clone_or_copy(source: &File, target: &File, len: u64) -> io::Result<()> {
    if source.metadata()?.len() == len && reflink(source, target).is_ok() {
        return Ok(());
    }
    target.set_len(0)?;
    copy_range(source, 0, target, 0, len)?;
    target.set_len(len)
}

/// Copies `len` bytes from `source` at `from` to `dest` at `to`; the kernel clones shared extents
/// where it can (`copy_file_range`).
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

    /// What a process killed now leaves on disk: the area's files as they are while it is open.
    fn left_by_a_kill(from: &Path, to: &Path) {
        std::fs::create_dir_all(data_dir(to)).unwrap();
        for name in ["area.redb", "area.log"] {
            std::fs::copy(from.join(name), to.join(name)).unwrap();
        }
        for file in std::fs::read_dir(data_dir(from)).unwrap() {
            let file = file.unwrap();
            std::fs::copy(file.path(), data_dir(to).join(file.file_name())).unwrap();
        }
    }

    #[test]
    fn a_commit_that_does_not_wait_for_the_disk_outlives_a_kill() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live");
        let area = FileArea::open(&live, u64::MAX).unwrap();
        area.set_sync_writes(false);
        area.put_inline([(key(1), Bytes::from_static(b"a closed file"))])
            .unwrap();
        area.set_reference("media/one", &key(1)).unwrap();
        area.adopt_bytes(
            &key(2),
            &[9; 4096],
            Retention::LocalOnly,
            &[([3; 32], 0, 4096)],
        )
        .unwrap();
        assert!(area.has_unsynced());

        let killed = dir.path().join("killed");
        left_by_a_kill(&live, &killed);
        let reopened = FileArea::open(&killed, u64::MAX).unwrap();
        assert_eq!(reopened.get_inline(&key(1)).unwrap(), b"a closed file");
        assert_eq!(reopened.reference("media/one"), Some(key(1)));
        assert!(reopened.entry(&key(2)).unwrap().complete());
        assert_eq!(reopened.find_chunk(&[3; 32]).unwrap().key, key(2));
        assert!(
            !reopened.has_unsynced(),
            "opening applies the log and empties it"
        );

        // The database alone does not hold those commits: the log is what keeps them.
        let without_log = dir.path().join("without-log");
        left_by_a_kill(&live, &without_log);
        std::fs::remove_file(without_log.join("area.log")).unwrap();
        let reopened = FileArea::open(&without_log, u64::MAX).unwrap();
        assert!(reopened.get_inline(&key(1)).is_none());

        area.sync_db().unwrap();
        assert!(!area.has_unsynced(), "a sync empties the log");
        let synced = dir.path().join("synced");
        left_by_a_kill(&live, &synced);
        std::fs::remove_file(synced.join("area.log")).unwrap();
        let reopened = FileArea::open(&synced, u64::MAX).unwrap();
        assert_eq!(reopened.get_inline(&key(1)).unwrap(), b"a closed file");
    }

    /// A store syncing on a window leaves a written entry file to the barrier, and the record
    /// names its bytes at once: a process killed before the barrier keeps them, a barrier makes
    /// them durable before the index.
    #[test]
    fn a_window_leaves_entry_bytes_to_the_barrier() {
        let dir = tempfile::tempdir().unwrap();
        let area = FileArea::open(dir.path(), u64::MAX).unwrap();
        area.set_sync_writes(false);
        area.adopt_bytes(
            &key(7),
            &[5; 8192],
            Retention::LocalOnly,
            &[([9; 32], 0, 8192)],
        )
        .unwrap();
        assert!(
            area.has_unsynced_data(),
            "the window leaves the file to the barrier"
        );
        assert!(
            area.entry(&key(7)).is_some_and(|entry| entry.complete()),
            "the record names the bytes the moment they are written"
        );
        assert_eq!(area.read_at(&key(7), 0, 4).unwrap(), [5; 4]);
        assert!(
            area.sync_written().unwrap(),
            "the barrier syncs them and makes the index durable"
        );
        assert!(
            !area.has_unsynced_data(),
            "nothing waits for the next barrier"
        );
    }

    #[test]
    fn a_commit_that_waits_for_the_disk_adds_nothing_to_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let area = FileArea::open(dir.path(), u64::MAX).unwrap();
        area.put_inline([(key(1), Bytes::from_static(b"synced"))])
            .unwrap();
        assert!(!area.has_unsynced());
        assert_eq!(
            std::fs::metadata(dir.path().join("area.log"))
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn inline_blobs_round_trip_and_survive_reopening() {
        let dir = tempfile::tempdir().unwrap();
        {
            let area = FileArea::open(dir.path(), u64::MAX).unwrap();
            area.put_inline([
                (key(1), Bytes::from_static(b"a manifest")),
                (key(2), Bytes::from(vec![7u8; 300_000])),
            ])
            .unwrap();
            assert_eq!(
                area.get_inline(&key(1)).unwrap(),
                b"a manifest".to_vec(),
                "inline bytes read back what was written"
            );
            assert!(area.has_inline(&key(2)));
            assert!(!area.has_inline(&key(3)));
            let keys = {
                let mut keys = area.inline_keys();
                keys.sort_unstable();
                keys
            };
            assert_eq!(keys, vec![key(1), key(2)]);
            assert!(
                area.entry(&key(1)).is_none(),
                "inline blobs are not file entries"
            );
            assert!(area.remove_inline(&key(1)).unwrap());
            assert!(!area.remove_inline(&key(1)).unwrap());
            area.flush().unwrap();
        }
        let area = FileArea::open(dir.path(), u64::MAX).unwrap();
        assert!(area.get_inline(&key(1)).is_none());
        assert_eq!(area.get_inline(&key(2)).unwrap().len(), 300_000);
    }

    #[test]
    fn whole_blobs_keep_their_outboard_and_are_listed_as_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let area = FileArea::open(dir.path(), u64::MAX).unwrap();
        let bytes: Vec<u8> = (0..70_000_u32).map(|n| (n % 251) as u8).collect();
        area.adopt_blob_bytes(&key(1), &bytes, b"outboard").unwrap();
        assert_eq!(area.get_inline(&key(1)), None);
        assert!(area.is_blob(&key(1)));
        assert_eq!(area.blob_keys(), vec![key(1)]);
        assert_eq!(area.read_at(&key(1), 4096, 16).unwrap(), bytes[4096..4112]);
        assert_eq!(area.outboard(&key(1)).unwrap(), b"outboard");
        assert_eq!(
            area.entry(&key(1)).unwrap(),
            EntryInfo {
                size: 70_000,
                resident: 70_000,
                retention: Retention::LocalOnly,
            }
        );
        area.flush().unwrap();
        drop(area);

        let area = FileArea::open(dir.path(), u64::MAX).unwrap();
        assert!(area.is_blob(&key(1)));
        assert_eq!(area.outboard(&key(1)).unwrap(), b"outboard");
        area.remove(&key(1)).unwrap();
        assert!(
            area.outboard(&key(1)).is_none(),
            "removal drops the outboard"
        );
        assert!(area.blob_keys().is_empty());
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

        let dir = tempfile::tempdir().unwrap();
        let area = FileArea::open(dir.path(), 0).unwrap();
        area.put_inline([(key(1), Bytes::from(vec![1u8; 4096]))])
            .unwrap();
        area.adopt_bytes(&key(2), &[2u8; 4096], Retention::Cached, &[])
            .unwrap();
        assert_eq!(
            area.evict_to_budget(&[]),
            4096,
            "the cached entry goes, the inline blob stays"
        );
        assert!(area.entry(&key(2)).is_none());
        assert_eq!(area.get_inline(&key(1)).unwrap().len(), 4096);
    }

    tilo_test_env::scenario! {
        fn the_file_area_gives_up_bytes_in_order_and_finds_resident_chunks;

        phase the_budget_drops_a_fetchable_entrys_bytes_and_keeps_where_they_came_from;
        {
            let dir = tempfile::tempdir().unwrap();
            let origin = "https://example.org/catalogue.csv";
            let data = [9_u8; 100];
            {
                let area = FileArea::open(dir.path(), 250).unwrap();
                area.adopt_fetched(&key(1), &data, origin).unwrap();
                area.adopt_bytes(&key(2), &data, Retention::Cached, &[])
                    .unwrap();
                area.adopt_bytes(&key(3), &data, Retention::LocalOnly, &[])
                    .unwrap();
                area.touch(&key(2));
                area.adopt_bytes(&key(4), &data, Retention::Cached, &[])
                    .unwrap();
                assert_eq!(area.evict_to_budget(&[]), 100);
                let entry = area.entry(&key(1)).expect("the entry stays");
                assert_eq!(
                    (entry.retention, entry.size, entry.resident),
                    (Retention::Fetchable, 100, 0),
                    "only its bytes went"
                );
                assert_eq!(area.origin(&key(1)).as_deref(), Some(origin));
                assert!(area.entry(&key(2)).is_some() && area.entry(&key(4)).is_some());
                assert_eq!(area.missing(&key(1), &(0..100)), vec![0..100]);
            }
            let area = FileArea::open(dir.path(), u64::MAX).unwrap();
            assert_eq!(area.entry(&key(1)).map(|e| e.resident), Some(0));
            assert_eq!(area.origin(&key(1)).as_deref(), Some(origin));
            area.adopt_fetched(&key(1), &data, origin).unwrap();
            assert!(area.entry(&key(1)).unwrap().complete());
            assert_eq!(area.read_at(&key(1), 0, 100).unwrap(), data);
            area.remove(&key(1)).unwrap();
            assert_eq!(area.origin(&key(1)), None, "removed with its entry");
        }

        phase the_budget_gives_up_what_an_always_on_device_holds_first;
        {
            let dir = tempfile::tempdir().unwrap();
            let area = FileArea::open(dir.path(), u64::MAX).unwrap();
            let data = [7_u8; 100];
            let working_set: Vec<u8> = (10..10 + u8::try_from(WORKING_SET_ENTRIES).unwrap()).collect();
            for n in (1..=4).chain(working_set.iter().copied()) {
                area.adopt_bytes(&key(n), &data, Retention::Cached, &[])
                    .unwrap();
                area.touch(&key(n));
            }
            area.adopt_bytes(&key(5), &data, Retention::Pinned, &[])
                .unwrap();
            area.adopt_bytes(&key(6), &data, Retention::LocalOnly, &[])
                .unwrap();
            let last = *working_set.last().unwrap();
            area.set_held_elsewhere(Arc::new(move || {
                [key(3), key(4), key(last), key(5), key(6)].into()
            }));

            let cached = 100 * (4 + WORKING_SET_ENTRIES as u64);
            area.set_budget(cached - 200);
            assert_eq!(area.evict_to_budget(&[]), 200);
            assert!(area.entry(&key(3)).is_none() && area.entry(&key(4)).is_none());
            assert!(
                area.entry(&key(1)).is_some() && area.entry(&key(2)).is_some(),
                "content only this device's cache may still hold outlives what the host keeps"
            );
            assert!(
                area.entry(&key(last)).is_some(),
                "the working set is not given up for being held elsewhere"
            );

            area.set_budget(0);
            area.evict_to_budget(&[]);
            assert!(
                area.entry(&key(1)).is_none() && area.entry(&key(last)).is_none(),
                "the budget is still met once nothing held elsewhere is left"
            );
            assert!(
                area.entry(&key(5)).is_some() && area.entry(&key(6)).is_some(),
                "being held elsewhere never makes pinned or local-only content evictable"
            );
        }

        phase chunks_are_found_where_they_are_resident;
        {
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
