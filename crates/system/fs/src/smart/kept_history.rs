//! The mount's opened-together history, kept on this device across restarts.
//!
//! [`super::history::AccessHistory`] learns which files are opened together;
//! without this module every restart forgets it, and a pair needs two fresh
//! occasions before it predicts anything again. The keeper writes the pairs
//! to one small file per workspace in the cache directory
//! (`<cache root>/prefetch-history/<workspace>`) and reads them back when the
//! mount starts.
//!
//! - **Off the request path.** An open only records into memory and wakes the
//!   keeper; the keeper waits a quiet interval, so a burst of opens costs one
//!   write, and writes from a blocking worker.
//! - **Stable, already-known identifiers only.** Pairs are kept by the
//!   namespace's record ids, never paths or names, and a record that no
//!   longer resolves to exactly one live file is dropped on load. Partners
//!   are still re-checked against the access rules at prediction time.
//! - **Bounded.** At most [`MAX_PAIRS`] pairs, none older than [`MAX_AGE`].
//! - **Crash-tolerant.** The file is replaced atomically and checksummed; a
//!   torn, truncated or otherwise invalid file is deleted, never partly
//!   trusted.
//! - **Private.** Owner-only, never synced to other devices or members,
//!   deleted when the person turns prefetching off, and deleted for any
//!   workspace the cache directory no longer serves.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs_protocol::FileRecordId;
use tilo_lock::MutexRecover as _;
use uuid::Uuid;

use super::Heuristics;
use super::history::{AccessHistory, PairRecord};
use crate::namespace::Namespace;

/// The directory under the cache root that holds one file per workspace.
pub const PREFETCH_HISTORY_DIR: &str = "prefetch-history";
/// Pairs kept at most: the in-memory table's own bound.
pub const MAX_PAIRS: usize = 2048;
/// A pair not opened together for this long is forgotten.
pub const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// How long the keeper waits after a change before writing, so the opens of
/// one sitting share a write.
pub const FLUSH_DELAY: Duration = Duration::from_secs(20);

/// Format and version; a different version is discarded like corruption.
const MAGIC: &[u8; 8] = b"TILOPFH1";
const HEADER_BYTES: usize = MAGIC.len() + 4;
/// Two record ids, the occasions and when the pair was last seen.
const ENTRY_BYTES: usize = 16 + 16 + 4 + 8;
const CHECKSUM_BYTES: usize = 32;
/// The largest valid file; anything larger is not read.
pub const MAX_FILE_BYTES: usize = HEADER_BYTES + MAX_PAIRS * ENTRY_BYTES + CHECKSUM_BYTES;

/// Why a kept file was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Corrupt {
    Truncated,
    WrongMagic,
    TooManyPairs,
    WrongLength,
    Checksum,
    BadEntry,
}

impl std::fmt::Display for Corrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self {
            Self::Truncated => "truncated",
            Self::WrongMagic => "not a prefetch history of this version",
            Self::TooManyPairs => "more pairs than the bound",
            Self::WrongLength => "length does not match its pair count",
            Self::Checksum => "checksum mismatch",
            Self::BadEntry => "invalid pair",
        };
        f.write_str(what)
    }
}

fn unix_seconds(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The file's bytes for `pairs` as of `now`: the freshest [`MAX_PAIRS`]
/// pairs no older than [`MAX_AGE`].
#[must_use]
pub fn encode(pairs: &[PairRecord<FileRecordId>], now: SystemTime) -> Vec<u8> {
    let mut kept: Vec<&PairRecord<FileRecordId>> = pairs
        .iter()
        .filter(|pair| pair.age <= MAX_AGE && pair.a != pair.b && pair.occasions > 0)
        .collect();
    kept.sort_by(|x, y| x.age.cmp(&y.age).then(x.a.cmp(&y.a)).then(x.b.cmp(&y.b)));
    kept.truncate(MAX_PAIRS);
    let now = unix_seconds(now);
    let count = u32::try_from(kept.len()).unwrap_or(u32::MAX);
    let mut bytes = Vec::with_capacity(
        kept.len()
            .saturating_mul(ENTRY_BYTES)
            .saturating_add(HEADER_BYTES + CHECKSUM_BYTES),
    );
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&count.to_le_bytes());
    for pair in kept {
        bytes.extend_from_slice(pair.a.0.as_bytes());
        bytes.extend_from_slice(pair.b.0.as_bytes());
        bytes.extend_from_slice(&pair.occasions.to_le_bytes());
        bytes.extend_from_slice(&now.saturating_sub(pair.age.as_secs()).to_le_bytes());
    }
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    bytes
}

/// The pairs in a kept file as of `now`, or why the whole file is refused.
/// Pairs older than [`MAX_AGE`] are dropped; a pair stamped in the future
/// (the clock moved back) counts as just seen.
pub fn decode(bytes: &[u8], now: SystemTime) -> Result<Vec<PairRecord<FileRecordId>>, Corrupt> {
    let (body, checksum) = bytes
        .split_at_checked(bytes.len().saturating_sub(CHECKSUM_BYTES))
        .filter(|(body, _)| body.len() >= HEADER_BYTES)
        .ok_or(Corrupt::Truncated)?;
    let (magic, rest) = body
        .split_at_checked(MAGIC.len())
        .ok_or(Corrupt::Truncated)?;
    if magic != MAGIC {
        return Err(Corrupt::WrongMagic);
    }
    let (count, entries) = rest.split_at_checked(4).ok_or(Corrupt::Truncated)?;
    let count = u32::from_le_bytes(count.try_into().map_err(|_| Corrupt::Truncated)?);
    let count = usize::try_from(count).map_err(|_| Corrupt::TooManyPairs)?;
    if count > MAX_PAIRS {
        return Err(Corrupt::TooManyPairs);
    }
    if entries.len() != count.saturating_mul(ENTRY_BYTES) {
        return Err(Corrupt::WrongLength);
    }
    if blake3::hash(body).as_bytes().as_slice() != checksum {
        return Err(Corrupt::Checksum);
    }
    let now = unix_seconds(now);
    let mut pairs = Vec::with_capacity(count);
    for entry in entries.chunks_exact(ENTRY_BYTES) {
        let field = |range: std::ops::Range<usize>| entry.get(range).ok_or(Corrupt::BadEntry);
        let id = |range| -> Result<FileRecordId, Corrupt> {
            let bytes: [u8; 16] = field(range)?.try_into().map_err(|_| Corrupt::BadEntry)?;
            Ok(FileRecordId(Uuid::from_bytes(bytes)))
        };
        let (a, b) = (id(0..16)?, id(16..32)?);
        let occasions =
            u32::from_le_bytes(field(32..36)?.try_into().map_err(|_| Corrupt::BadEntry)?);
        let last = u64::from_le_bytes(field(36..44)?.try_into().map_err(|_| Corrupt::BadEntry)?);
        if a == b || occasions == 0 {
            return Err(Corrupt::BadEntry);
        }
        let age = Duration::from_secs(now.saturating_sub(last));
        if age <= MAX_AGE {
            pairs.push(PairRecord {
                a,
                b,
                occasions,
                age,
            });
        }
    }
    Ok(pairs)
}

/// Where `workspace`'s history is kept under `cache_root`.
#[must_use]
pub fn path_for(cache_root: &Path, workspace: Uuid) -> PathBuf {
    cache_root
        .join(PREFETCH_HISTORY_DIR)
        .join(workspace.to_string())
}

#[derive(Debug, Default)]
struct FileState {
    /// Bumped by every [`KeptHistory::clear`]: a flush that began before a
    /// clear must not write what the clear removed.
    epoch: u64,
    written_revision: u64,
}

/// Keeps one mount's opened-together history in its workspace's file.
pub struct KeptHistory {
    path: PathBuf,
    history: Arc<Mutex<AccessHistory>>,
    namespace: Arc<Mutex<Namespace>>,
    heuristics: Arc<Heuristics>,
    state: Mutex<FileState>,
    wake: Arc<tokio::sync::Notify>,
    writes: AtomicU64,
}

impl std::fmt::Debug for KeptHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeptHistory")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// What [`KeptHistory::open`] keeps and where.
pub struct KeptHistorySource<'a> {
    pub cache_root: &'a Path,
    pub workspace: Uuid,
    pub history: Arc<Mutex<AccessHistory>>,
    pub namespace: Arc<Mutex<Namespace>>,
    pub heuristics: Arc<Heuristics>,
}

impl KeptHistory {
    /// Start keeping `source.history` for `source.workspace`: forget the
    /// files of workspaces this cache directory no longer serves, restore
    /// what was kept for this one (or delete it, if the person has turned
    /// prefetching off), follow the person's switch, and start the keeper
    /// on `runtime`, which writes `flush_delay` after a change.
    pub fn open(
        source: KeptHistorySource<'_>,
        runtime: &tokio::runtime::Handle,
        flush_delay: Duration,
    ) -> Arc<Self> {
        let path = path_for(source.cache_root, source.workspace);
        forget_other_workspaces(&path);
        let kept = Arc::new(Self {
            path,
            history: source.history,
            namespace: source.namespace,
            heuristics: source.heuristics,
            state: Mutex::new(FileState::default()),
            wake: Arc::new(tokio::sync::Notify::new()),
            writes: AtomicU64::new(0),
        });
        if kept.heuristics.prefetch_enabled() {
            kept.restore();
        } else {
            kept.remove_file();
        }
        let weak = Arc::downgrade(&kept);
        kept.heuristics.on_prefetch_off(Box::new(move || {
            if let Some(kept) = weak.upgrade() {
                kept.clear();
            }
        }));
        spawn_keeper(
            runtime,
            Arc::downgrade(&kept),
            Arc::clone(&kept.wake),
            flush_delay,
        );
        kept
    }

    /// Tell the keeper the history changed. Never blocks and never writes:
    /// this is what an open on the request path may call.
    pub fn changed(&self) {
        self.wake.notify_one();
    }

    /// How many times the file was written.
    #[must_use]
    pub fn writes(&self) -> u64 {
        self.writes.load(Ordering::Acquire)
    }

    /// The file this history is kept in.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, FileState> {
        self.state.lock_recover()
    }

    fn lock_history(&self) -> std::sync::MutexGuard<'_, AccessHistory> {
        self.history.lock_recover()
    }

    fn restore(&self) {
        let bytes = match read_bounded(&self.path) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return,
            Err(error) => {
                eprintln!(
                    "discarding the prefetch history at {}: {error}",
                    self.path.display()
                );
                self.remove_file();
                return;
            }
        };
        let pairs = match decode(&bytes, SystemTime::now()) {
            Ok(pairs) => pairs,
            Err(corrupt) => {
                eprintln!(
                    "discarding the prefetch history at {}: {corrupt}",
                    self.path.display()
                );
                self.remove_file();
                return;
            }
        };
        let resolved: Vec<PairRecord<u64>> = {
            let mut namespace = self.namespace.lock_recover();
            let mut inodes: HashMap<FileRecordId, Option<u64>> = HashMap::new();
            let mut ino = |record: FileRecordId| {
                *inodes
                    .entry(record)
                    .or_insert_with(|| namespace.ensure_ino_for_record_id(record))
            };
            pairs
                .into_iter()
                .filter_map(|pair| {
                    Some(PairRecord {
                        a: ino(pair.a)?,
                        b: ino(pair.b)?,
                        occasions: pair.occasions,
                        age: pair.age,
                    })
                })
                .collect()
        };
        let mut state = self.lock_state();
        let mut history = self.lock_history();
        let now = Instant::now();
        for pair in resolved {
            history.restore_pair(pair, now);
        }
        state.written_revision = history.revision();
    }

    /// Write the history if it changed since the last write and the person
    /// still allows prefetching. Returns whether it wrote.
    ///
    /// # Errors
    ///
    /// Returns the write's error; the next change retries.
    pub fn flush(&self) -> io::Result<bool> {
        if !self.heuristics.prefetch_enabled() {
            return Ok(false);
        }
        let (epoch, written) = {
            let state = self.lock_state();
            (state.epoch, state.written_revision)
        };
        let (revision, pairs) = {
            let history = self.lock_history();
            (history.revision(), history.pairs(Instant::now()))
        };
        if revision == written {
            return Ok(false);
        }
        let records: Vec<PairRecord<FileRecordId>> = {
            let namespace = self.namespace.lock_recover();
            let mut records: HashMap<u64, Option<FileRecordId>> = HashMap::new();
            let mut record = |ino: u64| {
                *records
                    .entry(ino)
                    .or_insert_with(|| namespace.record_id_for(ino))
            };
            pairs
                .into_iter()
                .filter_map(|pair| {
                    Some(PairRecord {
                        a: record(pair.a)?,
                        b: record(pair.b)?,
                        occasions: pair.occasions,
                        age: pair.age,
                    })
                })
                .collect()
        };
        let bytes = encode(&records, SystemTime::now());
        let mut state = self.lock_state();
        // Re-checked under the lock a clear takes: nothing written after the
        // person turned prefetching off, and nothing a clear removed.
        if state.epoch != epoch || !self.heuristics.prefetch_enabled() {
            return Ok(false);
        }
        tilo_files::native::atomic::write_private(&self.path, &bytes)?;
        self.writes.fetch_add(1, Ordering::AcqRel);
        state.written_revision = revision;
        drop(state);
        Ok(true)
    }

    /// Forget the history, in memory and on disk, at once.
    pub fn clear(&self) {
        let mut state = self.lock_state();
        state.epoch = state.epoch.wrapping_add(1);
        let mut history = self.lock_history();
        history.clear();
        state.written_revision = history.revision();
        drop(history);
        // Still under the state lock, so no flush lands between the two.
        self.remove_file();
        drop(state);
    }

    fn remove_file(&self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => eprintln!(
                "could not delete the prefetch history at {}: {error}",
                self.path.display()
            ),
        }
    }
}

impl Drop for KeptHistory {
    /// The mount is going away: keep what changed since the last write, and
    /// let the keeper task end.
    fn drop(&mut self) {
        if let Err(error) = self.flush() {
            eprintln!(
                "could not keep the prefetch history at {}: {error}",
                self.path.display()
            );
        }
        self.wake.notify_one();
    }
}

/// The keeper: after each change, wait `delay` for the burst to end, then
/// write from a blocking worker. Ends once the history is gone.
fn spawn_keeper(
    runtime: &tokio::runtime::Handle,
    kept: Weak<KeptHistory>,
    wake: Arc<tokio::sync::Notify>,
    delay: Duration,
) {
    runtime.spawn(async move {
        loop {
            wake.notified().await;
            if kept.strong_count() == 0 {
                return;
            }
            tokio::time::sleep(delay).await;
            let Some(history) = kept.upgrade() else {
                return;
            };
            let flushed = tokio::task::spawn_blocking(move || history.flush()).await;
            if let Ok(Err(error)) = flushed {
                eprintln!("could not keep the prefetch history: {error}");
            }
        }
    });
}

/// Read `path` if it exists and is no larger than a valid file can be.
fn read_bounded(path: &Path) -> io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let limit = u64::try_from(MAX_FILE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "larger than any valid prefetch history",
        ));
    }
    Ok(Some(bytes))
}

/// Delete every kept history beside `path` but `path` itself: one cache
/// directory serves one workspace, so the others' files name records this
/// device no longer serves.
fn forget_other_workspaces(path: &Path) {
    let (Some(dir), Some(keep)) = (path.parent(), path.file_name()) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() != keep
            && entry.file_type().is_ok_and(|kind| kind.is_file())
            && let Err(error) = std::fs::remove_file(entry.path())
        {
            eprintln!(
                "could not delete the prefetch history at {}: {error}",
                entry.path().display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects, reason = "test fixtures")]

    use super::*;

    fn record(n: u128) -> FileRecordId {
        FileRecordId(Uuid::from_u128(n))
    }

    fn pair(a: u128, b: u128, occasions: u32, age_secs: u64) -> PairRecord<FileRecordId> {
        PairRecord {
            a: record(a),
            b: record(b),
            occasions,
            age: Duration::from_secs(age_secs),
        }
    }

    #[test]
    fn pairs_round_trip_with_their_age() {
        let now = SystemTime::now();
        let pairs = vec![pair(1, 2, 2, 60), pair(3, 4, 5, 3600)];
        let decoded = decode(&encode(&pairs, now), now).unwrap();
        assert_eq!(decoded, pairs);
        let later = now + Duration::from_secs(600);
        assert_eq!(
            decode(&encode(&pairs, now), later).unwrap()[0].age,
            Duration::from_secs(660),
            "a pair ages while the device is off"
        );
    }

    #[test]
    fn the_file_is_bounded_in_pairs_and_age() {
        let now = SystemTime::now();
        let mut pairs: Vec<_> = (0..(MAX_PAIRS as u128 * 2))
            .map(|n| pair(n * 2 + 1, n * 2 + 2, 2, u64::try_from(n).unwrap()))
            .collect();
        pairs.push(pair(1, 9_999_999, 9, MAX_AGE.as_secs() + 1));
        let bytes = encode(&pairs, now);
        assert!(bytes.len() <= MAX_FILE_BYTES);
        assert_eq!(bytes.len(), MAX_FILE_BYTES);
        let decoded = decode(&bytes, now).unwrap();
        assert_eq!(decoded.len(), MAX_PAIRS);
        assert!(
            decoded
                .iter()
                .all(|pair| pair.age.as_secs() < MAX_PAIRS as u64),
            "the freshest pairs are the ones kept"
        );
        let much_later = now + MAX_AGE + Duration::from_secs(MAX_PAIRS as u64);
        assert!(
            decode(&bytes, much_later).unwrap().is_empty(),
            "pairs older than the bound are dropped on load"
        );
    }

    #[test]
    fn a_torn_or_altered_file_is_refused_whole() {
        let now = SystemTime::now();
        let bytes = encode(&[pair(1, 2, 2, 60), pair(3, 4, 2, 60)], now);
        for cut in [0, 5, HEADER_BYTES, bytes.len() - 1] {
            assert!(decode(&bytes[..cut], now).is_err(), "torn at {cut}");
        }
        for index in 0..bytes.len() {
            let mut altered = bytes.clone();
            altered[index] ^= 0x40;
            assert!(decode(&altered, now).is_err(), "byte {index} flipped");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(decode(&longer, now).is_err());
        assert_eq!(decode(b"", now), Err(Corrupt::Truncated));
        let mut other_version = bytes;
        other_version[7] = b'2';
        assert_eq!(decode(&other_version, now), Err(Corrupt::WrongMagic));
    }

    #[test]
    fn a_checksummed_file_with_an_impossible_pair_is_refused() {
        let now = SystemTime::now();
        let mut bytes = encode(&[pair(1, 2, 2, 60)], now);
        bytes.truncate(bytes.len() - CHECKSUM_BYTES);
        bytes[HEADER_BYTES + 16..HEADER_BYTES + 32].copy_from_slice(record(1).0.as_bytes());
        let checksum = blake3::hash(&bytes);
        bytes.extend_from_slice(checksum.as_bytes());
        assert_eq!(decode(&bytes, now), Err(Corrupt::BadEntry));
    }

    #[test]
    fn only_the_served_workspace_keeps_a_file() {
        let root = tempfile::tempdir().unwrap();
        let served = Uuid::from_u128(1);
        let removed = Uuid::from_u128(2);
        for workspace in [served, removed] {
            let path = path_for(root.path(), workspace);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        }
        forget_other_workspaces(&path_for(root.path(), served));
        assert!(path_for(root.path(), served).exists());
        assert!(!path_for(root.path(), removed).exists());
    }
}
