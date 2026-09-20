//! Metadata-only Directory comparison retains its native owner through both append cuts.
use super::DirectoryOutcome;
use crate::connection::store::{
    ConnectionStore, DirectoryAncestorPatchV1, LiveOperation, NativeOperationKind, OperationId,
    StoreError,
};
use std::{
    sync::{Weak, atomic::AtomicBool},
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::connection) struct DirectoryBaselineValue {
    _private: (),
}
trait DirectoryEvidence {
    fn fresh(
        &mut self,
        store: &ConnectionStore,
        live: &LiveOperation,
        cancel: &AtomicBool,
    ) -> Result<DirectoryBaselineValue, StoreError>;
    fn deadline(&self) -> Instant;
    #[cfg(test)]
    fn revoke_for_test(&mut self);
}
impl DirectoryEvidence for DirectoryOutcome<'_> {
    fn fresh(
        &mut self,
        store: &ConnectionStore,
        live: &LiveOperation,
        cancel: &AtomicBool,
    ) -> Result<DirectoryBaselineValue, StoreError> {
        self.validate(live)?;
        self.observe_leaf(store, live, cancel)?;
        self.validate(live)?;
        Ok(DirectoryBaselineValue { _private: () })
    }
    fn deadline(&self) -> Instant {
        self.deadline()
    }
    #[cfg(test)]
    fn revoke_for_test(&mut self) {
        self.revoke_for_test();
    }
}
pub(in crate::connection) struct VerifiedDirectoryComparison<'a> {
    evidence: &'a mut dyn DirectoryEvidence,
    receipt: Option<DirectoryAncestorPatchV1>,
    id: OperationId,
    revision: u64,
    fence: u64,
    lifetime: Weak<()>,
    value: DirectoryBaselineValue,
}
impl DirectoryOutcome<'_> {
    pub(in crate::connection::transaction) fn comparison<'a>(
        &'a mut self,
        store: &ConnectionStore,
        live: &LiveOperation,
        cancel: &AtomicBool,
    ) -> Result<VerifiedDirectoryComparison<'a>, StoreError> {
        let receipt = self.comparison_ancestors(store, live, cancel)?;
        let value = DirectoryEvidence::fresh(self, store, live, cancel)?;
        Ok(VerifiedDirectoryComparison {
            evidence: self,
            receipt,
            id: live.id(),
            revision: live.revision(),
            fence: live.fence(),
            lifetime: live.lifetime().clone(),
            value,
        })
    }
}
impl VerifiedDirectoryComparison<'_> {
    pub(in crate::connection) fn validate(
        &mut self,
        store: &ConnectionStore,
        live: &LiveOperation,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        if live.native_kind() != Some(NativeOperationKind::DirectoryV1)
            || self.id != live.id()
            || self.revision != live.revision()
            || self.fence != live.fence()
            || !self.lifetime.ptr_eq(live.lifetime())
            || self.lifetime.strong_count() == 0
        {
            return Err(StoreError::StaleRevision);
        }
        if self.evidence.fresh(store, live, cancel)? != self.value {
            return Err(StoreError::RecoveryRequired);
        }
        Ok(())
    }
    pub(in crate::connection) fn value(&self) -> DirectoryBaselineValue {
        self.value
    }
    pub(in crate::connection) fn receipt(&self) -> Option<&DirectoryAncestorPatchV1> {
        self.receipt.as_ref()
    }
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.evidence.deadline()
    }
    #[cfg(test)]
    pub(in crate::connection) fn revoke_for_test(&mut self) {
        self.evidence.revoke_for_test();
    }
}
