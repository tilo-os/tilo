//! Metadata-only projection of canonical records retained outside the FUSE tree.
//! These entries are discovery data, never native access or replay authority.

use super::{
    FileBindingV1, FileNameV1, FileNodeKind, FileRecordId, HashSet, Namespace, ObjId, ObjType,
    ROOT, ReadDoc, ScalarValue, Transactable, Value,
};
use fs_protocol::{FileMutationOperation, FileVersionId, FileVersionV1};
use serde::{Deserialize, Serialize};

use crate::external_history::coordinator::{ObservedExternalReplacement, SeededExternalRecords};
use crate::record_transaction::RecordTransactionJournalEntry;

const EXTERNAL_RECORDS_KEY: &str = "external_record_projection_v1";
const MAX_EXTERNAL_RECORDS: usize = 4096;
const MAX_EXTERNAL_RECORD_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalRecordProjection {
    pub binding: FileBindingV1,
    pub version: Option<FileVersionId>,
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ExternalProjectionError {
    #[error("external record projection has no current authenticated admission")]
    Admission,
    #[error("external record projection is malformed or ambiguous")]
    Invalid,
    #[error("external record projection precondition changed")]
    Precondition,
    #[error("external record projection could not be persisted")]
    Persistence,
    #[error("external record projection exceeds its bounded catalogue")]
    Limit,
}

struct ProjectionChange {
    next: ExternalRecordProjection,
    expected: Option<ExpectedProjection>,
}

struct ExpectedProjection {
    parent: FileRecordId,
    name: FileNameV1,
    version: FileVersionId,
}

/// Constructible only from the coordinator's actual retained admission tokens.
/// A signed DTO by itself cannot construct a batch or authorize its application.
enum ProjectionAdmission<'a> {
    Seed(&'a SeededExternalRecords),
    Observed(
        &'a ObservedExternalReplacement,
        &'a RecordTransactionJournalEntry,
    ),
}

pub(crate) struct ExternalProjectionBatch<'a> {
    changes: Vec<ProjectionChange>,
    admission: ProjectionAdmission<'a>,
}

impl<'a> ExternalProjectionBatch<'a> {
    fn check_admission(&self) -> Result<(), ExternalProjectionError> {
        match self.admission {
            ProjectionAdmission::Seed(seed) => seed.check(),
            ProjectionAdmission::Observed(completion, entry) => completion.check_entry(entry),
        }
        .map_err(|_| ExternalProjectionError::Admission)
    }

    pub(crate) fn from_seed(
        seed: &'a SeededExternalRecords,
    ) -> Result<Self, ExternalProjectionError> {
        seed.check()
            .map_err(|_| ExternalProjectionError::Admission)?;
        let bindings = seed.bindings();
        let versions = seed.versions();
        if bindings.len() != 3 || versions.len() > bindings.len() {
            return Err(ExternalProjectionError::Invalid);
        }
        let mut unique_records = HashSet::new();
        let mut changes = Vec::with_capacity(bindings.len());
        for binding in bindings {
            if !unique_records.insert(binding.record_id) {
                return Err(ExternalProjectionError::Invalid);
            }
            let matching: Vec<_> = versions
                .iter()
                .filter(|v| v.record_id == binding.record_id)
                .collect();
            let version = match matching.as_slice() {
                [version] => Some(metadata_only_version(version)?),
                [] if binding.kind == FileNodeKind::Directory => None,
                _ => return Err(ExternalProjectionError::Invalid),
            };
            let next = ExternalRecordProjection {
                binding: binding.clone(),
                version,
            };
            validate_projection(&next)?;
            changes.push(ProjectionChange {
                next,
                expected: None,
            });
        }
        if versions
            .iter()
            .any(|version| !unique_records.contains(&version.record_id))
        {
            return Err(ExternalProjectionError::Invalid);
        }
        seed.check()
            .map_err(|_| ExternalProjectionError::Admission)?;
        Ok(Self {
            changes,
            admission: ProjectionAdmission::Seed(seed),
        })
    }

    pub(crate) fn from_observed(
        completion: &'a ObservedExternalReplacement,
        entry: &'a RecordTransactionJournalEntry,
    ) -> Result<Self, ExternalProjectionError> {
        completion
            .check_entry(entry)
            .map_err(|_| ExternalProjectionError::Admission)?;
        entry
            .validate()
            .map_err(|_| ExternalProjectionError::Invalid)?;
        let expected = match &entry.transaction.manifest.operation {
            FileMutationOperation::ReplacementRename {
                source_parent,
                source_name,
                destination_parent,
                destination_name,
                source_record,
                replaced_record,
            } => vec![
                (*source_record, *source_parent, source_name.clone()),
                (
                    *replaced_record,
                    *destination_parent,
                    destination_name.clone(),
                ),
            ],
            FileMutationOperation::AtomicSave {
                parent_record,
                document_name,
                temporary_name,
                document_record,
                temporary_record,
                ..
            } => vec![
                (*document_record, *parent_record, document_name.clone()),
                (*temporary_record, *parent_record, temporary_name.clone()),
            ],
            _ => return Err(ExternalProjectionError::Invalid),
        };
        let mut changes = Vec::with_capacity(2);
        for participant in &entry.transaction.manifest.participants {
            let (_, parent, name) = expected
                .iter()
                .find(|(record, _, _)| *record == participant.record_id)
                .ok_or(ExternalProjectionError::Invalid)?;
            if participant
                .proposed_content_revision
                .as_ref()
                .is_none_or(|revision| {
                    revision.content_hash.is_some()
                        || revision.record_id != participant.record_id
                        || revision.version_id != participant.proposed_version.id
                        || revision.metadata_hash != participant.proposed_version.metadata_hash
                })
            {
                return Err(ExternalProjectionError::Invalid);
            }
            let next = ExternalRecordProjection {
                binding: participant.proposed_binding.clone(),
                version: Some(metadata_only_version(&participant.proposed_version)?),
            };
            validate_projection(&next)?;
            changes.push(ProjectionChange {
                next,
                expected: Some(ExpectedProjection {
                    parent: *parent,
                    name: name.clone(),
                    version: participant.expected_version,
                }),
            });
        }
        completion
            .check_entry(entry)
            .map_err(|_| ExternalProjectionError::Admission)?;
        Ok(Self {
            changes,
            admission: ProjectionAdmission::Observed(completion, entry),
        })
    }
}

fn metadata_only_version(
    version: &FileVersionV1,
) -> Result<FileVersionId, ExternalProjectionError> {
    if version.id.0.is_nil() || version.content_hash.is_some() || version.validate().is_err() {
        return Err(ExternalProjectionError::Invalid);
    }
    Ok(version.id)
}

fn validate_projection(value: &ExternalRecordProjection) -> Result<(), ExternalProjectionError> {
    if value.binding.validate().is_err()
        || value.version.is_some_and(|version| version.0.is_nil())
        || (value.binding.kind == FileNodeKind::File && value.version.is_none())
    {
        return Err(ExternalProjectionError::Invalid);
    }
    Ok(())
}

impl Namespace {
    /// Exact canonical identity query only. The caller must independently verify
    /// the returned binding/version against the authenticated record repository.
    pub(crate) fn external_record(
        &self,
        record: FileRecordId,
    ) -> Result<Option<ExternalRecordProjection>, ExternalProjectionError> {
        let Some(map) = self.external_projection_map()? else {
            return Ok(None);
        };
        let values = self
            .doc
            .get_all(&map, record.0.to_string())
            .map_err(|_| ExternalProjectionError::Invalid)?;
        match values.as_slice() {
            [] => Ok(None),
            [(Value::Scalar(value), _)] => {
                let ScalarValue::Bytes(bytes) = value.as_ref() else {
                    return Err(ExternalProjectionError::Invalid);
                };
                if bytes.len() > MAX_EXTERNAL_RECORD_BYTES {
                    return Err(ExternalProjectionError::Limit);
                }
                let value: ExternalRecordProjection =
                    collab_types::canonical::from_canonical_bytes(bytes)
                        .map_err(|_| ExternalProjectionError::Invalid)?;
                validate_projection(&value)?;
                if value.binding.record_id != record {
                    return Err(ExternalProjectionError::Invalid);
                }
                Ok(Some(value))
            }
            _ => Err(ExternalProjectionError::Invalid),
        }
    }

    fn external_projection_map(&self) -> Result<Option<ObjId>, ExternalProjectionError> {
        let values = self
            .doc
            .get_all(ROOT, EXTERNAL_RECORDS_KEY)
            .map_err(|_| ExternalProjectionError::Invalid)?;
        match values.as_slice() {
            [] => Ok(None),
            [(Value::Object(ObjType::Map), object)] => Ok(Some(object.clone())),
            _ => Err(ExternalProjectionError::Invalid),
        }
    }

    /// Publish a complete metadata projection on a clone, retaining the same
    /// namespace document/delta while allocating no FUSE inode or visible node.
    pub(crate) fn apply_external_projection(
        &mut self,
        batch: &ExternalProjectionBatch<'_>,
    ) -> Result<Option<Vec<u8>>, ExternalProjectionError> {
        batch.check_admission()?;
        let mut candidate = self.clone();
        let existing_map = candidate.external_projection_map()?;
        let count = existing_map
            .as_ref()
            .map_or(0, |map| candidate.doc.keys(map).count());
        let mut new_records = 0usize;
        let mut updates = Vec::new();
        for change in &batch.changes {
            let current = candidate.external_record(change.next.binding.record_id)?;
            if current.as_ref() == Some(&change.next) {
                continue;
            }
            match (&change.expected, &current) {
                (None, None) => {
                    new_records = new_records
                        .checked_add(1)
                        .ok_or(ExternalProjectionError::Limit)?;
                }
                (Some(expected), Some(current))
                    if current.binding.parent == Some(expected.parent)
                        && current.binding.name == expected.name
                        && !current.binding.tombstoned
                        && current.binding.kind == change.next.binding.kind
                        && current.version == Some(expected.version) => {}
                _ => return Err(ExternalProjectionError::Precondition),
            }
            let bytes = collab_types::canonical_bytes(&change.next)
                .map_err(|_| ExternalProjectionError::Invalid)?;
            if bytes.len() > MAX_EXTERNAL_RECORD_BYTES {
                return Err(ExternalProjectionError::Limit);
            }
            updates.push((change.next.binding.record_id.0.to_string(), bytes));
        }
        if updates.is_empty() {
            batch.check_admission()?;
            return Ok(None);
        }
        if count
            .checked_add(new_records)
            .ok_or(ExternalProjectionError::Limit)?
            > MAX_EXTERNAL_RECORDS
        {
            return Err(ExternalProjectionError::Limit);
        }
        let map = match existing_map {
            Some(map) => map,
            None => candidate
                .doc
                .put_object(ROOT, EXTERNAL_RECORDS_KEY, ObjType::Map)
                .map_err(|_| ExternalProjectionError::Persistence)?,
        };
        for (record, bytes) in updates {
            candidate
                .doc
                .put(&map, record, ScalarValue::Bytes(bytes))
                .map_err(|_| ExternalProjectionError::Persistence)?;
        }
        let delta = candidate.save_incremental();
        batch.check_admission()?;
        *self = candidate;
        Ok(Some(delta))
    }
}
