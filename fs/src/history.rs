//! Crash-safe immutable file history and first-class conflict records.
//!
//! History is keyed by [`FileRecordId`], never by path. Every mutation appends
//! a node whose parents already exist locally; importing out-of-order history
//! must therefore stage it before calling this store. Disk state is validated
//! in full on open so corrupt ancestry cannot become an eviction or restore
//! root.

use std::collections::{BTreeMap, BTreeSet};
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
pub const HISTORY_XATTR: &str = "user.qos.history";
pub const HISTORY_CONTROL_XATTR: &str = "user.qos.history-control";
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

#[derive(Clone, Debug, Default)]
struct HistoryState {
    records: BTreeMap<FileRecordId, RecordState>,
}

/// A bounded copy suitable for metadata/UI projection without exposing the
/// mutable store lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordHistorySnapshot {
    pub record_id: FileRecordId,
    pub versions: Vec<FileVersionV1>,
    pub heads: Vec<FileVersionId>,
    pub conflicts: Vec<FileConflictV1>,
}

/// Single-writer, crash-safe history database for one workspace.
pub struct FileHistoryStore {
    path: PathBuf,
    state: Mutex<HistoryState>,
}

impl FileHistoryStore {
    /// Open and fully validate a durable history store, recovering only from a
    /// retained generation that passes the same validation.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O, malformed history, invalid graph/conflicts, or
    /// damaged state without a valid retained generation.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, HistoryError> {
        let path = path.into();
        let state = match crate::durable_backup::read_primary(&path)? {
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
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
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
    /// a cloned candidate (in input order), the complete candidate is written
    /// with one atomic file replacement, and only then is it published in
    /// memory. A failure therefore exposes neither half of a multi-record
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
        if versions.is_empty() {
            return Ok(Vec::new());
        }
        for version in versions {
            version.validate()?;
        }
        let mut current = self.state.lock().map_err(|_| HistoryError::StatePoisoned)?;
        let mut next = current.clone();
        let mut appended = Vec::with_capacity(versions.len());
        for version in versions {
            if let Some(existing) = find_version(&next, version.id) {
                if existing == version {
                    appended.push(false);
                    continue;
                }
                return Err(HistoryError::VersionIdCollision(version.id));
            }

            let record = next.records.entry(version.record_id).or_default();
            if record.versions.is_empty() {
                if let Some(parent) = version.parents.first() {
                    return Err(HistoryError::MissingParent(*parent));
                }
            } else {
                if version.parents.is_empty() {
                    return Err(HistoryError::DisconnectedVersion(version.id));
                }
                for parent in &version.parents {
                    if !record.versions.contains_key(parent) {
                        return Err(HistoryError::MissingParent(*parent));
                    }
                }
            }
            for parent in &version.parents {
                record.heads.remove(parent);
            }
            record.heads.insert(version.id);
            record.versions.insert(version.id, version.clone());
            appended.push(true);
        }
        if appended.iter().any(|was_appended| *was_appended) {
            persist(&self.path, &next)?;
            *current = next;
        }
        Ok(appended)
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
        conflict.validate()?;
        let mut current = self.state.lock().map_err(|_| HistoryError::StatePoisoned)?;
        let record = current
            .records
            .get(&conflict.record_id)
            .ok_or(HistoryError::UnknownRecord(conflict.record_id))?;
        validate_conflict(record, &conflict)?;
        if let Some(existing) = record.conflicts.get(&conflict.id) {
            return if existing == &conflict {
                Ok(false)
            } else {
                Err(HistoryError::ConflictIdCollision(conflict.id))
            };
        }
        let mut next = current.clone();
        next.records
            .get_mut(&conflict.record_id)
            .ok_or(HistoryError::UnknownRecord(conflict.record_id))?
            .conflicts
            .insert(conflict.id, conflict);
        persist(&self.path, &next)?;
        *current = next;
        Ok(true)
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
        let mut current = self.state.lock().map_err(|_| HistoryError::StatePoisoned)?;
        let record = current
            .records
            .get(&record_id)
            .ok_or(HistoryError::UnknownRecord(record_id))?;
        let conflict = record
            .conflicts
            .get(&conflict_id)
            .ok_or(HistoryError::UnknownConflict(conflict_id))?;
        if let Some(existing) = &conflict.resolution
            && (existing == &resolution
                || !collab_types::prefer_conflict_resolution(&resolution, existing))
        {
            return Ok(false);
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

        let mut next = current.clone();
        let record = next
            .records
            .get_mut(&record_id)
            .ok_or(HistoryError::UnknownRecord(record_id))?;
        let conflict = record
            .conflicts
            .get_mut(&conflict_id)
            .ok_or(HistoryError::UnknownConflict(conflict_id))?;
        conflict.status = ConflictStatus::Resolved;
        conflict.resolution = Some(resolution);
        conflict.validate()?;
        persist(&self.path, &next)?;
        *current = next;
        Ok(true)
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

fn find_version(state: &HistoryState, id: FileVersionId) -> Option<&FileVersionV1> {
    state
        .records
        .values()
        .find_map(|record| record.versions.get(&id))
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
    let mut global_versions = BTreeSet::new();
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
            if !global_versions.insert(version_id) {
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
    fn append_branches_and_restart_preserve_exact_heads() {
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

    #[test]
    fn missing_parent_and_global_id_collision_leave_disk_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let store = FileHistoryStore::open(&path).unwrap();
        let record = FileRecordId::new();
        let root = version(record, vec![], FileProvenance::Create, 1);
        store.append(root.clone()).unwrap();
        let before = std::fs::read(&path).unwrap();
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
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn multi_record_batch_is_all_or_nothing_and_survives_restart() {
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
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            store.append_batch(&[source_move.clone(), destination_delete]),
            Err(HistoryError::MissingParent(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
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

    #[test]
    fn conflict_resolution_is_first_class_idempotent_and_durable() {
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

    #[test]
    fn corrupt_heads_are_rejected_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let store = FileHistoryStore::open(&path).unwrap();
        let record = FileRecordId::new();
        store
            .append(version(record, vec![], FileProvenance::Create, 1))
            .unwrap();
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

    #[test]
    fn xattr_projection_is_bounded_recent_and_round_trips() {
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
