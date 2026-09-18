//! Private transaction admission. Durable facts are never mutation authority.
pub(super) mod native;
#[cfg(test)]
mod tests;

use super::{
    model::ConnectionId,
    store::{ConnectionStore, LiveOperation, StoreError},
};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Read-only complete planning preview. Observation never implies acceptance.
pub(super) struct InitialPreview {
    record: super::store::ConnectionRecord,
    lifetime: Weak<()>,
    digest: [u8; 32],
    source: super::model::ObservedTree,
    destination: super::model::ObservedTree,
}
pub(super) struct ReviewedInitial {
    record: super::store::ConnectionRecord,
    lifetime: Weak<()>,
    preview: [u8; 32],
    selection: super::store::OperationIntent,
}
impl InitialPreview {
    fn from_observations(
        store: &ConnectionStore,
        id: ConnectionId,
        source: super::model::ObservedTree,
        destination: super::model::ObservedTree,
    ) -> Result<Self, StoreError> {
        let (record, lifetime, digest) = store.initial_review_basis(id, &source, &destination)?;
        Ok(Self {
            record,
            lifetime,
            digest,
            source,
            destination,
        })
    }
    /// Called only for the explicitly accepted preview/selection, never by observation.
    pub(super) fn accept(
        self,
        selection: super::store::OperationIntent,
    ) -> Result<ReviewedInitial, StoreError> {
        if selection.connection != self.record.id
            || selection.expected_revision != self.record.revision
            || selection.expected_baseline.is_some()
            || self.source.entries().get(&selection.path) != selection.source_before.as_ref()
            || self.destination.entries().get(&selection.path)
                != selection.destination_before.as_ref()
            || matches!(
                selection.decision,
                super::model::Decision::RecoverDeletion { .. }
                    | super::model::Decision::Pending(_)
                    | super::model::Decision::InitialPreview
            )
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(ReviewedInitial {
            record: self.record,
            lifetime: self.lifetime,
            preview: self.digest,
            selection,
        })
    }
}
impl ReviewedInitial {
    pub(super) fn regular_prerequisite(
        &self,
        store: &ConnectionStore,
    ) -> Result<Option<StructuralPending>, StoreError> {
        self.validate(store, &self.selection)?;
        let absent = super::model::VerifiedBaseline::absent(
            self.record.id,
            self.record.source.identity,
            self.record.destination.identity,
        );
        Ok(missing_baseline_ancestor(&self.selection.path, &absent))
    }
    pub(super) fn validate(
        &self,
        store: &ConnectionStore,
        intent: &super::store::OperationIntent,
    ) -> Result<[u8; 32], StoreError> {
        if &self.selection != intent
            || self.lifetime.strong_count() == 0
            || !store.same_native_lifetime(&self.lifetime)
            || store.load_connection(intent.connection)? != self.record
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(self.preview)
    }
}
#[cfg(test)]
pub(super) fn initial_preview_for_test(
    store: &ConnectionStore,
    id: ConnectionId,
    source: super::model::ObservedTree,
    destination: super::model::ObservedTree,
) -> Result<InitialPreview, StoreError> {
    InitialPreview::from_observations(store, id, source, destination)
}

/// Closes admission without waiting for the serialized store owner or its I/O.
/// A closed latch is only a request; the store's durable fence acknowledges it.
pub(super) struct AdmissionGate {
    state: Mutex<GateState>,
    stopped: AtomicBool,
    progress: AtomicU64,
}
#[derive(Default)]
struct GateState {
    closed: bool,
    in_flight: usize,
}
impl AdmissionGate {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            stopped: AtomicBool::new(false),
            progress: AtomicU64::new(0),
        }
    }
    pub(super) fn require_quiescent(&self) -> Result<(), StoreError> {
        let state = self
            .state
            .lock()
            .map_err(|_| StoreError::RecoveryRequired)?;
        if state.in_flight != 0 {
            return Err(StoreError::Busy);
        }
        drop(state);
        Ok(())
    }
    pub(super) fn close(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
        }
    }
    pub(super) fn stopped(&self) -> &AtomicBool {
        &self.stopped
    }
    pub(super) fn set_progress(&self, revision: u64) {
        self.progress.store(revision, Ordering::Release);
    }
}
pub(super) struct TransactionSession {
    connection: ConnectionId,
    lifetime: Weak<()>,
    gate: Arc<AdmissionGate>,
}
impl TransactionSession {
    pub(super) fn new(
        connection: ConnectionId,
        lifetime: Weak<()>,
        gate: Arc<AdmissionGate>,
    ) -> Self {
        Self {
            connection,
            lifetime,
            gate,
        }
    }
    pub(super) fn request_disconnect(&self) {
        self.gate.close();
    }
    pub(super) fn validate(
        &self,
        connection: ConnectionId,
        lifetime: &Arc<()>,
    ) -> Result<(), StoreError> {
        if self.connection != connection
            || !self.lifetime.ptr_eq(&Arc::downgrade(lifetime))
            || self.lifetime.strong_count() == 0
        {
            return Err(StoreError::StaleRevision);
        }
        if self.gate.stopped.load(Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        Ok(())
    }
    pub(super) fn require_idle(&self) -> Result<(), StoreError> {
        let state = self
            .gate
            .state
            .lock()
            .map_err(|_| StoreError::RecoveryRequired)?;
        if state.closed || self.gate.stopped.load(Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        if state.in_flight != 0 {
            return Err(StoreError::Busy);
        }
        drop(state);
        Ok(())
    }
    pub(super) fn admit_step(
        &self,
        store: &ConnectionStore,
        live: &LiveOperation,
        kind: StepKind,
        cancel: &AtomicBool,
    ) -> Result<StepAdmission, StoreError> {
        store.check_native(live)?;
        if !live.action().is_reconciliation()
            || live.intent().decision == super::model::Decision::VerifyEqual
            || (matches!(
                live.intent().decision,
                super::model::Decision::RecoverDeletion { .. }
            ) && !live.deletion_baseline_checked())
        {
            return Err(StoreError::RecoveryRequired);
        }
        self.admit_current(live, kind, cancel)
    }
    #[cfg(target_os = "linux")]
    fn admit_selected_step(
        &self,
        store: &ConnectionStore,
        live: &LiveOperation,
        proof: &mut native::NativeRestore<'_>,
        kind: StepKind,
        cancel: &AtomicBool,
    ) -> Result<StepAdmission, StoreError> {
        proof.validate(store, live)?;
        self.admit_current(live, kind, cancel)
    }
    fn admit_current(
        &self,
        live: &LiveOperation,
        kind: StepKind,
        cancel: &AtomicBool,
    ) -> Result<StepAdmission, StoreError> {
        if self.connection != live.id().connection || !self.lifetime.ptr_eq(live.lifetime()) {
            return Err(StoreError::StaleRevision);
        }
        let mut state = self
            .gate
            .state
            .lock()
            .map_err(|_| StoreError::RecoveryRequired)?;
        if state.closed
            || self.gate.stopped.load(Ordering::Acquire)
            || cancel.load(Ordering::Acquire)
        {
            return Err(StoreError::Cancelled);
        }
        if state.in_flight != 0 {
            return Err(StoreError::Busy);
        }
        state.in_flight = 1;
        drop(state);
        Ok(StepAdmission {
            gate: Arc::clone(&self.gate),
            lifetime: self.lifetime.clone(),
            operation: live.id(),
            revision: live.revision(),
            kind,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StepKind {
    Namespace,
    Stage,
    Relocate,
    Preserve,
    Publish,
}
/// One admitted step only; construction is confined to the transaction owner.
pub(super) struct StepAdmission {
    gate: Arc<AdmissionGate>,
    lifetime: Weak<()>,
    operation: super::store::OperationId,
    revision: u64,
    kind: StepKind,
}
impl StepAdmission {
    pub(super) fn validate(
        &self,
        id: super::store::OperationId,
        lifetime: &Weak<()>,
        revision: u64,
        kind: StepKind,
    ) -> Result<(), StoreError> {
        if self.operation != id
            || !self.lifetime.ptr_eq(lifetime)
            || self.lifetime.strong_count() == 0
            || self.revision != revision
            || self.kind != kind
            || self.gate.progress.load(Ordering::Acquire) != revision
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(())
    }
    pub(super) fn stopped(&self) -> &AtomicBool {
        self.gate.stopped()
    }
}
impl Drop for StepAdmission {
    fn drop(&mut self) {
        if let Ok(mut state) = self.gate.state.lock() {
            state.in_flight = state.in_flight.saturating_sub(1);
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn maybe_run_lease_receiver(args: &[String]) -> Option<i32> {
    native::linux::maybe_run_lease_receiver(args)
}

/// Explicit reason for the later directory-continuation owner/UI. No observation
/// of a directory can silently create its durable baseline authority here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StructuralPending {
    pub(super) path: super::model::RelativePath,
    pub(super) missing_baseline_ancestor: super::model::RelativePath,
}
fn missing_baseline_ancestor(
    path: &super::model::RelativePath,
    baseline: &super::model::VerifiedBaseline,
) -> Option<StructuralPending> {
    let parent = path.parent()?;
    if baseline.entries().and_then(|entries| {
        entries
            .get(&parent)
            .or_else(|| entries.get(&parent.logical()))
    }) == Some(&super::model::EntryState::Directory)
    {
        return None;
    }
    Some(StructuralPending {
        path: path.clone(),
        missing_baseline_ancestor: parent,
    })
}
/// Read-only dispatch proposal. A plan never grants native step authority.
pub(super) enum RegularPlan {
    InitialReview(InitialPreview),
    StructuralPending(StructuralPending),
    Ready(super::store::OperationIntent),
    Settled,
    Pending,
}
#[expect(
    clippy::suspicious_operation_groupings,
    reason = "Join distinct durable correlation fields to their canonical operation, endpoint or current proof; similarly named fields are not interchangeable."
)]
pub(super) fn plan_regular(
    store: &ConnectionStore,
    id: ConnectionId,
    baseline: &super::model::VerifiedBaseline,
    source: super::model::ObservedTree,
    destination: super::model::ObservedTree,
) -> Result<RegularPlan, StoreError> {
    use super::model::{Decision, EntryState, Observation};
    let record = store.load_connection(id)?;
    if record.lifecycle != super::store::Lifecycle::Active {
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
        return Ok(RegularPlan::InitialReview(
            InitialPreview::from_observations(store, id, source, destination)?,
        ));
    }
    let plan = super::reconcile::reconcile(baseline, &source, &destination);
    let mut pending = !plan.issues.is_empty();
    let mut structural = None;
    for proposal in plan.proposals {
        if proposal.source == proposal.destination && proposal.source == proposal.baseline {
            continue;
        }
        if [&proposal.source, &proposal.destination, &proposal.baseline]
            .into_iter()
            .any(|state| {
                state
                    .as_ref()
                    .is_some_and(|state| !matches!(state, EntryState::File { .. }))
            })
            || matches!(
                proposal.decision,
                Decision::Pending(_) | Decision::InitialPreview
            )
        {
            pending = true;
            continue;
        }
        if let Some(reason) = missing_baseline_ancestor(&proposal.path, baseline) {
            structural.get_or_insert(reason);
            continue;
        }
        let intent = super::store::OperationIntent {
            connection: id,
            expected_revision: record.revision,
            expected_baseline: record.baseline_revision,
            path: proposal.path,
            source_before: proposal.source,
            destination_before: proposal.destination,
            decision: proposal.decision,
        };
        if store.executor_candidate_allowed(&intent)? {
            return Ok(RegularPlan::Ready(intent));
        }
        pending = true;
    }
    Ok(if let Some(reason) = structural {
        RegularPlan::StructuralPending(reason)
    } else if pending {
        RegularPlan::Pending
    } else {
        RegularPlan::Settled
    })
}

pub(super) enum TransactionOutcome {
    InitialReview(InitialPreview),
    StructuralPending(StructuralPending),
    Verified {
        operation: super::store::OperationReceipt,
        up_to_date: bool,
    },
    Conflict(super::store::ConflictRecord),
    AccountedPending {
        operation: Option<super::store::OperationReceipt>,
    },
    RecoveryRequired {
        operation: Option<super::store::OperationId>,
        error: StoreError,
    },
}
#[cfg(target_os = "linux")]
pub(super) use native::linux::{execute_next_regular, execute_reviewed_initial, resolve_regular};
