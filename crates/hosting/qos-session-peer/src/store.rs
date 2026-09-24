//! One OS-custodied writer, atomic replacement, and a mandatory directory barrier.
//!
//! Commits either replace the signed checkpoint `session.bin` atomically (file
//! barrier, rename, directory barrier) or append one self-validating delta
//! record to `session.journal` followed by one file barrier (see
//! `store/journal.rs`). Either way a commit returns only after its bytes are
//! durable, and reopening reconstructs exactly the last acknowledged state (or
//! the state of an unacknowledged commit whose record did reach the disk).
use crate::action_timing::Timing;
use anyhow::{Context, Result, bail};
use atomic_file::{FileIdentity, Privacy, PublicationState, PublishMode, StorageDirectory};
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[path = "store/journal.rs"]
mod journal;
#[path = "store/side_log.rs"]
mod side_log;
pub(crate) use side_log::SideLogFull;

/// Journal bound before the next commit checkpoints: the larger of this and
/// the current checkpoint's size, so compaction costs at most one extra
/// checkpoint write per journal worth of records.
const JOURNAL_MIN_BYTES: u64 = 1024 * 1024;

/// Process-wide storage counters for host-efficiency measurements.
static COMMITS: AtomicU64 = AtomicU64::new(0);
static CHECKPOINTS: AtomicU64 = AtomicU64::new(0);
static APPENDS: AtomicU64 = AtomicU64::new(0);
static FULL_BARRIERS: AtomicU64 = AtomicU64::new(0);
static BYTES_WRITTEN: AtomicU64 = AtomicU64::new(0);
static SIDE_APPENDS: AtomicU64 = AtomicU64::new(0);
fn count(counter: &AtomicU64, value: u64) {
    counter.fetch_add(value, Ordering::Relaxed);
}
pub(crate) fn io_summary() -> String {
    let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
    format!(
        "commits={} checkpoints={} journal_appends={} side_appends={} full_barriers={} bytes_written={}",
        load(&COMMITS),
        load(&CHECKPOINTS),
        load(&APPENDS),
        load(&SIDE_APPENDS),
        load(&FULL_BARRIERS),
        load(&BYTES_WRITTEN)
    )
}
/// A full barrier (`F_FULLFSYNC` on Apple platforms, `fsync` elsewhere).
fn full_barrier(file: &File) -> std::io::Result<()> {
    count(&FULL_BARRIERS, 1);
    file.sync_all()
}

/// Whether commits may append to the journal. Unit tests across the crate
/// inspect or sabotage `session.bin` directly as the whole durable state, so
/// they keep whole-file commits unless a test opts in (`test_journal`) or the
/// suite runs with `QOS_TEST_SESSION_JOURNAL=1`.
fn journal_enabled() -> bool {
    #[cfg(test)]
    {
        test_journal::enabled()
    }
    #[cfg(not(test))]
    {
        cfg!(unix) && !crate::action_pump_cadence::host_perf_baseline()
    }
}

/// In-memory mirror of the durable journal, valid only while not poisoned.
struct JournalState {
    /// The exact bytes last acknowledged (or recovered): the diff base.
    current: Vec<u8>,
    /// blake3 and length of the checkpoint the journal header must name.
    checkpoint_digest: [u8; 32],
    checkpoint_len: u64,
    /// Identity of the named checkpoint file the journal extends. Appends
    /// require it unchanged: records against a replaced checkpoint would be
    /// acknowledged yet unrecoverable (their header would name another file).
    checkpoint_identity: FileIdentity,
    /// Open journal handle and its valid length (header plus records); a zero
    /// length means the next append must write a fresh header first.
    file: Option<File>,
    valid_len: u64,
    /// Whether the on-disk journal may hold bytes beyond `valid_len`
    /// (a torn tail or a stale journal) that must be truncated first.
    dirty_tail: bool,
}

pub const MAX_AGGREGATE_BYTES: usize = 64 * 1024 * 1024;

pub struct SessionStore {
    root: PathBuf,
    _lock: Custody,
    directory: File,
    poisoned: bool,
    // Inert identity of bytes acknowledged by this instance's actual barrier.
    // Opening/reading a file alone never fills this slot.
    acknowledged_digest: Option<[u8; 32]>,
    journal: Option<JournalState>,
    journal_enabled: bool,
    compact_on_drop: bool,
    /// Present only for an owner-recovered store (see `store/side_log.rs`).
    side_log: Option<side_log::SideLog>,
}

/// Custody belongs to this owner, rather than a fork child's inherited open
/// file description. Normal drop explicitly releases the lock before close.
/// Abrupt process death still follows the operating system's descriptor rules.
pub(crate) struct Custody {
    file: File,
    acquired_by: u32,
}
impl Drop for Custody {
    fn drop(&mut self) {
        // An inherited guard must not unlock a still-live acquiring process.
        if self.acquired_by == std::process::id() {
            let _ = self.file.unlock();
        }
    }
}
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    sync_dir(path.parent().context("directory has no parent")?)
}
pub(crate) fn check_dir(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        bail!("state directory must be a real directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            bail!("state directory must be private and owned by this operator");
        }
    }
    Ok(())
}
pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
pub(crate) fn custody(path: &Path) -> Result<Custody> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("custody path must be a regular file");
    }
    file.try_lock()
        .context("another process owns this native session or identity")?;
    Ok(Custody {
        file,
        acquired_by: std::process::id(),
    })
}
impl SessionStore {
    pub(crate) fn acknowledged_digest(&self) -> Result<[u8; 32]> {
        if self.poisoned {
            bail!("store durability is uncertain");
        }
        self.acknowledged_digest
            .context("store has no acknowledged aggregate digest")
    }
    pub fn create(root: &Path) -> Result<Self> {
        private_dir(root).context("initialization requires a new state directory")?;
        let mut store = Self::open(root)?;
        // A new directory has no side records; its creating owner may append.
        store.side_log = Some(side_log::SideLog {
            file: None,
            valid_len: 0,
            dirty_tail: false,
        });
        Ok(store)
    }
    pub fn open(root: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let directory = options.open(root)?;
        if !directory.metadata()?.is_dir() {
            bail!("state root is not a directory");
        }
        #[cfg(all(test, unix))]
        fd_tests::directory_opened();
        let admitted = StorageDirectory::from_file(directory.try_clone()?)?;
        admitted.validate_privacy(Privacy::OwnerOnly)?;
        let lock = Self::custody_in(&admitted)?;
        let store = Self {
            directory,
            root: root.to_path_buf(),
            _lock: lock,
            poisoned: false,
            acknowledged_digest: None,
            journal: None,
            journal_enabled: journal_enabled(),
            compact_on_drop: true,
            side_log: None,
        };
        store.checked_directory()?;
        Ok(store)
    }
    fn custody_in(directory: &StorageDirectory) -> Result<Custody> {
        let name = OsStr::new("owner.lock");
        let file = match directory.open_read(name, Privacy::OwnerOnly) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Only an absent sentinel permits creation, and only a genuine
                // competing IfAbsent publication permits reopening after error.
                match directory.publish(name, b"", PublishMode::IfAbsent, Privacy::OwnerOnly) {
                    Ok(()) => {}
                    Err(error)
                        if error.state() == PublicationState::NotPublished
                            && error.io_error().kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                directory.open_read(name, Privacy::OwnerOnly)?
            }
            Err(error) => return Err(error.into()),
        };
        file.try_lock()
            .context("another process owns this native session or identity")?;
        Ok(Custody {
            file,
            acquired_by: std::process::id(),
        })
    }
    fn checked_directory(&self) -> Result<StorageDirectory> {
        let directory = StorageDirectory::from_file(self.directory.try_clone()?)?;
        directory.validate_privacy(Privacy::OwnerOnly)?;
        let named_lock = directory.open_read(OsStr::new("owner.lock"), Privacy::OwnerOnly)?;
        if FileIdentity::of(&named_lock)? != FileIdentity::of(&self._lock.file)? {
            bail!("native session custody sentinel changed");
        }
        Ok(directory)
    }
    /// Administrative storage uses this already admitted root and owner lock.
    /// The pathname remains diagnostic; this never reopens it as authority.
    pub(crate) fn administrative_directory(&self) -> Result<StorageDirectory> {
        if self.poisoned {
            bail!("store durability is uncertain");
        }
        self.checked_directory()
    }
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn preflight(
        &self,
        next_signed_file_cap: usize,
    ) -> std::result::Result<(), StorePreflightFailure> {
        if next_signed_file_cap > MAX_AGGREGATE_BYTES {
            return Err(StorePreflightFailure::Capacity);
        }
        let (available, total, unit) = self.filesystem_space()?;
        check_space(available, total, unit, next_signed_file_cap)
    }
    fn filesystem_space(&self) -> std::result::Result<(u64, u64, u64), StorePreflightFailure> {
        #[cfg(test)]
        if let Some(stats) = test_space::statistics(&self.root) {
            return Ok((stats.available, stats.total, stats.unit));
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
            if unsafe { libc::fstatvfs(self.directory.as_raw_fd(), stats.as_mut_ptr()) } != 0 {
                return Err(StorePreflightFailure::Unavailable);
            }
            let stats = unsafe { stats.assume_init() };
            let fragment = stats.f_frsize as u64;
            let unit = stats.f_bsize as u64;
            let available = (stats.f_bavail as u64)
                .checked_mul(fragment)
                .ok_or(StorePreflightFailure::Unavailable)?;
            let total = (stats.f_blocks as u64)
                .checked_mul(fragment)
                .ok_or(StorePreflightFailure::Unavailable)?;
            if fragment == 0 {
                return Err(StorePreflightFailure::Unavailable);
            }
            Ok((available, total, unit))
        }
        #[cfg(not(unix))]
        {
            Err(StorePreflightFailure::Unavailable)
        }
    }
    /// The exact durable state: the checkpoint plus every complete journal
    /// record bound to it.
    pub fn read(&self) -> Result<Vec<u8>> {
        Ok(self.recover()?.state)
    }
    fn read_bounded(file: File) -> Result<Vec<u8>> {
        if file.metadata()?.len() > MAX_AGGREGATE_BYTES as u64 {
            bail!("aggregate exceeds the local storage size limit");
        }
        let mut bytes = vec![];
        file.take(MAX_AGGREGATE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_AGGREGATE_BYTES {
            bail!("aggregate exceeds the local storage size limit");
        }
        Ok(bytes)
    }
    fn recover(&self) -> Result<Recovered> {
        let directory = self.checked_directory()?;
        let file = directory.open_read(OsStr::new("session.bin"), Privacy::OwnerOnly)?;
        let checkpoint_identity = FileIdentity::of(&file)?;
        let checkpoint = Self::read_bounded(file)?;
        let checkpoint_digest = *blake3::hash(&checkpoint).as_bytes();
        let checkpoint_len = checkpoint.len() as u64;
        let (journal_len, replay) =
            match directory.open_read(OsStr::new(journal::NAME), Privacy::OwnerOnly) {
                Ok(file) => {
                    let bytes = Self::read_bounded(file)?;
                    (
                        bytes.len() as u64,
                        journal::replay(checkpoint, &bytes, MAX_AGGREGATE_BYTES),
                    )
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
                    0,
                    journal::Replay {
                        state: checkpoint,
                        valid_len: 0,
                        records: 0,
                    },
                ),
                Err(error) => return Err(error.into()),
            };
        self.checked_directory()?;
        Ok(Recovered {
            state: replay.state,
            checkpoint_digest,
            checkpoint_len,
            checkpoint_identity,
            journal_exists: journal_len > 0 || replay.valid_len > 0,
            journal_len,
            valid_len: replay.valid_len,
            records: replay.records,
        })
    }
    /// Open-time recovery: returns the durable state and primes the journal
    /// so the next commit can append against it.
    pub(crate) fn read_for_owner(&mut self) -> Result<Vec<u8>> {
        let recovered = self.recover()?;
        if recovered.records > 0 || recovered.journal_len != recovered.valid_len {
            tracing::info!(
                records = recovered.records,
                discarded_tail_bytes = recovered.journal_len - recovered.valid_len,
                "session journal replayed onto checkpoint"
            );
        }
        if self.journal_enabled && !self.poisoned {
            let directory = self.checked_directory()?;
            let file = if recovered.journal_exists {
                Some(open_journal(&directory)?)
            } else {
                None
            };
            self.journal = Some(JournalState {
                current: recovered.state.clone(),
                checkpoint_digest: recovered.checkpoint_digest,
                checkpoint_len: recovered.checkpoint_len,
                checkpoint_identity: recovered.checkpoint_identity,
                dirty_tail: recovered.journal_len != recovered.valid_len,
                file,
                valid_len: recovered.valid_len,
            });
        }
        Ok(recovered.state)
    }
    /// Simulates abrupt process death: releases custody without folding the
    /// journal into the checkpoint.
    #[cfg(test)]
    pub(crate) fn abandon(mut self) {
        self.compact_on_drop = false;
    }
    #[cfg(test)]
    pub(crate) fn crash_on_drop(&mut self) {
        self.compact_on_drop = false;
    }
    /// Replace the checkpoint with the current journaled state and empty the
    /// journal. Called on orderly shutdown so the checkpoint alone is complete.
    pub(crate) fn compact(&mut self) -> Result<()> {
        let Some(state) = self.journal.as_ref().filter(|state| state.valid_len > 0) else {
            return Ok(());
        };
        let bytes = state.current.clone();
        let digest = *blake3::hash(&bytes).as_bytes();
        self.commit_checkpoint(&bytes, digest, |directory| Ok(directory.sync_all()?), None)
    }
    pub fn commit(&mut self, bytes: &[u8]) -> Result<()> {
        self.commit_with_barrier(bytes, |directory| Ok(directory.sync_all()?), None)
    }
    pub(crate) fn commit_timed(&mut self, bytes: &[u8], timing: &Timing) -> Result<()> {
        self.commit_with_barrier(bytes, |directory| Ok(directory.sync_all()?), Some(timing))
    }
    fn commit_with_barrier(
        &mut self,
        bytes: &[u8],
        barrier: impl FnOnce(&File) -> Result<()>,
        timing: Option<&Timing>,
    ) -> Result<()> {
        if self.poisoned {
            bail!("durable barrier failed; reopen the owner before proceeding");
        }
        if bytes.len() > MAX_AGGREGATE_BYTES {
            bail!("aggregate exceeds the local storage size limit");
        }
        // A capacity/statistics refusal has performed no write and cannot poison custody.
        self.preflight(bytes.len())?;
        count(&COMMITS, 1);
        let digest = *blake3::hash(bytes).as_bytes();
        if let Some(timing) = timing {
            timing.mark("journal_diff_start");
        }
        let record = self.journal.as_ref().and_then(|state| {
            let record = journal::record(&state.current, bytes, &digest)?;
            let header = if state.valid_len == 0 {
                journal::HEADER_LEN as u64
            } else {
                0
            };
            let after = state.valid_len + header + record.len() as u64;
            (after <= JOURNAL_MIN_BYTES.max(state.checkpoint_len)).then_some(record)
        });
        if let Some(timing) = timing {
            timing.mark("journal_diff_end");
        }
        let result = match record {
            Some(record) => self.append(bytes, digest, &record, barrier, timing),
            None => self.commit_checkpoint(bytes, digest, barrier, timing),
        };
        if result.is_ok() {
            // A caller commit on an owner-recovered store is built from the
            // owner's in-memory aggregate, which carries every applied side
            // record. (`compact` re-publishes older bytes and never gets here.)
            self.truncate_folded_side_log();
        }
        if COMMITS.load(Ordering::Relaxed) % 256 == 0 {
            tracing::info!(summary = %io_summary(), "session store io");
        }
        result
    }
    /// Append one record and barrier the journal file only.
    fn append(
        &mut self,
        bytes: &[u8],
        digest: [u8; 32],
        record: &[u8],
        barrier: impl FnOnce(&File) -> Result<()>,
        timing: Option<&Timing>,
    ) -> Result<()> {
        let result = self.append_inner(bytes, record, barrier, timing);
        self.finish(bytes, digest, result, None)
    }
    fn append_inner(
        &mut self,
        bytes: &[u8],
        record: &[u8],
        barrier: impl FnOnce(&File) -> Result<()>,
        timing: Option<&Timing>,
    ) -> Result<()> {
        let directory = self.checked_directory()?;
        if let Some(timing) = timing {
            timing.mark_count("aggregate_file_bytes", bytes.len());
            timing.mark_count("journal_record_bytes", record.len());
            timing.mark("atomic_write_start");
        }
        #[cfg(test)]
        test_fault::checkpoint(&self.root, test_fault::Point::BeforeWrite)?;
        let state = self
            .journal
            .as_mut()
            .expect("append requires journal state");
        let named = directory.open_read(OsStr::new("session.bin"), Privacy::OwnerOnly)?;
        if FileIdentity::of(&named)? != state.checkpoint_identity {
            bail!("session checkpoint changed outside this owner");
        }
        let mut framed = Vec::with_capacity(journal::HEADER_LEN + record.len());
        if state.valid_len == 0 {
            framed.extend_from_slice(&journal_header(state));
        }
        framed.extend_from_slice(record);
        match state.file.as_mut() {
            Some(file) => {
                // A torn tail or stale journal is cut before appending; the
                // same barrier below makes the cut and the record durable.
                if state.dirty_tail || state.valid_len == 0 {
                    file.set_len(state.valid_len)?;
                    state.dirty_tail = false;
                }
                file.write_all(&framed)?;
            }
            None => {
                // First journal for this store: publish its complete first
                // contents atomically (file barrier included) and make the
                // new name durable with a directory barrier.
                directory.publish(
                    OsStr::new(journal::NAME),
                    &framed,
                    PublishMode::Replace,
                    Privacy::OwnerOnly,
                )?;
                count(&FULL_BARRIERS, 1);
                full_barrier(&self.directory)?;
                state.file = Some(open_journal(&directory)?);
            }
        }
        count(&BYTES_WRITTEN, framed.len() as u64);
        state.valid_len += framed.len() as u64;
        #[cfg(test)]
        test_fault::checkpoint(&self.root, test_fault::Point::AfterAtomicWrite)?;
        if let Some(timing) = timing {
            timing.mark("atomic_write_end");
            timing.mark("directory_sync_start");
        }
        #[cfg(test)]
        test_fault::checkpoint(&self.root, test_fault::Point::DirectoryBarrier)?;
        count(&FULL_BARRIERS, 1);
        barrier(state.file.as_ref().expect("journal file installed"))?;
        self.checked_directory()?;
        if let Some(timing) = timing {
            timing.mark("directory_sync_end");
        }
        Ok(())
    }
    /// Replace the whole checkpoint atomically, then empty the journal.
    fn commit_checkpoint(
        &mut self,
        bytes: &[u8],
        digest: [u8; 32],
        barrier: impl FnOnce(&File) -> Result<()>,
        timing: Option<&Timing>,
    ) -> Result<()> {
        let result = self.checkpoint_inner(bytes, digest, barrier, timing);
        let identity = result.as_ref().ok().cloned();
        self.finish(bytes, digest, result.map(|_| ()), identity)
    }
    /// Returns the identity of the named checkpoint that now holds `bytes`.
    fn checkpoint_inner(
        &mut self,
        bytes: &[u8],
        digest: [u8; 32],
        barrier: impl FnOnce(&File) -> Result<()>,
        timing: Option<&Timing>,
    ) -> Result<FileIdentity> {
        let directory = self.checked_directory()?;
        if let Some(timing) = timing {
            timing.mark_count("aggregate_file_bytes", bytes.len());
            timing.mark("atomic_write_start");
        }
        #[cfg(test)]
        test_fault::checkpoint(&self.root, test_fault::Point::BeforeWrite)?;
        // If the checkpoint on disk already holds exactly these bytes, the
        // journal records after it are the only difference: emptying the
        // journal alone yields `bytes`.
        let unchanged_checkpoint = self.journal.as_ref().is_some_and(|state| {
            state.checkpoint_len == bytes.len() as u64 && state.checkpoint_digest == digest
        });
        if !unchanged_checkpoint {
            directory.publish(
                OsStr::new("session.bin"),
                bytes,
                PublishMode::Replace,
                Privacy::OwnerOnly,
            )?;
            count(&CHECKPOINTS, 1);
            count(&BYTES_WRITTEN, bytes.len() as u64);
            // publish barriers the file (one full barrier) and syncs the
            // directory; the directory barrier below is the second.
            count(&FULL_BARRIERS, 1);
        }
        // publish has completed its real file and directory barriers. These
        // retained owner hooks and the extra sync remain conservative
        // acknowledgement fences, not pre-publication failure claims.
        #[cfg(test)]
        test_fault::checkpoint(&self.root, test_fault::Point::AfterAtomicWrite)?;
        if let Some(timing) = timing {
            timing.mark("atomic_write_end");
            timing.mark("directory_sync_start");
        }
        #[cfg(test)]
        test_fault::checkpoint(&self.root, test_fault::Point::DirectoryBarrier)?;
        count(&FULL_BARRIERS, 1);
        barrier(&self.directory)?;
        let identity =
            FileIdentity::of(&directory.open_read(OsStr::new("session.bin"), Privacy::OwnerOnly)?)?;
        // The new checkpoint supersedes every journal record. A journal still
        // naming the previous checkpoint is ignored on reopen; emptying it
        // here also keeps it from ever being replayed onto a later checkpoint
        // with identical bytes.
        match self.journal.as_mut() {
            Some(state) => {
                if let Some(file) = state
                    .file
                    .as_ref()
                    .filter(|_| state.valid_len > 0 || state.dirty_tail)
                {
                    file.set_len(0)?;
                    full_barrier(file)?;
                }
                state.valid_len = 0;
                state.dirty_tail = false;
                state.checkpoint_digest = digest;
                state.checkpoint_len = bytes.len() as u64;
                state.checkpoint_identity = identity.clone();
            }
            None => match directory.open_read(OsStr::new(journal::NAME), Privacy::OwnerOnly) {
                // A store that was not opened through owner recovery may still
                // hold an older journal on disk.
                Ok(file) if file.metadata()?.len() > 0 => {
                    let file = open_journal(&directory)?;
                    file.set_len(0)?;
                    full_barrier(&file)?;
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            },
        }
        self.checked_directory()?;
        if let Some(timing) = timing {
            timing.mark("directory_sync_end");
        }
        Ok(identity)
    }
    /// `checkpoint` is the new checkpoint's identity, or `None` for an append.
    fn finish(
        &mut self,
        bytes: &[u8],
        digest: [u8; 32],
        result: Result<()>,
        checkpoint: Option<FileIdentity>,
    ) -> Result<()> {
        if result.is_err() {
            self.poisoned = true;
            self.journal = None;
            self.side_log = None;
        } else {
            if checkpoint.is_none() {
                count(&APPENDS, 1);
            }
            self.acknowledged_digest = Some(digest);
            if let Some(state) = self.journal.as_mut() {
                state.current.clear();
                state.current.extend_from_slice(bytes);
            } else if let Some(identity) = checkpoint.filter(|_| self.journal_enabled) {
                // The first commit of a created store: start journaling after it.
                self.journal = Some(JournalState {
                    current: bytes.to_vec(),
                    checkpoint_digest: digest,
                    checkpoint_len: bytes.len() as u64,
                    checkpoint_identity: identity,
                    file: None,
                    valid_len: 0,
                    dirty_tail: false,
                });
            }
        }
        result
    }
}
/// The journal is appended through the held, admitted state directory.
#[cfg(unix)]
fn open_journal(directory: &StorageDirectory) -> Result<File> {
    Ok(directory.open_append_existing(OsStr::new(journal::NAME), Privacy::OwnerOnly)?)
}
#[cfg(not(unix))]
fn open_journal(_: &StorageDirectory) -> Result<File> {
    bail!("session journal requires a Unix storage directory")
}
fn journal_header(state: &JournalState) -> [u8; journal::HEADER_LEN] {
    journal::header_for(state.checkpoint_len, &state.checkpoint_digest)
}
struct Recovered {
    state: Vec<u8>,
    checkpoint_digest: [u8; 32],
    checkpoint_len: u64,
    checkpoint_identity: FileIdentity,
    journal_exists: bool,
    journal_len: u64,
    valid_len: u64,
    records: usize,
}
impl Drop for SessionStore {
    fn drop(&mut self) {
        // Orderly shutdown folds the journal into the checkpoint so that the
        // checkpoint alone is complete; a crash leaves the journal to replay.
        if self.compact_on_drop
            && !self.poisoned
            && self
                .journal
                .as_ref()
                .is_some_and(|state| state.valid_len > 0)
        {
            if let Err(error) = self.compact() {
                tracing::warn!(%error, "session journal compaction deferred to next open");
            }
        }
        if COMMITS.load(Ordering::Relaxed) > 0 {
            tracing::info!(summary = %io_summary(), "session store io");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_startup_digest_requires_actual_barrier_and_never_read_or_poison() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut store = SessionStore::create(&root).unwrap();
        assert!(store.acknowledged_digest().is_err());
        store.commit(b"acknowledged").unwrap();
        assert_eq!(
            store.acknowledged_digest().unwrap(),
            *blake3::hash(b"acknowledged").as_bytes()
        );
        drop(store);
        let mut store = SessionStore::open(&root).unwrap();
        assert_eq!(store.read().unwrap(), b"acknowledged");
        assert!(
            store.acknowledged_digest().is_err(),
            "read is not an acknowledged barrier"
        );
        store.commit(b"acknowledged").unwrap();
        assert!(
            store
                .commit_with_barrier(b"uncertain", |_| bail!("injected directory barrier"), None)
                .is_err()
        );
        assert!(
            store.acknowledged_digest().is_err(),
            "poison hides even the preceding digest"
        );
    }
    #[test]
    fn inherited_custody_cleanup_cannot_unlock_the_acquiring_process() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("custody.lock");
        let mut inherited = custody(&path).unwrap();
        // A duplicate keeps the shared open-file description alive exactly as
        // the acquiring process would after fork. Simulate only the inherited
        // guard's process identity; no Rust code runs inside a fork child.
        let acquiring_process_file = inherited.file.try_clone().unwrap();
        inherited.acquired_by = std::process::id().wrapping_add(1);
        drop(inherited);
        assert!(
            custody(&path).is_err(),
            "non-acquiring cleanup unlocked a live owner's description"
        );
        drop(acquiring_process_file);
        assert!(
            custody(&path).is_ok(),
            "final descriptor close must release custody"
        );
    }
    #[test]
    fn oversize_commit_cannot_replace_reopenable_state() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = SessionStore::create(&temp.path().join("state")).unwrap();
        store.commit(b"retained").unwrap();
        assert!(store.commit(&vec![0; MAX_AGGREGATE_BYTES + 1]).is_err());
        assert_eq!(store.read().unwrap(), b"retained");
        assert!(!store.is_poisoned());
    }
    #[test]
    fn directory_barrier_failure_is_not_an_acknowledgement() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = SessionStore::create(&temp.path().join("state")).unwrap();
        store.commit(b"old").unwrap();
        let missing = temp.path().join("missing-directory");
        assert!(
            store
                .commit_with_barrier(b"new", |_| sync_dir(&missing), None)
                .is_err()
        );
        assert!(store.commit(b"ignored").is_err());
        // Rename can have happened; restart recovers a complete value, never a receipt.
        drop(store);
        let mut reopened = SessionStore::open(&temp.path().join("state")).unwrap();
        assert_eq!(reopened.read().unwrap(), b"new");
        reopened.commit(b"new").unwrap();
    }
}

#[cfg(test)]
pub(crate) mod test_fault {
    use std::{
        cell::RefCell,
        path::{Path, PathBuf},
    };
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Point {
        BeforeWrite,
        AfterAtomicWrite,
        DirectoryBarrier,
    }
    thread_local! { static FAULT: RefCell<Option<(PathBuf, Point)>> = const { RefCell::new(None) }; }
    struct Restore(Option<(PathBuf, Point)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAULT.with(|fault| *fault.borrow_mut() = self.0.take());
        }
    }
    pub(crate) fn with_fault<T>(root: &Path, point: Point, operation: impl FnOnce() -> T) -> T {
        let guard = Restore(FAULT.with(|fault| fault.replace(Some((root.to_owned(), point)))));
        let result = operation();
        drop(guard);
        result
    }
    /// Arm one fault on this thread until it fires (for async tests whose
    /// owner runs on the current-thread runtime).
    pub(crate) fn arm(root: &Path, point: Point) {
        FAULT.with(|fault| *fault.borrow_mut() = Some((root.to_owned(), point)));
    }
    pub(super) fn checkpoint(root: &Path, point: Point) -> anyhow::Result<()> {
        let fail = FAULT.with(|fault| {
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
        anyhow::ensure!(!fail, "injected session barrier failure at {point:?}");
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test_journal {
    use std::cell::Cell;
    thread_local! { static FORCED: Cell<bool> = const { Cell::new(false) }; }
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCED.with(|forced| forced.set(self.0));
        }
    }
    pub(crate) fn enabled() -> bool {
        FORCED.with(Cell::get)
            || std::env::var_os("QOS_TEST_SESSION_JOURNAL").is_some_and(|value| value == "1")
    }
    /// Stores opened inside `operation` on this thread journal their commits.
    pub(crate) fn with_journal<T>(operation: impl FnOnce() -> T) -> T {
        let _restore = Restore(FORCED.with(|forced| forced.replace(true)));
        operation()
    }
}

#[cfg(test)]
pub(crate) mod test_space {
    use std::{
        cell::RefCell,
        path::{Path, PathBuf},
    };
    #[derive(Clone, Copy)]
    pub(crate) struct Statistics {
        pub available: u64,
        pub total: u64,
        pub unit: u64,
    }
    thread_local! { static SPACE: RefCell<Option<(PathBuf,Statistics)>> = const { RefCell::new(None) }; }
    struct Restore(Option<(PathBuf, Statistics)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SPACE.with(|space| *space.borrow_mut() = self.0.take());
        }
    }
    pub(crate) fn with_space<T>(
        root: &Path,
        stats: Statistics,
        operation: impl FnOnce() -> T,
    ) -> T {
        let guard = Restore(SPACE.with(|space| space.replace(Some((root.to_owned(), stats)))));
        let result = operation();
        drop(guard);
        result
    }
    pub(super) fn statistics(root: &Path) -> Option<Statistics> {
        SPACE.with(|space| {
            space
                .borrow()
                .as_ref()
                .filter(|(path, _)| path == root)
                .map(|(_, stats)| *stats)
        })
    }
}
#[cfg(test)]
mod resource_tests {
    use super::*;
    #[test]
    fn store_resource_disk_refusal_does_not_write_or_poison() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = SessionStore::create(&root).unwrap();
        store.commit(b"old").unwrap();
        let refused = test_space::with_space(
            &root,
            test_space::Statistics {
                available: 0,
                total: 2 * 1024 * 1024 * 1024,
                unit: 4096,
            },
            || store.commit(b"new"),
        );
        assert!(
            refused.is_err(),
            "low disk must refuse before the atomic write"
        );
        assert_eq!(store.read().unwrap(), b"old");
        assert!(
            !store.is_poisoned(),
            "no write occurred, so old owner remains usable"
        );
        store.commit(b"healthy").unwrap();
    }
    #[test]
    fn store_resource_disk_threshold_rounds_stage_and_preserves_low_water() {
        use StorePreflightFailure::{Capacity, Unavailable};
        let cases = [
            (2 * 1024 * 1024 * 1024, 4097, 1_073_815_552),
            (40 * 1024 * 1024 * 1024, 1, 2_147_553_280),
        ];
        for (total, cap, required) in cases {
            assert_eq!(check_space(required, total, 4096, cap), Ok(()));
            assert_eq!(check_space(required - 1, total, 4096, cap), Err(Capacity));
        }
        assert_eq!(check_space(u64::MAX, u64::MAX, 4096, 1), Err(Unavailable));
        assert_eq!(check_space(2, 1, 4096, 1), Err(Unavailable));
        assert_eq!(check_space(1, 1, 0, 1), Err(Unavailable));
    }
    #[cfg(unix)]
    #[test]
    fn store_resource_statistics_use_retained_checked_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let store = SessionStore::create(&root).unwrap();
        std::fs::rename(&root, temp.path().join("retained-store")).unwrap();
        std::fs::write(&root, b"replacement is not a directory").unwrap();
        assert!(
            store.preflight(4097).is_ok(),
            "statistics must query retained directory descriptor"
        );
    }
    #[test]
    fn store_resource_unknown_space_does_not_begin_commit() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = SessionStore::create(&root).unwrap();
        store.commit(b"old").unwrap();
        let refused = test_space::with_space(
            &root,
            test_space::Statistics {
                available: u64::MAX,
                total: 0,
                unit: 0,
            },
            || store.commit(b"new"),
        );
        assert!(
            refused.is_err(),
            "invalid filesystem statistics must fail closed"
        );
        assert_eq!(store.read().unwrap(), b"old");
        assert!(!store.is_poisoned());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StorePreflightFailure {
    Capacity,
    Unavailable,
}
impl std::fmt::Display for StorePreflightFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Capacity => "session disk capacity unavailable",
            Self::Unavailable => "session filesystem statistics unavailable",
        })
    }
}
impl std::error::Error for StorePreflightFailure {}
fn check_space(
    available: u64,
    total: u64,
    unit: u64,
    file_cap: usize,
) -> std::result::Result<(), StorePreflightFailure> {
    use StorePreflightFailure::{Capacity, Unavailable};
    if total == 0 || unit == 0 || available > total {
        return Err(Unavailable);
    }
    let round = |value: u64| -> Option<u64> {
        value
            .checked_add(unit - 1)?
            .checked_div(unit)?
            .checked_mul(unit)
    };
    let low = total
        .checked_add(19)
        .ok_or(Unavailable)?
        .checked_div(20)
        .ok_or(Unavailable)?
        .max(1024 * 1024 * 1024);
    let stage = round(u64::try_from(file_cap).map_err(|_| Unavailable)?).ok_or(Unavailable)?;
    // Explicit metadata headroom; not a filesystem allocation guarantee.
    let metadata = round(64 * 1024).ok_or(Unavailable)?;
    let required = low
        .checked_add(stage)
        .and_then(|n| n.checked_add(metadata))
        .ok_or(Unavailable)?;
    if available < required {
        Err(Capacity)
    } else {
        Ok(())
    }
}

#[path = "store/host_work.rs"]
mod host_work;
pub use crate::staging_manifest::StageIdentity;
pub use host_work::{
    HostWorkFailure, HostWorkInventory, LargeDecodeLease, SidecarBytes, SnapshotDirectory,
    SnapshotReservation, StagingDirectory, StagingReservation,
};

#[cfg(all(test, unix))]
#[path = "store/fd_tests.rs"]
mod fd_tests;
#[cfg(test)]
#[path = "store/journal_tests.rs"]
mod journal_tests;
#[cfg(test)]
#[path = "store/side_log_tests.rs"]
mod side_log_tests;
