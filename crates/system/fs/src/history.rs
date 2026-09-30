//! Crash-safe immutable file history and first-class conflict records.
//!
//! History is keyed by [`FileRecordId`], never by path. Every mutation appends
//! a node whose parents already exist locally; importing out-of-order history
//! must therefore stage it before calling this store. Disk state is validated
//! in full on open so corrupt ancestry cannot become an eviction or restore
//! root.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use collab_types::{
    ConflictResolution, ConflictStatus, FileConflictV1, FileProvenance, FileRecordError,
    FileRecordId, FileVersionId, FileVersionV1,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const HISTORY_SCHEMA_VERSION: u16 = 1;
pub const HISTORY_XATTR: &str = "user.tilo.history";
pub const HISTORY_CONTROL_XATTR: &str = "user.tilo.history-control";
pub const HISTORY_PROJECTION_VERSION: u16 = 1;
pub const MAX_HISTORY_PROJECTION_BYTES: usize = 16 * 1024;
const MAX_PROJECTED_VERSIONS: usize = 12;
const MAX_PROJECTED_CONFLICTS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryProjectionV1 {
    pub schema_version: u16,
    pub record_id: FileRecordId,
    pub total_versions: u64,
    pub versions_truncated: bool,
    pub heads: Vec<FileVersionId>,
    pub versions: Vec<FileVersionV1>,
    pub conflicts_truncated: bool,
    pub conflicts: Vec<FileConflictV1>,
}

impl HistoryProjectionV1 {
    /// Serialize this bounded projection for the history xattr.
    ///
    /// # Errors
    ///
    /// Returns an error when JSON encoding fails or the result exceeds the
    /// projection byte limit.
    pub fn to_xattr_bytes(&self) -> Result<Vec<u8>, HistoryError> {
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_HISTORY_PROJECTION_BYTES {
            return Err(HistoryError::ProjectionTooLarge(bytes.len()));
        }
        Ok(bytes)
    }

    /// Parse and fully validate a history xattr projection.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized or malformed JSON, unsupported schema,
    /// invalid records/conflicts, or an inconsistent record id/count.
    pub fn parse_xattr(bytes: &[u8]) -> Result<Self, HistoryError> {
        if bytes.len() > MAX_HISTORY_PROJECTION_BYTES {
            return Err(HistoryError::ProjectionTooLarge(bytes.len()));
        }
        let value: Self = serde_json::from_slice(bytes)?;
        if value.schema_version != HISTORY_PROJECTION_VERSION {
            return Err(HistoryError::UnsupportedProjectionVersion(
                value.schema_version,
            ));
        }
        if value.versions.len() > MAX_PROJECTED_VERSIONS
            || value.conflicts.len() > MAX_PROJECTED_CONFLICTS
            || value.total_versions < value.versions.len() as u64
        {
            return Err(HistoryError::InvalidProjection);
        }
        for version in &value.versions {
            version.validate()?;
            if version.record_id != value.record_id {
                return Err(HistoryError::WrongRecord);
            }
        }
        for conflict in &value.conflicts {
            conflict.validate()?;
            if conflict.record_id != value.record_id {
                return Err(HistoryError::WrongRecord);
            }
        }
        Ok(value)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryDocument {
    schema_version: u16,
    records: Vec<RecordDocument>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordDocument {
    record_id: FileRecordId,
    versions: Vec<FileVersionV1>,
    heads: Vec<FileVersionId>,
    conflicts: Vec<FileConflictV1>,
}

#[derive(Clone, Debug, Default)]
struct RecordState {
    versions: BTreeMap<FileVersionId, FileVersionV1>,
    heads: BTreeSet<FileVersionId>,
    conflicts: BTreeMap<Uuid, FileConflictV1>,
}

#[derive(Debug, Default)]
struct HistoryState {
    records: BTreeMap<FileRecordId, RecordState>,
    /// The record owning each version id; ids are unique workspace-wide, so
    /// collision checks are a lookup instead of a scan of every record.
    version_owner: HashMap<FileVersionId, FileRecordId>,
    log: HistoryLog,
}

/// The open append-only log beside the snapshot and how much of it is
/// known-good.
#[derive(Debug, Default)]
struct HistoryLog {
    file: Option<std::fs::File>,
    len: u64,
    entries: usize,
    /// Mutations already installed in memory whose log write is deferred
    /// (see [`FileHistoryStore::defer_durability`]), in install order.
    unwritten: Vec<LogEntry>,
}

/// One durable history mutation. Each is validated once when it is made and
/// once when the log is replayed at open; appending never re-reads or
/// re-validates the rest of the workspace's history.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum LogEntry {
    Versions {
        versions: Vec<FileVersionV1>,
    },
    Conflict {
        conflict: FileConflictV1,
    },
    Resolve {
        record_id: FileRecordId,
        conflict_id: Uuid,
        resolution: ConflictResolution,
    },
}

/// A bounded copy suitable for metadata/UI projection without exposing the
/// mutable store lock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecordHistorySnapshot {
    pub record_id: FileRecordId,
    pub versions: Vec<FileVersionV1>,
    pub heads: Vec<FileVersionId>,
    pub conflicts: Vec<FileConflictV1>,
}

/// Single-writer, crash-safe history database for one workspace.
///
/// State is a validated snapshot (`history.json`, the format written by
/// earlier releases, read as-is) plus an append-only log of later mutations
/// (`history.json.log`). Every line of the log carries the blake3 hash of
/// its entry; a torn final line (a write that never returned) is discarded
/// at open, any other damage fails the open. After
/// [`LOG_COMPACT_AFTER_ENTRIES`] entries the log is folded into a new
/// snapshot, so an append costs O(the record it touches), not O(workspace).
pub struct FileHistoryStore {
    path: PathBuf,
    log_path: PathBuf,
    state: Mutex<HistoryState>,
    /// Threads whose appends are deferred to one durable write.
    deferring: Mutex<HashSet<std::thread::ThreadId>>,
    /// Log entries after which the log is folded into the snapshot;
    /// [`LOG_COMPACT_AFTER_ENTRIES`] outside tests.
    compact_after: usize,
}

/// Defers the durable log write of this thread's history appends until
/// [`Self::finish`], which writes all of them at once with one sync. Memory
/// is updated immediately, and any durable write made meanwhile (by any
/// thread) writes the deferred mutations first, so the log keeps the order
/// of memory.
pub struct HistoryDeferral<'a> {
    store: &'a FileHistoryStore,
    finished: bool,
}

impl HistoryDeferral<'_> {
    /// Write every deferred mutation durably.
    ///
    /// # Errors
    ///
    /// Returns an error when the log write or its sync fails; the
    /// mutations stay deferred and the next durable write retries them.
    pub fn finish(mut self) -> Result<(), HistoryError> {
        self.finished = true;
        self.store.stop_deferring();
        self.store.flush()
    }
}

impl Drop for HistoryDeferral<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.store.stop_deferring();
        }
    }
}

/// Log entries after which the log is folded into the snapshot.
pub const LOG_COMPACT_AFTER_ENTRIES: usize = 4096;

/// Existing history writer lock borrowed by a compound admission/projection.
pub(crate) struct HistoryLease<'a> {
    store: &'a FileHistoryStore,
    state: std::sync::MutexGuard<'a, HistoryState>,
    /// Whether writes may wait for the thread's deferred durable write.
    defer: bool,
}

pub(crate) struct HistoryCandidate {
    touched: BTreeMap<FileRecordId, RecordAdditions>,
    owners: Vec<(FileVersionId, FileRecordId)>,
    accepted: Vec<FileVersionV1>,
    appended: Vec<bool>,
}

impl HistoryLease<'_> {
    pub fn resolve(
        &mut self,
        record_id: FileRecordId,
        conflict_id: Uuid,
        resolution: ConflictResolution,
    ) -> Result<bool, HistoryError> {
        let Some(resolved) = stage_resolution(&self.state, record_id, conflict_id, &resolution)?
        else {
            return Ok(false);
        };
        self.store.append_entry(
            &mut self.state,
            &LogEntry::Resolve {
                record_id,
                conflict_id,
                resolution,
            },
            self.defer,
        )?;
        install_conflict(&mut self.state, resolved);
        self.store.maybe_compact(&mut self.state);
        Ok(true)
    }

    pub fn record_conflict(&mut self, conflict: FileConflictV1) -> Result<bool, HistoryError> {
        if !stage_conflict(&self.state, &conflict)? {
            return Ok(false);
        }
        self.store.append_entry(
            &mut self.state,
            &LogEntry::Conflict {
                conflict: conflict.clone(),
            },
            self.defer,
        )?;
        install_conflict(&mut self.state, conflict);
        self.store.maybe_compact(&mut self.state);
        Ok(true)
    }

    pub fn prepare_versions(
        &self,
        versions: &[FileVersionV1],
    ) -> Result<HistoryCandidate, HistoryError> {
        stage_versions(&self.state, versions)
    }

    pub fn install(&mut self, candidate: HistoryCandidate) -> Result<Vec<bool>, HistoryError> {
        if candidate.accepted.is_empty() {
            return Ok(candidate.appended);
        }
        self.store.append_entry(
            &mut self.state,
            &LogEntry::Versions {
                versions: candidate.accepted.clone(),
            },
            self.defer,
        )?;
        let appended = install_versions(&mut self.state, candidate);
        self.store.maybe_compact(&mut self.state);
        Ok(appended)
    }

    pub fn append_batch(&mut self, versions: &[FileVersionV1]) -> Result<Vec<bool>, HistoryError> {
        let candidate = self.prepare_versions(versions)?;
        self.install(candidate)
    }

    pub fn snapshot(&self, record_id: FileRecordId) -> Option<RecordHistorySnapshot> {
        self.state
            .records
            .get(&record_id)
            .map(|record| snapshot(record_id, record))
    }
}

/// The versions a batch adds to one record and the heads that result.
#[derive(Debug, Default)]
struct RecordAdditions {
    versions: BTreeMap<FileVersionId, FileVersionV1>,
    heads: BTreeSet<FileVersionId>,
}

/// Validate `versions` against `state` (in input order) without touching it.
/// Nothing is copied but the batch itself and the heads of touched records.
fn stage_versions(
    state: &HistoryState,
    versions: &[FileVersionV1],
) -> Result<HistoryCandidate, HistoryError> {
    for version in versions {
        version.validate()?;
    }
    let mut touched: BTreeMap<FileRecordId, RecordAdditions> = BTreeMap::new();
    let mut owners: HashMap<FileVersionId, FileRecordId> = HashMap::new();
    let mut accepted = Vec::new();
    let mut appended = Vec::with_capacity(versions.len());
    for version in versions {
        let owner = owners
            .get(&version.id)
            .or_else(|| state.version_owner.get(&version.id))
            .copied();
        if let Some(owner) = owner {
            let existing = touched
                .get(&owner)
                .and_then(|additions| additions.versions.get(&version.id))
                .or_else(|| {
                    state
                        .records
                        .get(&owner)
                        .and_then(|record| record.versions.get(&version.id))
                });
            if existing == Some(version) {
                appended.push(false);
                continue;
            }
            return Err(HistoryError::VersionIdCollision(version.id));
        }

        let current = state.records.get(&version.record_id);
        let additions = touched
            .entry(version.record_id)
            .or_insert_with(|| RecordAdditions {
                versions: BTreeMap::new(),
                heads: current
                    .map(|record| record.heads.clone())
                    .unwrap_or_default(),
            });
        let known = |id: &FileVersionId| {
            additions.versions.contains_key(id)
                || current.is_some_and(|record| record.versions.contains_key(id))
        };
        let empty = additions.versions.is_empty()
            && current.is_none_or(|record| record.versions.is_empty());
        if empty {
            if let Some(parent) = version.parents.first() {
                return Err(HistoryError::MissingParent(*parent));
            }
        } else {
            if version.parents.is_empty() {
                return Err(HistoryError::DisconnectedVersion(version.id));
            }
            if let Some(parent) = version.parents.iter().find(|parent| !known(parent)) {
                return Err(HistoryError::MissingParent(*parent));
            }
        }
        for parent in &version.parents {
            additions.heads.remove(parent);
        }
        additions.heads.insert(version.id);
        additions.versions.insert(version.id, version.clone());
        owners.insert(version.id, version.record_id);
        accepted.push(version.clone());
        appended.push(true);
    }
    Ok(HistoryCandidate {
        touched,
        owners: owners.into_iter().collect(),
        accepted,
        appended,
    })
}

fn install_versions(state: &mut HistoryState, candidate: HistoryCandidate) -> Vec<bool> {
    for (record_id, additions) in candidate.touched {
        let record = state.records.entry(record_id).or_default();
        record.versions.extend(additions.versions);
        record.heads = additions.heads;
    }
    state.version_owner.extend(candidate.owners);
    candidate.appended
}

/// `Ok(true)` when `conflict` is new and valid, `Ok(false)` for an exact
/// replay.
fn stage_conflict(state: &HistoryState, conflict: &FileConflictV1) -> Result<bool, HistoryError> {
    conflict.validate()?;
    let record = state
        .records
        .get(&conflict.record_id)
        .ok_or(HistoryError::UnknownRecord(conflict.record_id))?;
    validate_conflict(record, conflict)?;
    match record.conflicts.get(&conflict.id) {
        Some(existing) if existing == conflict => Ok(false),
        Some(_) => Err(HistoryError::ConflictIdCollision(conflict.id)),
        None => Ok(true),
    }
}

fn install_conflict(state: &mut HistoryState, conflict: FileConflictV1) {
    if let Some(record) = state.records.get_mut(&conflict.record_id) {
        record.conflicts.insert(conflict.id, conflict);
    }
}

/// The resolved conflict to install, or `None` when `resolution` changes
/// nothing (an exact replay, or a resolution the current one is preferred to).
fn stage_resolution(
    state: &HistoryState,
    record_id: FileRecordId,
    conflict_id: Uuid,
    resolution: &ConflictResolution,
) -> Result<Option<FileConflictV1>, HistoryError> {
    let record = state
        .records
        .get(&record_id)
        .ok_or(HistoryError::UnknownRecord(record_id))?;
    let conflict = record
        .conflicts
        .get(&conflict_id)
        .ok_or(HistoryError::UnknownConflict(conflict_id))?;
    if let Some(existing) = &conflict.resolution
        && (existing == resolution
            || !collab_types::prefer_conflict_resolution(resolution, existing))
    {
        return Ok(None);
    }
    let result = record
        .versions
        .get(&resolution.result)
        .ok_or(HistoryError::UnknownResolutionVersion(resolution.result))?;
    if result.provenance != FileProvenance::ConflictResolution {
        return Err(HistoryError::ResolutionProvenance);
    }
    if result.actor != resolution.actor {
        return Err(HistoryError::ResolutionActorMismatch);
    }
    if result.timestamp_ms != resolution.timestamp_ms {
        return Err(HistoryError::ResolutionTimestampMismatch);
    }
    let mut resolved = conflict.clone();
    resolved.status = ConflictStatus::Resolved;
    resolved.resolution = Some(resolution.clone());
    resolved.validate()?;
    Ok(Some(resolved))
}

/// Apply one replayed log entry with the same validation as a live append.
/// Entries already contained in the snapshot are exact replays.
fn replay_entry(state: &mut HistoryState, entry: LogEntry) -> Result<(), HistoryError> {
    match entry {
        LogEntry::Versions { versions } => {
            let candidate = stage_versions(state, &versions)?;
            install_versions(state, candidate);
        }
        LogEntry::Conflict { conflict } => {
            if stage_conflict(state, &conflict)? {
                install_conflict(state, conflict);
            }
        }
        LogEntry::Resolve {
            record_id,
            conflict_id,
            resolution,
        } => {
            if let Some(resolved) = stage_resolution(state, record_id, conflict_id, &resolution)? {
                install_conflict(state, resolved);
            }
        }
    }
    Ok(())
}

fn encode_log_line(entry: &LogEntry) -> Result<Vec<u8>, HistoryError> {
    let json = serde_json::to_vec(entry)?;
    let mut line = Vec::with_capacity(json.len().saturating_add(66));
    line.extend_from_slice(blake3::hash(&json).to_hex().as_bytes());
    line.push(b' ');
    line.extend_from_slice(&json);
    line.push(b'\n');
    Ok(line)
}

fn decode_log_line(line: &[u8]) -> Result<LogEntry, String> {
    let (digest, json) = line
        .split_first_chunk::<64>()
        .ok_or_else(|| "log line is too short".to_string())?;
    let json = json
        .strip_prefix(b" ")
        .ok_or_else(|| "log line has no separator".to_string())?;
    if blake3::hash(json).to_hex().as_bytes() != digest {
        return Err("log line does not match its digest".into());
    }
    serde_json::from_slice(json).map_err(|error| error.to_string())
}

/// Replay the log at `log_path` into `state`. Returns the length of the
/// good prefix and its entry count; a torn final line is reported so the
/// caller can drop it.
fn replay_log(
    log_path: &Path,
    state: &mut HistoryState,
) -> Result<(u64, usize, bool), HistoryError> {
    let bytes = match std::fs::read(log_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((0, 0, false));
        }
        Err(error) => return Err(error.into()),
    };
    let mut good = 0_usize;
    let mut entries = 0_usize;
    let mut rest = bytes.as_slice();
    while let Some(newline) = rest.iter().position(|byte| *byte == b'\n') {
        let (line, tail) = rest.split_at(newline);
        let tail = tail.get(1..).unwrap_or_default();
        match decode_log_line(line) {
            Ok(entry) => replay_entry(state, entry)?,
            // A write that never returned can leave a damaged last line;
            // anything before it was acknowledged and must be intact.
            Err(_) if tail.is_empty() => break,
            Err(error) => {
                return Err(HistoryError::DamagedLog(format!(
                    "{} at byte {good}: {error}",
                    log_path.display()
                )));
            }
        }
        good = good.saturating_add(newline).saturating_add(1);
        entries = entries.saturating_add(1);
        rest = tail;
    }
    Ok((good as u64, entries, good < bytes.len()))
}

impl FileHistoryStore {
    /// The writer lock for a compound admission or projection. Its writes
    /// are always durable when they return: callers advance their own
    /// journals on it.
    pub(crate) fn lease(&self) -> Result<HistoryLease<'_>, HistoryError> {
        Ok(HistoryLease {
            store: self,
            state: self.state.lock().map_err(|_| HistoryError::StatePoisoned)?,
            defer: false,
        })
    }

    /// The writer lock for a plain append, deferred when this thread defers.
    fn append_lease(&self) -> Result<HistoryLease<'_>, HistoryError> {
        let defer = self.is_deferring();
        Ok(HistoryLease {
            store: self,
            state: self.state.lock().map_err(|_| HistoryError::StatePoisoned)?,
            defer,
        })
    }

    fn is_deferring(&self) -> bool {
        self.deferring
            .lock()
            .is_ok_and(|threads| threads.contains(&std::thread::current().id()))
    }

    fn stop_deferring(&self) {
        if let Ok(mut threads) = self.deferring.lock() {
            threads.remove(&std::thread::current().id());
        }
    }

    /// Defer this thread's appends until the returned guard finishes.
    pub fn defer_durability(&self) -> HistoryDeferral<'_> {
        if let Ok(mut threads) = self.deferring.lock() {
            threads.insert(std::thread::current().id());
        }
        HistoryDeferral {
            store: self,
            finished: false,
        }
    }

    /// Write deferred mutations durably now.
    ///
    /// # Errors
    ///
    /// Returns an error when the log write or its sync fails.
    pub fn flush(&self) -> Result<(), HistoryError> {
        let mut state = self.state.lock().map_err(|_| HistoryError::StatePoisoned)?;
        let result = self.write_lines(&mut state, None);
        drop(state);
        result
    }

    /// Durably append `entry`, or with `defer` keep it for the next durable
    /// write.
    fn append_entry(
        &self,
        state: &mut HistoryState,
        entry: &LogEntry,
        defer: bool,
    ) -> Result<(), HistoryError> {
        if defer {
            // Consecutive version batches fold into one entry, so a
            // deferred batch is one log line however many files it touched.
            match (state.log.unwritten.last_mut(), entry) {
                (Some(LogEntry::Versions { versions }), LogEntry::Versions { versions: more }) => {
                    versions.extend(more.iter().cloned());
                }
                _ => state.log.unwritten.push(entry.clone()),
            }
            return Ok(());
        }
        self.write_lines(state, Some(entry))
    }

    /// Write the deferred mutations, then `entry`, in one write and one
    /// sync. On failure the log is cut back to its last good length, so a
    /// failed append never leaves a torn line in the middle of the log, and
    /// the deferred mutations stay deferred.
    fn write_lines(
        &self,
        state: &mut HistoryState,
        entry: Option<&LogEntry>,
    ) -> Result<(), HistoryError> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;

        if state.log.unwritten.is_empty() && entry.is_none() {
            return Ok(());
        }
        let mut line = Vec::new();
        let mut count = 0_usize;
        for pending in state.log.unwritten.iter().chain(entry) {
            line.extend_from_slice(&encode_log_line(pending)?);
            count = count.saturating_add(1);
        }
        if state.log.file.is_none() {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&self.log_path)?;
            if let Some(parent) = self.log_path.parent()
                && !parent.as_os_str().is_empty()
            {
                tilo_files::native::atomic::sync_directory(parent)?;
            }
            file.set_len(state.log.len)?;
            state.log.file = Some(file);
        }
        let Some(file) = state.log.file.as_mut() else {
            return Err(HistoryError::StatePoisoned);
        };
        if let Err(error) = file.write_all(&line).and_then(|()| file.sync_data()) {
            let _ = file.set_len(state.log.len);
            state.log.file = None;
            return Err(error.into());
        }
        state.log.len = state.log.len.saturating_add(line.len() as u64);
        state.log.entries = state.log.entries.saturating_add(count);
        state.log.unwritten.clear();
        Ok(())
    }

    fn maybe_compact(&self, state: &mut HistoryState) {
        if state.log.entries >= self.compact_after
            && let Err(error) = self.compact_locked(state)
        {
            // The log already holds every entry durably; compaction only
            // bounds its length and is retried on the next append.
            eprintln!(
                "could not fold history log {} into its snapshot: {error}",
                self.log_path.display()
            );
        }
    }

    /// Write the full state as a new snapshot, then empty the log. A crash
    /// between the two replays entries the snapshot already holds, which
    /// are exact replays.
    fn compact_locked(&self, state: &mut HistoryState) -> Result<(), HistoryError> {
        // The snapshot holds everything in memory, deferred mutations too.
        persist(&self.path, state)?;
        state.log.unwritten.clear();
        match state.log.file.as_mut() {
            Some(file) => {
                file.set_len(0)?;
                file.sync_all()?;
            }
            None => match std::fs::OpenOptions::new().write(true).open(&self.log_path) {
                Ok(file) => {
                    file.set_len(0)?;
                    file.sync_all()?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            },
        }
        state.log.len = 0;
        state.log.entries = 0;
        Ok(())
    }

    /// Fold the log into the snapshot now.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot be written or the log
    /// cannot be emptied.
    pub fn compact(&self) -> Result<(), HistoryError> {
        let mut state = self.state.lock().map_err(|_| HistoryError::StatePoisoned)?;
        let result = self.compact_locked(&mut state);
        drop(state);
        result
    }

    /// Entries in the log that are not yet folded into the snapshot.
    #[must_use]
    pub fn pending_log_entries(&self) -> usize {
        self.state.lock().map_or(0, |state| state.log.entries)
    }

    /// Open and fully validate a durable history store: the snapshot (with
    /// recovery only from a retained generation that passes the same
    /// validation), then every log entry.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O, malformed history, invalid graph/conflicts, a
    /// damaged log, or damaged state without a valid retained generation.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, HistoryError> {
        let path = path.into();
        let mut state = match crate::durable_backup::read_primary(&path)? {
            Some(bytes) => match decode_document(&bytes) {
                Ok(state) => state,
                Err(primary_error) => {
                    let outcome = match crate::durable_backup::read_or_recover(&path, |bytes| {
                        decode_document(bytes).is_ok()
                    }) {
                        Ok(Some(outcome)) => outcome,
                        Ok(None) => return Err(primary_error),
                        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                            return Err(primary_error);
                        }
                        Err(error) => return Err(error.into()),
                    };
                    if let Some(recovery) = &outcome.recovery {
                        eprintln!(
                            "RECOVERED damaged history {} from {}; evidence {}; receipt {}",
                            path.display(),
                            recovery.backup_path.display(),
                            recovery.damaged_evidence_path.display(),
                            recovery.receipt_path.display()
                        );
                    }
                    decode_document(&outcome.bytes)?
                }
            },
            None => HistoryState::default(),
        };
        let log_path = log_path_for(&path);
        let (good, entries, torn) = replay_log(&log_path, &mut state)?;
        if torn {
            let file = std::fs::OpenOptions::new().write(true).open(&log_path)?;
            file.set_len(good)?;
            file.sync_all()?;
        }
        state.log.len = good;
        state.log.entries = entries;
        Ok(Self {
            path,
            log_path,
            state: Mutex::new(state),
            deferring: Mutex::new(HashSet::new()),
            compact_after: LOG_COMPACT_AFTER_ENTRIES,
        })
    }

    /// Fold the log after `entries` instead of [`LOG_COMPACT_AFTER_ENTRIES`],
    /// so a test crosses the bound without thousands of synced appends.
    #[cfg(test)]
    pub(crate) fn with_compact_after(mut self, entries: usize) -> Self {
        self.compact_after = entries;
        self
    }

    /// Append one immutable version. Returns `false` for an exact replay and
    /// rejects an id reused with different bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when validation, ancestry, locking, or persistence
    /// fails, or the version id is already bound to different content.
    pub fn append(&self, version: FileVersionV1) -> Result<bool, HistoryError> {
        let versions = [version];
        let appended = self.append_batch(&versions)?;
        Ok(appended.first().copied().unwrap_or(false))
    }

    /// Atomically append versions which may belong to different records.
    ///
    /// This is the history-side commit primitive for operations such as a
    /// POSIX replacement rename, where moving the source and deleting the
    /// destination are one logical mutation. Every item is validated against
    /// copies of the records it touches (in input order), the accepted items
    /// are written as one durable log entry, and only then are they published
    /// in memory. A failure therefore exposes neither half of a multi-record
    /// history transition. Exact replays are reported as `false` per item.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid versions, missing/disconnected parents,
    /// id collisions, poisoned state, or persistence failure.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the writer lock intentionally spans candidate validation, durable replacement, and in-memory publication"
    )]
    pub fn append_batch(&self, versions: &[FileVersionV1]) -> Result<Vec<bool>, HistoryError> {
        self.append_lease()?.append_batch(versions)
    }

    /// Persist an open or already-resolved conflict. Conflict records are
    /// immutable except for the explicit [`resolve`](Self::resolve) transition.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid/unknown conflict, an id collision,
    /// poisoned state, or persistence failure.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the writer lock intentionally spans conflict validation, durable replacement, and in-memory publication"
    )]
    pub fn record_conflict(&self, conflict: FileConflictV1) -> Result<bool, HistoryError> {
        self.append_lease()?.record_conflict(conflict)
    }

    /// Resolve an open conflict. The result must already be a durable
    /// `ConflictResolution` version and its actor must match the resolution.
    /// Repeating the same resolution is idempotent; a different one is refused.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown or invalid conflict/version state,
    /// mismatched resolution provenance/actor/time, poisoned state, or I/O.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the writer lock intentionally spans resolution validation, durable replacement, and in-memory publication"
    )]
    pub fn resolve(
        &self,
        record_id: FileRecordId,
        conflict_id: Uuid,
        resolution: ConflictResolution,
    ) -> Result<bool, HistoryError> {
        self.append_lease()?
            .resolve(record_id, conflict_id, resolution)
    }

    pub fn snapshot(&self, record_id: FileRecordId) -> Option<RecordHistorySnapshot> {
        let current = self.state.lock().ok()?;
        let record = current.records.get(&record_id)?;
        let result = snapshot(record_id, record);
        drop(current);
        Some(result)
    }

    /// Content roots retained by the durable immutable history.  A home peer
    /// uses these to keep historical versions recoverable while reclaiming
    /// only blobs proven unreachable from both the live namespace and history.
    pub fn retained_content_hashes(&self) -> Vec<collab_types::ContentHash> {
        let Ok(current) = self.state.lock() else {
            return Vec::new();
        };
        let mut hashes = current
            .records
            .values()
            .flat_map(|record| record.versions.values())
            .filter_map(|version| version.content_hash)
            .collect::<Vec<_>>();
        hashes.sort_unstable_by_key(|hash| hash.0);
        hashes.dedup();
        hashes
    }

    pub fn projection(&self, record_id: FileRecordId) -> Option<HistoryProjectionV1> {
        let snapshot = self.snapshot(record_id)?;
        let total_versions = snapshot.versions.len() as u64;
        let versions_start = snapshot
            .versions
            .len()
            .saturating_sub(MAX_PROJECTED_VERSIONS);
        let conflicts_start = snapshot
            .conflicts
            .len()
            .saturating_sub(MAX_PROJECTED_CONFLICTS);
        Some(HistoryProjectionV1 {
            schema_version: HISTORY_PROJECTION_VERSION,
            record_id,
            total_versions,
            versions_truncated: versions_start > 0,
            heads: snapshot.heads,
            versions: snapshot
                .versions
                .get(versions_start..)
                .unwrap_or_default()
                .to_vec(),
            conflicts_truncated: conflicts_start > 0,
            conflicts: snapshot
                .conflicts
                .get(conflicts_start..)
                .unwrap_or_default()
                .to_vec(),
        })
    }
}

fn snapshot(record_id: FileRecordId, record: &RecordState) -> RecordHistorySnapshot {
    let mut versions: Vec<_> = record.versions.values().cloned().collect();
    versions.sort_by_key(|version| (version.timestamp_ms, version.id));
    RecordHistorySnapshot {
        record_id,
        versions,
        heads: record.heads.iter().copied().collect(),
        conflicts: record.conflicts.values().cloned().collect(),
    }
}

fn validate_conflict(record: &RecordState, conflict: &FileConflictV1) -> Result<(), HistoryError> {
    for version in &conflict.versions {
        if !record.versions.contains_key(version) {
            return Err(HistoryError::UnknownConflictVersion(*version));
        }
    }
    if let Some(resolution) = &conflict.resolution {
        let result = record
            .versions
            .get(&resolution.result)
            .ok_or(HistoryError::UnknownResolutionVersion(resolution.result))?;
        if result.provenance != FileProvenance::ConflictResolution {
            return Err(HistoryError::ResolutionProvenance);
        }
        if result.actor != resolution.actor {
            return Err(HistoryError::ResolutionActorMismatch);
        }
        if result.timestamp_ms != resolution.timestamp_ms {
            return Err(HistoryError::ResolutionTimestampMismatch);
        }
    }
    Ok(())
}

fn decode_document(bytes: &[u8]) -> Result<HistoryState, HistoryError> {
    let document: HistoryDocument = serde_json::from_slice(bytes)?;
    if document.schema_version != HISTORY_SCHEMA_VERSION {
        return Err(HistoryError::UnsupportedVersion(document.schema_version));
    }
    let mut state = HistoryState::default();
    for document_record in document.records {
        if state.records.contains_key(&document_record.record_id) {
            return Err(HistoryError::DuplicateRecord(document_record.record_id));
        }
        let mut record = RecordState::default();
        for version in document_record.versions {
            let version_id = version.id;
            version.validate()?;
            if version.record_id != document_record.record_id {
                return Err(HistoryError::WrongRecord);
            }
            if state
                .version_owner
                .insert(version_id, document_record.record_id)
                .is_some()
            {
                return Err(HistoryError::VersionIdCollision(version_id));
            }
            if record.versions.insert(version_id, version).is_some() {
                return Err(HistoryError::VersionIdCollision(version_id));
            }
        }
        validate_graph(&record.versions)?;
        let expected_heads = computed_heads(&record.versions);
        let stored_heads: BTreeSet<_> = document_record.heads.into_iter().collect();
        if stored_heads != expected_heads {
            return Err(HistoryError::InvalidHeads);
        }
        record.heads = expected_heads;
        for conflict in document_record.conflicts {
            let conflict_id = conflict.id;
            conflict.validate()?;
            if conflict.record_id != document_record.record_id {
                return Err(HistoryError::WrongRecord);
            }
            validate_conflict(&record, &conflict)?;
            if record.conflicts.insert(conflict_id, conflict).is_some() {
                return Err(HistoryError::ConflictIdCollision(conflict_id));
            }
        }
        state.records.insert(document_record.record_id, record);
    }
    Ok(state)
}

fn visit_version(
    id: FileVersionId,
    versions: &BTreeMap<FileVersionId, FileVersionV1>,
    visiting: &mut BTreeSet<FileVersionId>,
    visited: &mut BTreeSet<FileVersionId>,
) -> Result<(), HistoryError> {
    if visited.contains(&id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(HistoryError::Cycle(id));
    }
    let version = versions.get(&id).ok_or(HistoryError::MissingParent(id))?;
    for parent in &version.parents {
        visit_version(*parent, versions, visiting, visited)?;
    }
    visiting.remove(&id);
    visited.insert(id);
    Ok(())
}

fn validate_graph(versions: &BTreeMap<FileVersionId, FileVersionV1>) -> Result<(), HistoryError> {
    for version in versions.values() {
        for parent in &version.parents {
            let parent_version = versions
                .get(parent)
                .ok_or(HistoryError::MissingParent(*parent))?;
            if parent_version.record_id != version.record_id {
                return Err(HistoryError::WrongRecord);
            }
        }
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for id in versions.keys().copied() {
        visit_version(id, versions, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn computed_heads(versions: &BTreeMap<FileVersionId, FileVersionV1>) -> BTreeSet<FileVersionId> {
    let mut heads: BTreeSet<_> = versions.keys().copied().collect();
    for version in versions.values() {
        for parent in &version.parents {
            heads.remove(parent);
        }
    }
    heads
}

/// The log lives beside the snapshot: `history.json` → `history.json.log`.
fn log_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".log");
    path.with_file_name(name)
}

fn persist(path: &Path, state: &HistoryState) -> Result<(), HistoryError> {
    let document = HistoryDocument {
        schema_version: HISTORY_SCHEMA_VERSION,
        records: state
            .records
            .iter()
            .map(|(&record_id, record)| {
                let snapshot = snapshot(record_id, record);
                RecordDocument {
                    record_id,
                    versions: snapshot.versions,
                    heads: snapshot.heads,
                    conflicts: snapshot.conflicts,
                }
            })
            .collect(),
    };
    let bytes = serde_json::to_vec_pretty(&document)?;
    crate::durable_backup::write(path, &bytes, |candidate| decode_document(candidate).is_ok())?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid history JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid file record: {0}")]
    Record(#[from] FileRecordError),
    #[error("unsupported history schema version {0}")]
    UnsupportedVersion(u16),
    #[error("duplicate record {0:?}")]
    DuplicateRecord(FileRecordId),
    #[error("version id {0:?} is already bound to different content")]
    VersionIdCollision(FileVersionId),
    #[error("conflict id {0} is already bound to different content")]
    ConflictIdCollision(Uuid),
    #[error("version history references missing parent {0:?}")]
    MissingParent(FileVersionId),
    #[error("non-genesis version {0:?} has no parent")]
    DisconnectedVersion(FileVersionId),
    #[error("version ancestry contains a cycle at {0:?}")]
    Cycle(FileVersionId),
    #[error("stored history heads do not match the immutable graph")]
    InvalidHeads,
    #[error("record id does not match its containing history")]
    WrongRecord,
    #[error("record {0:?} has no history")]
    UnknownRecord(FileRecordId),
    #[error("conflict {0} does not exist")]
    UnknownConflict(Uuid),
    #[error("conflict references unknown version {0:?}")]
    UnknownConflictVersion(FileVersionId),
    #[error("resolution references unknown result version {0:?}")]
    UnknownResolutionVersion(FileVersionId),
    #[error("a conflict-resolution result must have ConflictResolution provenance")]
    ResolutionProvenance,
    #[error("resolution actor does not match the result version actor")]
    ResolutionActorMismatch,
    #[error("resolution timestamp does not match the result version timestamp")]
    ResolutionTimestampMismatch,
    #[error("conflict {0} was already resolved differently")]
    AlreadyResolved(Uuid),
    #[error("history projection is too large ({0} bytes)")]
    ProjectionTooLarge(usize),
    #[error("unsupported history projection version {0}")]
    UnsupportedProjectionVersion(u16),
    #[error("history projection is internally inconsistent")]
    InvalidProjection,
    #[error("signed record publication failed: {0}")]
    Publication(String),
    #[error("record cannot take another signed version: {0}")]
    CapacityExhausted(String),
    #[error("history log is damaged: {0}")]
    DamagedLog(String),
    #[error("history state lock is poisoned")]
    StatePoisoned,
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{
        ConflictStrategy, ContentHash, Ed25519PublicKey, FileActor, FileProvenance,
    };

    fn actor(byte: u8) -> FileActor {
        FileActor {
            person: Ed25519PublicKey([byte; 32]),
            device: Ed25519PublicKey([byte.wrapping_add(1); 32]),
        }
    }

    fn version(
        record_id: FileRecordId,
        parents: Vec<FileVersionId>,
        provenance: FileProvenance,
        timestamp_ms: u64,
    ) -> FileVersionV1 {
        FileVersionV1 {
            id: FileVersionId::new(),
            record_id,
            parents,
            actor: actor(1),
            timestamp_ms,
            content_hash: Some(ContentHash([timestamp_ms.to_le_bytes()[0]; 32])),
            metadata_hash: ContentHash([9; 32]),
            provenance,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn history_appends_and_batches_are_exact_atomic_and_durable() {
        // Phase `deferred_appends_are_one_log_line_written_when_the_batch_finishes`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.json");
            let record = FileRecordId::new();
            let root = version(record, vec![], FileProvenance::Create, 1);
            let edit = version(record, vec![root.id], FileProvenance::Edit, 2);
            let store = FileHistoryStore::open(&path).unwrap();
            let log = log_path_for(&path);

            let batch = store.defer_durability();
            store.append(root).unwrap();
            store.append(edit.clone()).unwrap();
            // Visible at once, on disk only when the batch finishes.
            assert_eq!(store.snapshot(record).unwrap().versions.len(), 2);
            assert!(std::fs::read(&log).unwrap_or_default().is_empty());
            // Another thread's append is durable when it returns: it writes the
            // deferred ones first, so the log keeps the order of memory.
            let third = version(record, vec![edit.id], FileProvenance::Edit, 3);
            std::thread::scope(|scope| {
                scope.spawn(|| store.append(third.clone()).unwrap());
            });
            assert_eq!(store.pending_log_entries(), 2);
            batch.finish().unwrap();
            drop(store);
            assert_eq!(
                FileHistoryStore::open(&path)
                    .unwrap()
                    .snapshot(record)
                    .unwrap()
                    .versions
                    .len(),
                3
            );

            // A batch that never finishes (a crash) leaves nothing behind.
            let other = FileRecordId::new();
            let store = FileHistoryStore::open(&path).unwrap();
            let batch = store.defer_durability();
            store
                .append(version(other, vec![], FileProvenance::Create, 4))
                .unwrap();
            std::mem::forget(batch);
            let on_disk = std::fs::read(&log).unwrap();
            drop(store);
            assert_eq!(std::fs::read(&log).unwrap(), on_disk);
            assert!(
                FileHistoryStore::open(&path)
                    .unwrap()
                    .snapshot(other)
                    .is_none()
            );
        }
        // Phase `append_branches_and_restart_preserve_exact_heads`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.json");
            let store = FileHistoryStore::open(&path).unwrap();
            let record = FileRecordId::new();
            let root = version(record, vec![], FileProvenance::Create, 1);
            assert!(store.append(root.clone()).unwrap());
            assert!(!store.append(root.clone()).unwrap(), "exact replay");
            let left = version(record, vec![root.id], FileProvenance::Edit, 2);
            let right = version(record, vec![root.id], FileProvenance::Edit, 3);
            store.append(left.clone()).unwrap();
            store.append(right.clone()).unwrap();
            drop(store);

            let reopened = FileHistoryStore::open(path).unwrap();
            let snapshot = reopened.snapshot(record).unwrap();
            assert_eq!(snapshot.versions, vec![root, left.clone(), right.clone()]);
            let mut expected_heads = vec![left.id, right.id];
            expected_heads.sort();
            assert_eq!(snapshot.heads, expected_heads);
        }
        // Phase `missing_parent_and_global_id_collision_leave_disk_unchanged`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.json");
            let store = FileHistoryStore::open(&path).unwrap();
            let record = FileRecordId::new();
            let root = version(record, vec![], FileProvenance::Create, 1);
            store.append(root.clone()).unwrap();
            let before = disk_state(&path);
            let orphan = version(record, vec![FileVersionId::new()], FileProvenance::Edit, 2);
            assert!(matches!(
                store.append(orphan),
                Err(HistoryError::MissingParent(_))
            ));
            let mut collision = version(FileRecordId::new(), vec![], FileProvenance::Create, 1);
            collision.id = root.id;
            assert!(matches!(
                store.append(collision),
                Err(HistoryError::VersionIdCollision(_))
            ));
            assert_eq!(disk_state(&path), before);
        }
        // Phase `multi_record_batch_is_all_or_nothing_and_survives_restart`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.json");
            let store = FileHistoryStore::open(&path).unwrap();
            let source_record = FileRecordId::new();
            let destination_record = FileRecordId::new();
            let source_root = version(source_record, vec![], FileProvenance::Create, 1);
            let destination_root = version(destination_record, vec![], FileProvenance::Create, 1);
            assert_eq!(
                store
                    .append_batch(&[source_root.clone(), destination_root.clone()])
                    .unwrap(),
                vec![true, true]
            );

            let source_move = version(
                source_record,
                vec![source_root.id],
                FileProvenance::Rename,
                2,
            );
            let mut destination_delete = version(
                destination_record,
                vec![destination_root.id],
                FileProvenance::Delete,
                2,
            );
            destination_delete.parents = vec![FileVersionId::new()];
            let before = disk_state(&path);
            assert!(matches!(
                store.append_batch(&[source_move.clone(), destination_delete]),
                Err(HistoryError::MissingParent(_))
            ));
            assert_eq!(disk_state(&path), before);
            assert_eq!(store.snapshot(source_record).unwrap().versions.len(), 1);
            assert_eq!(
                store.snapshot(destination_record).unwrap().versions.len(),
                1
            );

            let destination_delete = version(
                destination_record,
                vec![destination_root.id],
                FileProvenance::Delete,
                2,
            );
            assert_eq!(
                store
                    .append_batch(&[source_move.clone(), destination_delete.clone()])
                    .unwrap(),
                vec![true, true]
            );
            drop(store);

            let reopened = FileHistoryStore::open(path).unwrap();
            assert_eq!(
                reopened.snapshot(source_record).unwrap().versions,
                vec![source_root, source_move]
            );
            assert_eq!(
                reopened.snapshot(destination_record).unwrap().versions,
                vec![destination_root, destination_delete]
            );
        }
        // Phase `conflict_resolution_is_first_class_idempotent_and_durable`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.json");
            let store = FileHistoryStore::open(&path).unwrap();
            let record = FileRecordId::new();
            let root = version(record, vec![], FileProvenance::Create, 1);
            let left = version(record, vec![root.id], FileProvenance::Edit, 2);
            let right = version(record, vec![root.id], FileProvenance::Edit, 3);
            for item in [root, left.clone(), right.clone()] {
                store.append(item).unwrap();
            }
            let conflict_id = Uuid::new_v4();
            let conflict = FileConflictV1 {
                id: conflict_id,
                record_id: record,
                versions: vec![left.id, right.id],
                status: ConflictStatus::Open,
                resolution: None,
            };
            assert!(store.record_conflict(conflict.clone()).unwrap());
            assert!(!store.record_conflict(conflict).unwrap());

            let result = version(
                record,
                vec![left.id, right.id],
                FileProvenance::ConflictResolution,
                4,
            );
            store.append(result.clone()).unwrap();
            let resolution = ConflictResolution {
                strategy: ConflictStrategy::KeepCurrent,
                result: result.id,
                actor: result.actor,
                timestamp_ms: result.timestamp_ms,
            };
            assert!(
                store
                    .resolve(record, conflict_id, resolution.clone())
                    .unwrap()
            );
            assert!(!store.resolve(record, conflict_id, resolution).unwrap());
            drop(store);

            let reopened = FileHistoryStore::open(path).unwrap();
            let conflict = &reopened.snapshot(record).unwrap().conflicts[0];
            assert_eq!(conflict.status, ConflictStatus::Resolved);
            assert_eq!(conflict.resolution.as_ref().unwrap().result, result.id);
        }
    }

    /// Everything the store keeps on disk: snapshot and log bytes.
    fn disk_state(path: &Path) -> (Vec<u8>, Vec<u8>) {
        (
            std::fs::read(path).unwrap_or_default(),
            std::fs::read(log_path_for(path)).unwrap_or_default(),
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn history_logs_read_legacy_and_fail_closed_on_damage() {
        // Phase `a_whole_file_history_from_earlier_releases_is_read_and_extended`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.json");
            let record = FileRecordId::new();
            let root = version(record, vec![], FileProvenance::Create, 1);
            let edit = version(record, vec![root.id], FileProvenance::Edit, 2);
            // Earlier releases rewrote this one document on every append.
            let mut legacy = HistoryState::default();
            let candidate = stage_versions(&legacy, &[root.clone(), edit.clone()]).unwrap();
            install_versions(&mut legacy, candidate);
            persist(&path, &legacy).unwrap();
            let legacy_bytes = std::fs::read(&path).unwrap();

            let store = FileHistoryStore::open(&path).unwrap();
            assert_eq!(
                store.snapshot(record).unwrap().versions,
                vec![root.clone(), edit.clone()]
            );
            let next = version(record, vec![edit.id], FileProvenance::Edit, 3);
            assert!(store.append(next.clone()).unwrap());
            assert!(!store.append(root.clone()).unwrap(), "exact replay");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                legacy_bytes,
                "an append writes only the log"
            );
            assert_eq!(store.pending_log_entries(), 1);
            drop(store);

            let reopened = FileHistoryStore::open(&path).unwrap();
            assert_eq!(
                reopened.snapshot(record).unwrap().versions,
                vec![root.clone(), edit.clone(), next.clone()]
            );
            reopened.compact().unwrap();
            assert_eq!(reopened.pending_log_entries(), 0);
            assert!(disk_state(&path).1.is_empty());
            drop(reopened);
            let compacted = FileHistoryStore::open(&path).unwrap();
            assert_eq!(
                compacted.snapshot(record).unwrap().versions,
                vec![root, edit, next]
            );
        }
        // Phase `a_torn_final_log_line_is_dropped_and_damage_before_it_fails_closed`.
        {
            use std::io::Write as _;

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.json");
            let record = FileRecordId::new();
            let root = version(record, vec![], FileProvenance::Create, 1);
            let edit = version(record, vec![root.id], FileProvenance::Edit, 2);
            let store = FileHistoryStore::open(&path).unwrap();
            store.append(root).unwrap();
            let edit_id = edit.id;
            store.append(edit).unwrap();
            drop(store);
            let log = log_path_for(&path);
            let intact = std::fs::read(&log).unwrap();

            // A write that never returned: half a line, no newline.
            let mut file = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
            file.write_all(b"0123456789abcdef {\"op\":\"vers").unwrap();
            drop(file);
            let store = FileHistoryStore::open(&path).unwrap();
            assert_eq!(store.snapshot(record).unwrap().versions.len(), 2);
            assert_eq!(std::fs::read(&log).unwrap(), intact, "torn tail removed");
            let later = version(record, vec![edit_id], FileProvenance::Edit, 3);
            store.append(later).unwrap();
            drop(store);
            assert_eq!(
                FileHistoryStore::open(&path)
                    .unwrap()
                    .snapshot(record)
                    .unwrap()
                    .versions
                    .len(),
                3
            );

            // Damage to an acknowledged entry is never silently skipped.
            let mut damaged = std::fs::read(&log).unwrap();
            damaged[70] ^= 0x01;
            std::fs::write(&log, &damaged).unwrap();
            assert!(matches!(
                FileHistoryStore::open(&path),
                Err(HistoryError::DamagedLog(_))
            ));
            assert_eq!(std::fs::read(&log).unwrap(), damaged, "evidence kept");
        }
        // Phase `corrupt_heads_are_rejected_on_open`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.json");
            let store = FileHistoryStore::open(&path).unwrap();
            let record = FileRecordId::new();
            store
                .append(version(record, vec![], FileProvenance::Create, 1))
                .unwrap();
            store.compact().unwrap();
            drop(store);
            let mut document: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            document["records"][0]["heads"] = serde_json::json!([]);
            std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
            assert!(matches!(
                FileHistoryStore::open(path),
                Err(HistoryError::InvalidHeads)
            ));
        }
        // Phase `xattr_projection_is_bounded_recent_and_round_trips`.
        {
            let dir = tempfile::tempdir().unwrap();
            let store = FileHistoryStore::open(dir.path().join("history.json")).unwrap();
            let record = FileRecordId::new();
            let mut parent = None;
            for timestamp in 0..20 {
                let item = version(
                    record,
                    parent.into_iter().collect(),
                    if timestamp == 0 {
                        FileProvenance::Create
                    } else {
                        FileProvenance::Edit
                    },
                    timestamp,
                );
                parent = Some(item.id);
                store.append(item).unwrap();
            }
            let projection = store.projection(record).unwrap();
            assert_eq!(projection.total_versions, 20);
            assert!(projection.versions_truncated);
            assert_eq!(projection.versions.len(), MAX_PROJECTED_VERSIONS);
            assert_eq!(projection.versions.first().unwrap().timestamp_ms, 8);
            let bytes = projection.to_xattr_bytes().unwrap();
            assert!(bytes.len() <= MAX_HISTORY_PROJECTION_BYTES);
            assert_eq!(
                HistoryProjectionV1::parse_xattr(&bytes).unwrap(),
                projection
            );
        }
    }

    #[test]
    fn the_log_folds_into_the_snapshot_after_its_bound() {
        const BOUND: usize = 64;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let store = FileHistoryStore::open(&path)
            .unwrap()
            .with_compact_after(BOUND);
        let record = FileRecordId::new();
        let mut parent = version(record, vec![], FileProvenance::Create, 0);
        store.append(parent.clone()).unwrap();
        for i in 1..BOUND {
            assert_eq!(
                store.pending_log_entries(),
                i,
                "the log is not folded before its bound"
            );
            let next = version(record, vec![parent.id], FileProvenance::Edit, i as u64);
            store.append(next.clone()).unwrap();
            parent = next;
        }
        assert_eq!(store.pending_log_entries(), 0, "compacted at the bound");
        assert!(disk_state(&path).1.is_empty());
        drop(store);
        let reopened = FileHistoryStore::open(&path).unwrap();
        assert_eq!(reopened.snapshot(record).unwrap().versions.len(), BOUND);
    }
}
