//! Immutable file history and first-class conflict records.
//!
//! History is keyed by [`FileRecordId`], never by path, and is a derived query
//! index over the signed record operation log in
//! [`crate::record_repository`]. It owns no durable authority of its own: the
//! signed `AppendVersion`, `AppendConflict` and `ResolveConflict` operations
//! are the history, and every accepted projection is folded into this index.
//! The index is rebuilt from the repository's verified projections on open;
//! it keeps no persistent copy of its own. Both rebuild and fold validate
//! ancestry, immutable identity and conflict references, so corrupt or
//! cross-record projections cannot become an eviction or restore root.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fs_protocol::projection::{
    HISTORY_PROJECTION_VERSION, MAX_PROJECTED_CONFLICTS, MAX_PROJECTED_VERSIONS,
};
use fs_protocol::{
    ConflictResolution, ConflictStatus, FileConflictV1, FileProvenance, FileRecordError,
    FileRecordId, FileVersionId, FileVersionV1, HistoryProjectionV1,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

mod read_query;

const HISTORY_SCHEMA_VERSION: u16 = 1;
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

#[derive(Clone, Debug, Default)]
struct HistoryState {
    records: BTreeMap<FileRecordId, RecordState>,
    /// The record owning each version id; ids are unique workspace-wide, so
    /// collision checks are a lookup instead of a scan of every record.
    version_owner: HashMap<FileVersionId, FileRecordId>,
}

/// One entry in a legacy `history.json.log`, read only for import. Every
/// entry is validated with the same staging rules as a live fold; the file is
/// never appended to, truncated or compacted.
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

impl RecordHistorySnapshot {
    /// The existing bounded metadata projection of this exact immutable snapshot.
    pub(crate) fn projection(self) -> HistoryProjectionV1 {
        let total_versions = self.versions.len() as u64;
        let versions_start = self.versions.len().saturating_sub(MAX_PROJECTED_VERSIONS);
        let conflicts_start = self.conflicts.len().saturating_sub(MAX_PROJECTED_CONFLICTS);
        HistoryProjectionV1 {
            schema_version: HISTORY_PROJECTION_VERSION,
            record_id: self.record_id,
            total_versions,
            versions_truncated: versions_start > 0,
            heads: self.heads,
            versions: self
                .versions
                .get(versions_start..)
                .unwrap_or_default()
                .to_vec(),
            conflicts_truncated: conflicts_start > 0,
            conflicts: self
                .conflicts
                .get(conflicts_start..)
                .unwrap_or_default()
                .to_vec(),
        }
    }
}

/// The derived, rebuildable history query index for one workspace.
///
/// The signed record operation log is the durable source. A legacy
/// `history.json` (and its `.log`) from an earlier release is read once as a
/// read-only import for records the signed repository does not know; it is
/// never written, compacted or treated as authority. The index has no
/// persistent cache: a cache from an earlier release is neither read nor
/// written, so it can never pin or replace accepted history.
pub struct FileHistoryStore {
    /// The legacy read-only document path, also the import anchor.
    legacy_path: Option<PathBuf>,
    state: Mutex<HistoryState>,
    /// Set once this index was replaced by a complete repository fold.
    derived: Mutex<bool>,
}

/// Existing history writer lock borrowed by a compound admission/projection.
pub(crate) struct HistoryLease<'a> {
    state: std::sync::MutexGuard<'a, HistoryState>,
}

pub(crate) struct HistoryCandidate {
    touched: BTreeMap<FileRecordId, RecordAdditions>,
    owners: Vec<(FileVersionId, FileRecordId)>,
    appended: Vec<bool>,
}

impl HistoryLease<'_> {
    pub fn resolve(
        &mut self,
        record_id: FileRecordId,
        conflict_id: Uuid,
        resolution: &ConflictResolution,
    ) -> Result<bool, HistoryError> {
        let Some(resolved) = stage_resolution(&self.state, record_id, conflict_id, resolution)?
        else {
            return Ok(false);
        };
        install_conflict(&mut self.state, resolved);
        Ok(true)
    }

    pub fn record_conflict(&mut self, conflict: FileConflictV1) -> Result<bool, HistoryError> {
        if !stage_conflict(&self.state, &conflict)? {
            return Ok(false);
        }
        install_conflict(&mut self.state, conflict);
        Ok(true)
    }

    pub fn prepare_versions(
        &self,
        versions: &[FileVersionV1],
    ) -> Result<HistoryCandidate, HistoryError> {
        stage_versions(&self.state, versions)
    }

    pub fn install(&mut self, candidate: HistoryCandidate) -> Vec<bool> {
        install_versions(&mut self.state, candidate)
    }

    pub fn append_batch(&mut self, versions: &[FileVersionV1]) -> Result<Vec<bool>, HistoryError> {
        let candidate = self.prepare_versions(versions)?;
        Ok(self.install(candidate))
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
        appended.push(true);
    }
    Ok(HistoryCandidate {
        touched,
        owners: owners.into_iter().collect(),
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
            || !fs_protocol::prefer_conflict_resolution(resolution, existing))
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

#[cfg(test)]
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

/// Fold the read-only legacy log at `log_path` into `state`. A torn final
/// line (a write that never returned) is ignored; damage to any acknowledged
/// entry fails closed. This function never truncates or rewrites the legacy
/// file: the signed record log is the only durable history now.
fn replay_legacy_log(log_path: &Path, state: &mut HistoryState) -> Result<(), HistoryError> {
    let bytes = match std::fs::read(log_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut good = 0_usize;
    let mut rest = bytes.as_slice();
    while let Some(newline) = rest.iter().position(|byte| *byte == b'\n') {
        let (line, tail) = rest.split_at(newline);
        let tail = tail.get(1..).unwrap_or_default();
        match decode_log_line(line) {
            Ok(entry) => replay_entry(state, entry)?,
            Err(_) if tail.is_empty() => break,
            Err(error) => {
                return Err(HistoryError::DamagedLog(format!(
                    "{} at byte {good}: {error}",
                    log_path.display()
                )));
            }
        }
        good = good.saturating_add(newline).saturating_add(1);
        rest = tail;
    }
    Ok(())
}

impl FileHistoryStore {
    /// The writer lock for a compound admission or projection. Its folds are
    /// published in memory when they return; the signed repository already
    /// holds their durable form.
    pub(crate) fn lease(&self) -> Result<HistoryLease<'_>, HistoryError> {
        Ok(HistoryLease {
            state: self.state.lock().map_err(|_| HistoryError::StatePoisoned)?,
        })
    }

    fn append_lease(&self) -> Result<HistoryLease<'_>, HistoryError> {
        self.lease()
    }

    /// Open the derived index anchored at `path`.
    ///
    /// A `history.json` (and `history.json.log`) written by an earlier
    /// release is imported read-only for records the signed record repository
    /// does not know. Nothing is written back to it. No persistent cache is
    /// read: a checkpoint from an earlier release cannot pin or replace the
    /// repository's accepted history.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, HistoryError> {
        let legacy_path = path.into();
        let state = import_legacy(&legacy_path)?;
        Ok(Self {
            legacy_path: Some(legacy_path),
            state: Mutex::new(state),
            derived: Mutex::new(false),
        })
    }

    /// A derived index with no legacy import and no persistent cache.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            legacy_path: None,
            state: Mutex::new(HistoryState::default()),
            derived: Mutex::new(false),
        }
    }

    /// Replace this index with the complete history of `workspace_id` as the
    /// signed repository projects it. The repository is the only source: the
    /// fold takes one repository lock, clones each record-bound projection
    /// once and validates it like a live fold. Repeat calls are no-ops: later
    /// folds keep the index current.
    ///
    /// # Errors
    ///
    /// Returns an error when a repository projection is unavailable or the
    /// derived history violates ancestry, identity or conflict validation.
    pub(crate) fn rebuild_from_repository(
        &self,
        repository: &crate::record_repository::RecordRepository,
        workspace_id: Uuid,
    ) -> Result<(), HistoryError> {
        {
            let derived = self
                .derived
                .lock()
                .map_err(|_| HistoryError::StatePoisoned)?;
            if *derived {
                return Ok(());
            }
        }
        let state = self.build_from_repository(repository, workspace_id)?;
        *self.state.lock().map_err(|_| HistoryError::StatePoisoned)? = state;
        *self
            .derived
            .lock()
            .map_err(|_| HistoryError::StatePoisoned)? = true;
        Ok(())
    }

    fn build_from_repository(
        &self,
        repository: &crate::record_repository::RecordRepository,
        workspace_id: Uuid,
    ) -> Result<HistoryState, HistoryError> {
        let mut state = HistoryState::default();
        let mut known = BTreeSet::new();
        for (key, snapshot) in repository.history_projections(workspace_id)? {
            install_projection(&mut state, key.record_id, &snapshot)?;
            known.insert(key.record_id);
        }
        if let Some(path) = &self.legacy_path {
            let legacy = import_legacy(path)?;
            for (record_id, record) in legacy.records {
                if known.contains(&record_id) {
                    continue;
                }
                merge_record(&mut state, record_id, record)?;
            }
        }
        Ok(state)
    }

    /// Fold one immutable version into the derived index. Returns `false` for
    /// an exact replay and rejects an id reused with different bytes.
    pub fn append(&self, version: FileVersionV1) -> Result<bool, HistoryError> {
        let versions = [version];
        let appended = self.append_batch(&versions)?;
        Ok(appended.first().copied().unwrap_or(false))
    }

    /// Atomically fold versions which may belong to different records.
    ///
    /// This is the history-side commit primitive for operations such as a
    /// POSIX replacement rename, where moving the source and deleting the
    /// destination are one logical mutation. Every item is validated against
    /// copies of the records it touches (in input order), and only then are
    /// the accepted items published in the derived index. The signed record
    /// repository is the durable authority; a failure here exposes neither
    /// half of a multi-record history transition and is retried by the
    /// repository fold. Exact replays are reported as `false` per item.
    pub fn append_batch(&self, versions: &[FileVersionV1]) -> Result<Vec<bool>, HistoryError> {
        self.append_lease()?.append_batch(versions)
    }

    /// Fold an open or already-resolved conflict. Conflict records are
    /// immutable except for the explicit [`resolve`](Self::resolve) transition.
    pub fn record_conflict(&self, conflict: FileConflictV1) -> Result<bool, HistoryError> {
        self.append_lease()?.record_conflict(conflict)
    }

    /// Fold a conflict resolution. The result must already be a signed
    /// `ConflictResolution` version and its actor must match the resolution.
    /// Repeating the same resolution is idempotent; a different one is refused.
    pub fn resolve(
        &self,
        record_id: FileRecordId,
        conflict_id: Uuid,
        resolution: &ConflictResolution,
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

    pub fn projection(&self, record_id: FileRecordId) -> Option<HistoryProjectionV1> {
        self.snapshot(record_id)
            .map(RecordHistorySnapshot::projection)
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
    document_state(serde_json::from_slice(bytes)?)
}

fn document_state(document: HistoryDocument) -> Result<HistoryState, HistoryError> {
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

/// The legacy log lies beside its document: `history.json` → `history.json.log`.
fn log_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".log");
    path.with_file_name(name)
}

/// Import any legacy document and log read-only. A damaged primary is
/// recovered from the newest retained generation that passes the same
/// document validation, with the damaged bytes preserved as evidence. A
/// missing document remains empty, and damage with no valid generation keeps
/// the primary untouched and fails closed.
fn import_legacy(path: &Path) -> Result<HistoryState, HistoryError> {
    let mut state = match crate::durable_backup::read_primary(path)? {
        Some(bytes) => match decode_document(&bytes) {
            Ok(state) => state,
            Err(primary_error) => {
                let outcome = match crate::durable_backup::read_or_recover(path, |bytes| {
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
    replay_legacy_log(&log_path_for(path), &mut state)?;
    Ok(state)
}

/// Install one verified repository projection, validating it as if it had
/// arrived through the signed fold: unique immutable ids workspace-wide, a
/// complete acyclic parent graph, exact heads and valid conflict references.
fn install_projection(
    state: &mut HistoryState,
    record_id: FileRecordId,
    projection: &crate::record_replication::RecordSnapshot,
) -> Result<(), HistoryError> {
    if projection.record_id != record_id {
        return Err(HistoryError::WrongRecord);
    }
    if state.records.contains_key(&record_id) {
        return Err(HistoryError::DuplicateRecord(record_id));
    }
    let mut record = RecordState::default();
    for version in &projection.versions {
        version.validate()?;
        if version.record_id != record_id {
            return Err(HistoryError::WrongRecord);
        }
        if state.version_owner.insert(version.id, record_id).is_some() {
            return Err(HistoryError::VersionIdCollision(version.id));
        }
        if record
            .versions
            .insert(version.id, version.clone())
            .is_some()
        {
            return Err(HistoryError::VersionIdCollision(version.id));
        }
    }
    validate_graph(&record.versions)?;
    record.heads = computed_heads(&record.versions);
    for conflict in &projection.conflicts {
        conflict.validate()?;
        if conflict.record_id != record_id {
            return Err(HistoryError::WrongRecord);
        }
        validate_conflict(&record, conflict)?;
        if record
            .conflicts
            .insert(conflict.id, conflict.clone())
            .is_some()
        {
            return Err(HistoryError::ConflictIdCollision(conflict.id));
        }
    }
    state.records.insert(record_id, record);
    Ok(())
}

/// Keep a record from the read-only legacy import only when the signed
/// repository does not know it. Its document and log were already validated.
fn merge_record(
    state: &mut HistoryState,
    record_id: FileRecordId,
    record: RecordState,
) -> Result<(), HistoryError> {
    if state.records.contains_key(&record_id) {
        return Ok(());
    }
    for version_id in record.versions.keys() {
        if state.version_owner.insert(*version_id, record_id).is_some() {
            return Err(HistoryError::VersionIdCollision(*version_id));
        }
    }
    state.records.insert(record_id, record);
    Ok(())
}

#[cfg(test)]
fn history_document(state: &HistoryState) -> HistoryDocument {
    HistoryDocument {
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
    }
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid history JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid file record: {0}")]
    Record(#[from] FileRecordError),
    #[error("signed record repository cannot supply history: {0}")]
    Repository(#[from] crate::record_repository::RecordRepositoryError),
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
    use std::io::Write as _;

    use super::*;
    use collab_types::{Ed25519PublicKey, generate_keypair};
    use fs_protocol::{ConflictStrategy, ContentHash, FileActor, FileProvenance};
    use uuid::Uuid;

    use crate::record_replication::RecordEnvelopeV1;
    use crate::record_replication::RecordOperationV1;
    use crate::record_replication::test_authorizers::AllowActor;
    use crate::record_repository::{RecordRepository, RecordRepositoryKey};

    fn signed_version(
        record_id: FileRecordId,
        parents: Vec<FileVersionId>,
        provenance: FileProvenance,
        timestamp_ms: u64,
        actor: FileActor,
    ) -> FileVersionV1 {
        let mut version = version(record_id, parents, provenance, timestamp_ms);
        version.actor = actor;
        version
    }

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
    fn derived_history_appends_and_batches_are_exact_atomic_and_validated() {
        let store = FileHistoryStore::in_memory();
        let record = FileRecordId::new();
        let root = version(record, vec![], FileProvenance::Create, 1);
        assert!(store.append(root.clone()).unwrap());
        assert!(!store.append(root.clone()).unwrap(), "exact replay");
        let left = version(record, vec![root.id], FileProvenance::Edit, 2);
        let right = version(record, vec![root.id], FileProvenance::Edit, 3);
        store.append(left.clone()).unwrap();
        store.append(right.clone()).unwrap();
        let snapshot = store.snapshot(record).unwrap();
        assert_eq!(
            snapshot.versions,
            vec![root.clone(), left.clone(), right.clone()]
        );
        let mut expected_heads = vec![left.id, right.id];
        expected_heads.sort();
        assert_eq!(snapshot.heads, expected_heads);

        let before = store.snapshot(record).unwrap();
        let orphan = version(record, vec![FileVersionId::new()], FileProvenance::Edit, 4);
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
        assert_eq!(store.snapshot(record).unwrap(), before);

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
        let before_source = store.snapshot(source_record).unwrap();
        let before_destination = store.snapshot(destination_record).unwrap();
        assert!(matches!(
            store.append_batch(&[source_move.clone(), destination_delete]),
            Err(HistoryError::MissingParent(_))
        ));
        assert_eq!(store.snapshot(source_record).unwrap(), before_source);
        assert_eq!(
            store.snapshot(destination_record).unwrap(),
            before_destination
        );
        let destination_delete = version(
            destination_record,
            vec![destination_root.id],
            FileProvenance::Delete,
            2,
        );
        assert_eq!(
            store
                .append_batch(&[source_move, destination_delete])
                .unwrap(),
            vec![true, true]
        );

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
        assert!(store.resolve(record, conflict_id, &resolution).unwrap());
        assert!(!store.resolve(record, conflict_id, &resolution).unwrap());
        let conflict = &store.snapshot(record).unwrap().conflicts[0];
        assert_eq!(conflict.status, ConflictStatus::Resolved);
        assert_eq!(conflict.resolution.as_ref().unwrap().result, result.id);
    }

    #[test]
    fn legacy_history_is_imported_read_only_and_damage_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let record = FileRecordId::new();
        let root = version(record, vec![], FileProvenance::Create, 1);
        let edit = version(record, vec![root.id], FileProvenance::Edit, 2);
        let mut legacy = HistoryState::default();
        let candidate = stage_versions(&legacy, &[root.clone(), edit.clone()]).unwrap();
        install_versions(&mut legacy, candidate);
        std::fs::write(
            &path,
            serde_json::to_vec(&history_document(&legacy)).unwrap(),
        )
        .unwrap();
        let legacy_bytes = std::fs::read(&path).unwrap();
        let log = log_path_for(&path);
        let next = version(record, vec![edit.id], FileProvenance::Edit, 3);
        let later = version(record, vec![next.id], FileProvenance::Edit, 4);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap();
        for versions in [vec![next.clone()], vec![later.clone()]] {
            file.write_all(&encode_log_line(&LogEntry::Versions { versions }).unwrap())
                .unwrap();
        }
        drop(file);
        let log_bytes = std::fs::read(&log).unwrap();

        let store = FileHistoryStore::open(&path).unwrap();
        assert_eq!(
            store.snapshot(record).unwrap().versions,
            vec![root.clone(), edit, next, later]
        );
        assert!(!store.append(root).unwrap(), "exact replay");
        assert_eq!(store.snapshot(record).unwrap().versions.len(), 4);
        assert_eq!(std::fs::read(&path).unwrap(), legacy_bytes);
        assert_eq!(std::fs::read(&log).unwrap(), log_bytes, "never compacted");
        drop(store);

        // A torn final line is a write that never returned: ignored, and the
        // legacy bytes stay untouched as evidence.
        let mut file = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        file.write_all(b"0123456789abcdef {\"op\":\"vers").unwrap();
        drop(file);
        let damaged_tail = std::fs::read(&log).unwrap();
        assert_eq!(
            FileHistoryStore::open(&path)
                .unwrap()
                .snapshot(record)
                .unwrap()
                .versions
                .len(),
            4
        );
        assert_eq!(std::fs::read(&log).unwrap(), damaged_tail);
        std::fs::write(&log, &log_bytes).unwrap();

        // Damage to an acknowledged entry is never silently skipped.
        let mut damaged = log_bytes;
        damaged[70] ^= 0x01;
        std::fs::write(&log, &damaged).unwrap();
        assert!(matches!(
            FileHistoryStore::open(&path),
            Err(HistoryError::DamagedLog(_))
        ));
        assert_eq!(std::fs::read(&log).unwrap(), damaged, "evidence kept");

        // A legacy document whose heads do not match its graph is refused.
        std::fs::remove_file(&log).unwrap();
        let mut document: serde_json::Value = serde_json::from_slice(&legacy_bytes).unwrap();
        document["records"][0]["heads"] = serde_json::json!([]);
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(matches!(
            FileHistoryStore::open(&path),
            Err(HistoryError::InvalidHeads)
        ));
    }

    fn projection(
        record_id: FileRecordId,
        versions: Vec<FileVersionV1>,
    ) -> crate::record_replication::RecordSnapshot {
        crate::record_replication::RecordSnapshot {
            record_id,
            binding: None,
            metadata: None,
            content_revision: None,
            versions,
            conflicts: Vec::new(),
            attributions: Vec::new(),
        }
    }

    #[test]
    fn repository_fold_still_refuses_broken_ancestry_and_cross_record_ids() {
        let mut state = HistoryState::default();
        let record = FileRecordId::new();
        let orphan = version(record, vec![FileVersionId::new()], FileProvenance::Edit, 1);
        assert!(matches!(
            install_projection(&mut state, record, &projection(record, vec![orphan])),
            Err(HistoryError::MissingParent(_))
        ));
        assert!(state.records.is_empty());

        let other = FileRecordId::new();
        let first = version(other, vec![], FileProvenance::Create, 1);
        install_projection(&mut state, other, &projection(other, vec![first.clone()])).unwrap();
        let mut collided = first;
        collided.record_id = record;
        assert!(matches!(
            install_projection(&mut state, record, &projection(record, vec![collided])),
            Err(HistoryError::VersionIdCollision(_))
        ));
        assert!(!state.records.contains_key(&record));
    }

    fn repository_with_signed_history() -> (
        tempfile::TempDir,
        RecordRepository,
        Uuid,
        RecordRepositoryKey,
        FileActor,
        ed25519_dalek::SigningKey,
        [FileVersionV1; 3],
    ) {
        let dir = tempfile::tempdir().unwrap();
        let repository = RecordRepository::open(dir.path().join("records")).unwrap();
        let workspace_id = Uuid::new_v4();
        let record_id = FileRecordId::new();
        let key = RecordRepositoryKey {
            workspace_id,
            record_id,
        };
        let (signing_key, device) = generate_keypair();
        let (_, person) = generate_keypair();
        let actor = FileActor { person, device };
        let root = signed_version(record_id, vec![], FileProvenance::Create, 1, actor);
        let edit = signed_version(record_id, vec![root.id], FileProvenance::Edit, 2, actor);
        let result = signed_version(
            record_id,
            vec![root.id, edit.id],
            FileProvenance::ConflictResolution,
            3,
            actor,
        );
        let conflict = FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![root.id, edit.id],
            status: ConflictStatus::Open,
            resolution: None,
        };
        let operations = [
            RecordOperationV1::AppendVersion(root.clone()),
            RecordOperationV1::AppendVersion(edit.clone()),
            RecordOperationV1::AppendVersion(result.clone()),
            RecordOperationV1::AppendConflict(conflict.clone()),
            RecordOperationV1::ResolveConflict {
                conflict_id: conflict.id,
                resolution: ConflictResolution {
                    strategy: ConflictStrategy::KeepCurrent,
                    result: result.id,
                    actor,
                    timestamp_ms: result.timestamp_ms,
                },
            },
        ];
        for (index, operation) in operations.into_iter().enumerate() {
            let sequence = u64::try_from(index).unwrap().saturating_add(1);
            let envelope =
                RecordEnvelopeV1::signed(record_id, actor, sequence, operation, &signing_key)
                    .unwrap();
            repository
                .apply(key, &envelope, &AllowActor(actor))
                .unwrap();
        }
        (
            dir,
            repository,
            workspace_id,
            key,
            actor,
            signing_key,
            [root, edit, result],
        )
    }

    #[test]
    fn derived_query_index_rebuilds_from_the_signed_log_and_keeps_legacy_only_records() {
        let (dir, repository, workspace_id, key, actor, signing_key, versions) =
            repository_with_signed_history();
        let record_id = key.record_id;
        let path = dir.path().join("history.json");
        let extra_record = FileRecordId::new();
        let extra = version(extra_record, vec![], FileProvenance::Create, 9);
        let mut legacy = HistoryState::default();
        let candidate = stage_versions(&legacy, std::slice::from_ref(&extra)).unwrap();
        install_versions(&mut legacy, candidate);
        let legacy_bytes = serde_json::to_vec(&history_document(&legacy)).unwrap();
        std::fs::write(&path, &legacy_bytes).unwrap();

        let store = FileHistoryStore::open(&path).unwrap();
        store
            .rebuild_from_repository(&repository, workspace_id)
            .unwrap();
        let snapshot = store.snapshot(record_id).unwrap();
        assert_eq!(snapshot.versions, versions.to_vec());
        assert_eq!(snapshot.conflicts.len(), 1);
        assert_eq!(snapshot.conflicts[0].status, ConflictStatus::Resolved);
        assert_eq!(
            store.snapshot(extra_record).unwrap().versions,
            vec![extra],
            "a record the signed repository does not know keeps its legacy read view"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            legacy_bytes,
            "the derived index never writes the legacy history document"
        );
        assert!(
            !log_path_for(&path).exists(),
            "the derived index owns no second history log"
        );
        assert!(
            !dir.path().join("history.index.json").exists(),
            "the derived index owns no persistent cache"
        );

        // A new signed version after the first fold is picked up by the next
        // fresh fold; no cache can pin the index to the older log.
        let later = signed_version(
            record_id,
            vec![versions[2].id],
            FileProvenance::Edit,
            4,
            actor,
        );
        let envelope = RecordEnvelopeV1::signed(
            record_id,
            actor,
            6,
            RecordOperationV1::AppendVersion(later.clone()),
            &signing_key,
        )
        .unwrap();
        repository
            .apply(key, &envelope, &AllowActor(actor))
            .unwrap();
        let restarted = FileHistoryStore::open(&path).unwrap();
        restarted
            .rebuild_from_repository(&repository, workspace_id)
            .unwrap();
        assert_eq!(
            restarted.snapshot(record_id).unwrap().versions.last(),
            Some(&later)
        );
    }

    #[test]
    fn stale_corrupt_or_missing_query_cache_never_replaces_accepted_history() {
        let (dir, repository, workspace_id, key, _actor, _signing_key, versions) =
            repository_with_signed_history();
        let record_id = key.record_id;
        let path = dir.path().join("history.json");
        let cache = dir.path().join("history.index.json");
        let stale = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "workspace_id": workspace_id,
            "source_digest": vec![0_u8; 32],
            "document": history_document(&HistoryState::default()),
        }))
        .unwrap();

        for state in ["stale", "corrupt", "missing"] {
            match state {
                "stale" => std::fs::write(&cache, &stale).unwrap(),
                "corrupt" => std::fs::write(&cache, b"{\"truncated\":").unwrap(),
                "missing" => {
                    if cache.exists() {
                        std::fs::remove_file(&cache).unwrap();
                    }
                }
                _ => unreachable!(),
            }
            let store = FileHistoryStore::open(&path).unwrap();
            store
                .rebuild_from_repository(&repository, workspace_id)
                .unwrap();
            let snapshot = store.snapshot(record_id).unwrap();
            assert_eq!(
                snapshot.versions, versions,
                "the {state} cache must not replace accepted history"
            );
            assert_eq!(snapshot.conflicts.len(), 1);
            if state == "stale" {
                assert_eq!(
                    std::fs::read(&cache).unwrap(),
                    stale,
                    "an old cache file is left untouched evidence, never rewritten"
                );
            }
        }
        assert!(
            !cache.exists(),
            "a missing cache stays absent: the repository fold owns no cache write"
        );
    }

    #[test]
    fn damaged_legacy_document_recovers_its_newest_valid_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let record = FileRecordId::new();
        let root = version(record, vec![], FileProvenance::Create, 1);
        let mut first = HistoryState::default();
        let candidate = stage_versions(&first, std::slice::from_ref(&root)).unwrap();
        install_versions(&mut first, candidate);
        let first_bytes = serde_json::to_vec(&history_document(&first)).unwrap();
        let edit = version(record, vec![root.id], FileProvenance::Edit, 2);
        let mut second = first.clone();
        let candidate = stage_versions(&second, std::slice::from_ref(&edit)).unwrap();
        install_versions(&mut second, candidate);
        let second_bytes = serde_json::to_vec(&history_document(&second)).unwrap();
        crate::durable_backup::write(&path, &first_bytes, |bytes| decode_document(bytes).is_ok())
            .unwrap();
        crate::durable_backup::write(&path, &second_bytes, |bytes| decode_document(bytes).is_ok())
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), second_bytes);

        // The primary is torn; the newest retained valid generation (the
        // first document) is recovered with evidence, as before this index
        // became derived.
        std::fs::write(&path, b"damaged").unwrap();
        let store = FileHistoryStore::open(&path).unwrap();
        assert_eq!(
            store.snapshot(record).unwrap().versions,
            vec![root],
            "the valid legacy backup is recovered, not discarded"
        );
        assert_eq!(std::fs::read(&path).unwrap(), first_bytes);
    }
}
