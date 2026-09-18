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
        let state = self.gate.state.lock().map_err(|_| StoreError::RecoveryRequired)?;
        if state.closed || self.gate.stopped.load(Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        if state.in_flight != 0 { return Err(StoreError::Busy); }
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
