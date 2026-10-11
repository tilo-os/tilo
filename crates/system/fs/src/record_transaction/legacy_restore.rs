//! Unsigned legacy intent in the existing transaction journal. This preserves
//! the legacy namespace/history owners; it never fabricates a signed actor key.
use super::{
    Deserialize, MAX_RECORD_TRANSACTION_ENTRIES, MAX_RECORD_TRANSACTION_ENTRY_BYTES, Path,
    RecordTransactionJournal, RecordTransactionJournalError, RecordTransactionPhase, Serialize,
    Uuid, entry_path, persist_entry,
};
use fs_protocol::files::{CheckedHistoryRequestV1, FileRestoreReceiptV1};
use fs_protocol::{FileProvenance, FileVersionV1};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegacyRestoreEntry {
    pub schema_version: u16,
    pub request: CheckedHistoryRequestV1,
    pub receipt: FileRestoreReceiptV1,
    pub version: FileVersionV1,
    pub phase: RecordTransactionPhase,
    /// The bounded record delta is prepared before admission and replayed into
    /// current namespace state, never installed as a whole namespace snapshot.
    pub namespace_delta: Vec<u8>,
}

impl LegacyRestoreEntry {
    pub fn validate(&self) -> Result<(), RecordTransactionJournalError> {
        if self.schema_version != 2 {
            return Err(RecordTransactionJournalError::UnsupportedVersion(
                self.schema_version,
            ));
        }
        self.request
            .validate()
            .map_err(|_| RecordTransactionJournalError::InvalidTransaction)?;
        self.version
            .validate()
            .map_err(|_| RecordTransactionJournalError::InvalidTransaction)?;
        self.receipt
            .prior_revision
            .validate()
            .map_err(|_| RecordTransactionJournalError::InvalidTransaction)?;
        self.receipt
            .restored_revision
            .validate()
            .map_err(|_| RecordTransactionJournalError::InvalidTransaction)?;
        let action_matches = match self.request.operation {
            fs_protocol::files::CheckedHistoryOperationV1::Restore { version_id } => {
                self.receipt.selected_version == version_id && self.receipt.compensates.is_none()
            }
            fs_protocol::files::CheckedHistoryOperationV1::Undo { restore_intent } => {
                self.receipt.compensates == Some(restore_intent)
            }
            fs_protocol::files::CheckedHistoryOperationV1::Redo { undo_intent } => {
                self.receipt.compensates == Some(undo_intent)
            }
        };
        if !action_matches
            || self.receipt.schema_version != 1
            || self.request.intent_id != self.receipt.intent_id
            || self.request.review.workspace_id != self.receipt.workspace_id
            || self.request.review.mount_lifetime != self.receipt.mount_lifetime
            || self.request.review.record_id != self.receipt.record_id
            || self.version.record_id != self.receipt.record_id
            || self.version.id != self.receipt.restored_revision.version_id
            || self.version.provenance
                != (FileProvenance::Restore {
                    from: self.receipt.selected_version,
                })
            || self.version.content_hash != self.receipt.restored_revision.content_hash
            || self.version.metadata_hash != self.receipt.restored_revision.metadata_hash
            || self.receipt.prior_revision.record_id != self.version.record_id
            || self.receipt.restored_revision.record_id != self.version.record_id
            || !self
                .version
                .parents
                .contains(&self.receipt.prior_revision.version_id)
            || self.namespace_delta.len() > MAX_RECORD_TRANSACTION_ENTRY_BYTES / 2
        {
            return Err(RecordTransactionJournalError::InvalidTransaction);
        }
        Ok(())
    }
}

impl RecordTransactionJournal {
    pub(crate) fn ensure_checked_intent_durable(
        &self,
        id: Uuid,
    ) -> Result<(), RecordTransactionJournalError> {
        if let Some(entry) = self.legacy_restores.get(&id) {
            return persist_legacy(&self.root, entry);
        }
        persist_entry(
            &self.root,
            self.entries
                .get(&id)
                .ok_or(RecordTransactionJournalError::UnknownTransaction(id))?,
        )
    }

    pub(crate) fn retained_count(&self) -> usize {
        self.entries
            .len()
            .saturating_add(self.legacy_restores.len())
    }

    pub(crate) fn checked_request(&self, id: Uuid) -> Option<CheckedHistoryRequestV1> {
        if let Some(entry) = self.legacy_restores.get(&id) {
            return Some(entry.request);
        }
        let fs_protocol::FileMutationOperation::RestoreContent { request, .. } =
            &self.entries.get(&id)?.transaction.manifest.operation
        else {
            return None;
        };
        Some(*request)
    }

    pub(crate) fn checked_receipt(&self, id: Uuid) -> Option<FileRestoreReceiptV1> {
        if let Some(entry) = self.legacy_restores.get(&id) {
            return Some(entry.receipt.clone());
        }
        let entry = self.entries.get(&id)?;
        let fs_protocol::FileMutationOperation::RestoreContent {
            request,
            selected_version,
            prior_revision,
            compensates,
        } = &entry.transaction.manifest.operation
        else {
            return None;
        };
        Some(FileRestoreReceiptV1 {
            schema_version: 1,
            intent_id: id,
            workspace_id: request.review.workspace_id,
            mount_lifetime: request.review.mount_lifetime,
            record_id: request.review.record_id,
            selected_version: *selected_version,
            prior_revision: *prior_revision,
            prior_reference: fs_protocol::files::FileRestorePriorReferenceV1::ObservedRevision,
            restored_revision: entry
                .transaction
                .manifest
                .participants
                .first()?
                .proposed_content_revision?,
            compensates: *compensates,
        })
    }

    pub(crate) fn checked_phase(&self, id: Uuid) -> Option<RecordTransactionPhase> {
        self.legacy_restores
            .get(&id)
            .map(|entry| entry.phase)
            .or_else(|| self.entries.get(&id).map(|entry| entry.phase))
    }

    pub(crate) fn legacy_entries(&self) -> impl Iterator<Item = &LegacyRestoreEntry> {
        self.legacy_restores.values()
    }

    pub(crate) fn insert_legacy_restore(
        &mut self,
        entry: LegacyRestoreEntry,
    ) -> Result<(), RecordTransactionJournalError> {
        entry.validate()?;
        let id = entry.request.intent_id;
        if entry.phase != RecordTransactionPhase::Prepared {
            return Err(RecordTransactionJournalError::InvalidInitialPhase);
        }
        if let Some(existing) = self.legacy_restores.get(&id) {
            return if existing == &entry {
                Ok(())
            } else {
                Err(RecordTransactionJournalError::TransactionIdCollision(id))
            };
        }
        if self.entries.contains_key(&id) {
            return Err(RecordTransactionJournalError::TransactionIdCollision(id));
        }
        if self.retained_count() >= MAX_RECORD_TRANSACTION_ENTRIES {
            return Err(RecordTransactionJournalError::TooManyEntries);
        }
        let result = persist_legacy(&self.root, &entry);
        self.legacy_restores.insert(id, entry);
        result?;
        Ok(())
    }

    pub(super) fn advance_legacy(
        &mut self,
        id: Uuid,
        phase: RecordTransactionPhase,
        delta: Option<&[u8]>,
    ) -> Result<bool, RecordTransactionJournalError> {
        let current = self
            .legacy_restores
            .get(&id)
            .ok_or(RecordTransactionJournalError::UnknownTransaction(id))?;
        if delta.is_some_and(|delta| delta != current.namespace_delta.as_slice()) {
            return Err(RecordTransactionJournalError::InvalidNamespaceDelta);
        }
        if current.phase == phase {
            return Ok(false);
        }
        if current.phase.successor() != Some(phase) {
            return Err(RecordTransactionJournalError::InvalidPhaseTransition {
                current: current.phase,
                proposed: phase,
            });
        }
        let mut next = current.clone();
        next.phase = phase;
        next.validate()?;
        persist_legacy(&self.root, &next)?;
        self.legacy_restores.insert(id, next);
        Ok(true)
    }
}

fn persist_legacy(
    root: &Path,
    entry: &LegacyRestoreEntry,
) -> Result<(), RecordTransactionJournalError> {
    let bytes = serde_json::to_vec(entry)?;
    if bytes.len() > MAX_RECORD_TRANSACTION_ENTRY_BYTES {
        return Err(RecordTransactionJournalError::EntryTooLarge);
    }
    tilo_files::native::atomic::write_creating_parent(
        &entry_path(root, entry.request.intent_id),
        &bytes,
    )?;
    Ok(())
}
