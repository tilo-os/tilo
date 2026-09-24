//! Durability and preserving decode for the existing Cloud aggregate.
//!
//! `state.json` is the atomically replaced checkpoint (file barrier, rename,
//! directory barrier). Unless journaling is disabled (`QOS_HOST_PERF_BASELINE=1`
//! or a non-Unix target), a commit whose delta against the last persisted
//! state is small instead appends one self-validating record to
//! `state.journal` (see `storage/journal.rs`, ported from qos-session-peer)
//! followed by one file barrier. Either way a commit returns only after its
//! bytes are durable.
//!
//! Every reader of the aggregate goes through [`Aggregate`]: it opens both
//! names under the caller's exclusive Store lock, reconstructs checkpoint plus
//! journal, and makes exactly the observed bytes durable (journal barrier and
//! the existing `commit_existing` reconciliation of `state.json`) before any
//! state is usable. No other process or tool reads `state.json` directly.
use anyhow::{Context, Result, ensure};
use serde::{
    Deserialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{fmt, path::Path};

pub(crate) mod journal;
#[cfg(test)]
mod journal_tests;
pub(crate) mod transaction;

const CHECKPOINT: &str = "state.json";
/// Journal bound before the next commit checkpoints: the larger of this and
/// the current checkpoint's size, so compaction costs at most one extra
/// checkpoint write per journal worth of records.
const JOURNAL_MIN_BYTES: u64 = 1024 * 1024;
/// Records before the next commit checkpoints. Every Store transaction
/// reconstructs the journal, applying each record (a state-sized copy), so
/// this bounds that per-read cost; checkpoints then amortize to one
/// whole-file write per this many commits.
const JOURNAL_MAX_RECORDS: usize = 32;
/// Largest aggregate a journal replay may reconstruct.
const MAX_STATE_BYTES: usize = 512 * 1024 * 1024;

/// `QOS_HOST_PERF_BASELINE=1` restores this crate's pre-journal host
/// behaviour in one binary (whole-file Store commits, 20 ms busy tick), for
/// paired measurement only. Read once per process.
pub(crate) fn host_perf_baseline() -> bool {
    static BASELINE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *BASELINE.get_or_init(|| std::env::var_os("QOS_HOST_PERF_BASELINE").is_some_and(|v| v == "1"))
}

/// Whether commits may append to the journal. Unit tests across the crate
/// inspect or sabotage `state.json` directly as the whole durable state, so
/// they keep whole-file commits unless a test opts in (`test_journal`) or the
/// suite runs with `QOS_TEST_CLOUD_JOURNAL=1`. Reads always reconstruct the
/// journal, whatever this returns.
pub(crate) fn journal_enabled() -> bool {
    #[cfg(test)]
    {
        test_journal::enabled()
    }
    #[cfg(not(test))]
    {
        cfg!(unix) && !host_perf_baseline()
    }
}

/// Process-wide storage counters for host-efficiency measurements.
mod counters {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub(super) static COMMITS: AtomicU64 = AtomicU64::new(0);
    pub(super) static CHECKPOINTS: AtomicU64 = AtomicU64::new(0);
    pub(super) static APPENDS: AtomicU64 = AtomicU64::new(0);
    pub(super) static BYTES_WRITTEN: AtomicU64 = AtomicU64::new(0);
    pub(super) static COMMIT_NS: AtomicU64 = AtomicU64::new(0);
    /// Digest and delta search (CPU) inside `COMMIT_NS`.
    pub(super) static DELTA_NS: AtomicU64 = AtomicU64::new(0);
    pub(super) fn add(counter: &AtomicU64, value: u64) {
        counter.fetch_add(value, Ordering::Relaxed);
    }
    pub(super) fn load(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}
/// Store write counters so far (logged every 256 commits).
pub(crate) fn io_summary() -> String {
    use counters::*;
    format!(
        "commits={} checkpoints={} journal_appends={} bytes_written={} commit_ns={} delta_ns={}",
        load(&COMMITS),
        load(&CHECKPOINTS),
        load(&APPENDS),
        load(&BYTES_WRITTEN),
        load(&COMMIT_NS),
        load(&DELTA_NS)
    )
}

/// The durable shape one reconciled read observed: the diff base of the next
/// journal append. Every field describes the files under the same Store lock
/// hold that produced the bytes.
#[derive(Clone)]
pub(crate) struct Base {
    checkpoint_identity: atomic_file::FileIdentity,
    checkpoint_len: u64,
    checkpoint_digest: [u8; 32],
    /// Identity and actual file length of `state.journal`, when it exists.
    journal: Option<(atomic_file::FileIdentity, u64)>,
    /// Header plus complete valid records; bytes beyond it are a torn or
    /// stale tail that the next append cuts first.
    valid_len: u64,
    /// Complete valid records.
    records: usize,
}
/// The last persisted state and the files it lives in.
#[derive(Clone)]
pub(crate) struct Tip {
    base: Base,
    current: Vec<u8>,
}
impl Tip {
    pub(crate) fn len(&self) -> usize {
        self.current.len()
    }
}
/// Reconstructed, reconciled aggregate bytes.
pub(crate) struct Persisted {
    pub(crate) bytes: Vec<u8>,
    base: Base,
}
impl Persisted {
    /// The retained diff base, only while journaling is enabled.
    pub(crate) fn into_tip(self) -> Option<Tip> {
        journal_enabled().then_some(Tip {
            base: self.base,
            current: self.bytes,
        })
    }
}

/// Both aggregate files opened under the caller's exclusive Store lock. The
/// open handles pin both inodes: while held, an equal identity names the same
/// file object, whose content only this crate's single locked writer changes.
#[cfg(unix)]
pub(crate) struct Aggregate {
    checkpoint: std::fs::File,
    checkpoint_identity: atomic_file::FileIdentity,
    checkpoint_len: u64,
    journal: Option<(std::fs::File, atomic_file::FileIdentity, u64)>,
    /// Set by a read whose journal had no bytes beyond its valid prefix. Only
    /// such an observation may be compared later: a journal only grows
    /// between checkpoints except for a torn-tail cut, and a new checkpoint
    /// or journal retirement replaces a file (see `same`).
    clean: bool,
}
#[cfg(unix)]
impl Aggregate {
    /// Under the caller's Store lock. A missing checkpoint is `NotFound`.
    pub(crate) fn open(directory: &atomic_file::StorageDirectory) -> std::io::Result<Self> {
        use atomic_file::{FileIdentity, Privacy};
        use std::ffi::OsStr;
        let checkpoint = directory.open_read(OsStr::new(CHECKPOINT), Privacy::OwnerOnly)?;
        let checkpoint_identity = FileIdentity::of(&checkpoint)?;
        let checkpoint_len = checkpoint.metadata()?.len();
        let journal = match directory.open_read(OsStr::new(journal::NAME), Privacy::OwnerOnly) {
            Ok(file) => {
                let identity = FileIdentity::of(&file)?;
                let len = file.metadata()?.len();
                Some((file, identity, len))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok(Self {
            checkpoint,
            checkpoint_identity,
            checkpoint_len,
            journal,
            clean: false,
        })
    }
    /// Persisted byte length (checkpoint plus journal): a sizing hint.
    pub(crate) fn length(&self) -> u64 {
        self.checkpoint_len
            .saturating_add(self.journal.as_ref().map_or(0, |(_, _, len)| *len))
    }
    pub(crate) fn clean(&self) -> bool {
        self.clean
    }
    /// Under the Store lock, with `self` a clean earlier observation still
    /// holding both handles: whether `current` observes the identical
    /// aggregate. Checkpoints are only ever replaced, never written in place;
    /// a journal is only appended to (strictly growing) or, after a crash, cut
    /// back to its valid prefix (never below a clean observation's length)
    /// before an append; retirement replaces it with a new empty file. The
    /// pinned inodes cannot be reused while held.
    pub(crate) fn same(&self, current: &Aggregate) -> bool {
        use atomic_file::FileIdentity;
        self.clean
            && self.checkpoint_identity == current.checkpoint_identity
            && self.checkpoint_len == current.checkpoint_len
            && FileIdentity::of(&self.checkpoint).is_ok_and(|held| held == self.checkpoint_identity)
            && match (&self.journal, &current.journal) {
                (None, None) => true,
                (Some((held, identity, len)), Some((_, other, other_len))) => {
                    identity == other
                        && len == other_len
                        && FileIdentity::of(held).is_ok_and(|held| held == *identity)
                }
                _ => false,
            }
    }
    /// Cheap unlocked hint that the directory still names both retained
    /// files at their retained lengths. The next turn re-proves it locked.
    pub(crate) fn matches_unlocked(&self, directory: &std::fs::File) -> bool {
        let named = |name: &std::ffi::CStr, file: &std::fs::File, length: u64| {
            use std::os::unix::fs::MetadataExt;
            let (Some(stat), Ok(held)) = (stat_at(directory, name), file.metadata()) else {
                return false;
            };
            stat.st_ino as u64 == held.ino()
                && stat.st_dev as u64 == held.dev()
                && stat.st_size as u64 == held.len()
                && held.len() == length
        };
        self.clean
            && named(c"state.json", &self.checkpoint, self.checkpoint_len)
            && match &self.journal {
                Some((file, _, len)) => named(c"state.journal", file, *len),
                None => stat_at(directory, c"state.journal").is_none(),
            }
    }
    /// Read and reconstruct checkpoint plus journal, then make exactly the
    /// observed bytes durable before anything may use them: a journal barrier
    /// (records a crashed writer wrote but never barriered become durable
    /// here, or were never observed), then the existing file/name
    /// reconciliation of `state.json` whose directory barrier also covers the
    /// journal's name. A failed barrier never yields usable state.
    pub(crate) fn read_reconciled(
        &mut self,
        directory: &atomic_file::StorageDirectory,
    ) -> Result<Persisted> {
        // Exactly the files this process last wrote and barriered: their
        // state is known without reading, hashing or replaying, and the
        // journal needs no barrier. `state.json` is still reconciled as ever.
        if let Some((bytes, base)) = durable::lookup(directory, self) {
            self.reconcile_checkpoint(directory)?;
            self.clean = true;
            return Ok(Persisted { bytes, base });
        }
        let checkpoint = read_exact_len(&self.checkpoint, self.checkpoint_len)?;
        let journaled = self.journal.as_ref().is_some_and(|(_, _, len)| *len > 0);
        // Needed to validate a journal header, or as the diff base's
        // checkpoint binding; the baseline reader of a bare checkpoint skips it.
        let checkpoint_digest = if journaled || journal_enabled() {
            *blake3::hash(&checkpoint).as_bytes()
        } else {
            [0; 32]
        };
        let checkpoint_len = checkpoint.len() as u64;
        let (bytes, valid_len, records) = match &self.journal {
            Some((file, _, len)) if *len > 0 => {
                let journal_bytes = read_exact_len(file, *len)?;
                let replay = journal::replay_fast(
                    checkpoint,
                    &checkpoint_digest,
                    &journal_bytes,
                    MAX_STATE_BYTES,
                );
                if replay.records > 0 || replay.valid_len != *len {
                    tracing::debug!(
                        records = replay.records,
                        ignored_tail_bytes = *len - replay.valid_len,
                        "cloud state journal replayed onto checkpoint"
                    );
                }
                (replay.state, replay.valid_len, replay.records)
            }
            _ => (checkpoint, 0, 0),
        };
        self.reconcile(directory)?;
        let journal = self
            .journal
            .as_ref()
            .map(|(_, identity, len)| (identity.clone(), *len));
        self.clean = journal.as_ref().is_none_or(|(_, len)| *len == valid_len);
        Ok(Persisted {
            bytes,
            base: Base {
                checkpoint_identity: self.checkpoint_identity.clone(),
                checkpoint_len,
                checkpoint_digest,
                journal,
                valid_len,
                records,
            },
        })
    }
    /// The durability barriers of `read_reconciled`, for callers that read
    /// the bytes through their own paid buffers.
    pub(crate) fn reconcile(&self, directory: &atomic_file::StorageDirectory) -> Result<()> {
        use atomic_file::{FileIdentity, Privacy};
        use std::ffi::OsStr;
        if let Some((_, identity, len)) = &self.journal {
            if *len > 0 {
                let writable = directory
                    .open_append_existing(OsStr::new(journal::NAME), Privacy::OwnerOnly)?;
                ensure!(
                    FileIdentity::of(&writable)? == *identity,
                    "cloud state journal changed while locked"
                );
                writable
                    .sync_all()
                    .context("cloud observed journal durability reconciliation failed")?;
            }
        }
        self.reconcile_checkpoint(directory)
    }
    /// The original reconciliation of the observed `state.json` file/name.
    fn reconcile_checkpoint(&self, directory: &atomic_file::StorageDirectory) -> Result<()> {
        use atomic_file::Privacy;
        use std::ffi::OsStr;
        directory
            .commit_existing(
                OsStr::new(CHECKPOINT),
                &self.checkpoint_identity,
                Privacy::OwnerOnly,
            )
            .context("cloud observed state durability reconciliation failed")?;
        Ok(())
    }
    pub(crate) fn checkpoint_file(&mut self) -> &mut std::fs::File {
        &mut self.checkpoint
    }
    /// The journal file when it holds any bytes.
    pub(crate) fn journal_file(&mut self) -> Option<&mut std::fs::File> {
        self.journal
            .as_mut()
            .filter(|(_, _, len)| *len > 0)
            .map(|(file, _, _)| file)
    }
}
#[cfg(unix)]
fn read_exact_len(file: &std::fs::File, len: u64) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = file;
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(usize::try_from(len)?)?;
    file.take(len.saturating_add(1)).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 == len, "aggregate changed while locked");
    Ok(bytes)
}
/// The state a checkpoint plus its journal bytes reconstruct (for readers
/// that read both files through their own paid buffers).
pub(crate) fn reconstruct(checkpoint: &[u8], journal: &[u8]) -> Vec<u8> {
    let digest = *blake3::hash(checkpoint).as_bytes();
    journal::replay_fast(checkpoint.to_vec(), &digest, journal, MAX_STATE_BYTES).state
}
/// `fstatat` relative to the bound Store directory; never an ambient lookup.
#[cfg(unix)]
pub(crate) fn stat_at(directory: &std::fs::File, name: &std::ffi::CStr) -> Option<libc::stat> {
    use std::os::fd::AsRawFd;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    (result == 0).then(|| unsafe { stat.assume_init() })
}
/// Persisted aggregate length (checkpoint plus journal) observed without the
/// Store lock: a sizing hint only. `None` when the checkpoint is missing.
#[cfg(unix)]
pub(crate) fn persisted_len_hint(directory: &std::fs::File) -> Option<u64> {
    let checkpoint = stat_at(directory, c"state.json")?;
    if checkpoint.st_mode & libc::S_IFMT != libc::S_IFREG {
        return None;
    }
    let journal = stat_at(directory, c"state.journal")
        .filter(|stat| stat.st_mode & libc::S_IFMT == libc::S_IFREG)
        .map_or(0, |stat| stat.st_size as u64);
    Some((checkpoint.st_size as u64).saturating_add(journal))
}

/// What this process last wrote and barriered, per Store directory.
///
/// Every Cloud component in the process opens its own Store per transaction;
/// without this, each would re-read, hash and replay the aggregate its
/// neighbour just wrote, and barrier the journal again. An entry pins both
/// files, so an equal identity names the same file object; a checkpoint is
/// never written in place and journal bytes below a valid prefix are never
/// rewritten (appenders only cut beyond their valid prefix, which contains
/// every barriered record; retirement replaces the file). Equal identities
/// and lengths therefore mean exactly the bytes this process barriered, whose
/// reconstruction is the remembered state.
#[cfg(unix)]
mod durable {
    use super::{Aggregate, Base, journal};
    use atomic_file::{FileIdentity, Privacy, StorageDirectory};
    use std::{ffi::OsStr, fs::File, sync::Mutex};
    struct Entry {
        directory: FileIdentity,
        checkpoint: File,
        journal: Option<File>,
        base: Base,
        state: Vec<u8>,
    }
    /// A few Store directories per process (tests open many).
    const ENTRIES: usize = 4;
    static DURABLE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

    pub(super) fn forget(directory: &StorageDirectory) {
        let Ok(identity) = directory.identity() else {
            return;
        };
        if let Ok(mut entries) = DURABLE.lock() {
            entries.retain(|entry| entry.directory != identity);
        }
    }
    /// After a successful, barriered write whose result `base`/`state` names.
    pub(super) fn remember(directory: &StorageDirectory, base: &Base, state: &[u8]) {
        forget(directory);
        let pinned = (|| {
            let identity = directory.identity().ok()?;
            let checkpoint = directory
                .open_read(OsStr::new(super::CHECKPOINT), Privacy::OwnerOnly)
                .ok()?;
            if FileIdentity::of(&checkpoint).ok()? != base.checkpoint_identity {
                return None;
            }
            let journal = match &base.journal {
                Some((expected, _)) => {
                    let file = directory
                        .open_read(OsStr::new(journal::NAME), Privacy::OwnerOnly)
                        .ok()?;
                    (FileIdentity::of(&file).ok()? == *expected).then_some(Some(file))?
                }
                None => None,
            };
            Some(Entry {
                directory: identity,
                checkpoint,
                journal,
                base: base.clone(),
                state: state.to_vec(),
            })
        })();
        if let (Some(entry), Ok(mut entries)) = (pinned, DURABLE.lock()) {
            if entries.len() >= ENTRIES {
                entries.remove(0);
            }
            entries.push(entry);
        }
    }
    pub(super) fn lookup(
        directory: &StorageDirectory,
        observed: &Aggregate,
    ) -> Option<(Vec<u8>, Base)> {
        let identity = directory.identity().ok()?;
        let entries = DURABLE.lock().ok()?;
        let entry = entries.iter().find(|entry| entry.directory == identity)?;
        let base = &entry.base;
        let checkpoint = base.checkpoint_identity == observed.checkpoint_identity
            && base.checkpoint_len == observed.checkpoint_len
            && FileIdentity::of(&entry.checkpoint)
                .is_ok_and(|held| held == base.checkpoint_identity);
        let journal = match (&base.journal, &entry.journal, &observed.journal) {
            (None, None, None) => true,
            (Some((expected, len)), Some(held), Some((_, identity, observed_len))) => {
                expected == identity
                    && len == observed_len
                    && *len == base.valid_len
                    && FileIdentity::of(held).is_ok_and(|held| held == *expected)
            }
            _ => false,
        };
        (checkpoint && journal).then(|| (entry.state.clone(), base.clone()))
    }
    #[cfg(test)]
    pub(super) fn clear() {
        if let Ok(mut entries) = DURABLE.lock() {
            entries.clear();
        }
    }
}
#[cfg(test)]
pub(crate) fn forget_durable_for_test() {
    durable::clear();
}

/// Reconcile the observed aggregate under the caller's exclusive state lock.
/// No replacement is performed; a failed barrier never yields usable state.
#[cfg(unix)]
pub(crate) fn read_reconciled(
    directory: &atomic_file::StorageDirectory,
    root: &Path,
) -> Result<Option<Persisted>> {
    let mut aggregate = match Aggregate::open(directory) {
        Ok(aggregate) => aggregate,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling symlink or other existing invalid entry is not first run.
            match std::fs::symlink_metadata(root.join(CHECKPOINT)) {
                Err(absent) if absent.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                _ => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    aggregate.read_reconciled(directory).map(Some)
}

/// Reconcile an existing state through the caller's admitted directory handle.
/// Missing or invalid entries are errors; this does not provision any namespace.
#[cfg(unix)]
pub(crate) fn read_reconciled_existing(
    directory: &atomic_file::StorageDirectory,
) -> Result<Persisted> {
    Aggregate::open(directory)?.read_reconciled(directory)
}
#[cfg(not(unix))]
pub(crate) fn read_reconciled(
    _: &atomic_file::StorageDirectory,
    _: &Path,
) -> Result<Option<Persisted>> {
    anyhow::bail!("cloud Store durability requires qualified Unix storage")
}

/// Reject duplicate keys before serde_json::Value can silently collapse them.
pub(crate) fn decode_unique_json(bytes: &[u8]) -> Result<Value> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = Unique::deserialize(&mut decoder)?.0;
    decoder.end()?;
    Ok(value)
}
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: de::Deserializer<'de>>(decoder: D) -> std::result::Result<Self, D::Error> {
        decoder.deserialize_any(UniqueVisitor).map(Unique)
    }
}
struct UniqueVisitor;
impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = Value;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON with unique object keys")
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<Value, E> {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_string<E: de::Error>(self, value: String) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element::<Unique>()? {
            values.push(value.0);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate cloud state JSON key"));
            }
            values.insert(key, map.next_value::<Unique>()?.0);
        }
        Ok(Value::Object(values))
    }
}

/// A failed Store write and what is known about the aggregate on disk.
#[derive(Debug)]
pub(crate) struct WriteError {
    pub(crate) state: atomic_file::PublicationState,
    pub(crate) error: std::io::Error,
}
impl WriteError {
    fn new(state: atomic_file::PublicationState, error: std::io::Error) -> Self {
        Self { state, error }
    }
    fn publication(error: atomic_file::PublicationError) -> Self {
        Self::new(error.state(), error.into_io_error())
    }
    /// A failure before this write changed any file.
    fn before(error: impl Into<std::io::Error>) -> Self {
        Self::new(atomic_file::PublicationState::NotPublished, error.into())
    }
}

/// Durably persist `bytes` as the whole aggregate under the caller's held
/// Store lock. With a known `tip` (the last persisted state and its files)
/// and journaling enabled, a small delta is appended to the journal;
/// otherwise the checkpoint is replaced and any journal retired. On success
/// `tip` describes `bytes`; on failure it is cleared (the caller latches the
/// failure and requires a reopen).
pub(crate) fn write(
    directory: &std::fs::File,
    root: &Path,
    tip: &mut Option<Tip>,
    bytes: &[u8],
) -> std::result::Result<(), WriteError> {
    let started = std::time::Instant::now();
    #[cfg(not(test))]
    let result = write_inner(directory, tip, bytes, None, root);
    #[cfg(test)]
    let result = match test_support::take_failure(root) {
        Some(point) => atomic_file::test_support::with_failure(point, || {
            write_inner(directory, tip, bytes, Some(point), root)
        }),
        None => write_inner(directory, tip, bytes, None, root),
    };
    if result.is_err() {
        *tip = None;
    }
    counters::add(&counters::COMMITS, 1);
    counters::add(&counters::COMMIT_NS, started.elapsed().as_nanos() as u64);
    let commits = counters::load(&counters::COMMITS);
    if commits % 256 == 0 {
        tracing::info!(summary = %io_summary(), "cloud store io");
    }
    result
}

#[cfg(not(test))]
type FailurePoint = ();
#[cfg(test)]
type FailurePoint = atomic_file::test_support::Checkpoint;

fn write_inner(
    directory: &std::fs::File,
    tip: &mut Option<Tip>,
    bytes: &[u8],
    failure: Option<FailurePoint>,
    root: &Path,
) -> std::result::Result<(), WriteError> {
    // As before the journal: an admission failure is not classified further.
    let unclassified =
        |error| WriteError::new(atomic_file::PublicationState::PublicationUnknown, error);
    let admitted =
        atomic_file::StorageDirectory::from_file(directory.try_clone().map_err(unclassified)?)
            .map_err(unclassified)?;
    admitted
        .validate_privacy(atomic_file::Privacy::OwnerOnly)
        .map_err(unclassified)?;
    let started = std::time::Instant::now();
    // The whole-file baseline writer keeps no diff base and needs no digest.
    let digest = if journal_enabled() || tip.is_some() {
        *blake3::hash(bytes).as_bytes()
    } else {
        [0; 32]
    };
    let record = tip.as_ref().filter(|_| journal_enabled()).and_then(|tip| {
        let record = journal::record(&tip.current, bytes, &digest)?;
        let header = if tip.base.valid_len == 0 {
            journal::HEADER_LEN as u64
        } else {
            0
        };
        let after = tip.base.valid_len + header + record.len() as u64;
        (after <= JOURNAL_MIN_BYTES.max(tip.base.checkpoint_len)
            && tip.base.records < JOURNAL_MAX_RECORDS)
            .then_some(record)
    });
    counters::add(&counters::DELTA_NS, started.elapsed().as_nanos() as u64);
    // Nothing this process remembered is known to be on disk any more
    // until this write has succeeded (a failure may leave unbarriered bytes).
    durable::forget(&admitted);
    match (record, tip.as_mut()) {
        (Some(record), Some(state)) => {
            append(&admitted, state, &record, failure, root)?;
            state.current.clear();
            state.current.extend_from_slice(bytes);
            counters::add(&counters::APPENDS, 1);
        }
        _ => {
            let base = checkpoint(&admitted, tip.as_ref(), bytes, digest, failure, root)?;
            *tip = journal_enabled().then(|| Tip {
                base,
                current: bytes.to_vec(),
            });
        }
    }
    if let Some(tip) = tip.as_ref() {
        durable::remember(&admitted, &tip.base, &tip.current);
    }
    Ok(())
}

/// Append one record and barrier the journal file only.
#[cfg(unix)]
fn append(
    directory: &atomic_file::StorageDirectory,
    tip: &mut Tip,
    record: &[u8],
    failure: Option<FailurePoint>,
    root: &Path,
) -> std::result::Result<(), WriteError> {
    use atomic_file::{FileIdentity, Privacy, PublicationState, PublishMode};
    use std::{ffi::OsStr, io::Write};
    let _ = (&failure, root);
    let base = &mut tip.base;
    // Records against a replaced checkpoint would be acknowledged yet
    // unrecoverable (their header names another file's bytes).
    let named = directory
        .open_read(OsStr::new(CHECKPOINT), Privacy::OwnerOnly)
        .map_err(WriteError::before)?;
    if FileIdentity::of(&named).map_err(WriteError::before)? != base.checkpoint_identity {
        return Err(WriteError::before(std::io::Error::other(
            "cloud state checkpoint changed outside the Store lock",
        )));
    }
    let mut framed = Vec::with_capacity(journal::HEADER_LEN + record.len());
    if base.valid_len == 0 {
        framed.extend_from_slice(&journal::header_for(
            base.checkpoint_len,
            &base.checkpoint_digest,
        ));
    }
    framed.extend_from_slice(record);
    #[cfg(test)]
    test_journal::fault(root, test_journal::Point::BeforeWrite, failure)
        .map_err(WriteError::before)?;
    match &base.journal {
        Some((identity, len)) => {
            let mut file = directory
                .open_append_existing(OsStr::new(journal::NAME), Privacy::OwnerOnly)
                .map_err(WriteError::before)?;
            let observed = file.metadata().map_err(WriteError::before)?.len();
            if FileIdentity::of(&file).map_err(WriteError::before)? != *identity || observed != *len
            {
                return Err(WriteError::before(std::io::Error::other(
                    "cloud state journal changed outside the Store lock",
                )));
            }
            let unknown = |error| WriteError::new(PublicationState::PublicationUnknown, error);
            // A torn tail or stale journal is cut before appending; the same
            // barrier below makes the cut and the record durable.
            if *len != base.valid_len {
                file.set_len(base.valid_len).map_err(unknown)?;
            }
            file.write_all(&framed).map_err(unknown)?;
            let written = base.valid_len + framed.len() as u64;
            #[cfg(test)]
            test_journal::fault(root, test_journal::Point::BeforeBarrier, failure).map_err(
                |error| WriteError::new(PublicationState::PublishedDurabilityUnknown, error),
            )?;
            file.sync_all().map_err(|error| {
                WriteError::new(PublicationState::PublishedDurabilityUnknown, error)
            })?;
            base.journal = Some((identity.clone(), written));
            base.valid_len = written;
            base.records += 1;
        }
        None => {
            // First journal beside this checkpoint: publish its complete first
            // contents atomically (file and directory barriers included).
            directory
                .publish(
                    OsStr::new(journal::NAME),
                    &framed,
                    PublishMode::Replace,
                    Privacy::OwnerOnly,
                )
                .map_err(WriteError::publication)?;
            let identity = directory
                .open_read(OsStr::new(journal::NAME), Privacy::OwnerOnly)
                .and_then(|file| FileIdentity::of(&file))
                .map_err(|error| {
                    WriteError::new(PublicationState::PublishedDurabilityUnknown, error)
                })?;
            base.journal = Some((identity, framed.len() as u64));
            base.valid_len = framed.len() as u64;
            base.records = 1;
        }
    }
    counters::add(&counters::BYTES_WRITTEN, framed.len() as u64);
    Ok(())
}

/// Replace the whole checkpoint atomically, then retire any journal by
/// replacing it with a new empty file. Returns the new base.
#[cfg(unix)]
fn checkpoint(
    directory: &atomic_file::StorageDirectory,
    tip: Option<&Tip>,
    bytes: &[u8],
    digest: [u8; 32],
    failure: Option<FailurePoint>,
    root: &Path,
) -> std::result::Result<Base, WriteError> {
    use atomic_file::{FileIdentity, Privacy, PublicationState, PublishMode};
    use std::ffi::OsStr;
    let _ = (&failure, root);
    // If the checkpoint on disk already holds exactly these bytes, the journal
    // records after it are the only difference: retiring the journal alone
    // yields `bytes`, and republishing identical bytes would leave its header
    // valid for the "new" checkpoint.
    let unchanged = tip.is_some_and(|tip| {
        tip.base.checkpoint_len == bytes.len() as u64 && tip.base.checkpoint_digest == digest
    });
    if !unchanged {
        directory
            .publish(
                OsStr::new(CHECKPOINT),
                bytes,
                PublishMode::Replace,
                Privacy::OwnerOnly,
            )
            .map_err(WriteError::publication)?;
        counters::add(&counters::CHECKPOINTS, 1);
        counters::add(&counters::BYTES_WRITTEN, bytes.len() as u64);
    }
    // From here the new checkpoint is durable, except for an unchanged one,
    // whose commit point is the journal retirement below.
    let after = |error| {
        WriteError::new(
            if unchanged {
                PublicationState::PublicationUnknown
            } else {
                PublicationState::PublishedDurabilityUnknown
            },
            error,
        )
    };
    let checkpoint_identity = directory
        .open_read(OsStr::new(CHECKPOINT), Privacy::OwnerOnly)
        .and_then(|file| FileIdentity::of(&file))
        .map_err(after)?;
    // The journal on disk: known from the tip, or observed for a writer that
    // was given no tip (it may hold records bound to the old checkpoint).
    let journal = match tip {
        Some(tip) => tip.base.journal.clone(),
        None => match directory.open_read(OsStr::new(journal::NAME), Privacy::OwnerOnly) {
            Ok(file) => Some((
                FileIdentity::of(&file).map_err(after)?,
                file.metadata().map_err(after)?.len(),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(after(error)),
        },
    };
    #[cfg(test)]
    test_journal::fault(root, test_journal::Point::BeforeRetire, failure).map_err(after)?;
    let journal = match journal {
        Some((_, len)) if len > 0 => {
            // A new, empty inode: a journal a reader retained (and pinned) can
            // never be observed again at a length it already saw.
            directory
                .publish(
                    OsStr::new(journal::NAME),
                    &[],
                    PublishMode::Replace,
                    Privacy::OwnerOnly,
                )
                .map_err(|error| {
                    if unchanged {
                        WriteError::publication(error)
                    } else {
                        after(error.into_io_error())
                    }
                })?;
            let identity = directory
                .open_read(OsStr::new(journal::NAME), Privacy::OwnerOnly)
                .and_then(|file| FileIdentity::of(&file))
                .map_err(|error| {
                    WriteError::new(PublicationState::PublishedDurabilityUnknown, error)
                })?;
            Some((identity, 0))
        }
        other => other,
    };
    Ok(Base {
        checkpoint_identity,
        checkpoint_len: bytes.len() as u64,
        checkpoint_digest: digest,
        journal,
        valid_len: 0,
        records: 0,
    })
}
#[cfg(not(unix))]
fn append(
    _: &atomic_file::StorageDirectory,
    _: &mut Tip,
    _: &[u8],
    _: Option<FailurePoint>,
    _: &Path,
) -> std::result::Result<(), WriteError> {
    Err(WriteError::before(std::io::Error::other(
        "journal requires Unix storage",
    )))
}
#[cfg(not(unix))]
fn checkpoint(
    _: &atomic_file::StorageDirectory,
    _: Option<&Tip>,
    bytes: &[u8],
    _: [u8; 32],
    _: Option<FailurePoint>,
    _: &Path,
) -> std::result::Result<Base, WriteError> {
    Err(WriteError::before(std::io::Error::other(
        "cloud Store requires Unix storage",
    )))
}

/// `compact_locked` after waiting for the existing Store lock.
#[cfg(unix)]
pub(crate) fn compact(root: &Path) -> Result<bool> {
    use transaction::{ExistingStoreLock, LockFailure};
    let directory = std::fs::File::open(root)?;
    let locked =
        ExistingStoreLock::acquire(root, directory, true).map_err(|error| match error {
            LockFailure::Busy => anyhow::anyhow!("Cloud Store busy"),
            LockFailure::Unavailable(error) => error,
        })?;
    compact_locked(locked.directory(), root)
}
#[cfg(not(unix))]
pub(crate) fn compact(_: &Path) -> Result<bool> {
    Ok(false)
}
/// Fold a non-empty journal into the checkpoint, under the Store lock, so the
/// checkpoint alone is complete (orderly shutdown; also what an older binary
/// that does not know the journal requires before it may open the Store).
#[cfg(unix)]
pub(crate) fn compact_locked(directory: &std::fs::File, root: &Path) -> Result<bool> {
    let admitted = atomic_file::StorageDirectory::from_file(directory.try_clone()?)?;
    let mut aggregate = Aggregate::open(&admitted)?;
    if aggregate
        .journal
        .as_ref()
        .is_none_or(|(_, _, len)| *len == 0)
    {
        return Ok(false);
    }
    let persisted = aggregate.read_reconciled(&admitted)?;
    let digest = *blake3::hash(&persisted.bytes).as_bytes();
    let tip = Tip {
        base: persisted.base,
        current: persisted.bytes,
    };
    checkpoint(&admitted, Some(&tip), &tip.current, digest, None, root)
        .map_err(|failure| anyhow::Error::from(failure.error))?;
    Ok(true)
}

#[cfg(test)]
pub(crate) mod test_support {
    use atomic_file::test_support::Checkpoint;
    use std::{
        cell::RefCell,
        path::{Path, PathBuf},
    };
    thread_local! { static FAILURE: RefCell<Option<(PathBuf, Checkpoint)>> = const { RefCell::new(None) }; }
    struct Restore(Option<(PathBuf, Checkpoint)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAILURE.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    pub(super) fn take_failure(root: &Path) -> Option<Checkpoint> {
        FAILURE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|(path, _)| path == root) {
                slot.take().map(|(_, point)| point)
            } else {
                None
            }
        })
    }
    /// Scope one actual Store publication failure without faulting a relay's files.
    pub(crate) fn with_save_failure<T>(
        root: &Path,
        point: Checkpoint,
        operation: impl FnOnce() -> T,
    ) -> T {
        let old = FAILURE.with(|slot| slot.replace(Some((root.into(), point))));
        let _restore = Restore(old);
        operation()
    }
}

#[cfg(test)]
pub(crate) mod test_journal {
    use std::{
        cell::{Cell, RefCell},
        path::{Path, PathBuf},
    };
    thread_local! {
        static FORCED: Cell<bool> = const { Cell::new(false) };
        static FAULT: RefCell<Option<(PathBuf, Point)>> = const { RefCell::new(None) };
    }
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCED.with(|forced| forced.set(self.0));
        }
    }
    pub(crate) fn enabled() -> bool {
        FORCED.with(Cell::get)
            || std::env::var_os("QOS_TEST_CLOUD_JOURNAL").is_some_and(|value| value == "1")
    }
    /// Stores written inside `operation` on this thread journal their commits.
    pub(crate) fn with_journal<T>(operation: impl FnOnce() -> T) -> T {
        let _restore = Restore(FORCED.with(|forced| forced.replace(true)));
        operation()
    }
    /// Journal-specific failure points (atomic publications keep using
    /// `atomic_file::test_support` through `test_support::with_save_failure`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Point {
        /// Append: before the journal is changed.
        BeforeWrite,
        /// Append: record written, barrier not issued.
        BeforeBarrier,
        /// Checkpoint: new checkpoint durable, journal not yet retired.
        BeforeRetire,
    }
    struct RestoreFault(Option<(PathBuf, Point)>);
    impl Drop for RestoreFault {
        fn drop(&mut self) {
            FAULT.with(|fault| *fault.borrow_mut() = self.0.take());
        }
    }
    pub(crate) fn with_fault<T>(root: &Path, point: Point, operation: impl FnOnce() -> T) -> T {
        let _restore =
            RestoreFault(FAULT.with(|fault| fault.replace(Some((root.to_owned(), point)))));
        operation()
    }
    /// A `with_save_failure` checkpoint maps onto the equivalent append phase.
    fn mapped(save: Option<atomic_file::test_support::Checkpoint>) -> Option<Point> {
        use atomic_file::test_support::Checkpoint::*;
        match save? {
            StageCreated | BeforeDataFlush | BeforePublish => Some(Point::BeforeWrite),
            PublishSubmitted | AfterPublish | BeforeCommit => Some(Point::BeforeBarrier),
            _ => None,
        }
    }
    pub(super) fn fault(
        root: &Path,
        point: Point,
        save: Option<atomic_file::test_support::Checkpoint>,
    ) -> std::io::Result<()> {
        let armed = FAULT.with(|fault| {
            let mut fault = fault.borrow_mut();
            if fault
                .as_ref()
                .is_some_and(|(path, at)| path == root && *at == point)
            {
                fault.take();
                true
            } else {
                false
            }
        });
        if armed || (point != Point::BeforeRetire && mapped(save) == Some(point)) {
            return Err(std::io::Error::other(format!(
                "injected cloud journal failure at {point:?}"
            )));
        }
        Ok(())
    }
}
