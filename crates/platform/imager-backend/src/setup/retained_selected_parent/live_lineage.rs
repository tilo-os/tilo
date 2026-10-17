//! Named selected-object continuity is separate from immutable admission provenance.
use super::super::super::retained_selected_edit::{
    AcknowledgedSelectedReplacement, PendingSelectedReplacement,
};
use super::*;

pub(super) struct LiveLineage {
    state: Mutex<State>,
}
struct State {
    identity: [u64; 2],
    pin: Arc<dyn WorkspaceMetadataBackend>,
    pending: Option<Arc<PendingSelectedReplacement>>,
    operations: usize,
    last_committed_pending: Option<Arc<PendingSelectedReplacement>>,
    transfer: CompletionTransfer<PendingSelectedReplacement>,
}
/// Completion custody is separate from the effect gate: current destination
/// reads are permitted, but another reservation cannot race ACK owner transfer.
/// This private phase never mints source authority or a completion receipt.
struct CompletionTransfer<T> {
    owner: Option<Arc<T>>,
}
impl<T> CompletionTransfer<T> {
    fn ready(&self) -> Result<(), String> {
        if self.owner.is_some() {
            return Err(transient());
        }
        Ok(())
    }
    fn hold(&mut self, owner: Arc<T>) -> Result<(), String> {
        self.ready()?;
        self.owner = Some(owner);
        Ok(())
    }
    fn release(&mut self, owner: &Arc<T>) -> Result<(), String> {
        if self
            .owner
            .as_ref()
            .is_some_and(|held| !Arc::ptr_eq(held, owner))
        {
            return Err(denied());
        }
        self.owner = None;
        Ok(())
    }
}
pub(super) struct Operation<'a>(&'a LiveLineage);
fn transient() -> String {
    "Selected replacement pending; file operation temporarily refused".into()
}
impl LiveLineage {
    pub(super) fn new(identity: [u64; 2], pin: Arc<dyn WorkspaceMetadataBackend>) -> Self {
        Self {
            state: Mutex::new(State {
                identity,
                pin,
                pending: None,
                operations: 0,
                last_committed_pending: None,
                transfer: CompletionTransfer { owner: None },
            }),
        }
    }
    /// This gate is separate from permission checking. Its refusal does not
    /// revoke authority; poisoning and actual permission failures still do.
    pub(super) fn operation(&self) -> Result<Operation<'_>, String> {
        let mut state = self.state.lock().map_err(|_| denied())?;
        if state.pending.is_some() {
            return Err(transient());
        }
        state.operations = state.operations.checked_add(1).ok_or_else(denied)?;
        Ok(Operation(self))
    }
    pub(super) fn identity(&self) -> Result<[u64; 2], String> {
        let state = self.state.lock().map_err(|_| denied())?;
        if state.pending.is_some() {
            return Err(transient());
        }
        Ok(state.identity)
    }
    /// Called under an actual callback operation guard before reserving work.
    /// The operation guard prevents commit racing this check and reservation.
    pub(super) fn check_ready_to_reserve(&self) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| denied())?;
        if state.pending.is_some() {
            return Err(transient());
        }
        state.transfer.ready()
    }
    /// The source parent calls this only after actual completed queue ownership
    /// and original root registry transfer have both been checked. A failed
    /// check leaves the phase held; current reads never release it.
    pub(super) fn finish_completed_transfer(
        &self,
        source: &SourceSelectedParent,
        proof: &AcknowledgedSelectedReplacement,
    ) -> Result<(), String> {
        source.check_authority()?;
        proof.check_custody(source)?;
        let mut state = self.state.lock().map_err(|_| denied())?;
        if state.pending.is_some()
            || !state
                .last_committed_pending
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(owner, proof.pending()))
            || state.identity != proof.observation().destination
        {
            return Err(denied());
        }
        let held = state.pin.metadata()?;
        let named = source
            .parent
            .stat(source.selected_name())?
            .ok_or_else(denied)?;
        if held.identity != state.identity
            || held.metadata.kind != EntryKind::File
            || named.identity != state.identity
            || named.metadata.kind != EntryKind::File
        {
            return Err(denied());
        }
        proof.check_custody(source)?;
        source.check_authority()?;
        state.transfer.release(proof.pending())
    }
    /// Compare to the constructor-private actual ACK adopted by this lineage,
    /// never to delivery order or a caller-supplied numeric identity alone.
    pub(super) fn matches_acknowledged(
        &self,
        proof: &AcknowledgedSelectedReplacement,
    ) -> Result<bool, String> {
        let state = self.state.lock().map_err(|_| denied())?;
        if state.pending.is_some() {
            return Err(transient());
        }
        Ok(state
            .last_committed_pending
            .as_ref()
            .is_some_and(|pending| {
                Arc::ptr_eq(pending, proof.pending())
                    && pending.observation().operation == proof.observation().operation
                    && state.identity == proof.observation().destination
            }))
    }
    /// No named-old-inode check during authentic pending exchange. The original
    /// source/root/parent/pin remain checked independently on every invocation.
    /// Pending.check_custody MUST use source.check_authority(), not source.check().
    pub(super) fn check(&self, source: &SourceSelectedParent) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| denied())?;
        let held = state.pin.metadata()?;
        if held.identity != state.identity || held.metadata.kind != EntryKind::File {
            return Err(denied());
        }
        if let Some(pending) = &state.pending {
            pending.check_custody(source)?;
        } else {
            let named = source
                .parent
                .stat(source.selected_name())?
                .ok_or_else(denied)?;
            if named.identity != state.identity || named.metadata.kind != EntryKind::File {
                return Err(denied());
            }
        }
        Ok(())
    }
    /// Own the genuine zero-effect reservation BEFORE native start can mutate
    /// selected. Closing the gate precedes draining existing callbacks. A busy
    /// return retains the pending capability and is retryable with that same Arc.
    pub(super) fn begin(
        &self,
        source: &SourceSelectedParent,
        pending: Arc<PendingSelectedReplacement>,
    ) -> Result<(), String> {
        source.check_authority()?;
        pending.check_custody(source)?;
        let observation = pending.observation();
        observation.validate()?;
        let mut state = self.state.lock().map_err(|_| denied())?;
        state.transfer.ready()?;
        if observation.expected_victim != state.identity {
            return Err(denied());
        }
        if let Some(current) = &state.pending {
            if !Arc::ptr_eq(current, &pending) {
                return Err(denied());
            }
        } else {
            let named = source
                .parent
                .stat(source.selected_name())?
                .ok_or_else(denied)?;
            if named.identity != state.identity || named.metadata.kind != EntryKind::File {
                return Err(denied());
            }
            state.pending = Some(pending);
        }
        if state.operations != 0 {
            return Err(transient());
        }
        Ok(())
    }
    /// Native start/handoff only after the exact installed root proof has
    /// closed the callback gate and all actual prior operations have drained.
    pub(super) fn check_drained_pending(
        &self,
        pending: &Arc<PendingSelectedReplacement>,
    ) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| denied())?;
        if state.operations != 0
            || !state
                .pending
                .as_ref()
                .is_some_and(|actual| Arc::ptr_eq(actual, pending))
        {
            return Err(transient());
        }
        Ok(())
    }
    /// Only the root producer's constructor-private acknowledged derivative can
    /// advance continuity. Receipt bytes or completion observations alone cannot.
    pub(super) fn commit(
        &self,
        source: &SourceSelectedParent,
        proof: &AcknowledgedSelectedReplacement,
    ) -> Result<(), String> {
        source.check_authority()?;
        proof.check_custody(source)?;
        let mut state = self.state.lock().map_err(|_| denied())?;
        let pending = state.pending.as_ref().ok_or_else(denied)?;
        if !Arc::ptr_eq(pending, proof.pending()) || state.operations != 0 {
            return Err(denied());
        }
        let before = pending.observation();
        let after = proof.observation();
        if before.operation != after.operation
            || before.source_owner != after.source_owner
            || before.source != after.source
            || before.expected_victim != after.victim
            || state.identity != after.victim
            || after.destination != after.source
            || after.capture == [0; 16]
        {
            return Err(denied());
        }
        let named = source
            .parent
            .stat(source.selected_name())?
            .ok_or_else(denied)?;
        if named.identity != after.destination || named.metadata.kind != EntryKind::File {
            return Err(denied());
        }
        // This is a fresh actual source-held metadata pin, not a DTO replacement
        // for the original pin. Native commitWithHistory has already acknowledged.
        let pin = source
            .parent
            .pin_metadata(source.selected_name(), after.destination)?;
        let held = pin.metadata()?;
        let named_after = source
            .parent
            .stat(source.selected_name())?
            .ok_or_else(denied)?;
        if held.identity != after.destination
            || held.metadata.kind != EntryKind::File
            || named_after.identity != after.destination
            || named_after.metadata.kind != EntryKind::File
        {
            return Err(denied());
        }
        proof.check_custody(source)?;
        source.check_authority()?;
        state.transfer.hold(proof.pending().clone())?;
        state.last_committed_pending = Some(proof.pending().clone());
        state.identity = after.destination;
        state.pin = pin;
        state.pending.take();
        Ok(())
    }
}
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            if let Some(count) = state.operations.checked_sub(1) {
                state.operations = count;
            }
        }
    }
}

#[cfg(test)]
mod completion_transfer_tests {
    use super::*;

    // Tests the exact production phase, not person/source admission or a fake ACK.
    #[test]
    fn committed_phase_blocks_next_reservation_until_same_owner_transfer() {
        let first = Arc::new(());
        let second = Arc::new(());
        let mut phase = CompletionTransfer { owner: None };
        phase.hold(first.clone()).unwrap();
        assert!(phase.ready().is_err());
        assert!(phase.hold(second.clone()).is_err());
        assert!(phase.release(&second).is_err());
        assert!(Arc::ptr_eq(phase.owner.as_ref().unwrap(), &first));
        phase.release(&first).unwrap();
        phase.ready().unwrap();
        phase.hold(second).unwrap();
    }

    #[test]
    fn unknown_owner_transfer_keeps_actual_owned_custody() {
        let owner = Arc::new(());
        let weak = Arc::downgrade(&owner);
        let mut phase = CompletionTransfer { owner: None };
        phase.hold(owner).unwrap();
        assert!(phase.ready().is_err());
        assert!(weak.upgrade().is_some());
        // No cleanup receipt, timeout or public job drop changes this phase.
        assert!(phase.ready().is_err());
        let held = weak.upgrade().unwrap();
        phase.release(&held).unwrap();
        drop(held);
        assert!(weak.upgrade().is_none());
    }
}
