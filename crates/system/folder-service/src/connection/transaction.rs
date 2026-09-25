//! Private transaction admission. Durable facts are never mutation authority.
pub(super) mod chain;
pub(super) mod directory;
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
    original: Vec<u8>,
    record: super::store::ConnectionRecord,
    lifetime: Weak<()>,
    digest: [u8; 32],
    source: super::model::ObservedTree,
    destination: super::model::ObservedTree,
}
pub(super) enum WorkerReviewed {
    Regular(ReviewedInitial),
    Empty(AcceptedEmpty),
}
/// Move-only explicit review; cannot be decoded from persisted comparison data.
pub(super) struct AcceptedEmpty {
    record: super::store::ConnectionRecord,
    lifetime: Weak<()>,
    gate: Weak<AdmissionGate>,
    preview: [u8; 32],
}
impl AcceptedEmpty {
    pub(super) fn validate(
        &self,
        store: &ConnectionStore,
        session: &TransactionSession,
    ) -> Result<super::store::InitialEmptyBaselineV1, StoreError> {
        store.executor_session(self.record.id, session)?;
        session.require_idle()?;
        if self.lifetime.strong_count() == 0
            || !store.same_native_lifetime(&self.lifetime)
            || !self.gate.ptr_eq(&Arc::downgrade(&session.gate))
            || store.load_connection(self.record.id)? != self.record
            || store.executor_position(self.record.id)? != (None, None)
            || self.record.baseline_revision.is_some()
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(super::store::InitialEmptyBaselineV1::from_review(
            &self.record,
            self.preview,
        ))
    }
}
pub(super) struct ReviewedInitial {
    record: super::store::ConnectionRecord,
    lifetime: Weak<()>,
    preview: [u8; 32],
    selection: super::store::OperationIntent,
    source: super::model::ObservedTree,
    destination: super::model::ObservedTree,
}
impl InitialPreview {
    fn check_fresh(
        &self,
        store: &ConnectionStore,
        source: &super::model::ObservedTree,
        destination: &super::model::ObservedTree,
    ) -> Result<(), StoreError> {
        let (_, _, digest) = store.initial_review_basis(self.record.id, source, destination)?;
        if digest != self.digest
            || *source != self.source
            || *destination != self.destination
            || store.initial_review_preimage(self.record.id)? != self.original
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(())
    }
    /// Private complete-comparison acceptance. The worker route is intentionally unchanged.
    #[cfg(test)]
    pub(super) fn accept_complete(
        self,
        store: &ConnectionStore,
        session: &TransactionSession,
        source: &super::endpoint::PinnedEndpoint,
        destination: &super::endpoint::PinnedEndpoint,
        cancel: &AtomicBool,
    ) -> Result<ReviewedInitialSet, StoreError> {
        let lease = store.review_read_lease()?;
        lease.check()?;
        if cancel.load(Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let fresh_source = super::endpoint::observe(source, cancel);
        lease.check()?;
        if cancel.load(Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let fresh_destination = super::endpoint::observe(destination, cancel);
        lease.check()?;
        if cancel.load(Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        self.accept_complete_observed(store, session, fresh_source, fresh_destination, lease)
    }
    #[cfg(test)]
    fn accept_complete_observed(
        self,
        store: &ConnectionStore,
        session: &TransactionSession,
        source: super::model::ObservedTree,
        destination: super::model::ObservedTree,
        lease: super::store::ReviewReadLease,
    ) -> Result<ReviewedInitialSet, StoreError> {
        self.check_fresh(store, &source, &destination)?;
        store.executor_session(self.record.id, session)?;
        session.require_idle()?;
        if self.worker_len() == 0 || self.worker_len() > 4096 {
            return Err(StoreError::LimitExceeded);
        }
        let owner = ReviewedInitialSet {
            record: self.record,
            lifetime: self.lifetime,
            gate: Arc::downgrade(&session.gate),
            original: self.original,
            preview: self.digest,
            source: self.source,
            destination: self.destination,
            lease,
        };
        owner.validate(store)?;
        Ok(owner)
    }
    fn worker_paths(&self) -> std::collections::BTreeSet<&super::model::RelativePath> {
        self.source
            .entries()
            .keys()
            .chain(self.destination.entries().keys())
            .collect()
    }
    pub(super) fn worker_len(&self) -> usize {
        self.worker_paths().len()
    }
    pub(super) fn worker_page(
        &self,
        offset: usize,
    ) -> Result<crate::connected::InitialPage, StoreError> {
        let paths = self.worker_paths();
        if offset > paths.len() {
            return Err(StoreError::LimitExceeded);
        }
        let total = paths.len();
        let entries: Vec<_> = paths
            .into_iter()
            .skip(offset)
            .take(crate::connected::PAGE_LIMIT)
            .map(|path| crate::connected::InitialEntry {
                path: path.clone(),
                source: self.source.entries().get(path).cloned(),
                destination: self.destination.entries().get(path).cloned(),
            })
            .collect();
        let next = offset
            .checked_add(entries.len())
            .ok_or(StoreError::LimitExceeded)?;
        Ok(crate::connected::InitialPage {
            entries,
            next_offset: (next < total).then_some(next),
        })
    }
    /// Whole-comparison acceptance selects existing safe regular-file decisions.
    /// The executor still requires fresh native evidence for the selected path.
    pub(super) fn worker_accept(
        self,
        store: &ConnectionStore,
        session: &TransactionSession,
        source: &super::endpoint::PinnedEndpoint,
        destination: &super::endpoint::PinnedEndpoint,
        cancel: &AtomicBool,
    ) -> Result<Option<WorkerReviewed>, StoreError> {
        use super::model::{Conflict, Decision, EntryState, Side};
        let fresh_source = super::endpoint::observe(source, cancel);
        let fresh_destination = super::endpoint::observe(destination, cancel);
        self.check_fresh(store, &fresh_source, &fresh_destination)?;
        if self.worker_len() == 0 {
            return self
                .accept_empty(store, session)
                .map(|value| Some(WorkerReviewed::Empty(value)));
        }
        let mut selection = None;
        for path in self.worker_paths() {
            let left = self.source.entries().get(path);
            let right = self.destination.entries().get(path);
            if path.components().len() != 1
                || [left, right]
                    .into_iter()
                    .flatten()
                    .any(|entry| !matches!(entry, EntryState::File { .. }))
            {
                continue;
            }
            let decision = match (left, right) {
                (Some(left), Some(right)) if left == right => Decision::VerifyEqual,
                (Some(_), None) => Decision::Propagate { from: Side::Source },
                (None, Some(_)) => Decision::Propagate {
                    from: Side::Destination,
                },
                (Some(_), Some(_)) => Decision::Conflict(Conflict::DivergentEdits),
                (None, None) => continue,
            };
            let conflict = matches!(decision, Decision::Conflict(_));
            selection = Some(super::store::OperationIntent {
                connection: self.record.id,
                expected_revision: self.record.revision,
                expected_baseline: None,
                path: path.clone(),
                source_before: left.cloned(),
                destination_before: right.cloned(),
                decision,
            });
            // Prefer a baseline-establishing step; otherwise preserve both.
            if !conflict {
                break;
            }
        }
        selection
            .map(|selection| self.accept(selection).map(WorkerReviewed::Regular))
            .transpose()
    }
    pub(super) fn accept_empty(
        self,
        store: &ConnectionStore,
        session: &TransactionSession,
    ) -> Result<AcceptedEmpty, StoreError> {
        if !self.source.entries().is_empty() || !self.destination.entries().is_empty() {
            return Err(StoreError::StaleRevision);
        }
        let accepted = AcceptedEmpty {
            record: self.record,
            lifetime: self.lifetime,
            gate: Arc::downgrade(&session.gate),
            preview: self.digest,
        };
        accepted.validate(store, session)?;
        Ok(accepted)
    }
    fn from_observations(
        store: &ConnectionStore,
        id: ConnectionId,
        source: super::model::ObservedTree,
        destination: super::model::ObservedTree,
    ) -> Result<Self, StoreError> {
        let (record, lifetime, digest) = store.initial_review_basis(id, &source, &destination)?;
        Ok(Self {
            original: store.initial_review_preimage(id)?,
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
            source: self.source,
            destination: self.destination,
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
        Ok(
            regular_ancestor_prerequisite(
                &self.selection,
                &absent,
                &self.source,
                &self.destination,
            )
            .err(),
        )
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

/// Move-only, nonserializable consent owner; persisted review pages cannot mint it.
#[cfg(test)]
pub(super) struct ReviewedInitialSet {
    record: super::store::ConnectionRecord,
    lifetime: Weak<()>,
    gate: Weak<AdmissionGate>,
    original: Vec<u8>,
    preview: [u8; 32],
    source: super::model::ObservedTree,
    destination: super::model::ObservedTree,
    lease: super::store::ReviewReadLease,
}
#[cfg(test)]
impl ReviewedInitialSet {
    pub(super) fn record(&self) -> &super::store::ConnectionRecord {
        &self.record
    }
    pub(super) fn original(&self) -> &[u8] {
        &self.original
    }
    pub(super) fn preview(&self) -> [u8; 32] {
        self.preview
    }
    pub(super) fn trees(&self) -> [&super::model::ObservedTree; 2] {
        [&self.source, &self.destination]
    }

    pub(super) fn validate(&self, store: &ConnectionStore) -> Result<(), StoreError> {
        self.lease.check()?;
        store.validate_review_owner(&self.record, &self.lifetime, &self.gate, &self.original)?;
        self.lease.check()
    }
}
#[cfg(test)]
pub(super) fn complete_review_for_test(
    store: &ConnectionStore,
    session: &TransactionSession,
    preview: InitialPreview,
    source: super::model::ObservedTree,
    destination: super::model::ObservedTree,
) -> Result<ReviewedInitialSet, StoreError> {
    // Host fixture for the exact production post-observation acceptance checks.
    // It is not evidence that endpoint::observe ran or a native effect is authorized.
    let lease = store.review_read_lease()?;
    preview.accept_complete_observed(store, session, source, destination, lease)
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
    #[cfg(test)]
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
        if live.native_kind() == Some(super::store::NativeOperationKind::DirectoryV1)
            && !matches!(
                kind,
                StepKind::Namespace | StepKind::Stage | StepKind::Publish
            )
        {
            return Err(StoreError::RecoveryRequired);
        }
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
/// A bounded requirement, never live filesystem evidence or permission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExistingAncestorRequirement {
    path: super::model::RelativePath,
    prefixes: u64,
}
impl ExistingAncestorRequirement {
    pub(super) fn from_baseline(
        path: &super::model::RelativePath,
        entries: &std::collections::BTreeMap<super::model::RelativePath, super::model::EntryState>,
    ) -> Result<Self, StoreError> {
        let mut prefixes = 0;
        let mut parent = path.parent();
        while let Some(value) = parent {
            match entries
                .iter()
                .find(|(key, _)| key.logical() == value.logical())
                .map(|(_, state)| state)
            {
                Some(super::model::EntryState::Directory) => (),
                Some(_) => return Err(StoreError::RecoveryRequired),
                None => prefixes |= 1_u64 << value.components().len(),
            }
            parent = value.parent();
        }
        Ok(Self {
            path: path.clone(),
            prefixes,
        })
    }
    pub(super) fn prefixes(&self) -> u64 {
        self.prefixes
    }
    pub(super) fn paths(&self) -> Vec<super::model::RelativePath> {
        let mut result = Vec::new();
        let mut parent = self.path.parent();
        while let Some(value) = parent {
            if self.prefixes & (1_u64 << value.components().len()) != 0 {
                result.push(value.clone());
            }
            parent = value.parent();
        }
        result.reverse();
        result
    }
}
fn regular_ancestor_prerequisite(
    intent: &super::store::OperationIntent,
    baseline: &super::model::VerifiedBaseline,
    source: &super::model::ObservedTree,
    destination: &super::model::ObservedTree,
) -> Result<ExistingAncestorRequirement, StructuralPending> {
    use super::model::{Decision, EntryState};
    let empty = std::collections::BTreeMap::new();
    let entries = baseline.entries().unwrap_or(&empty);
    let mut parent = intent.path.parent();
    while let Some(value) = parent {
        let prior = entries
            .iter()
            .find(|(key, _)| key.logical() == value.logical())
            .map(|(_, state)| state);
        let observed = |tree: &super::model::ObservedTree| {
            tree.entries()
                .iter()
                .find(|(key, _)| key.logical() == value.logical())
                .map(|(_, state)| state)
                == Some(&EntryState::Directory)
        };
        if !observed(source)
            || !observed(destination)
            || prior.is_some_and(|state| state != &EntryState::Directory)
            || (prior.is_none()
                && !(matches!(
                    intent.decision,
                    Decision::VerifyEqual | Decision::Propagate { .. }
                ) && [&intent.source_before, &intent.destination_before]
                    .into_iter()
                    .any(|state| matches!(state, Some(EntryState::File { .. })))))
        {
            return Err(StructuralPending {
                path: intent.path.clone(),
                missing_baseline_ancestor: value,
            });
        }
        parent = value.parent();
    }
    // Every possible structural failure was identified above.
    ExistingAncestorRequirement::from_baseline(&intent.path, entries).map_err(|_| {
        StructuralPending {
            path: intent.path.clone(),
            missing_baseline_ancestor: intent.path.clone(),
        }
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
        let intent = super::store::OperationIntent {
            connection: id,
            expected_revision: record.revision,
            expected_baseline: record.baseline_revision,
            path: proposal.path,
            source_before: proposal.source,
            destination_before: proposal.destination,
            decision: proposal.decision,
        };
        if let Err(reason) = regular_ancestor_prerequisite(&intent, baseline, &source, &destination)
        {
            structural.get_or_insert(reason);
            continue;
        }
        store.preflight_regular_baseline(&intent, baseline)?;
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
    VerifiedEmpty {
        revision: super::store::Revision,
    },
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

#[cfg(target_os = "linux")]
pub(super) use native::linux::initial_baseline::{InitialEmptyProof, execute_empty};
