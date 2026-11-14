//! Reading a record's immutable versions out of the tree document.
//!
//! The versions live inside the document the tree is ([`crate::tree`]), so a host
//! that serves a mounted file's history and a page that reads the workspace's
//! tree over its session read the same bytes the same way. Writing them is the
//! filesystem's ([`crate::files`] types, `tilo_fs::namespace`); what is here is
//! the read both need.

use std::collections::{BTreeMap, BTreeSet};

use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::files::{
    ContentRevisionV1, FileConflictV1, FileHistoryPageV1, FileHistoryReadErrorV1 as Error,
    FileHistoryReadQueryV1 as Query, FileHistoryReadResponseV1, FileHistoryReadScopeV1 as Scope,
    FileHistoryReadValueV1 as ReadValue, FileRecordId, FileVersionAttributionV1, FileVersionId,
    FileVersionV1, MAX_HISTORY_PAGE_VERSIONS, MAX_HISTORY_READ_BYTES,
};
use crate::projection::{
    HISTORY_PROJECTION_VERSION, HistoryProjectionV1, MAX_PROJECTED_CONFLICTS,
    MAX_PROJECTED_VERSIONS,
};

/// The document's map of record histories.
pub const HISTORY_KEY: &str = "history";
/// One record's map of versions.
pub const VERSIONS_KEY: &str = "versions";
/// One record's map of conflicts.
pub const CONFLICTS_KEY: &str = "conflicts";
/// One record's map of who wrote each version an agent or an app did.
pub const ATTRIBUTIONS_KEY: &str = "attributions";

/// A record's immutable versions, heads and conflicts, as one bounded copy.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RecordHistorySnapshot {
    pub record_id: FileRecordId,
    pub versions: Vec<FileVersionV1>,
    pub heads: Vec<FileVersionId>,
    pub conflicts: Vec<FileConflictV1>,
}

impl RecordHistorySnapshot {
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
            attributions: Vec::new(),
        }
    }
}

/// The version a record is at now: the newest one it holds, and who wrote it
/// when that was not the person themselves. `None` for a record the document
/// holds no version of.
#[must_use]
pub fn latest<D: ReadDoc>(
    doc: &D,
    record_id: FileRecordId,
) -> Option<(FileVersionV1, Option<FileVersionAttributionV1>)> {
    let versions = record_versions(doc, record_id);
    let heads = computed_heads(&versions);
    let version = versions.into_values().max_by_key(|version| {
        (
            heads.contains(&version.id),
            version.timestamp_ms,
            version.id,
        )
    })?;
    let attribution = record_attributions(doc, record_id).remove(&version.id);
    Some((version, attribution))
}

/// Every record the document holds a history of.
#[must_use]
pub fn records<D: ReadDoc>(doc: &D) -> Vec<FileRecordId> {
    let mut records = BTreeSet::new();
    for root in roots(doc) {
        for key in doc.keys(&root) {
            if let Ok(id) = uuid::Uuid::parse_str(&key) {
                records.insert(FileRecordId(id));
            }
        }
    }
    records.into_iter().collect()
}

/// A record's versions, by their id.
#[must_use]
pub fn record_versions<D: ReadDoc>(
    doc: &D,
    record_id: FileRecordId,
) -> BTreeMap<FileVersionId, FileVersionV1> {
    let mut versions = BTreeMap::new();
    for record in record_maps(doc, record_id) {
        for map in sub_maps(doc, &record, VERSIONS_KEY) {
            for key in doc.keys(&map) {
                let Some(bytes) = scalar_bytes(doc.get(&map, &key).ok().flatten()) else {
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

/// A record's conflicts, by their id.
#[must_use]
pub fn record_conflicts<D: ReadDoc>(
    doc: &D,
    record_id: FileRecordId,
) -> BTreeMap<uuid::Uuid, FileConflictV1> {
    let mut conflicts = BTreeMap::new();
    for record in record_maps(doc, record_id) {
        for map in sub_maps(doc, &record, CONFLICTS_KEY) {
            for key in doc.keys(&map) {
                let Some(bytes) = scalar_bytes(doc.get(&map, &key).ok().flatten()) else {
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

/// Who wrote each of a record's versions that its person did not write
/// themselves, by the version's id.
#[must_use]
pub fn record_attributions<D: ReadDoc>(
    doc: &D,
    record_id: FileRecordId,
) -> BTreeMap<FileVersionId, FileVersionAttributionV1> {
    let mut attributions = BTreeMap::new();
    for record in record_maps(doc, record_id) {
        for map in sub_maps(doc, &record, ATTRIBUTIONS_KEY) {
            for key in doc.keys(&map) {
                let Some(bytes) = scalar_bytes(doc.get(&map, &key).ok().flatten()) else {
                    continue;
                };
                let Ok(attributed) = collab_types::canonical::from_canonical_bytes::<
                    FileVersionAttributionV1,
                >(&bytes) else {
                    continue;
                };
                attributions.insert(attributed.version_id, attributed);
            }
        }
    }
    attributions
}

/// The record's versions, its newest last, and the heads they compute.
#[must_use]
pub fn record_history<D: ReadDoc>(
    doc: &D,
    record_id: FileRecordId,
) -> Option<RecordHistorySnapshot> {
    let versions = record_versions(doc, record_id);
    let conflicts = record_conflicts(doc, record_id);
    if versions.is_empty() && conflicts.is_empty() {
        return None;
    }
    let heads = computed_heads(&versions);
    let mut versions: Vec<_> = versions.into_values().collect();
    versions.sort_by_key(|version| (version.timestamp_ms, version.id));
    Some(RecordHistorySnapshot {
        record_id,
        versions,
        heads: heads.into_iter().collect(),
        conflicts: conflicts.into_values().collect(),
    })
}

/// The bounded value of a record's history: its newest versions and conflicts,
/// with who wrote each version an agent or an app did.
#[must_use]
pub fn record_history_projection<D: ReadDoc>(
    doc: &D,
    record_id: FileRecordId,
) -> Option<HistoryProjectionV1> {
    let mut projection = record_history(doc, record_id).map(RecordHistorySnapshot::projection)?;
    let shown: std::collections::HashSet<FileVersionId> = projection
        .versions
        .iter()
        .map(|version| version.id)
        .collect();
    projection.attributions = record_attributions(doc, record_id)
        .into_values()
        .filter(|attributed| shown.contains(&attributed.version_id))
        .collect();
    Some(projection)
}

/// The versions `record_id` descends from having no child: its current states.
#[must_use]
pub fn computed_heads(
    versions: &BTreeMap<FileVersionId, FileVersionV1>,
) -> BTreeSet<FileVersionId> {
    let mut heads: BTreeSet<_> = versions.keys().copied().collect();
    for version in versions.values() {
        for parent in &version.parents {
            heads.remove(parent);
        }
    }
    heads
}

impl RecordHistorySnapshot {
    pub fn read_query(
        self,
        scope: Scope,
        current_revision: Option<ContentRevisionV1>,
        query: Query,
        attributions: &[FileVersionAttributionV1],
    ) -> Result<ReadValue, Error> {
        // Who caused one version, when an agent did, as its own signing
        // device recorded it.
        let attribution_of = |version_id| {
            attributions
                .iter()
                .find(|attributed: &&FileVersionAttributionV1| attributed.version_id == version_id)
        };
        match query {
            Query::Page { cursor } => {
                let start = match cursor {
                    None => 0,
                    Some((expected, id)) => {
                        if expected != scope.frontier {
                            return Err(Error::Stale);
                        }
                        self.versions
                            .iter()
                            .position(|v| v.id == id)
                            .and_then(|i| i.checked_add(1))
                            .ok_or(Error::InvalidRequest)?
                    }
                };
                let remaining = self.versions.get(start..).ok_or(Error::InvalidRequest)?;
                let mut count = remaining.len().min(MAX_HISTORY_PAGE_VERSIONS);
                loop {
                    let versions = remaining
                        .get(..count)
                        .ok_or(Error::InvalidRequest)?
                        .to_vec();
                    let next_after = if remaining.len() > count {
                        versions.last().map(|v| v.id)
                    } else {
                        None
                    };
                    let attributions = versions
                        .iter()
                        .filter_map(|version| attribution_of(version.id))
                        .cloned()
                        .collect();
                    let value = ReadValue::Page(FileHistoryPageV1 {
                        scope,
                        versions,
                        total_versions: self.versions.len() as u64,
                        heads: self.heads.clone(),
                        current_revision,
                        next_after,
                        attributions,
                    });
                    let size = serde_json::to_vec(&FileHistoryReadResponseV1 {
                        schema_version: 1,
                        result: Ok(value.clone()),
                    })
                    .map_err(|_| Error::Unavailable)?
                    .len();
                    if size <= MAX_HISTORY_READ_BYTES {
                        return Ok(value);
                    }
                    count = count
                        .checked_sub(1)
                        .filter(|count| *count > 0)
                        .ok_or(Error::TooLarge)?;
                }
            }
            Query::Version {
                frontier: expected,
                version_id,
            } => {
                if scope.frontier != expected {
                    return Err(Error::Stale);
                }
                let version = self
                    .versions
                    .into_iter()
                    .find(|v| v.id == version_id)
                    .ok_or(Error::MissingVersion)?;
                let attribution =
                    attribution_of(version_id).map(|attributed| attributed.attribution.clone());
                Ok(ReadValue::Version {
                    scope,
                    version,
                    attribution,
                })
            }
            Query::Preview { .. } => Err(Error::InvalidRequest),
        }
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

/// Every map holding record histories: one until two devices that each made
/// their own document meet, and one per device after, since a merge keeps both
/// maps under the key. A reader takes the maps together, or it would see the
/// versions of one device and none of the other's.
/// Every map holding record histories, in this document.
#[must_use]
pub fn roots<D: ReadDoc>(doc: &D) -> Vec<ObjId> {
    doc.get_all(automerge::ROOT, HISTORY_KEY)
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

fn sub_maps<D: ReadDoc>(doc: &D, parent: &ObjId, key: &str) -> Vec<ObjId> {
    doc.get_all(parent, key)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(value, id)| match value {
            Value::Object(ObjType::Map) => Some(id),
            _ => None,
        })
        .collect()
}

/// Every map `record_id`'s history has: two devices that both wrote it each made
/// one.
fn record_maps<D: ReadDoc>(doc: &D, record_id: FileRecordId) -> Vec<ObjId> {
    let mut maps = Vec::new();
    for root in roots(doc) {
        maps.extend(sub_maps(doc, &root, &record_id.0.to_string()));
    }
    maps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentHash, Ed25519PublicKey, FileActor, FileProvenance};
    use automerge::transaction::Transactable as _;

    fn version(record: FileRecordId, id: u128, parent: Option<FileVersionId>) -> FileVersionV1 {
        FileVersionV1 {
            id: FileVersionId(uuid::Uuid::from_u128(id)),
            record_id: record,
            parents: parent.into_iter().collect(),
            actor: FileActor {
                person: Ed25519PublicKey::from_bytes([1; 32]),
                device: Ed25519PublicKey::from_bytes([2; 32]),
            },
            timestamp_ms: u64::try_from(id).unwrap(),
            content_hash: None,
            metadata_hash: ContentHash([3; 32]),
            provenance: FileProvenance::Edit,
        }
    }

    /// Write `version` and its attribution where the filesystem keeps them.
    fn write(doc: &mut automerge::AutoCommit, version: &FileVersionV1, agent: Option<&str>) {
        let root = match doc.get(automerge::ROOT, HISTORY_KEY) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(automerge::ROOT, HISTORY_KEY, ObjType::Map)
                .unwrap(),
        };
        let record = match doc.get(&root, version.record_id.0.to_string()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&root, version.record_id.0.to_string(), ObjType::Map)
                .unwrap(),
        };
        let versions = match doc.get(&record, VERSIONS_KEY) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc.put_object(&record, VERSIONS_KEY, ObjType::Map).unwrap(),
        };
        doc.put(
            &versions,
            version.id.0.to_string(),
            collab_types::canonical_bytes(version).unwrap(),
        )
        .unwrap();
        if let Some(actor) = agent {
            let attributed = FileVersionAttributionV1 {
                version_id: version.id,
                attribution: crate::files::FileAttributionV1 {
                    actor: actor.to_owned(),
                    label: "Claude Code".into(),
                    color: [1, 2, 3],
                },
            };
            let map = doc
                .put_object(&record, ATTRIBUTIONS_KEY, ObjType::Map)
                .unwrap();
            doc.put(
                &map,
                version.id.0.to_string(),
                collab_types::canonical_bytes(&attributed).unwrap(),
            )
            .unwrap();
        }
        doc.commit();
    }

    #[test]
    fn the_latest_version_is_the_head_the_tree_holds_and_carries_its_writer() {
        let record = FileRecordId::new();
        let mut doc = automerge::AutoCommit::new();
        let first = version(record, 1, None);
        write(&mut doc, &first, None);
        let second = version(record, 2, Some(first.id));
        write(&mut doc, &second, Some("agent:claude:abc"));

        let (latest, attribution) = latest(&doc, record).unwrap();
        assert_eq!(latest.id, second.id);
        assert_eq!(
            attribution.map(|attributed| attributed.attribution.actor),
            Some("agent:claude:abc".into())
        );
        assert_eq!(records(&doc), vec![record]);
        let projection = record_history_projection(&doc, record).unwrap();
        assert_eq!(projection.versions.len(), 2);
        assert_eq!(projection.heads, vec![second.id]);
        assert_eq!(projection.attributions.len(), 1);
    }

    #[test]
    fn a_record_the_document_holds_no_version_of_reads_as_none() {
        let doc = automerge::AutoCommit::new();
        let record = FileRecordId::new();
        assert!(latest(&doc, record).is_none());
        assert!(record_history(&doc, record).is_none());
    }
}
