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
    /// What it acts in, named as the step's line says it: the item's own label
    /// where the invocation names one, else the record's.
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
    let parsed = tilo_actions::remote::InvokeArgument::from_slot(argument);
    // A field edit that splices a rich text names the characters it writes,
    // whatever it is addressed to: the editor showing that text draws them as
    // the writer's, and their caret where the write ends.
    if let tilo_actions::remote::InvokeArgument::Json(argument) = &parsed
        && let Some((region, block, start, end)) = written_text(argument)
    {
        return InteractionAnchor::Text {
            region,
            ranges: vec![tilo_actions::InteractionTextRange { block, start, end }],
        };
    }
    // A verb on an item that writes one of its text fields from its parameter
    // of that name (a rename's `title`) names the text it writes there: the
    // field showing it draws it as the writer's, with its caret at its end.
    // The item is the item resource it is offered on, or, offered on the
    // record, its first parameter's id.
    if let Some(field) = action.contract.field.as_deref()
        && let tilo_actions::remote::InvokeArgument::Json(argument) = &parsed
        && let Some(item) = item_of_resource(&resource.id, &resource.kind).or_else(|| {
            let first = action
                .params
                .first()
                .filter(|param| param.name.as_ref() != field)?;
            Some(argument.get(first.name.as_ref())?.as_str()?.to_owned())
        })
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
    // An item published as its own resource (`<kind>/<field>/<item>`) is what
    // its actions act on, and the view draws it as a row.
    if let Some(item) = item_of_resource(&resource.id, &resource.kind)
        && !action
            .params
            .iter()
            .any(tilo_actions::ActionParam::is_place)
    {
        return InteractionAnchor::ListItems { ids: vec![item] };
    }
    let tilo_actions::remote::InvokeArgument::Json(argument) = parsed else {
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

/// The item a resource is, when it is one: `<kind>/<field>/<item>` of kind
/// `<kind>.<field>`, as apps publish the items of a collection. An id and a
/// kind that do not agree that way name no item.
fn item_of_resource(resource: &str, kind: &str) -> Option<String> {
    let (record, field) = kind.rsplit_once('.')?;
    let rest = resource.strip_prefix(record)?.strip_prefix('/')?;
    let (named, item) = rest.split_once('/')?;
    (named == field && !item.is_empty() && !item.contains('/')).then(|| item.to_owned())
}

/// The name the surface published for the item `argument` names of the record
/// `action` acts in, in words its app chose ("Book room 4 for Friday"): the
/// label the item's own published resource carries.
///
/// An item's actions are published on the item's own resource and again on the
/// record, with the item's id as an argument (`<record>#rename` addressed by
/// `issue`), so a caller naming an item either way names that item. `None`
/// where the argument names none: the step then says the record.
fn item_named<'a>(
    action: &PublishedAction,
    surface: &'a PublishedSurface,
    argument: Option<&str>,
) -> Option<&'a str> {
    /// Every string `value` holds, at any depth.
    fn strings<'v>(value: &'v serde_json::Value, into: &mut Vec<&'v str>) {
        match value {
            serde_json::Value::String(text) => into.push(text),
            serde_json::Value::Array(items) => {
                for item in items {
                    strings(item, into);
                }
            }
            serde_json::Value::Object(fields) => {
                for item in fields.values() {
                    strings(item, into);
                }
            }
            _ => {}
        }
    }
    let tilo_actions::remote::InvokeArgument::Json(argument) =
        tilo_actions::remote::InvokeArgument::from_slot(argument)
    else {
        return None;
    };
    let argument = serde_json::Value::Object(argument);
    let mut said: Vec<&str> = Vec::new();
    strings(&argument, &mut said);
    surface.actions.iter().find_map(|descriptor| {
        // The same verb on an item of this record, by the item resource the app
        // published for it: what a person reads it by.
        if descriptor.action != action.action
            || descriptor.resource_kind.rsplit_once('.')?.0 != action.resource_kind
        {
            return None;
        }
        let item = item_of_resource(&descriptor.resource, &descriptor.resource_kind)?;
        said.contains(&item.as_str())
            .then_some(descriptor.resource_label.as_str())
    })
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
        let shown = caller.map_or_else(|| actor.shown(), Caller::actor);
        let remote = caller.is_some_and(|caller| caller.device_public_key().is_some());
        if !shown.kind.acts_for_someone() && shown.for_person.is_none() && !remote {
            return None;
        }
        let label = item_named(action, surface, argument).unwrap_or(&action.resource_label);
        let resource = tilo_actions::ActionResource {
            id: action.resource.clone(),
            kind: action.resource_kind.clone(),
            label: label.to_owned(),
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
            input: crate::collab_app_broker::PublishedInput::None,
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
    fn slot(arguments: &serde_json::Value) -> String {
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
        issue.resource_label = "Book room 4 for Friday".into();
        let surface = PublishedSurface {
            peer_id: 1,
            app_id: "so.tilo.tasks".into(),
            route: CrdtRoute::app("so.tilo.tasks", "workspace"),
            actions: vec![record.0.clone(), issue],
            live: true,
        };
        (surface, record.0)
    }

    /// A step on one of a collection's items names that item, by the label its
    /// app published for it: a rename of an issue by its id reads "Editing Book
    /// room 4 for Friday", never only "Editing Tasks". A step naming no item
    /// says the record.
    #[test]
    fn a_step_on_an_item_names_the_item_its_app_labelled() {
        let (surface, rename) = tracker();
        let named = slot(&serde_json::json!({
            "issue": "7d2b",
            "title": "Book room four for Friday"
        }));
        assert_eq!(
            item_named(&rename, &surface, Some(&named)),
            Some("Book room 4 for Friday"),
        );
        // A title that merely looks like an id names no item: only an item of
        // this record, addressed by its id, is the one the step acts on.
        assert_eq!(
            item_named(
                &rename,
                &surface,
                Some(&slot(&serde_json::json!({
                    "issue": "nothing",
                    "title": "Book room four for Friday"
                })))
            ),
            None
        );
        assert_eq!(item_named(&rename, &surface, None), None);
        // The same verb on another record's item is another step's item.
        let elsewhere = slot(&serde_json::json!({ "issue": "7d2b" }));
        let (mut other, _) = published(
            "notes.notebook/pages/7d2b",
            "notes.notebook.pages",
            "rename",
            Vec::new(),
        );
        other.resource_label = "Trip plan".into();
        let surface = PublishedSurface {
            actions: vec![rename.clone(), other],
            ..surface
        };
        assert_eq!(item_named(&rename, &surface, Some(&elsewhere)), None);
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

    #[test]
    fn an_item_action_names_the_item_it_acts_on() {
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
        let typed = slot(&serde_json::json!({ "done": true }));
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

    /// A field edit of rich text names the characters it writes, so the editor showing that
    /// text draws them as the writer's and its caret where the write ends — the same mark a
    /// note's body shows of an agent's write, in Tasks' descriptions as in Notes.
    #[test]
    fn a_rich_text_field_edit_names_the_characters_it_writes() {
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
        let spliced = slot(&serde_json::json!({
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
        let replaced = slot(&serde_json::json!({
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
        let set = slot(&serde_json::json!({
            "edit": {
                "path": [{"field": "issues"}, {"item": "7d2b"}, {"field": "title"}],
                "op": "set",
                "value": "Book room four"
            }
        }));
        assert_eq!(
            anchor_of(&action, &resource, Some(&set)),
            tilo_actions::InteractionAnchor::Resource,
        );
    }

    /// A text field's own edit names the characters it writes in the item holding the field,
    /// so the field draws them as the writer's with its caret; a verb setting an item's text
    /// field from its parameter of that name (a rename) names the whole text it writes there.
    #[test]
    fn a_text_field_edit_and_a_rename_name_the_characters_they_write() {
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
        let typed = slot(&serde_json::json!({
            "edit": {
                "path": [{"field": "issues"}, {"item": "7d2b"}, {"field": "title"}],
                "op": "splice", "at": 5, "delete": 0, "insert": "rôom"
            }
        }));
        assert_eq!(
            text_range(&anchor_of(&action, &resource, Some(&typed))),
            ("title".into(), "7d2b".into(), 5, 9),
        );
        let replaced = slot(&serde_json::json!({
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
        let renamed = slot(&serde_json::json!({ "title": "Book room four" }));
        assert_eq!(
            text_range(&anchor_of(&action, &resource, Some(&renamed))),
            ("title".into(), "7d2b".into(), 0, 14),
        );
        // Offered on the record, the item is its first parameter's id.
        let (mut on_record, resource) = published(
            "so.tilo.tasks",
            "tasks.tracker",
            "rename",
            vec![
                ActionParam::required_owned("issue", ParamType::Text, "Issue"),
                ActionParam::required_owned("title", ParamType::Text, "Title"),
            ],
        );
        on_record.contract.field = Some("title".into());
        let renamed_by_id = slot(&serde_json::json!({ "issue": "7d2b", "title": "Book" }));
        assert_eq!(
            text_range(&anchor_of(&on_record, &resource, Some(&renamed_by_id))),
            ("title".into(), "7d2b".into(), 0, 4),
        );
        let resource = tilo_actions::ActionResource {
            id: "so.tilo.tasks/issues/7d2b".into(),
            kind: "so.tilo.tasks.issues".into(),
            label: "Label".into(),
        };
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

    #[test]
    fn a_place_names_the_characters_a_text_edit_writes() {
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
        let replaced = slot(&serde_json::json!({
            "at": { "item": "b1", "start": 4, "end": 9 },
            "text": "hello",
        }));
        assert_eq!(
            text_range(&anchor_of(&action, &resource, Some(&replaced))),
            ("text".into(), "b1".into(), 4, 9),
            "the characters it wrote, not the place it wrote them at",
        );
        // At a caret, the written characters are inserted there.
        let inserted = slot(&serde_json::json!({
            "at": { "item": "b1", "at": 12 },
            "text": "héllo",
        }));
        assert_eq!(
            text_range(&anchor_of(&action, &resource, Some(&inserted))),
            ("text".into(), "b1".into(), 12, 17),
            "characters, as the place counts them, not bytes",
        );
        // A deletion writes nothing: the characters it removed are the change.
        let deleted = slot(&serde_json::json!({
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
                Some(&slot(&serde_json::json!({ "title": "x" })))
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
