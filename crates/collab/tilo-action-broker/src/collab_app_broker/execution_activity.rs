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
    pub(super) actor: tilo_actions::InteractionActor,
    pub(super) action: tilo_actions::InteractionAction,
    pub(super) resource: tilo_actions::ActionResource,
    /// The narrowest target the invocation names, as far as its published
    /// action and the caller's own argument say: the record or item it acts on,
    /// or the characters of a text it writes.
    pub(super) anchor: tilo_actions::InteractionAnchor,
    /// The run or task it works in, whose label names an actor the workspace's
    /// roster does not know (a Shortcut's run).
    pub(super) group: Option<tilo_actions::WorkGroup>,
    /// Last sequence published for this invocation.
    pub(super) sequence: Arc<std::sync::atomic::AtomicU64>,
}

/// The region name the ranges of a place inside a text sit in: the collection
/// field the place names (`blocks`), or the resource's text when it names
/// none.
fn text_region(action: &PublishedAction) -> String {
    action
        .params
        .iter()
        .filter(|param| param.is_place())
        .find_map(|param| param.place_field().map(str::to_owned))
        .unwrap_or_else(|| "text".to_owned())
}

/// How many characters of a text an invocation writes: what its parameter of
/// type [`Text`](tilo_actions::ParamType::Text) holds, counted as characters
/// because a place counts them that way.
fn written_chars(
    action: &PublishedAction,
    argument: &serde_json::Map<String, serde_json::Value>,
) -> u64 {
    action
        .params
        .iter()
        .filter(|param| param.ty == tilo_actions::ParamType::Text)
        .filter_map(|param| argument.get(param.name.as_ref())?.as_str())
        .map(|text| text.chars().count() as u64)
        .next()
        .unwrap_or(0)
}

/// Where the invocation acts, from what its action publishes and the argument
/// its caller passed: a place inside the resource names an item, or the
/// characters of a text edit; a resource of an item's own names that item.
/// Anything else is the resource.
fn anchor_of(
    action: &PublishedAction,
    resource: &tilo_actions::ActionResource,
    argument: Option<&str>,
) -> tilo_actions::InteractionAnchor {
    use tilo_actions::InteractionAnchor;
    // An item published as its own resource (`<kind>/<field>/<item>`) is what
    // its actions act on, and the view draws it as a row.
    if let Some(item) = item_of_resource(&resource.id, &resource.kind)
        && !action.params.iter().any(|param| param.is_place())
    {
        return InteractionAnchor::ListItems { ids: vec![item] };
    }
    let tilo_actions::remote::InvokeArgument::Json(argument) =
        tilo_actions::remote::InvokeArgument::from_slot(argument)
    else {
        return InteractionAnchor::Resource;
    };
    let Some(place) = action
        .params
        .iter()
        .filter(|param| param.is_place())
        .find_map(|param| argument.get(param.name.as_ref())?.as_object())
    else {
        return InteractionAnchor::Resource;
    };
    let offset = |name: &str| place.get(name).and_then(serde_json::Value::as_u64);
    let Some(item) = place
        .get("item")
        .and_then(serde_json::Value::as_str)
        .filter(|item| !item.is_empty())
    else {
        return InteractionAnchor::Resource;
    };
    let written = written_chars(action, &argument);
    match (offset("start"), offset("end"), offset("at")) {
        // The range it writes: the characters it replaces, or the characters
        // it inserts where a caret was.
        (Some(start), end, _) => {
            let end = if written == 0 {
                end.unwrap_or(start)
            } else {
                start.saturating_add(written)
            };
            InteractionAnchor::Text {
                region: text_region(action),
                ranges: vec![tilo_actions::InteractionTextRange {
                    block: item.to_owned(),
                    start,
                    end: end.max(start),
                }],
            }
        }
        (None, _, Some(at)) => InteractionAnchor::Text {
            region: text_region(action),
            ranges: vec![tilo_actions::InteractionTextRange {
                block: item.to_owned(),
                start: at,
                end: at.saturating_add(written),
            }],
        },
        (None, _, None) => InteractionAnchor::ListItems {
            ids: vec![item.to_owned()],
        },
    }
}

/// The item a resource is, when it is one (`<kind>/<field>/<item>`, as apps
/// publish the items of a collection).
fn item_of_resource(resource: &str, kind: &str) -> Option<String> {
    let rest = resource
        .strip_prefix(kind)?
        .strip_prefix('/')?
        .split_once('/')?;
    let (field, item) = rest;
    (!field.is_empty() && !item.is_empty() && !item.contains('/')).then(|| item.to_owned())
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
        argument: Option<&str>,
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
        let resource = tilo_actions::ActionResource {
            id: action.resource.clone(),
            kind: action.resource_kind.clone(),
            label: action.resource_label.clone(),
        };
        Some(Self {
            actor: shown,
            action: tilo_actions::InteractionAction {
                id: action.action.clone(),
                label: action.action_label.clone(),
                app: Some(tilo_actions::ActionApp {
                    id: surface.app_id.clone(),
                    label: surface.app_id.clone(),
                }),
            },
            anchor: anchor_of(action, &resource, argument),
            resource,
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
        argument: Option<&str>,
    ) -> Option<Self> {
        let shown = ShownActor::of(caller, actor, context, surface, action, argument);
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
                && caller.workspace() == Some(self.route.cluster_id.as_str())
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
