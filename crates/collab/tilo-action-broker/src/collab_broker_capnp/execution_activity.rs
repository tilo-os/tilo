//! Bounded host-local observations of real action admission and receipts.
//!
//! This stream is not an authentication protocol. A network publisher must
//! recheck current certified membership and seal to the exact route. Names,
//! execution UUIDs and these public values alone grant no authority.
use super::{BrokerActor, CrdtRoute, PublishedAction, PublishedSurface};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{Caller, CallerPrincipal, ExecutionLease};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;
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
    Ended,
}

/// One broker-authored observation, without action arguments or result content.
#[derive(Clone, Debug)]
pub struct ExecutionActivity {
    lifetime: ExecutionLease,
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

impl ExecutionActivity {
    /// Whether its transport run still exists; buffered observations cannot revive it.
    #[must_use]
    pub fn execution_is_live(&self) -> bool {
        self.lifetime.is_live()
    }
}

/// Only trusted action-host admission constructs this input, not action context.
#[derive(Clone)]
pub(super) struct ExecutionScope {
    lifetime: ExecutionLease,
    principal: String,
    /// The run is an agent's its person's device handed it to.
    agent: bool,
    execution: uuid::Uuid,
    invocation: InvocationId,
}
impl ExecutionScope {
    pub(super) fn for_call(caller: &Caller, invocation: InvocationId) -> Option<Self> {
        // A person's run, or an agent's their own device handed it to.
        let (principal, agent) = match caller.principal() {
            CallerPrincipal::Person(principal) => (principal.clone(), false),
            CallerPrincipal::Agent(_) if caller.acting_for_person().is_some() => {
                (BrokerActor::for_caller(caller).id().to_owned(), true)
            }
            CallerPrincipal::Agent(_) | CallerPrincipal::App(_) | CallerPrincipal::Bot(_) => {
                return None;
            }
        };
        let lifetime = caller.execution_lease()?;
        Some(Self {
            lifetime,
            principal,
            agent,
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
        if actor.is_agent != self.agent || actor.id != self.principal {
            return Err("execution principal mismatch".into());
        }
        Ok(tilo_actions::ParticipantId::for_execution(self.execution))
    }
}

/// Where an invocation runs and who runs it: what the observation bus
/// reports for an execution, and what the action's view shows of an actor
/// that is not the person at its own keyboard.
#[derive(Clone)]
pub(super) struct ActivityTarget {
    scope: Option<ExecutionScope>,
    principal: String,
    peer_id: u64,
    route: CrdtRoute,
    address: ActionAddress,
    /// The actor as its presence in the view names it, when it is shown
    /// there: an agent, an app, a person acting from another device, or
    /// anyone acting for someone else.
    shown: Option<ShownActor>,
}

#[derive(Clone)]
pub(super) struct ShownActor {
    /// Fresh for each admitted execution, even if its caller reuses a UUID.
    pub(super) paint_origin: uuid::Uuid,
    pub(super) actor: tilo_actions::InteractionActor,
    pub(super) action: tilo_actions::InteractionAction,
    pub(super) resource: tilo_actions::ActionResource,
    /// The run or task it works in, whose label names an actor the room's
    /// roster does not know (a Shortcut's run).
    pub(super) group: Option<tilo_actions::ActionGroup>,
    /// Last sequence published for this invocation.
    pub(super) sequence: Arc<std::sync::atomic::AtomicU64>,
}

impl ShownActor {
    /// How `caller`, admitted as `actor`, appears in the view it acts in;
    /// `None` for a person acting for themselves on this device, whose own
    /// view already shows what they do.
    pub(super) fn of(
        caller: Option<&Caller>,
        actor: &BrokerActor,
        context: Option<&tilo_actions::ActionContext>,
        surface: &PublishedSurface,
        action: &PublishedAction,
    ) -> Option<Self> {
        let shown = caller.map_or_else(
            || {
                tilo_actions::InteractionActor::new(
                    actor.id.clone(),
                    if actor.is_agent {
                        tilo_actions::ActorKind::Agent
                    } else {
                        tilo_actions::ActorKind::Person
                    },
                )
            },
            Caller::actor,
        );
        let remote = caller.is_some_and(|caller| caller.device_public_key().is_some());
        if !shown.kind.acts_for_someone() && shown.for_person.is_none() && !remote {
            return None;
        }
        Some(Self {
            paint_origin: uuid::Uuid::new_v4(),
            actor: shown,
            action: tilo_actions::InteractionAction {
                id: action.action.clone(),
                label: action.action_label.clone(),
                app: Some(tilo_actions::ActionApp {
                    id: surface.app_id.clone(),
                    label: surface.app_id.clone(),
                }),
            },
            resource: tilo_actions::ActionResource {
                id: action.resource.clone(),
                kind: action.resource_kind.clone(),
                label: action.resource_label.clone(),
            },
            group: context.and_then(|context| context.group.clone()),
            sequence: Arc::default(),
        })
    }
}

impl ActivityTarget {
    pub(super) fn new(
        scope: Option<ExecutionScope>,
        (actor, caller, context): (
            &BrokerActor,
            Option<&Caller>,
            Option<&tilo_actions::ActionContext>,
        ),
        surface: &PublishedSurface,
        action: &PublishedAction,
    ) -> Option<Self> {
        let shown = ShownActor::of(caller, actor, context, surface, action);
        if scope.is_none() && shown.is_none() {
            return None;
        }
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
            shown,
        })
    }

    pub(super) fn belongs_to(&self, caller: &Caller) -> bool {
        // The host-owned lease is shared with a delegated agent. The admitted
        // person can retire it without changing the actor of its running jobs.
        self.scope.as_ref().is_some_and(|scope| {
            caller.execution_lease().as_ref() == Some(&scope.lifetime)
                && caller.room() == Some(self.route.cluster_id.as_str())
        })
    }

    pub(super) fn retire_scope(&mut self) {
        // Invocation-level attribution outlives the transport: work still
        // runs in its view and must show its actual progress and outcome.
        self.scope = None;
    }

    pub(super) fn route(&self) -> &CrdtRoute {
        &self.route
    }

    pub(super) fn shown(&self) -> Option<&ShownActor> {
        self.shown.as_ref()
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
        let bus = self.0.lock_recover();
        match &bus.sender {
            Some(sender) => sender.subscribe(),
            None => broadcast::channel(1).1,
        }
    }
    pub(super) fn close(&self) {
        self.0.lock_recover().sender.take();
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
        let Some((target, scope)) =
            target.and_then(|target| target.scope.as_ref().map(|scope| (target, scope)))
        else {
            return;
        };
        if status != ExecutionStatus::Ended && !scope.lifetime.is_live() {
            return;
        }
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
            lifetime: scope.lifetime.clone(),
            epoch: bus.epoch,
            sequence,
            source_time_ms,
            execution: scope.execution,
            invocation: scope.invocation,
            principal: target.principal.clone(),
            peer_id: target.peer_id,
            route: target.route.clone(),
            address: target.address.clone(),
            status,
        }));
    }
}
