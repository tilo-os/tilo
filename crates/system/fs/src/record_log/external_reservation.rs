//! Durable canonical ID reservation for one actual native replacement.
//! The operation UUID and digest are descriptive. Only the daemon's private
//! authenticated pending-capability coordinator may use these IDs to publish.
//! Native inode hints never restore previous canonical identity here.
use super::{RecordLog, RecordLogError};
use fs_protocol::{FileRecordId, FileVersionId};
use redb::{ReadableTable, ReadableTableMetadata, TableDefinition};
use uuid::Uuid;

const RESERVATIONS: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("external_native_operation_records_v1");
const MAX_RESERVATIONS: u64 = 1024;
const MAX_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ExternalCanonicalIds {
    pub parent: FileRecordId,
    pub source: FileRecordId,
    pub source_initial: FileVersionId,
    pub source_proposed: FileVersionId,
    pub victim: FileRecordId,
    pub victim_initial: FileVersionId,
    pub victim_proposed: FileVersionId,
    pub observed_at_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Reservation {
    schema: u32,
    workspace: Uuid,
    person: [u8; 32],
    source_owner: [u8; 32],
    operation: Uuid,
    intent: [u8; 32],
    archive_plan: [u8; 32],
    ids: ExternalCanonicalIds,
    previous: Option<PreviousCanonicalIds>,
}
/// Previous IDs are accepted only from the actual live coordinator's held
/// signed commit, separately rechecked against current source/pin admission.
/// Deserializing this pair cannot establish that admission.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PreviousCanonicalIds {
    pub parent: FileRecordId,
    pub selected: FileRecordId,
    pub version: FileVersionId,
}
pub(crate) struct ExternalReservationInput {
    pub workspace: Uuid,
    pub person: [u8; 32],
    pub source_owner: [u8; 32],
    pub operation: Uuid,
    pub intent: [u8; 32],
    pub archive_plan: [u8; 32],
    pub previous: Option<PreviousCanonicalIds>,
}
impl RecordLog {
    pub(crate) fn reserve_external_operation(
        &self,
        input: &ExternalReservationInput,
    ) -> Result<ExternalCanonicalIds, RecordLogError> {
        if input.workspace.is_nil()
            || input.operation.is_nil()
            || input.person == [0; 32]
            || input.source_owner == [0; 32]
            || input.intent == [0; 32]
            || input.archive_plan == [0; 32]
            || input.previous.as_ref().is_some_and(|p| {
                p.parent.0.is_nil() || p.selected.0.is_nil() || p.version.0.is_nil()
            })
        {
            return Err(RecordLogError::Corrupt);
        }
        let key = postcard::to_allocvec(&(
            1u32,
            input.workspace,
            input.person,
            input.source_owner,
            input.operation,
        ))
        .map_err(|_| RecordLogError::Corrupt)?;
        let write = self.db.begin_write()?;
        let ids = {
            let mut table = write.open_table(RESERVATIONS)?;
            let existing = table.get(key.as_slice())?.map(|v| v.value().to_vec());
            match existing {
                Some(bytes) => {
                    if bytes.len() > MAX_BYTES {
                        return Err(RecordLogError::Corrupt);
                    }
                    let stored: Reservation =
                        postcard::from_bytes(&bytes).map_err(|_| RecordLogError::Corrupt)?;
                    if stored.schema != 1
                        || stored.workspace != input.workspace
                        || stored.person != input.person
                        || stored.source_owner != input.source_owner
                        || stored.operation != input.operation
                        || stored.intent != input.intent
                        || stored.archive_plan != input.archive_plan
                        || stored.previous != input.previous
                        || postcard::to_allocvec(&stored).map_err(|_| RecordLogError::Corrupt)?
                            != bytes
                        || input.previous.as_ref().is_some_and(|p| {
                            stored.ids.parent != p.parent
                                || stored.ids.victim != p.selected
                                || stored.ids.victim_initial != p.version
                        })
                        || (input.previous.is_none() && stored.ids.source == stored.ids.victim)
                        || [
                            stored.ids.parent.0,
                            stored.ids.source.0,
                            stored.ids.victim.0,
                            stored.ids.source_initial.0,
                            stored.ids.source_proposed.0,
                            stored.ids.victim_initial.0,
                            stored.ids.victim_proposed.0,
                        ]
                        .iter()
                        .any(Uuid::is_nil)
                    {
                        return Err(RecordLogError::Corrupt);
                    }
                    let record_ids = [
                        stored.ids.parent.0,
                        stored.ids.source.0,
                        stored.ids.victim.0,
                    ];
                    let versions = [
                        stored.ids.source_initial.0,
                        stored.ids.source_proposed.0,
                        stored.ids.victim_initial.0,
                        stored.ids.victim_proposed.0,
                    ];
                    if record_ids
                        .into_iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != 3
                        || versions
                            .into_iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            != 4
                    {
                        return Err(RecordLogError::Corrupt);
                    }
                    stored.ids
                }
                None => {
                    if table.len()? >= MAX_RESERVATIONS {
                        return Err(RecordLogError::Corrupt);
                    }
                    let ids = ExternalCanonicalIds {
                        parent: input
                            .previous
                            .as_ref()
                            .map_or_else(FileRecordId::new, |p| p.parent),
                        source: FileRecordId::new(),
                        source_initial: FileVersionId::new(),
                        source_proposed: FileVersionId::new(),
                        victim: input
                            .previous
                            .as_ref()
                            .map_or_else(FileRecordId::new, |p| p.selected),
                        victim_initial: input
                            .previous
                            .as_ref()
                            .map_or_else(FileVersionId::new, |p| p.version),
                        victim_proposed: FileVersionId::new(),
                        observed_at_ms: crate::clock::unix_now_ms(),
                    };
                    let record = Reservation {
                        schema: 1,
                        workspace: input.workspace,
                        person: input.person,
                        source_owner: input.source_owner,
                        operation: input.operation,
                        intent: input.intent,
                        archive_plan: input.archive_plan,
                        ids: ids.clone(),
                        previous: input.previous.clone(),
                    };
                    let bytes =
                        postcard::to_allocvec(&record).map_err(|_| RecordLogError::Corrupt)?;
                    if bytes.len() > MAX_BYTES {
                        return Err(RecordLogError::Corrupt);
                    }
                    table.insert(key.as_slice(), bytes.as_slice())?;
                    ids
                }
            }
        };
        write.commit()?;
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_record_log_reservation_survives_reopen_and_refuses_changed_intent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record-log.redb");
        let mut input = ExternalReservationInput {
            workspace: Uuid::new_v4(),
            person: [1; 32],
            source_owner: [2; 32],
            operation: Uuid::new_v4(),
            intent: [3; 32],
            archive_plan: [4; 32],
            previous: None,
        };
        let log = RecordLog::open(&path).unwrap();
        let first = log.reserve_external_operation(&input).unwrap();
        assert_eq!(log.reserve_external_operation(&input).unwrap(), first);
        drop(log);
        let log = RecordLog::open(&path).unwrap();
        assert_eq!(log.reserve_external_operation(&input).unwrap(), first);
        input.intent = [5; 32];
        assert!(log.reserve_external_operation(&input).is_err());
        input.intent = [3; 32];
        input.operation = Uuid::new_v4();
        // This tests durable ID vocabulary only. A descriptive predecessor does
        // not construct coordinator/source admission or authorize publication.
        input.previous = Some(PreviousCanonicalIds {
            parent: first.parent,
            selected: first.source,
            version: first.source_proposed,
        });
        let next = log.reserve_external_operation(&input).unwrap();
        assert_eq!(next.parent, first.parent);
        assert_eq!(next.victim, first.source);
        assert_eq!(next.victim_initial, first.source_proposed);
        assert_ne!(next.source, first.source);
        assert_ne!(next.source_initial, first.source_initial);
    }
}
