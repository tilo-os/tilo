//! The checked-restore view of the journal: a retained restore entry is the
//! durable request, receipt and phase of one checked history operation.
use super::{
    RecordTransactionJournal, RecordTransactionJournalError, RecordTransactionPhase, Uuid,
    persist_entry,
};
use fs_protocol::files::{CheckedHistoryRequestV1, FileRestoreReceiptV1};

impl RecordTransactionJournal {
    pub(crate) fn ensure_checked_intent_durable(
        &self,
        id: Uuid,
    ) -> Result<(), RecordTransactionJournalError> {
        persist_entry(
            &self.root,
            self.entries
                .get(&id)
                .ok_or(RecordTransactionJournalError::UnknownTransaction(id))?,
        )
    }

    pub(crate) fn retained_count(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn checked_request(&self, id: Uuid) -> Option<CheckedHistoryRequestV1> {
        let fs_protocol::FileMutationOperation::RestoreContent { request, .. } =
            &self.entries.get(&id)?.transaction.manifest.operation
        else {
            return None;
        };
        Some(*request)
    }

    pub(crate) fn checked_receipt(&self, id: Uuid) -> Option<FileRestoreReceiptV1> {
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
        self.entries.get(&id).map(|entry| entry.phase)
    }
}
