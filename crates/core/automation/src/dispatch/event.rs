use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_actions::Acting;
use tilo_actions::context::{ActionContext, Cause};
use tilo_actions::entry::Did;

/// The item a change touched: its kind, id and label at the time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRef {
    pub kind: String,
    pub id: String,
    pub label: String,
}

/// One committed change to a record in the workspace, as a trigger sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentEvent {
    /// The workspace's id.
    pub workspace: String,
    /// The record's id.
    pub record: String,
    pub kind: String,
    /// The change's hash, hex: the event's identity.
    pub change: String,
    pub actor: Acting,
    pub did: Did,
    /// The full recorded arguments of the verb, when the change was one.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub arguments: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<Cause>,
    /// The member whose change this one copies, as their actor bytes read (`agent:<id>`), when
    /// a writer copied one ([`tilo_actions::ORIGIN_FACET`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The change carries the incoming-message facet.
    #[serde(default)]
    pub incoming_message: bool,
    /// The change is housekeeping about the record (marking a channel
    /// read, a person introducing themselves), not a change to what it
    /// holds.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub housekeeping: bool,
    /// The Shortcut whose run made the change (the `shortcut` facet an invocation
    /// writes), so that Shortcut never fires on its own output: its
    /// changes carry the person's id as actor, not the Shortcut's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
    /// The Shortcut run the change belongs to (its group id), so a device
    /// that sees an invocation another device started never starts it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
}

impl DocumentEvent {
    /// The Shortcut run this change belongs to, when it is one's.
    #[must_use]
    pub fn started_invocation(&self) -> Option<crate::AutomationInvocationId> {
        self.invocation
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .map(crate::AutomationInvocationId)
    }
}

/// The event the history entry `context` of change `change` (hex hash) to
/// record `record` of `kind` in `workspace` describes. Every host reads an
/// entry through this one function, so a person's devices and a workspace's
/// host see the same event.
#[must_use]
pub fn event_of(
    workspace: &str,
    record: &str,
    kind: &str,
    change: &str,
    context: &ActionContext,
) -> DocumentEvent {
    let facet = |key: &str| {
        context
            .facets
            .iter()
            .find(|facet| facet.key == key)
            .map(|facet| facet.value.clone())
    };
    DocumentEvent {
        workspace: workspace.to_owned(),
        record: record.to_owned(),
        kind: kind.to_owned(),
        change: change.to_owned(),
        actor: context
            .acting()
            .cloned()
            .unwrap_or_else(|| Acting::person(String::new())),
        did: context
            .what
            .clone()
            .unwrap_or_else(|| Did::verb(tilo_actions::ActionKey::new("", kind, ""), [])),
        arguments: arguments_of(context),
        item: context.item.as_ref().map(|item| ItemRef {
            kind: context
                .resource
                .as_ref()
                .map_or_else(String::new, |resource| resource.kind.clone()),
            id: item.id.clone(),
            label: item.label.clone(),
        }),
        before: None,
        after: None,
        caused_by: context.caused_by.clone(),
        origin: facet(tilo_actions::ORIGIN_FACET),
        incoming_message: facet(tilo_actions::INCOMING_MESSAGE_FACET).is_some(),
        housekeeping: context.is_housekeeping(),
        shortcut: facet("shortcut"),
        invocation: context
            .group
            .as_ref()
            .filter(|group| group.kind == "shortcut")
            .map(|group| group.id.clone()),
    }
}

/// The verb's full arguments: the recorded action's JSON payload, out of
/// the envelope an action API records it in (`{"op": {..}, "at": ..}`) and
/// unwrapped from its variant tag (`{"action": .., "args": {..}}` or
/// `{"variant": {..}}`), else the entry's plain arguments.
fn arguments_of(context: &ActionContext) -> Map<String, Value> {
    let payload = context
        .action
        .as_ref()
        .and_then(|action| serde_json::from_slice::<Value>(&action.payload).ok())
        .map(|payload| match payload {
            Value::Object(mut envelope)
                if envelope
                    .get("op")
                    .is_some_and(|op| op.get("action").is_some()) =>
            {
                envelope.remove("op").unwrap_or_default()
            }
            payload => payload,
        });
    match payload {
        Some(Value::Object(mut outer)) => match outer.remove("args") {
            Some(Value::Object(args)) => args,
            Some(other) => {
                outer.insert("args".into(), other);
                outer
            }
            None if outer.len() == 1 => match outer.values_mut().next() {
                Some(Value::Object(inner)) => std::mem::take(inner),
                _ => outer,
            },
            None => outer,
        },
        _ => match &context.what {
            Some(Did::Verb(call)) => call
                .args
                .iter()
                .map(|(name, scalar)| {
                    (
                        name.clone(),
                        serde_json::to_value(scalar).unwrap_or(Value::Null),
                    )
                })
                .collect(),
            _ => Map::new(),
        },
    }
}

/// An event from a connector's trigger source, with the service's id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectorEvent {
    pub app_id: String,
    pub source: String,
    /// The service's id for the event: its identity.
    pub id: String,
    pub at_ms: i64,
    #[serde(default)]
    pub payload: Map<String, Value>,
}

/// Something an application declared next to its actions, published as a
/// change to a record of its own kind in the workspace (`mail.message`,
/// `calendar.event`). The kind carries the declaring app, so no host code
/// knows any one app's events.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppEvent {
    /// The workspace whose record announced it.
    pub workspace: String,
    pub app_id: String,
    /// The announced event kind, such as `mail.message`.
    pub kind: String,
    /// What was declared about the event, as the app published it.
    #[serde(default)]
    pub payload: Map<String, Value>,
}

impl AppEvent {
    /// What a record change announced, when its kind is the app's id, a dot
    /// and an event kind, closed by `.event`: `so.tilo.mail` announcing
    /// `mail.message` publishes the kind `so.tilo.mail.mail.message.event`.
    /// The prefix is the app whose action the change invoked, so one app
    /// cannot speak for another.
    #[must_use]
    pub fn of_change(change: &DocumentEvent) -> Option<Self> {
        // The app id is read off the change, not off the kind: an id has dots
        // and a kind has dots, so the prefix decides which app spoke.
        let Did::Verb(call) = &change.did else {
            return None;
        };
        let app_id = call.action.app.as_str();
        if app_id.is_empty() {
            return None;
        }
        let kind = change
            .kind
            .strip_prefix(app_id)?
            .strip_prefix(SEPARATOR)?
            .strip_suffix(EVENT_KIND_SUFFIX)?
            .to_owned();
        if kind.is_empty() {
            return None;
        }
        let mut payload = change.arguments.clone();
        if let Some(item) = &change.item
            && !payload.contains_key("record")
        {
            payload.insert("record".into(), Value::from(item.id.as_str()));
        }
        Some(Self {
            workspace: change.workspace.clone(),
            app_id: app_id.to_owned(),
            kind,
            payload,
        })
    }
}

/// What a firing of an app-event trigger binds as its event input: the
/// application, the event it announced, and what it announced beside them —
/// the app event's own fields, one level down from the record event's.
#[must_use]
pub fn app_event_object(declared: &AppEvent) -> Map<String, Value> {
    let mut bound = Map::from_iter([
        ("app_id".to_string(), Value::from(declared.app_id.as_str())),
        ("kind".to_string(), Value::from(declared.kind.as_str())),
    ]);
    bound.extend(declared.payload.clone());
    bound
}

/// What an application publishes an event as: a record kind closed by this
/// suffix, after an app id and an event kind ([`AppEvent::of_change`]).
const EVENT_KIND_SUFFIX: &str = ".event";

/// The character `<app id>` and `<event kind>` are separated by.
const SEPARATOR: char = '.';

/// A fire time of one of a program's schedules coming round.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleEvent {
    /// The workspace whose host (or whose person's device) keeps the time.
    pub workspace: String,
    /// Index of the schedule among the program's triggers.
    pub trigger: usize,
    /// The fire time, ms since the epoch.
    pub due_ms: i64,
}

/// A moment before, at or after a time a resource holds having come
/// ([`crate::shortcut::TriggerSpec::ResourceTime`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeEvent {
    /// The workspace whose holder keeps the time.
    pub workspace: String,
    /// Index of the trigger among the program's triggers.
    pub trigger: usize,
    /// What holds the time: one item's field, or one occurrence of an entry
    /// that repeats. It stays the same when the time moves.
    pub key: String,
    /// The time held, ms since the epoch.
    pub at_ms: i64,
    /// What the invocation is told ([`crate::resource_time::told`]).
    #[serde(default)]
    pub payload: Map<String, Value>,
}

/// Something new found in a source
/// ([`crate::shortcut::TriggerSpec::SourceChanged`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceEvent {
    /// The workspace whose holder looks at the source.
    pub workspace: String,
    /// Index of the trigger among the program's triggers.
    pub trigger: usize,
    /// What names the new thing for good
    /// ([`crate::source_change::Found::key`]).
    pub key: String,
    /// What the invocation is told ([`crate::source_change::told`]).
    #[serde(default)]
    pub payload: Map<String, Value>,
}

/// Either kind of event.
#[derive(Clone, Copy, Debug)]
pub enum Event<'a> {
    Record(&'a DocumentEvent),
    Connector(&'a ConnectorEvent),
    AppEvent(&'a AppEvent),
    Schedule(&'a ScheduleEvent),
    Time(&'a TimeEvent),
    Source(&'a SourceEvent),
}

impl Event<'_> {
    /// What identifies the event: the change hash, `app/source/id` for
    /// a connector event, the fire time of a schedule, or what holds a time
    /// with the time it holds, so a time that moved is another event.
    #[must_use]
    pub fn identity(&self) -> String {
        match self {
            Self::Record(event) => event.change.clone(),
            Self::Connector(event) => format!("{}/{}/{}", event.app_id, event.source, event.id),
            // Two declarations of the same app and kind are the same event
            // announced twice.
            Self::AppEvent(event) => {
                let payload = serde_json::to_string(&event.payload).unwrap_or_default();
                format!("app_event\0{}\0{}\0{payload}", event.app_id, event.kind)
            }
            Self::Schedule(event) => format!(
                "schedule\0{}\0{}\0{}",
                event.workspace, event.trigger, event.due_ms
            ),
            Self::Time(event) => format!(
                "time\0{}\0{}\0{}\0{}",
                event.workspace, event.trigger, event.key, event.at_ms
            ),
            // The same thing found again is the same event.
            Self::Source(event) => format!(
                "source\0{}\0{}\0{}",
                event.workspace, event.trigger, event.key
            ),
        }
    }

    /// The value bound to the program's `event` input.
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::Record(event) => serde_json::to_value(event).unwrap_or(Value::Null),
            Self::Connector(event) => json!({
                "app_id": event.app_id,
                "source": event.source,
                "id": event.id,
                "at_ms": event.at_ms,
                "payload": Value::Object(event.payload.clone()),
            }),
            Self::AppEvent(event) => json!({
                "app_id": event.app_id,
                "kind": event.kind,
                "payload": Value::Object(event.payload.clone()),
            }),
            Self::Schedule(event) => json!({
                "due_ms": event.due_ms,
                "at_ms": event.due_ms,
            }),
            Self::Time(event) => Value::Object(event.payload.clone()),
            Self::Source(event) => Value::Object(event.payload.clone()),
        }
    }
}
