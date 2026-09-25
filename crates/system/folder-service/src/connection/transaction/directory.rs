//! Read-only Directory proposals; a plan carries no native authority.
use super::super::model::{
    Decision, EntryState, Observation, ObservedTree, Side, VerifiedBaseline,
};
use super::super::store::OperationIntent;
use super::{ConnectionId, ConnectionStore, StoreError, StructuralPending};

pub(in crate::connection) enum DirectoryPlan {
    Ready(OperationIntent),
    StructuralPending(StructuralPending),
    Pending,
    Settled,
}

pub(in crate::connection) fn addition_side(intent: &OperationIntent) -> Result<Side, StoreError> {
    match (
        &intent.source_before,
        &intent.destination_before,
        &intent.decision,
    ) {
        (Some(EntryState::Directory), None, Decision::Propagate { from: Side::Source }) => {
            Ok(Side::Source)
        }
        (
            None,
            Some(EntryState::Directory),
            Decision::Propagate {
                from: Side::Destination,
            },
        ) => Ok(Side::Destination),
        _ => Err(StoreError::RecoveryRequired),
    }
}

pub(in crate::connection) fn ancestor_requirement(
    intent: &OperationIntent,
    baseline: &VerifiedBaseline,
    source: &ObservedTree,
    destination: &ObservedTree,
) -> Result<super::ExistingAncestorRequirement, StructuralPending> {
    let failure = |path| StructuralPending {
        path: intent.path.clone(),
        missing_baseline_ancestor: path,
    };
    let entries = baseline
        .entries()
        .ok_or_else(|| failure(intent.path.clone()))?;
    addition_side(intent).map_err(|_| failure(intent.path.clone()))?;
    let mut parent = intent.path.parent();
    while let Some(path) = parent {
        for tree in [source, destination] {
            if tree
                .entries()
                .iter()
                .find(|(key, _)| key.logical() == path.logical())
                .map(|(_, state)| state)
                != Some(&EntryState::Directory)
            {
                return Err(failure(path));
            }
        }
        parent = path.parent();
    }
    super::ExistingAncestorRequirement::from_baseline(&intent.path, entries)
        .map_err(|_| failure(intent.path.clone()))
}

#[expect(
    clippy::suspicious_operation_groupings,
    reason = "Join both fresh endpoint observations to the corresponding durable endpoint binding."
)]
pub(in crate::connection) fn plan_directory(
    store: &ConnectionStore,
    id: ConnectionId,
    baseline: &VerifiedBaseline,
    source: &ObservedTree,
    destination: &ObservedTree,
) -> Result<DirectoryPlan, StoreError> {
    let record = store.load_connection(id)?;
    if record.lifecycle != super::super::store::Lifecycle::Active {
        return Err(StoreError::Disconnected);
    }
    if store.executor_position(id)?.0.is_some() {
        return Err(StoreError::RecoveryRequired);
    }
    if !matches!(source.observation(), Observation::Complete)
        || !matches!(destination.observation(), Observation::Complete)
        || source.endpoint().identity != record.source.identity
        || destination.endpoint().identity != record.destination.identity
    {
        return Err(StoreError::RecoveryRequired);
    }
    if baseline.connection != id
        || baseline.source != record.source.identity
        || baseline.destination != record.destination.identity
        || baseline.entries().is_some() != record.baseline_revision.is_some()
    {
        return Err(StoreError::StaleRevision);
    }
    if record.baseline_revision.is_none() {
        return Err(StoreError::RecoveryRequired);
    }
    let mut plan = super::super::reconcile::reconcile(baseline, source, destination);
    plan.proposals
        .sort_by_key(|proposal| (proposal.path.components().len(), proposal.path.logical()));
    let mut pending = !plan.issues.is_empty();
    let mut structural = None;
    for proposal in plan.proposals {
        if proposal.source == proposal.destination && proposal.source == proposal.baseline {
            continue;
        }
        pending = true;
        // Existing directory deletion/restoration and equal directory adoption have separate authority.
        if proposal.baseline.is_some() {
            continue;
        }
        let intent = OperationIntent {
            connection: id,
            expected_revision: record.revision,
            expected_baseline: record.baseline_revision,
            path: proposal.path,
            source_before: proposal.source,
            destination_before: proposal.destination,
            decision: proposal.decision,
        };
        if addition_side(&intent).is_err() {
            continue;
        }
        if let Err(reason) = ancestor_requirement(&intent, baseline, source, destination) {
            structural.get_or_insert(reason);
            continue;
        }
        store.preflight_directory_baseline(&intent, baseline)?;
        if store.executor_candidate_allowed(&intent)? {
            return Ok(DirectoryPlan::Ready(intent));
        }
    }
    Ok(if let Some(reason) = structural {
        DirectoryPlan::StructuralPending(reason)
    } else if pending {
        DirectoryPlan::Pending
    } else {
        DirectoryPlan::Settled
    })
}

/// Running ownership for accounting an already-published Directory. It cannot
/// satisfy any native mutation step and never grants a second publication.
#[cfg(target_os = "linux")]
pub(in crate::connection::transaction) struct DirectoryCompletion {
    gate: std::sync::Arc<super::AdmissionGate>,
    lifetime: std::sync::Weak<()>,
    operation: super::super::store::OperationId,
    revision: u64,
}
#[cfg(target_os = "linux")]
impl super::TransactionSession {
    pub(in crate::connection::transaction) fn admit_directory_completion(
        &self,
        store: &ConnectionStore,
        live: &super::LiveOperation,
        cancel: &super::AtomicBool,
    ) -> Result<DirectoryCompletion, StoreError> {
        store.check_native(live)?;
        if live.native_kind() != Some(super::super::store::NativeOperationKind::DirectoryV1)
            || self.connection != live.id().connection
            || !self.lifetime.ptr_eq(live.lifetime())
        {
            return Err(StoreError::StaleRevision);
        }
        let mut state = self
            .gate
            .state
            .lock()
            .map_err(|_| StoreError::RecoveryRequired)?;
        if state.closed
            || self.gate.stopped.load(super::Ordering::Acquire)
            || cancel.load(super::Ordering::Acquire)
        {
            return Err(StoreError::Cancelled);
        }
        if state.in_flight != 0 {
            return Err(StoreError::Busy);
        }
        state.in_flight = 1;
        drop(state);
        Ok(DirectoryCompletion {
            gate: std::sync::Arc::clone(&self.gate),
            lifetime: self.lifetime.clone(),
            operation: live.id(),
            revision: live.revision(),
        })
    }
}
#[cfg(target_os = "linux")]
impl DirectoryCompletion {
    pub(in crate::connection::transaction) fn validate(
        &self,
        live: &super::LiveOperation,
    ) -> Result<(), StoreError> {
        if self.operation != live.id()
            || self.revision != live.revision()
            || !self.lifetime.ptr_eq(live.lifetime())
            || self.lifetime.strong_count() == 0
            || self.gate.progress.load(super::Ordering::Acquire) != self.revision
        {
            return Err(StoreError::StaleRevision);
        }
        if self.gate.stopped.load(super::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
impl Drop for DirectoryCompletion {
    fn drop(&mut self) {
        if let Ok(mut state) = self.gate.state.lock() {
            state.in_flight = state.in_flight.saturating_sub(1);
        }
    }
}
