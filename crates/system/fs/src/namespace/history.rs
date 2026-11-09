//! Immutable file versions and conflicts, held in the tree document.
//!
//! A version records one state a record was in: the content hash, the
//! metadata hash, who made it, when, and which versions it descends from.
//! History is keyed by [`FileRecordId`], never by path, and it lives inside
//! the namespace document itself, so it syncs with the tree and every device
//! derives the same heads and conflicts from the same versions. Nothing is
//! stored beside the document: there is no second writer to fall out of step,
//! and a peer's version is simply a peer's part of the document.
//!
//! Versions are never rewritten. A restore appends a version, it does not
//! truncate: the graph only grows, so an eviction root or a restore target
//! stays resolvable.

use std::collections::{BTreeMap, BTreeSet};

use automerge::transaction::Transactable as _;
use automerge::{ObjId, ObjType, ReadDoc as _, ScalarValue, Value};
use fs_protocol::projection::{
    HISTORY_PROJECTION_VERSION, MAX_PROJECTED_CONFLICTS, MAX_PROJECTED_VERSIONS,
};
use fs_protocol::{
    ConflictResolution, ConflictStatus, FileConflictV1, FileProvenance, FileRecordError,
    FileRecordId, FileVersionId, FileVersionV1, HistoryProjectionV1, prefer_conflict_resolution,
};
use thiserror::Error;
use uuid::Uuid;

use super::Namespace;

/// Root key of the history the tree document carries.
pub(crate) const HISTORY_KEY: &str = "history";
const VERSIONS_KEY: &str = "versions";
const CONFLICTS_KEY: &str = "conflicts";

mod read_query;

/// What a version's metadata digest covers: the facts a restore must find
/// unchanged besides the bytes themselves.
#[derive(serde::Serialize)]
pub(crate) struct VersionMetadataDigest {
    pub path: Option<String>,
    pub kind: u8,
    pub byte_len: u64,
    pub deleted: bool,
}

/// A record's immutable versions, heads and conflicts, as one bounded copy.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RecordHistorySnapshot {
    pub record_id: FileRecordId,
    pub versions: Vec<FileVersionV1>,
    pub heads: Vec<FileVersionId>,
    pub conflicts: Vec<FileConflictV1>,
}

impl RecordHistorySnapshot {
    /// The bounded metadata projection of this exact immutable snapshot.
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

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error(transparent)]
    Document(#[from] automerge::AutomergeError),
    #[error(transparent)]
    Record(#[from] FileRecordError),
    #[error(transparent)]
    Canonical(#[from] collab_types::CanonicalError),
    #[error("version id {0:?} is already bound to different content")]
    VersionIdCollision(FileVersionId),
    #[error("conflict id {0} is already bound to different content")]
    ConflictIdCollision(Uuid),
    #[error("record {0:?} has no history")]
    UnknownRecord(FileRecordId),
    #[error("conflict {0} is unknown")]
    UnknownConflict(Uuid),
    #[error("conflict names unknown version {0:?}")]
    UnknownConflictVersion(FileVersionId),
    #[error("resolving version {0:?} is unknown")]
    UnknownResolutionVersion(FileVersionId),
    #[error("the record repository is at capacity: {0}")]
    CapacityExhausted(String),
    #[error("the version could not be published: {0}")]
    Publication(String),
    #[error("resolving version was not made by a conflict resolution")]
    ResolutionProvenance,
    #[error("resolution actor does not match the resolving version")]
    ResolutionActorMismatch,
    #[error("resolution time does not match the resolving version")]
    ResolutionTimestampMismatch,
}

fn object(value: Option<(Value<'_>, ObjId)>) -> Option<ObjId> {
    match value {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

fn scalar_bytes(value: Option<(Value<'_>, ObjId)>) -> Option<Vec<u8>> {
    match value {
        Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
            ScalarValue::Bytes(bytes) => Some(bytes.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The versions nothing supersedes.
fn computed_heads(versions: &BTreeMap<FileVersionId, FileVersionV1>) -> BTreeSet<FileVersionId> {
    let mut heads: BTreeSet<_> = versions.keys().copied().collect();
    for version in versions.values() {
        for parent in &version.parents {
            heads.remove(parent);
        }
    }
    heads
}

/// A conflict may only name versions the record has, and a resolution only a
/// version that made that resolution.
fn check_conflict(
    versions: &BTreeMap<FileVersionId, FileVersionV1>,
    conflict: &FileConflictV1,
) -> Result<(), HistoryError> {
    for version in &conflict.versions {
        if !versions.contains_key(version) {
            return Err(HistoryError::UnknownConflictVersion(*version));
        }
    }
    if let Some(resolution) = &conflict.resolution {
        let result = versions
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

impl Namespace {
    /// The document map holding every record's history, if the document has
    /// one.
    pub(super) fn history_root(&self) -> Option<ObjId> {
        object(self.doc.get(&automerge::ROOT, HISTORY_KEY).ok().flatten())
    }

    /// The document map holding every record's history, created when the
    /// document has none yet.
    pub(crate) fn ensure_history_root(&mut self) -> Result<ObjId, HistoryError> {
        if let Some(root) = self.history_root() {
            return Ok(root);
        }
        Ok(self
            .doc
            .put_object(&automerge::ROOT, HISTORY_KEY, ObjType::Map)?)
    }

    /// Every document map holding record histories: one until two devices that each made their
    /// own document meet, and one per device after, since a merge keeps both maps under the key
    /// rather than choosing between them. A reader takes the maps together, or it would see the
    /// versions of one device and none of the other's.
    fn history_roots(&self) -> Vec<ObjId> {
        self.doc
            .get_all(&automerge::ROOT, HISTORY_KEY)
            .map(|values| {
                values
                    .into_iter()
                    .filter_map(|(value, id)| match value {
                        Value::Object(ObjType::Map) => Some(id),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every map under `parent`'s `key`: more than one where two devices each wrote their own.
    fn sub_maps(&self, parent: &ObjId, key: &str) -> Vec<ObjId> {
        self.doc
            .get_all(parent, key)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(value, id)| match value {
                Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })
            .collect()
    }

    /// Every map `record_id`'s history has, for the same reason as [`Self::history_roots`]: two
    /// devices that both wrote the record's history each made one.
    fn record_maps(&self, record_id: FileRecordId) -> Vec<ObjId> {
        let mut maps = Vec::new();
        for root in self.history_roots() {
            maps.extend(self.sub_maps(&root, &record_id.0.to_string()));
        }
        maps
    }

    /// `record_id`'s versions, oldest first by version id.
    pub(crate) fn record_versions(
        &self,
        record_id: FileRecordId,
    ) -> BTreeMap<FileVersionId, FileVersionV1> {
        let mut versions = BTreeMap::new();
        for record in self.record_maps(record_id) {
            for map in self.sub_maps(&record, VERSIONS_KEY) {
                for key in self.doc.keys(&map) {
                    let Some(bytes) = scalar_bytes(self.doc.get(&map, &key).ok().flatten()) else {
                        continue;
                    };
                    let Ok(version) =
                        collab_types::canonical::from_canonical_bytes::<FileVersionV1>(&bytes)
                    else {
                        continue;
                    };
                    versions.insert(version.id, version);
                }
            }
        }
        versions
    }

    /// `record_id`'s conflicts, by id.
    pub(crate) fn record_conflicts(
        &self,
        record_id: FileRecordId,
    ) -> BTreeMap<Uuid, FileConflictV1> {
        let mut conflicts = BTreeMap::new();
        for record in self.record_maps(record_id) {
            for map in self.sub_maps(&record, CONFLICTS_KEY) {
                for key in self.doc.keys(&map) {
                    let Some(bytes) = scalar_bytes(self.doc.get(&map, &key).ok().flatten()) else {
                        continue;
                    };
                    let Ok(conflict) =
                        collab_types::canonical::from_canonical_bytes::<FileConflictV1>(&bytes)
                    else {
                        continue;
                    };
                    conflicts.insert(conflict.id, conflict);
                }
            }
        }
        conflicts
    }

    /// The map one record's history lives in, created when absent.
    fn history_slot(&mut self, record_id: FileRecordId) -> Result<ObjId, HistoryError> {
        let root = self.ensure_history_root()?;
        let key = record_id.0.to_string();
        if let Some(record) = object(self.doc.get(&root, &key)?) {
            return Ok(record);
        }
        Ok(self.doc.put_object(&root, key, ObjType::Map)?)
    }

    fn sub_map(&mut self, record: &ObjId, key: &str) -> Result<ObjId, HistoryError> {
        if let Some(map) = object(self.doc.get(record, key)?) {
            return Ok(map);
        }
        Ok(self.doc.put_object(record, key, ObjType::Map)?)
    }

    fn write_version(
        &mut self,
        record_id: FileRecordId,
        version: &FileVersionV1,
    ) -> Result<(), HistoryError> {
        let record = self.history_slot(record_id)?;
        let versions = self.sub_map(&record, VERSIONS_KEY)?;
        let bytes = collab_types::canonical_bytes(version)?;
        self.doc.put(&versions, version.id.0.to_string(), bytes)?;
        Ok(())
    }

    fn write_conflict(
        &mut self,
        record_id: FileRecordId,
        conflict: &FileConflictV1,
    ) -> Result<(), HistoryError> {
        let record = self.history_slot(record_id)?;
        let conflicts = self.sub_map(&record, CONFLICTS_KEY)?;
        let bytes = collab_types::canonical_bytes(conflict)?;
        self.doc.put(&conflicts, conflict.id.to_string(), bytes)?;
        Ok(())
    }

    /// The record's versions, heads and conflicts.
    pub fn record_history(&self, record_id: FileRecordId) -> Option<RecordHistorySnapshot> {
        let versions = self.record_versions(record_id);
        let conflicts = self.record_conflicts(record_id);
        if versions.is_empty() && conflicts.is_empty() {
            return None;
        }
        let heads = computed_heads(&versions);
        // The order a person reads history in: what happened, when.
        let mut versions: Vec<_> = versions.into_values().collect();
        versions.sort_by_key(|version| (version.timestamp_ms, version.id));
        Some(RecordHistorySnapshot {
            record_id,
            versions,
            heads: heads.into_iter().collect(),
            conflicts: conflicts.into_values().collect(),
        })
    }

    /// The bounded history projection of `record_id`.
    pub fn record_history_projection(
        &self,
        record_id: FileRecordId,
    ) -> Option<HistoryProjectionV1> {
        self.record_history(record_id)
            .map(RecordHistorySnapshot::projection)
    }

    /// The versions nothing supersedes.
    pub fn record_history_heads(&self, record_id: FileRecordId) -> Vec<FileVersionId> {
        computed_heads(&self.record_versions(record_id))
            .into_iter()
            .collect()
    }

    /// One version of `record_id`, and the record's current heads.
    pub fn record_version(
        &self,
        record_id: FileRecordId,
        version_id: FileVersionId,
    ) -> (Option<FileVersionV1>, Vec<FileVersionId>) {
        let versions = self.record_versions(record_id);
        let heads = computed_heads(&versions);
        (
            versions.get(&version_id).cloned(),
            heads.into_iter().collect(),
        )
    }

    /// Check a batch of versions against the record's history without
    /// writing it; the same checks [`Self::append_versions`] applies.
    ///
    /// # Errors
    ///
    /// Returns the first history violation in the batch.
    pub fn check_versions(&self, versions: &[FileVersionV1]) -> Result<(), HistoryError> {
        let mut known: BTreeMap<FileRecordId, BTreeMap<FileVersionId, FileVersionV1>> =
            BTreeMap::new();
        for version in versions {
            let current = known
                .entry(version.record_id)
                .or_insert_with(|| self.record_versions(version.record_id));
            check_version(current, version)?;
            current.insert(version.id, version.clone());
        }
        Ok(())
    }

    /// Append versions to their records. A version the record already holds
    /// is a replay and appends nothing.
    ///
    /// Versions arrive in any order: a version's parents may reach this
    /// device later than the version itself, so the batch is not required to
    /// carry them, and a child that arrives first simply has no head until
    /// its parent does.
    ///
    /// # Errors
    ///
    /// Returns a history violation when a version is invalid or collides with
    /// a different version of the same id.
    pub fn append_versions(
        &mut self,
        versions: &[FileVersionV1],
    ) -> Result<Vec<bool>, HistoryError> {
        let mut known: BTreeMap<FileRecordId, BTreeMap<FileVersionId, FileVersionV1>> =
            BTreeMap::new();
        let mut appended = Vec::with_capacity(versions.len());
        let mut accepted = Vec::new();
        for version in versions {
            let current = known
                .entry(version.record_id)
                .or_insert_with(|| self.record_versions(version.record_id));
            if check_version(current, version)? {
                current.insert(version.id, version.clone());
                accepted.push(version.clone());
                appended.push(true);
            } else {
                appended.push(false);
            }
        }
        for version in &accepted {
            self.write_version(version.record_id, version)?;
        }
        Ok(appended)
    }

    /// Append one version, returning whether the record gained it.
    ///
    /// # Errors
    ///
    /// As [`Self::append_versions`].
    pub fn append_version(&mut self, version: FileVersionV1) -> Result<bool, HistoryError> {
        Ok(self
            .append_versions(&[version])?
            .first()
            .copied()
            .unwrap_or(false))
    }

    /// Record a conflict between existing versions of its record.
    ///
    /// # Errors
    ///
    /// Returns a history violation when the conflict is invalid, names a
    /// version its record does not have, or collides with a different
    /// conflict of the same id.
    pub fn record_history_conflict(
        &mut self,
        conflict: &FileConflictV1,
    ) -> Result<bool, HistoryError> {
        conflict.validate()?;
        let versions = self.record_versions(conflict.record_id);
        if versions.is_empty() {
            return Err(HistoryError::UnknownRecord(conflict.record_id));
        }
        check_conflict(&versions, conflict)?;
        match self.record_conflicts(conflict.record_id).get(&conflict.id) {
            Some(existing) if existing == conflict => Ok(false),
            Some(_) => Err(HistoryError::ConflictIdCollision(conflict.id)),
            None => {
                self.write_conflict(conflict.record_id, conflict)?;
                Ok(true)
            }
        }
    }

    /// Resolve a conflict with a version that made the resolution.
    ///
    /// # Errors
    ///
    /// Returns a history violation when the conflict or the resolving version
    /// is unknown, or the resolution disagrees with that version.
    pub fn resolve_history_conflict(
        &mut self,
        record_id: FileRecordId,
        conflict_id: Uuid,
        resolution: &ConflictResolution,
    ) -> Result<bool, HistoryError> {
        let Some(conflict) = self.record_conflicts(record_id).get(&conflict_id).cloned() else {
            return Err(HistoryError::UnknownConflict(conflict_id));
        };
        if let Some(existing) = &conflict.resolution
            && (existing == resolution || !prefer_conflict_resolution(resolution, existing))
        {
            return Ok(false);
        }
        let versions = self.record_versions(record_id);
        let result = versions
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
        let mut resolved = conflict;
        resolved.status = ConflictStatus::Resolved;
        resolved.resolution = Some(resolution.clone());
        resolved.validate()?;
        self.write_conflict(record_id, &resolved)?;
        Ok(true)
    }

    /// Whether `record_id` has any version.
    pub fn has_history(&self, record_id: FileRecordId) -> bool {
        !self.record_versions(record_id).is_empty()
    }
}

/// Whether `version` may join `current`; `Ok(false)` for an exact replay.
fn check_version(
    current: &BTreeMap<FileVersionId, FileVersionV1>,
    version: &FileVersionV1,
) -> Result<bool, HistoryError> {
    version.validate()?;
    if let Some(existing) = current.get(&version.id) {
        if existing == version {
            return Ok(false);
        }
        return Err(HistoryError::VersionIdCollision(version.id));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use fs_protocol::{
        ConflictStrategy, ContentHash, Ed25519PublicKey, FileActor, FileProvenance, FileVersionId,
    };

    use super::*;

    fn actor() -> FileActor {
        FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
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
            actor: actor(),
            timestamp_ms,
            content_hash: Some(ContentHash([3; 32])),
            metadata_hash: ContentHash([4; 32]),
            provenance,
        }
    }

    fn history() -> Namespace {
        Namespace::new("history").unwrap()
    }

    #[test]
    fn versions_append_once_and_heads_follow_the_graph() {
        let mut namespace = history();
        let record = FileRecordId::new();
        let root = version(record, vec![], FileProvenance::Create, 1);
        assert!(namespace.append_version(root.clone()).unwrap());
        assert!(
            !namespace.append_version(root.clone()).unwrap(),
            "exact replay"
        );
        let left = version(record, vec![root.id], FileProvenance::Edit, 2);
        let right = version(record, vec![root.id], FileProvenance::Edit, 3);
        assert_eq!(
            namespace
                .append_versions(&[left.clone(), right.clone()])
                .unwrap(),
            vec![true, true]
        );

        let snapshot = namespace.record_history(record).unwrap();
        assert_eq!(
            snapshot.versions,
            vec![root.clone(), left.clone(), right.clone()]
        );
        let mut expected_heads = vec![left.id, right.id];
        expected_heads.sort();
        assert_eq!(snapshot.heads, expected_heads);
        assert_eq!(
            namespace.record_history_heads(record),
            expected_heads,
            "a version no other supersedes heads the record"
        );

        // The same id bound to different content is refused, and the record
        // keeps what it had.
        let mut collision = version(record, vec![], FileProvenance::Edit, 9);
        collision.id = root.id;
        let error = namespace.append_version(collision).unwrap_err();
        assert!(
            matches!(error, HistoryError::VersionIdCollision(_)),
            "{error:?}"
        );
        assert_eq!(namespace.record_history(record).unwrap(), snapshot);

        // A batch carrying a violation writes none of it.
        let other = FileRecordId::new();
        let good = version(other, vec![], FileProvenance::Create, 1);
        let mut collision = version(other, vec![], FileProvenance::Edit, 2);
        collision.id = good.id;
        assert!(matches!(
            namespace.append_versions(&[good, collision]),
            Err(HistoryError::VersionIdCollision(_))
        ));
        assert!(namespace.record_history(other).is_none());
    }

    /// A device may receive a child before its parent; both are kept, and the
    /// heads are what the graph says once the parent arrives.
    #[test]
    fn a_version_arriving_before_its_parent_is_kept() {
        let mut namespace = history();
        let record = FileRecordId::new();
        let root = version(record, vec![], FileProvenance::Create, 1);
        let child = version(record, vec![root.id], FileProvenance::Edit, 2);
        assert!(namespace.append_version(child.clone()).unwrap());
        assert_eq!(namespace.record_history_heads(record), vec![child.id]);
        assert!(namespace.append_version(root.clone()).unwrap());
        assert_eq!(namespace.record_history_heads(record), vec![child.id]);
        assert_eq!(
            namespace.record_history(record).unwrap().versions,
            vec![root, child]
        );
    }

    /// A version is a change of the tree document, so a peer that takes the
    /// document's changes takes the versions with it: nothing separate has to
    /// travel for the other device to show or restore the version.
    #[test]
    fn a_peer_that_takes_the_document_changes_has_the_versions() {
        let mut here = history();
        let record = FileRecordId::new();
        let root = version(record, vec![], FileProvenance::Create, 1);
        let edit = version(record, vec![root.id], FileProvenance::Edit, 2);
        here.append_versions(&[root.clone(), edit.clone()]).unwrap();

        let mut there = history();
        there
            .apply_incremental(&here.changes_since(&[]))
            .expect("the versions travel as the document's changes");
        assert_eq!(
            there.record_history(record).unwrap().versions,
            vec![root.clone(), edit.clone()]
        );
        assert_eq!(there.record_history_heads(record), vec![edit.id]);

        // The version taken from the peer restores its record's content here.
        let (found, heads) = there.record_version(record, root.id);
        assert_eq!(found, Some(root));
        assert_eq!(heads, vec![edit.id]);
    }

    #[test]
    fn conflicts_are_recorded_once_and_resolved_by_their_version() {
        let mut namespace = history();
        let record = FileRecordId::new();
        let root = version(record, vec![], FileProvenance::Create, 1);
        let left = version(record, vec![root.id], FileProvenance::Edit, 2);
        let right = version(record, vec![root.id], FileProvenance::Edit, 3);
        namespace
            .append_versions(&[root, left.clone(), right.clone()])
            .unwrap();
        let conflict = FileConflictV1 {
            id: Uuid::new_v4(),
            record_id: record,
            versions: vec![left.id, right.id],
            status: ConflictStatus::Open,
            resolution: None,
        };
        assert!(namespace.record_history_conflict(&conflict).unwrap());
        assert!(
            !namespace.record_history_conflict(&conflict).unwrap(),
            "exact replay"
        );
        let error = namespace
            .record_history_conflict(&FileConflictV1 {
                versions: vec![left.id, FileVersionId::new()],
                ..conflict.clone()
            })
            .unwrap_err();
        assert!(
            matches!(error, HistoryError::UnknownConflictVersion(_)),
            "{error:?}"
        );

        let result = version(
            record,
            vec![left.id, right.id],
            FileProvenance::ConflictResolution,
            4,
        );
        namespace.append_version(result.clone()).unwrap();
        let resolution = ConflictResolution {
            strategy: ConflictStrategy::KeepCurrent,
            result: result.id,
            actor: result.actor,
            timestamp_ms: result.timestamp_ms,
        };
        assert!(
            namespace
                .resolve_history_conflict(record, conflict.id, &resolution)
                .unwrap()
        );
        assert!(
            !namespace
                .resolve_history_conflict(record, conflict.id, &resolution)
                .unwrap(),
            "exact replay"
        );
        let stored = namespace.record_history(record).unwrap().conflicts[0].clone();
        assert_eq!(stored.status, ConflictStatus::Resolved);
        assert_eq!(
            stored
                .resolution
                .as_ref()
                .map(|resolution| resolution.result),
            Some(result.id)
        );
    }

    #[test]
    fn the_projection_is_bounded_and_round_trips() {
        let mut namespace = history();
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
            namespace.append_version(item).unwrap();
        }
        let projection = namespace.record_history_projection(record).unwrap();
        assert_eq!(projection.total_versions, 20);
        assert!(projection.versions_truncated);
        assert_eq!(projection.versions.len(), MAX_PROJECTED_VERSIONS);
        assert_eq!(projection.versions.first().unwrap().timestamp_ms, 8);
        let bytes = projection.to_xattr_bytes().unwrap();
        assert!(bytes.len() <= fs_protocol::projection::MAX_HISTORY_PROJECTION_BYTES);
        assert_eq!(
            HistoryProjectionV1::parse_xattr(&bytes).unwrap(),
            projection
        );
    }
}
