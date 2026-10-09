//! Bounded host-local observations of real action admission and receipts.
//!
//! This stream is not an authentication protocol. A network publisher must
//! recheck current certified membership and seal to the exact route. Names,
//! execution UUIDs and these public values alone grant no authority.
use super::{BrokerActor, CrdtRoute, PublishedAction, PublishedSurface};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{Caller, CallerPrincipal, ExecutionLease};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tokio::sync::broadcast;

/// Receipt semantics, independent of cursor geometry or visible mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionStatus {
    Accepted,
    Progress,
    Succeeded,
    Failed,
    Cancelled,
    CancellationRequested,
    OutcomeUnknown,
}

/// One broker-authored observation, without action arguments or result content.
#[derive(Clone, Debug)]
pub struct ExecutionActivity {
    /// Broker lifetime; a reconnect must not join two sequence namespaces.
    pub epoch: uuid::Uuid,
    /// Strict emission order within `epoch`; lag is reported by the receiver.
    pub sequence: u64,
    pub source_time_ms: u64,
    /// Host-minted run subordinate to `principal`, never a device identity.
    pub execution: uuid::Uuid,
    pub invocation: InvocationId,
    pub principal: String,
    pub peer_id: u64,
    pub route: CrdtRoute,
    pub address: ActionAddress,
    pub status: ExecutionStatus,
}

/// Only trusted action-host admission constructs this input, not action context.
#[derive(Clone)]
pub(super) struct ExecutionScope {
    lifetime: ExecutionLease,
    principal: String,
    execution: uuid::Uuid,
    invocation: InvocationId,
}
impl ExecutionScope {
    pub(super) fn for_call(caller: &Caller, invocation: InvocationId) -> Option<Self> {
        let CallerPrincipal::Person(principal) = caller.principal() else {
            return None;
        };
        let lifetime = caller.execution_lease()?;
        Some(Self {
            lifetime,
            principal: principal.clone(),
            execution: caller.execution_id()?,
            invocation,
        })
    }

    pub(super) fn invocation(&self) -> InvocationId {
        self.invocation
    }

    pub(super) fn lease(&self) -> ExecutionLease {
        self.lifetime.clone()
    }

    pub(super) fn participant(
        &self,
        actor: &BrokerActor,
    ) -> Result<tilo_actions::ParticipantId, String> {
        if !self.lifetime.is_live() {
            return Err("execution session ended".into());
        }
        if actor.is_agent || actor.id != self.principal {
            return Err("execution principal mismatch".into());
        }
        Ok(tilo_actions::ParticipantId::for_execution(self.execution))
    }
}

#[derive(Clone)]
pub(super) struct ActivityTarget {
    scope: ExecutionScope,
    principal: String,
    peer_id: u64,
    route: CrdtRoute,
    address: ActionAddress,
}
impl ActivityTarget {
    pub(super) fn new(
        scope: Option<ExecutionScope>,
        actor: &BrokerActor,
        surface: &PublishedSurface,
        action: &PublishedAction,
    ) -> Option<Self> {
        let scope = scope?;
        // A fixed queue length also needs a bound on each retained value.
        // Unsupported metadata yields no presence, never a guessed target.
        if [
            &actor.id,
            &surface.route.app_id,
            &surface.route.cluster_id,
            &surface.route.record_id,
            &action.resource,
            &action.action,
        ]
        .iter()
        .any(|field| field.len() > 4096)
        {
            return None;
        }
        Some(Self {
            scope,
            principal: actor.id.clone(),
            peer_id: surface.peer_id,
            route: surface.route.clone(),
            address: ActionAddress::new(action.resource.clone(), action.action.clone()),
        })
    }
}

pub(super) struct ActivityBus(Mutex<Bus>);
struct Bus {
    sender: Option<broadcast::Sender<Arc<ExecutionActivity>>>,
    epoch: uuid::Uuid,
    sequence: u64,
}
impl Default for ActivityBus {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self(Mutex::new(Bus {
            sender: Some(sender),
            epoch: uuid::Uuid::new_v4(),
            sequence: 0,
        }))
    }
}
impl ActivityBus {
    pub(super) fn subscribe(&self) -> broadcast::Receiver<Arc<ExecutionActivity>> {
        let bus = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &bus.sender {
            Some(sender) => sender.subscribe(),
            None => broadcast::channel(1).1,
        }
    }
    pub(super) fn close(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sender
            .take();
    }
    pub(super) fn publish(&self, target: Option<&ActivityTarget>, update: &InvocationUpdate) {
        let status = match update {
            InvocationUpdate::Accepted { .. } => ExecutionStatus::Accepted,
            InvocationUpdate::Progress { .. } | InvocationUpdate::Item { .. } => {
                ExecutionStatus::Progress
            }
            InvocationUpdate::Succeeded { .. } => ExecutionStatus::Succeeded,
            InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown" => {
                ExecutionStatus::OutcomeUnknown
            }
            InvocationUpdate::Failed { .. } => ExecutionStatus::Failed,
            InvocationUpdate::Cancelled { .. } => ExecutionStatus::Cancelled,
        };
        self.publish_status(target, status);
    }
    pub(super) fn publish_status(&self, target: Option<&ActivityTarget>, status: ExecutionStatus) {
        let Some(target) = target else {
            return;
        };
        let Ok(mut bus) = self.0.lock() else {
            return;
        };
        if bus
            .sender
            .as_ref()
            .is_none_or(|sender| sender.receiver_count() == 0)
        {
            return;
        }
        let Some(sequence) = bus.sequence.checked_add(1) else {
            return;
        };
        let Some(source_time_ms) = tilo_clock::try_unix_now_ms() else {
            return;
        };
        bus.sequence = sequence;
        let Some(sender) = &bus.sender else {
            return;
        };
        let _ = sender.send(Arc::new(ExecutionActivity {
            epoch: bus.epoch,
            sequence,
            source_time_ms,
            execution: target.scope.execution,
            invocation: target.scope.invocation,
            principal: target.principal.clone(),
            peer_id: target.peer_id,
            route: target.route.clone(),
            address: target.address.clone(),
            status,
        }));
    }
}
