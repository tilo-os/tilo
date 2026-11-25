//! Bounded host-local observations of real action admission and receipts.
//!
//! This stream is not an authentication protocol. A network publisher must
//! recheck current certified membership and seal to the exact route. Names,
//! execution UUIDs and these public values alone grant no authority.
use super::{BrokerActor, CrdtRoute, PublishedAction, PublishedSurface};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{Caller, ExecutionLease};
use tilo_actions::{ActionAddress, Actor, InvocationId, InvocationUpdate, MemberId};
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
    /// Host-minted run subordinate to `actor`, never a device identity.
    pub execution: uuid::Uuid,
    pub invocation: InvocationId,
    pub actor: Actor,
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
    actor: Actor,
    execution: uuid::Uuid,
    invocation: InvocationId,
}
impl ExecutionScope {
    pub(super) fn for_call(caller: &Caller, invocation: InvocationId) -> Option<Self> {
        // A person's run, or an agent's their own device handed it to.
        let actor = if let Some(person) = caller.acting().person_id() {
            MemberId::person(person).into()
        } else if caller.acting().is_agent() && caller.acting_for_person().is_some() {
            BrokerActor::for_caller(caller).actor().clone()
        } else {
            return None;
        };
        let lifetime = caller.execution_lease()?;
        Some(Self {
            lifetime,
            actor,
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

    pub(super) fn presence(&self, actor: &BrokerActor) -> Result<tilo_actions::PresenceId, String> {
        if !self.lifetime.is_live() {
            return Err("execution session ended".into());
        }
        if *actor.actor() != self.actor {
            return Err("execution actor mismatch".into());
        }
        Ok(tilo_actions::PresenceId::for_execution(self.execution))
    }
}

/// Where an invocation runs and who runs it: what the observation bus
/// reports for an execution, and what the action's view shows of an actor
/// that is not the person at its own keyboard.
#[derive(Clone)]
pub(super) struct ActivityTarget {
    scope: Option<ExecutionScope>,
    actor: Actor,
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
    pub(super) actor: tilo_actions::Acting,
    pub(super) action: tilo_actions::InteractionAction,
    /// What it acts in, named as the step's line says it: the item's own label
    /// where the invocation names one, else the record's.
    pub(super) resource: tilo_actions::ActionResource,
    /// The resource its presence is keyed by for its whole run (id and kind): the one it began on.
    pub(super) key: (String, String),
    /// The narrowest target the invocation names, as far as its published
    /// action and the caller's own argument say: the record or item it acts on,
    /// or the characters of a text it writes.
    pub(super) anchor: tilo_actions::InteractionAnchor,
    /// The field of the item it acts on that it adds to, and the characters it writes there: a
    /// comment's thread and its words.
    adds: Option<(String, u64)>,
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

/// The characters a field edit writes into a text: the field's region, the
/// block holding them and the range it leaves there — what it inserts where a
/// caret was, or the characters it replaces. A rich text's block is the
/// paragraph it splices; a text field's is the item holding the field (empty
/// for the record's own), and a whole new text is written from its start.
/// `None` for a field edit of anything else.
fn written_text(
    argument: &serde_json::Map<String, serde_json::Value>,
) -> Option<(String, String, u64, u64)> {
    let edit = argument.get("edit")?.as_object()?;
    let path = edit.get("path")?.as_array()?;
    let chars = |text: Option<&serde_json::Value>| {
        text.and_then(serde_json::Value::as_str)
            .map_or(0, |text| text.chars().count() as u64)
    };
    let offset = |splice: &serde_json::Map<String, serde_json::Value>, key: &str| {
        splice.get(key).and_then(serde_json::Value::as_u64)
    };
    let item = || {
        path.iter()
            .rev()
            .find_map(|segment| segment.get("item")?.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let (block, at, del, inserted) = match edit.get("op")?.as_str()? {
        "rich" => {
            let splice = edit
                .get("rich")?
                .as_object()?
                .get("SpliceText")?
                .as_object()?;
            let block = splice.get("block")?.as_str()?.to_owned();
            (
                block,
                offset(splice, "at")?,
                offset(splice, "del").unwrap_or(0),
                chars(splice.get("ins")),
            )
        }
        "splice" => (
            item(),
            offset(edit, "at")?,
            offset(edit, "delete").unwrap_or(0),
            chars(edit.get("insert")),
        ),
        "replace" => (item(), 0, 0, chars(edit.get("text"))),
        _ => return None,
    };
    // The region the field names: the last field of the edit's path (`body`).
    let region = path
        .iter()
        .rev()
        .find_map(|segment| segment.get("field")?.as_str())?
        .to_owned();
    // Past the end of the text is clamped by the reader; saturating keeps a
    // nonsensical offset from wrapping into the start of it.
    let end = if inserted == 0 {
        at.saturating_add(del)
    } else {
        at.saturating_add(inserted)
    };
    Some((region, block, at, end))
}

/// How many characters of a text an invocation writes: what its parameter of
/// type [`Text`](tilo_actions::ParamType::Text) holds, counted as characters
/// because a place counts them that way. The parameter named `names_item` names
/// the item it writes in ([`item_param`]), by its id or by what it says, and is
/// none of its text.
fn written_chars(
    action: &PublishedAction,
    argument: &serde_json::Map<String, serde_json::Value>,
    names_item: Option<&str>,
) -> u64 {
    action
        .params
        .iter()
        .filter(|param| param.ty == tilo_actions::ParamType::Text)
        .filter(|param| Some(param.name.as_ref()) != names_item)
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
    // An argument that is no set of named values names nothing in it.
    let argument = json_argument(argument).unwrap_or_default();
    // A field edit that splices a rich text names the characters it writes,
    // whatever it is addressed to: the editor showing that text draws them as
    // the writer's, and their caret where the write ends.
    if let Some((region, block, start, end)) = written_text(&argument) {
        return InteractionAnchor::Text {
            region,
            ranges: vec![tilo_actions::InteractionTextRange { block, start, end }],
        };
    }
    // A verb on an item that writes one of its text fields from its parameter
    // of that name (a rename's `title`) names the text it writes there: the
    // field showing it draws it as the writer's, with its caret at its end.
    // The item is the item resource it acts on ([`item_named`]).
    let item = item_of_resource(&resource.id, &resource.kind);
    if let Some(field) = action.contract.field.as_deref()
        && let Some(item) = item.clone()
        && action
            .params
            .iter()
            .any(|param| param.ty == tilo_actions::ParamType::Text && param.name.as_ref() == field)
        && let Some(text) = argument.get(field).and_then(serde_json::Value::as_str)
    {
        return InteractionAnchor::Text {
            region: field.to_owned(),
            ranges: vec![tilo_actions::InteractionTextRange {
                block: item,
                start: 0,
                end: text.chars().count() as u64,
            }],
        };
    }
    if let Some(written) = edited_field(action, resource, &argument) {
        return written;
    }
    // An item published as its own resource (`<kind>/<field>/<item>`), or the
    // one a verb of the record names, is what the step acts on, and the view
    // draws it as a row.
    if let Some(item) = item
        && !action
            .params
            .iter()
            .any(tilo_actions::ActionParam::is_place)
    {
        return InteractionAnchor::ListItems { ids: vec![item] };
    }
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
    let written = written_chars(action, &argument, None);
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

/// What an edit of whichever text field its `field` argument names writes (a tilolang
/// item's `edit-text`): the text of its other text parameter, in that field of the resource
/// it is offered on. The field showing it draws it as the writer's, with its caret at its end.
fn edited_field(
    action: &PublishedAction,
    resource: &tilo_actions::ActionResource,
    argument: &serde_json::Map<String, serde_json::Value>,
) -> Option<tilo_actions::InteractionAnchor> {
    let field = argument.get(tilo_actions::run::EDITED_FIELD)?.as_str()?;
    let text = action
        .params
        .iter()
        .filter(|param| param.ty == tilo_actions::ParamType::Text)
        .filter(|param| param.name.as_ref() != tilo_actions::run::EDITED_FIELD)
        .find_map(|param| argument.get(param.name.as_ref())?.as_str())?;
    Some(tilo_actions::InteractionAnchor::Text {
        region: field.to_owned(),
        ranges: vec![tilo_actions::InteractionTextRange {
            block: tilo_actions::logical_resource_id(&resource.id).into_owned(),
            start: 0,
            end: text.chars().count() as u64,
        }],
    })
}

/// The item a resource is, when it is one: `<kind>/<field>/<item>` of kind
/// `<kind>.<field>`, as apps publish the items of a collection, or a window's
/// alias of one. An id and a kind that do not agree that way name no item.
fn item_of_resource(resource: &str, kind: &str) -> Option<String> {
    // A window binds an item under an alias of its own; the item is the
    // resource the alias stands for.
    let address = tilo_actions::stable_item_address(&tilo_actions::logical_resource_id(resource))?;
    address
        .strip_prefix(kind)?
        .strip_prefix(':')
        .map(str::to_owned)
}

/// The same verb as `action` on each item of its resource the surface published, with the item it
/// is on.
fn item_copies<'a>(
    action: &'a PublishedAction,
    surface: &'a PublishedSurface,
) -> impl Iterator<Item = (&'a PublishedAction, String)> {
    surface.actions.iter().filter_map(|descriptor| {
        if descriptor.action != action.action
            || descriptor.resource_kind.rsplit_once('.')?.0 != action.resource_kind
        {
            return None;
        }
        Some((
            descriptor,
            item_of_resource(&descriptor.resource, &descriptor.resource_kind)?,
        ))
    })
}

/// The parameter of `action`, offered on the resource, that names the item it acts on.
///
/// An item's actions are published on the item's own resource and again on the
/// resource, the resource's copy addressing the item: the one parameter the item's
/// copy leaves out, its method's first. That is what says which value in the
/// argument names the item — an action can name another item beside it
/// (`move-under` takes the new parent) without that being the one it acts on.
fn item_param<'a>(action: &'a PublishedAction, surface: &PublishedSurface) -> Option<&'a str> {
    let (copy, _) = item_copies(action, surface).next()?;
    action
        .params
        .iter()
        .find(|param| !copy.params.iter().any(|own| own.name == param.name))
        .map(|param| param.name.as_ref())
}

/// The item `argument` names of the resource `action` acts in by its id, as the surface
/// published it: the same verb on the item's own resource, labelled in words its app chose
/// ("Book the studio for Friday"). `None` where the argument names no item by its id: the step
/// then says the resource.
fn item_named<'a>(
    action: &'a PublishedAction,
    surface: &'a PublishedSurface,
    argument: Option<&str>,
) -> Option<&'a PublishedAction> {
    let argument = json_argument(argument)?;
    let named = argument
        .get(item_param(action, surface)?)
        .and_then(serde_json::Value::as_str)?;
    item_copies(action, surface).find_map(|(copy, item)| (item == named).then_some(copy))
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
        let shown = caller.map_or_else(|| actor.shown(), |caller| caller.acting().clone());
        let remote = caller.is_some_and(|caller| caller.device_public_key().is_some());
        if shown.direct() && shown.app().is_none() && !remote {
            return None;
        }
        let on = item_named(action, surface, argument).unwrap_or(action);
        let resource = tilo_actions::ActionResource {
            id: on.resource.clone(),
            kind: on.resource_kind.clone(),
            label: on.resource_label.clone(),
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
                // What the app declared this action does, so the line beside
                // the actor's face says it in the reader's language, and what
                // a create makes, so it says that too.
                step: action.contract.step,
                creates: action.contract.creates.clone(),
                did: action.did.clone(),
            },
            anchor: anchor_of(action, &resource, argument),
            adds: adds_of(action, argument, item_param(action, surface)),
            key: (resource.id.clone(), resource.kind.clone()),
            resource,
            group: context.and_then(|context| context.group.clone()),
            sequence: Arc::default(),
        })
    }
}

impl ShownActor {
    /// It, at the place its step landed once it applied ([`landed_anchor`]), and on the item it
    /// made there ([`made_item`]); `None` keeps what it named as it began.
    pub(super) fn landed(&self, output: &tilo_actions::ActionOutput) -> Option<Self> {
        let anchor = landed_anchor(&self.anchor, self.adds.as_ref(), output);
        let resource = made_item(&self.resource, output);
        (anchor.is_some() || resource.is_some()).then(|| Self {
            anchor: anchor.unwrap_or_else(|| self.anchor.clone()),
            resource: resource.unwrap_or_else(|| self.resource.clone()),
            ..self.clone()
        })
    }
}

/// The item a step on `resource` made there, as its output names it: one item of a collection of
/// its kind (`<kind>/<field>/<item>` of kind `<kind>.<field>`).
fn made_item(
    resource: &tilo_actions::ActionResource,
    output: &tilo_actions::ActionOutput,
) -> Option<tilo_actions::ActionResource> {
    let [made] = output.resources.as_slice() else {
        return None;
    };
    let kind = tilo_actions::stable_item_address(made)?
        .rsplit_once(':')?
        .0
        .to_owned();
    let label = resource.label.clone();
    (kind.strip_prefix(resource.kind.as_str())?.starts_with('.')).then(|| {
        tilo_actions::ActionResource {
            id: made.clone(),
            kind,
            label,
        }
    })
}

/// The field of the item it names that `action` adds to, and the characters its argument writes
/// there: a comment's thread and its words. `None` for an action that names no field.
fn adds_of(
    action: &PublishedAction,
    argument: Option<&str>,
    names_item: Option<&str>,
) -> Option<(String, u64)> {
    let field = action.contract.field.clone()?;
    let written =
        json_argument(argument).map_or(0, |argument| written_chars(action, &argument, names_item));
    Some((field, written))
}

/// The named values a caller's argument holds, where it holds them so.
fn json_argument(argument: Option<&str>) -> Option<serde_json::Map<String, serde_json::Value>> {
    match tilo_actions::remote::InvokeArgument::from_slot(argument) {
        tilo_actions::remote::InvokeArgument::Json(argument) => Some(argument),
        _ => None,
    }
}

/// Where a step that began at `anchor` stands once it applied, from what it answered: the
/// characters it says it wrote; else the one item it made, which is where it acted. A comment
/// added to an issue's thread (`adds`) is its words there, under the issue it was made in
/// (`<issue>/<comment>`), so the thread draws them and a row showing the issue stands for them;
/// an item a verb of the tracker made (an issue created) is its row.
fn landed_anchor(
    anchor: &tilo_actions::InteractionAnchor,
    adds: Option<&(String, u64)>,
    output: &tilo_actions::ActionOutput,
) -> Option<tilo_actions::InteractionAnchor> {
    use tilo_actions::InteractionAnchor;
    if let Some(wrote) = output.wrote() {
        return Some(wrote);
    }
    let [made] = output.resources.as_slice() else {
        return None;
    };
    let (collection, id) = made.rsplit_once('/')?;
    let (under, field) = collection.rsplit_once('/').unwrap_or(("", collection));
    match (anchor, adds) {
        (_, Some((adds, chars))) if adds == field => {
            let (_, item) = under.rsplit_once('/')?;
            Some(InteractionAnchor::Text {
                region: field.to_owned(),
                ranges: vec![tilo_actions::InteractionTextRange {
                    block: format!("{item}/{id}"),
                    start: 0,
                    end: *chars,
                }],
            })
        }
        (InteractionAnchor::Resource, _) => Some(InteractionAnchor::ListItems {
            ids: vec![id.to_owned()],
        }),
        _ => None,
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
            actor.actor().id(),
            &surface.route.app_id,
            &surface.route.cluster_id,
            &surface.route.document_id,
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
            actor: actor.actor().clone(),
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
            actor: target.actor.clone(),
            peer_id: target.peer_id,
            route: target.route.clone(),
            address: target.address.clone(),
            status,
        }));
    }
}

#[cfg(test)]
mod anchor_tests {
    use super::*;
    use tilo_actions::{ActionParam, ParamType};

    /// `action` on `resource` of `kind`, declaring `params`.
    fn published(
        resource: &str,
        kind: &str,
        action: &str,
        params: Vec<ActionParam>,
    ) -> (PublishedAction, tilo_actions::ActionResource) {
        let published = PublishedAction {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            resource: resource.into(),
            action: action.into(),
            resource_kind: kind.into(),
            resource_label: "Label".into(),
            action_label: "Action".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params,
            contract: tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Undoable,
                ..Default::default()
            },
            input: crate::apps::collab_app_broker::PublishedInput::None,
            view: None,
        };
        let resource = tilo_actions::ActionResource {
            id: published.resource.clone(),
            kind: published.resource_kind.clone(),
            label: published.resource_label.clone(),
        };
        (published, resource)
    }

    /// The argument slot an invocation carries for `arguments`.
    fn argument(arguments: &serde_json::Value) -> String {
        tilo_actions::remote::InvokeArgument::Json(
            arguments.as_object().cloned().unwrap_or_default(),
        )
        .to_slot()
        .expect("a typed argument has a slot")
    }

    /// A surface of the tracker `tasks.tracker`, publishing one issue as its
    /// own resource so the app's label for it travels with its actions.
    fn tracker() -> (PublishedSurface, PublishedAction) {
        let mut record = published(
            "tasks.tracker",
            "tasks.tracker",
            "rename",
            vec![
                ActionParam::required_owned("issue", ParamType::Text, "Which issue"),
                ActionParam::required_owned("title", ParamType::Text, "Its new title"),
            ],
        );
        record.0.resource_label = "Tasks".into();
        let (mut issue, _) = published(
            "tasks.tracker/issues/7d2b",
            "tasks.tracker.issues",
            "rename",
            vec![ActionParam::required_owned(
                "title",
                ParamType::Text,
                "Its new title",
            )],
        );
        issue.resource_label = "Book the studio for Friday".into();
        let surface = PublishedSurface {
            peer_id: 1,
            app_id: "so.tilo.tasks".into(),
            route: CrdtRoute::app("so.tilo.tasks", "workspace"),
            actions: vec![record.0.clone(), issue],
            live: true,
        };
        (surface, record.0)
    }

    tilo_test_env::scenario! {
        fn an_activity_anchor_names_what_a_step_acts_on;

        phase a_step_on_an_item_names_the_item_its_app_labelled;
        // A step on one of a collection's items names that item, by the label its
        // app published for it: a rename of an issue by its id reads "Editing Book
        // the studio for Friday", never only "Editing Tasks". A step naming no item
        // says the record.
        {
            let (surface, rename) = tracker();
            let named = argument(&serde_json::json!({
                "issue": "7d2b",
                "title": "Book the studio for Friday"
            }));
            assert_eq!(
                item_named(&rename, &surface, Some(&named)).map(|on| on.resource_label.as_str()),
                Some("Book the studio for Friday"),
            );
            // A title that merely looks like an id names no item: only an item of
            // this record, addressed by its id, is the one the step acts on.
            assert_eq!(
                item_named(
                    &rename,
                    &surface,
                    Some(&argument(&serde_json::json!({
                        "issue": "nothing",
                        "title": "Book the studio for Friday"
                    })))
                ),
                None
            );
            assert_eq!(item_named(&rename, &surface, None), None);
            // An action names another item beside the one it acts on — moving an
            // issue under another — and the step still says the issue it moves.
            let under = |item: &str, label: &str| PublishedAction {
                resource: format!("tasks.tracker/issues/{item}"),
                resource_kind: "tasks.tracker.issues".into(),
                resource_label: label.into(),
                params: vec![ActionParam::required_owned(
                    "parent",
                    ParamType::Text,
                    "Which issue it goes under",
                )],
                ..published("", "tasks.tracker", "move-under", Vec::new()).0
            };
            let (mover, _) = published(
                "tasks.tracker",
                "tasks.tracker",
                "move-under",
                vec![
                    ActionParam::required_owned("issue", ParamType::Text, "Which issue to move"),
                    ActionParam::required_owned("parent", ParamType::Text, "Which issue it goes under"),
                ],
            );
            let moving_surface = PublishedSurface {
                actions: vec![
                    mover.clone(),
                    under("7d2b", "Book the studio for Friday"),
                    under("9c1e", "Draft the launch plan"),
                ],
                ..surface.clone()
            };
            let moving = argument(&serde_json::json!({ "issue": "7d2b", "parent": "9c1e" }));
            assert_eq!(
                item_named(&mover, &moving_surface, Some(&moving)).map(|on| on.resource_label.as_str()),
                Some("Book the studio for Friday"),
                "the issue it moves, not the one it moves it under",
            );
            // The same verb on another record's item is another step's item.
            let elsewhere = argument(&serde_json::json!({ "issue": "7d2b" }));
            let (mut other, _) = published(
                "notes.notebook/pages/7d2b",
                "notes.notebook.pages",
                "rename",
                vec![ActionParam::required_owned(
                    "title",
                    ParamType::Text,
                    "Its new title",
                )],
            );
            other.resource_label = "Trip plan".into();
            let surface = PublishedSurface {
                actions: vec![rename.clone(), other],
                ..surface
            };
            assert_eq!(item_named(&rename, &surface, Some(&elsewhere)), None);
        }

        phase an_item_action_names_the_item_it_acts_on;
        {
            // An item is published as its own resource (`<kind>/<field>/<item>`):
            // a verb on it acts on that item, which the view draws as a row.
            let (action, resource) = published(
                "so.tilo.tasks/issues/7d2b",
                "so.tilo.tasks.issues",
                "set-done",
                vec![ActionParam::required_owned(
                    "done",
                    ParamType::Boolean,
                    "Whether it is done",
                )],
            );
            let typed = argument(&serde_json::json!({ "done": true }));
            for argument in [None, Some("true"), Some(typed.as_str())] {
                assert_eq!(
                    anchor_of(&action, &resource, argument),
                    tilo_actions::InteractionAnchor::ListItems {
                        ids: vec!["7d2b".into()]
                    },
                    "the item the verb acts on, whatever it was told",
                );
            }
            // A resource that is no item of a collection is the resource.
            let (action, resource) = published(
                "so.tilo.tasks/issues",
                "so.tilo.tasks",
                "create-issue",
                Vec::new(),
            );
            assert_eq!(
                anchor_of(&action, &resource, None),
                tilo_actions::InteractionAnchor::Resource
            );
        }

        phase a_rich_text_field_edit_names_the_characters_it_writes;
        // A field edit of rich text names the characters it writes, so the editor showing that
        // text draws them as the writer's and its caret where the write ends — the same mark a
        // note's body shows of an agent's write, in Tasks' descriptions as in Notes.
        {
            let (action, resource) = published(
                "so.tilo.tasks",
                "tasks.tracker",
                "field-edit",
                vec![ActionParam::required_owned(
                    "edit",
                    ParamType::Object,
                    "The field and what to do to it",
                )],
            );
            let spliced = argument(&serde_json::json!({
                "edit": {
                    "path": [{"field": "issues"}, {"item": "7d2b"}, {"field": "body"}],
                    "op": "rich",
                    "rich": {
                        "SpliceText": {"region": "Body", "block": "b3", "at": 4, "del": 0, "ins": "héllo"}
                    }
                }
            }));
            assert_eq!(
                text_range(&anchor_of(&action, &resource, Some(&spliced))),
                ("body".into(), "b3".into(), 4, 9),
                "the characters it wrote, counted as the block counts them",
            );
            // Replacing a range leaves the range it wrote: what it inserts there.
            let replaced = argument(&serde_json::json!({
                "edit": {
                    "path": [{"field": "issues"}, {"item": "7d2b"}, {"field": "body"}],
                    "op": "rich",
                    "rich": {
                        "SpliceText": {"region": "Body", "block": "b3", "at": 2, "del": 6, "ins": "new"}
                    }
                }
            }));
            assert_eq!(
                text_range(&anchor_of(&action, &resource, Some(&replaced))),
                ("body".into(), "b3".into(), 2, 5),
            );
            // A field edit that writes no characters (a whole value set) names the
            // record, not a run.
            let set = argument(&serde_json::json!({
                "edit": {
                    "path": [{"field": "issues"}, {"item": "7d2b"}, {"field": "title"}],
                    "op": "set",
                    "value": "Book the studio"
                }
            }));
            assert_eq!(
                anchor_of(&action, &resource, Some(&set)),
                tilo_actions::InteractionAnchor::Resource,
            );
        }

        phase a_text_field_edit_and_a_rename_name_the_characters_they_write;
        // A text field's own edit names the characters it writes in the item holding the field,
        // so the field draws them as the writer's with its caret; a verb setting an item's text
        // field from its parameter of that name (a rename) names the whole text it writes there.
        {
            let (action, resource) = published(
                "so.tilo.tasks",
                "tasks.tracker",
                "field-edit",
                vec![ActionParam::required_owned(
                    "edit",
                    ParamType::Object,
                    "The field and what to do to it",
                )],
            );
            let typed = argument(&serde_json::json!({
                "edit": {
                    "path": [{"field": "issues"}, {"item": "7d2b"}, {"field": "title"}],
                    "op": "splice", "at": 5, "delete": 0, "insert": "rôom"
                }
            }));
            assert_eq!(
                text_range(&anchor_of(&action, &resource, Some(&typed))),
                ("title".into(), "7d2b".into(), 5, 9),
            );
            let replaced = argument(&serde_json::json!({
                "edit": {"path": [{"field": "name"}], "op": "replace", "text": "Mixer"}
            }));
            assert_eq!(
                text_range(&anchor_of(&action, &resource, Some(&replaced))),
                ("name".into(), String::new(), 0, 5),
            );
            let (mut action, resource) = published(
                "so.tilo.tasks/issues/7d2b",
                "so.tilo.tasks.issues",
                "rename",
                vec![ActionParam::required_owned(
                    "title",
                    ParamType::Text,
                    "Title",
                )],
            );
            action.contract.field = Some("title".into());
            let renamed = argument(&serde_json::json!({ "title": "Book the studio" }));
            assert_eq!(
                text_range(&anchor_of(&action, &resource, Some(&renamed))),
                ("title".into(), "7d2b".into(), 0, 15),
            );
            // Offered on the collection, it acts on the item its argument names ([`item_named`]).
            let (mut on_resource, _) = published(
                "so.tilo.tasks",
                "tasks.tracker",
                "rename",
                vec![
                    ActionParam::required_owned("issue", ParamType::Text, "Issue"),
                    ActionParam::required_owned("title", ParamType::Text, "Title"),
                ],
            );
            on_resource.contract.field = Some("title".into());
            let renamed_by_id = argument(&serde_json::json!({ "issue": "7d2b", "title": "Book" }));
            assert_eq!(
                text_range(&anchor_of(&on_resource, &resource, Some(&renamed_by_id))),
                ("title".into(), "7d2b".into(), 0, 4),
            );
            // Offered on the item under the alias its window bound it as (a note's rename
            // pressed through its row), the item is the one the alias stands for.
            let alias = format!(
                "{}{}:{}:{}",
                tilo_actions::BOUND_RESOURCE_PREFIX,
                uuid::Uuid::from_u128(1),
                uuid::Uuid::from_u128(2),
                hex::encode("so.tilo.tasks/issues/7d2b"),
            );
            let (mut bound, resource) = published(
                &alias,
                "so.tilo.tasks.issues",
                "rename",
                vec![ActionParam::required_owned("title", ParamType::Text, "Title")],
            );
            bound.contract.field = Some("title".into());
            assert_eq!(
                text_range(&anchor_of(&bound, &resource, Some(&renamed))),
                ("title".into(), "7d2b".into(), 0, 15),
            );
            // An edit naming the field it writes by its `field` argument writes it in the
            // resource it is offered on.
            let (edit, resource) = published(
                "tl:so.tilo.tasks:root/i1",
                "tl.so.tilo.tasks.Task",
                "edit-text",
                vec![
                    ActionParam::required_owned("field", ParamType::Text, "Field"),
                    ActionParam::required_owned("value", ParamType::Text, "Value"),
                ],
            );
            let edited = argument(&serde_json::json!({ "field": "title", "value": "Book film" }));
            assert_eq!(
                text_range(&anchor_of(&edit, &resource, Some(&edited))),
                ("title".into(), "tl:so.tilo.tasks:root/i1".into(), 0, 9),
            );
            let resource = tilo_actions::ActionResource {
                id: "so.tilo.tasks/issues/7d2b".into(),
                kind: "so.tilo.tasks.issues".into(),
                label: "Label".into(),
            };
            // Offered on the record, a verb that writes no text of the item (a comment, a
            // move to done) acts on the item its argument names: the row showing it.
            let (mut comment, _) = published(
                "so.tilo.tasks",
                "tasks.tracker",
                "comment",
                vec![
                    ActionParam::required_owned("issue", ParamType::Text, "Issue"),
                    ActionParam::required_owned("text", ParamType::Text, "What it says"),
                ],
            );
            comment.contract.field = Some("comments".into());
            let said = argument(&serde_json::json!({ "issue": "7d2b", "text": "The studio is free" }));
            assert_eq!(
                anchor_of(&comment, &resource, Some(&said)),
                tilo_actions::InteractionAnchor::ListItems {
                    ids: vec!["7d2b".into()]
                },
            );
            // Once it applied, the comment is the words it added under the issue: the thread
            // draws them, and a row showing the issue stands for them.
            let adds = adds_of(&comment, Some(&said), Some("issue"));
            assert_eq!(adds, Some(("comments".to_owned(), 18)), "its words, not the issue's id");
            // Named by what it says, the issue is no word of the comment either.
            let said_by_title =
                argument(&serde_json::json!({ "issue": "Book the studio", "text": "The studio is free" }));
            let by_title = adds_of(&comment, Some(&said_by_title), Some("issue"));
            assert_eq!(by_title, adds, "its words, not the issue's title");
            let made = tilo_actions::ActionOutput {
                value: None,
                resources: vec!["tasks.issues/issues/7d2b/comments/c9f1".into()],
            };
            let at_issue = tilo_actions::InteractionAnchor::ListItems {
                ids: vec!["7d2b".into()],
            };
            assert_eq!(
                text_range(&landed_anchor(&at_issue, adds.as_ref(), &made).expect("it landed")),
                ("comments".into(), "7d2b/c9f1".into(), 0, 18),
            );
            // Begun on the resource, an issue named by its title, it lands under the issue it was
            // made in.
            let on_resource_anchor = tilo_actions::InteractionAnchor::Resource;
            assert_eq!(
                text_range(&landed_anchor(&on_resource_anchor, by_title.as_ref(), &made).expect("it landed")),
                ("comments".into(), "7d2b/c9f1".into(), 0, 18),
            );
            // An issue made from the tracker's own control is its new row once it applied.
            assert_eq!(
                landed_anchor(
                    &tilo_actions::InteractionAnchor::Resource,
                    None,
                    &tilo_actions::ActionOutput {
                        value: None,
                        resources: vec!["tasks.issues/issues/e41a".into()],
                    },
                ),
                Some(tilo_actions::InteractionAnchor::ListItems {
                    ids: vec!["e41a".into()]
                }),
            );
            // A verb under a field whose text parameter is something else (a comment under
            // `comments`) acts on the item.
            action.contract.field = Some("comments".into());
            assert_eq!(
                anchor_of(&action, &resource, Some(&renamed)),
                tilo_actions::InteractionAnchor::ListItems {
                    ids: vec!["7d2b".into()]
                },
            );
        }

        phase a_step_that_made_an_item_names_it_once_it_landed;
        {
            // A note made with words is what it acted on once it landed: the note's own resource.
            let notes = tilo_actions::ActionResource {
                id: "doc:notebook".into(),
                kind: "notes.pages".into(),
                label: "Notes".into(),
            };
            let made = |resources: &[&str]| {
                let resources = resources.iter().map(|id| (*id).to_owned()).collect();
                made_item(&notes, &tilo_actions::ActionOutput { value: None, resources })
            };
            let note = made(&["notes.pages/pages/3e43"]).expect("the note it made");
            assert_eq!((note.id.as_str(), note.kind.as_str()), ("notes.pages/pages/3e43", "notes.pages.pages"));
            // Something made elsewhere, under an item, or several things name no item made here.
            assert_eq!(made(&["tasks.tracker/issues/e41a"]), None);
            assert_eq!(made(&["notes.pages/pages/3e43/comments/c9f1"]), None);
            assert_eq!(made(&["notes.pages/pages/3e43", "notes.pages/pages/9a1b"]), None);
        }

        phase a_place_names_the_characters_a_text_edit_writes;
        {
            // `replace-range`: the characters it writes replace the range it was
            // given, and are what the view highlights.
            let (action, resource) = published(
                "so.tilo.notes/pages/1",
                "so.tilo.notes.page",
                "replace-range",
                vec![
                    ActionParam::required_owned("at", ParamType::Object, "Where").as_place(),
                    ActionParam::required_owned("text", ParamType::Text, "What to write"),
                ],
            );
            let replaced = argument(&serde_json::json!({
                "at": { "item": "b1", "start": 4, "end": 9 },
                "text": "hello",
            }));
            assert_eq!(
                text_range(&anchor_of(&action, &resource, Some(&replaced))),
                ("text".into(), "b1".into(), 4, 9),
                "the characters it wrote, not the place it wrote them at",
            );
            // At a caret, the written characters are inserted there.
            let inserted = argument(&serde_json::json!({
                "at": { "item": "b1", "at": 12 },
                "text": "héllo",
            }));
            assert_eq!(
                text_range(&anchor_of(&action, &resource, Some(&inserted))),
                ("text".into(), "b1".into(), 12, 17),
                "characters, as the place counts them, not bytes",
            );
            // A deletion writes nothing: the characters it removed are the change.
            let deleted = argument(&serde_json::json!({
                "at": { "item": "b1", "start": 3, "end": 11 },
                "text": "",
            }));
            assert_eq!(
                text_range(&anchor_of(&action, &resource, Some(&deleted))),
                ("text".into(), "b1".into(), 3, 11),
            );
            // A place naming its collection says which region its blocks sit in.
            let (action, resource) = published(
                "so.tilo.notes/pages/1",
                "so.tilo.notes.page",
                "replace-range",
                vec![
                    ActionParam::required_owned("at", ParamType::Object, "Where").as_place_in("blocks"),
                    ActionParam::required_owned("text", ParamType::Text, "What to write"),
                ],
            );
            assert_eq!(
                text_range(&anchor_of(&action, &resource, Some(&replaced))).0,
                "blocks",
            );
            // No place parameter, or no place argument: the resource.
            let (action, resource) = published(
                "so.tilo.notes/pages/1",
                "so.tilo.notes.page",
                "retitle",
                vec![ActionParam::required_owned(
                    "title",
                    ParamType::Text,
                    "The new title",
                )],
            );
            assert_eq!(
                anchor_of(
                    &action,
                    &resource,
                    Some(&argument(&serde_json::json!({ "title": "x" }))),
                ),
                tilo_actions::InteractionAnchor::Resource,
            );
            let (action, resource) = published(
                "so.tilo.notes/pages/1",
                "so.tilo.notes.page",
                "replace-range",
                vec![
                    ActionParam::required_owned("at", ParamType::Object, "Where").as_place(),
                    ActionParam::required_owned("text", ParamType::Text, "What to write"),
                ],
            );
            assert_eq!(
                anchor_of(&action, &resource, None),
                tilo_actions::InteractionAnchor::Resource,
            );
        }
    }

    fn text_range(anchor: &tilo_actions::InteractionAnchor) -> (String, String, u64, u64) {
        let tilo_actions::InteractionAnchor::Text { region, ranges } = anchor else {
            panic!("a text edit names a text range, not {anchor:?}");
        };
        let [range] = ranges.as_slice() else {
            panic!("one place names one range");
        };
        (region.clone(), range.block.clone(), range.start, range.end)
    }
}
