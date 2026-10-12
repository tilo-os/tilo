//! Signed metadata is retained beside the existing receipt in the same held,
//! single-writer journal. The legacy receipt encoding stays unchanged. Only
//! an actual operation already admitted by this owner can acquire these two
//! bounded sidecars; neither file reconstructs native or person authority.
use super::*;
use crate::external_history::{CommittedReceipt, LIMIT, PreparedReceipt};
use std::ffi::OsStr;
use tilo_files::native::atomic::{Privacy, PublishMode};

impl OperationOwner {
    fn read_history_bytes(&self, id: &OperationId, kind: &str) -> Result<Vec<u8>, FsError> {
        let name = format!("{}.history-{kind}", id.native());
        let mut file = self
            .storage
            .open_read(OsStr::new(&name), Privacy::OwnerOnly)
            .map_err(io_error)?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.is_empty() || bytes.len() > LIMIT {
            return Err(FsError::StoreUnavailable);
        }
        Ok(bytes)
    }
    fn publish_history_bytes(
        &self,
        id: &OperationId,
        kind: &str,
        bytes: &[u8],
    ) -> Result<(), FsError> {
        if bytes.is_empty() || bytes.len() > LIMIT {
            return Err(FsError::LimitExceeded);
        }
        let mut store = lock(&self.state)?;
        if store.poisoned {
            return Err(FsError::StoreUnavailable);
        }
        let receipt = store.receipts.get(&id.native()).ok_or(FsError::NotFound)?;
        if receipt.item_count != 1 || receipt.steps.len() != 1 {
            return Err(FsError::Denied);
        }
        let phase = receipt.steps[0].phase.as_str();
        match kind {
            "prepare" if receipt.state == OperationState::Running && phase == "Admitted" => {}
            "commit" if matches!(phase, "ReplaceVictimCaptured" | "ReplaceHistoryOwned") => {}
            _ => return Err(FsError::Conflict),
        }
        // At most two <=4096-byte files per one of MAX_REPORTS existing
        // operations. UUID names prevent caller-controlled journal growth.
        let name = format!("{}.history-{kind}", id.native());
        match self.read_history_bytes(id, kind) {
            Ok(previous) => {
                return if previous == bytes {
                    Ok(())
                } else {
                    Err(FsError::Conflict)
                };
            }
            Err(FsError::NotFound) => {}
            Err(error) => return Err(error),
        }
        if let Err(error) = self.storage.publish(
            OsStr::new(&name),
            bytes,
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        ) {
            // An ambiguous durable write must not admit more work or permit
            // a reconstructed retry to treat the first attempt as absent.
            store.poisoned = true;
            return Err(FsError::Publication(error));
        }
        Ok(())
    }
    pub(crate) fn bind_history_prepare(
        &self,
        id: &OperationId,
        prepared: &PreparedReceipt,
    ) -> Result<(), FsError> {
        if prepared.operation_id != id.native().to_string() {
            return Err(FsError::Denied);
        }
        let bytes = serde_json::to_vec(prepared).map_err(|_| FsError::StoreUnavailable)?;
        PreparedReceipt::decode(&bytes).map_err(|_| FsError::Denied)?;
        self.publish_history_bytes(id, "prepare", &bytes)
    }
    pub(crate) fn history_prepare(&self, id: &OperationId) -> Result<PreparedReceipt, FsError> {
        let store = lock(&self.state)?;
        if store.poisoned || !store.receipts.contains_key(&id.native()) {
            return Err(FsError::StoreUnavailable);
        }
        let bytes = self.read_history_bytes(id, "prepare")?;
        let receipt = PreparedReceipt::decode(&bytes).map_err(|_| FsError::StoreUnavailable)?;
        if receipt.operation_id != id.native().to_string() {
            return Err(FsError::StoreUnavailable);
        }
        Ok(receipt)
    }
    pub(super) fn bind_history_commit(
        &self,
        id: &OperationId,
        committed: &CommittedReceipt,
    ) -> Result<(), FsError> {
        if committed.operation_id != id.native().to_string() {
            return Err(FsError::Denied);
        }
        let bytes = serde_json::to_vec(committed).map_err(|_| FsError::StoreUnavailable)?;
        CommittedReceipt::decode(&bytes).map_err(|_| FsError::Denied)?;
        self.publish_history_bytes(id, "commit", &bytes)
    }
}
impl NativeReplacementPin {
    /// Private proof retained by the actual journal/pin; only the verified
    /// commit path can publish this record. Native selected-leaf advancement
    /// rechecks it, rather than accepting a phase string as signed ownership.
    pub(crate) fn verify_owned_history(&self, current: &FsSession) -> Result<(), FsError> {
        let prepared = self.claim.owner.history_prepare(&self.operation)?;
        let committed = CommittedReceipt::decode(
            &self
                .claim
                .owner
                .read_history_bytes(&self.operation, "commit")?,
        )
        .map_err(|_| FsError::StoreUnavailable)?;
        committed
            .verify(&prepared, &current.retained_history_verifier()?)
            .map_err(|_| FsError::Denied)?;
        if committed.operation_id != self.operation.native().to_string()
            || committed.source_owner != self.source_owner
            || committed.capture_id != self.claim.recovery.to_string()
            || committed.source_identity != self.source
            || committed.victim_identity != self.victim
            || committed.destination_identity != self.destination
        {
            return Err(FsError::Denied);
        }
        let (_, step) = self.claim.owner.find_recovery(&self.capture_id())?;
        if step.phase != "ReplaceHistoryOwned" {
            return Err(FsError::Conflict);
        }
        Ok(())
    }
}
