use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_actions::context::{ActionContext, Cause};
use tilo_actions::entry::Did;
use tilo_actions::interaction::InteractionActor;

/// The item a change touched: its kind, id and label at the time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRef {
    pub kind: String,
    pub id: String,
    pub label: String,
}

/// One committed change to a record in the room, as a trigger sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordEvent {
    pub room: String,
    /// The record's id.
    pub record: String,
    pub kind: String,
    /// The change's hash, hex: the event's identity.
    pub change: String,
    pub actor: InteractionActor,
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
    /// The change carries the incoming-message facet.
    #[serde(default)]
    pub incoming_message: bool,
    /// The Shortcut whose run made the change (the `shortcut` facet a run
    /// writes), so that Shortcut never fires on its own output: its
    /// changes carry the person's id as actor, not the Shortcut's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
    /// The Shortcut run the change belongs to (its group id), so a device
    /// that sees a run another device started never starts it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

impl RecordEvent {
    /// The Shortcut run this change belongs to, when it is one's.
    #[must_use]
    pub fn started_run(&self) -> Option<crate::RunId> {
        self.run
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .map(crate::RunId)
    }
}

/// The event the history entry `context` of change `change` (hex hash) to
/// record `record` of `kind` in `room` describes. Every host reads an
/// entry through this one function, so a person's devices and a room's
/// host see the same event.
#[must_use]
pub fn event_of(
    room: &str,
    record: &str,
    kind: &str,
    change: &str,
    context: &ActionContext,
) -> RecordEvent {
    let invoker = context.invoker.clone().unwrap_or_default();
    let facet = |key: &str| {
        context
            .facets
            .iter()
            .find(|facet| facet.key == key)
            .map(|facet| facet.value.clone())
    };
    RecordEvent {
        room: room.to_owned(),
        record: record.to_owned(),
        kind: kind.to_owned(),
        change: change.to_owned(),
        actor: InteractionActor {
            id: invoker.actor,
            kind: invoker.kind,
            for_person: invoker.on_behalf_of,
        },
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
        incoming_message: facet(tilo_actions::INCOMING_MESSAGE_FACET).is_some(),
        shortcut: facet("shortcut"),
        run: context
            .group
            .as_ref()
            .filter(|group| group.kind == "shortcut")
            .map(|group| group.id.clone()),
    }
}

/// The verb's full arguments: the recorded action's JSON payload, out of
/// the envelope a door records it in (`{"op": {..}, "at": ..}`) and
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

/// Either kind of event.
#[derive(Clone, Copy, Debug)]
pub enum Event<'a> {
    Record(&'a RecordEvent),
    Connector(&'a ConnectorEvent),
}

impl Event<'_> {
    /// What identifies the event: the change hash, or `app/source/id` for
    /// a connector event.
    #[must_use]
    pub fn identity(&self) -> String {
        match self {
            Self::Record(event) => event.change.clone(),
            Self::Connector(event) => format!("{}/{}/{}", event.app_id, event.source, event.id),
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
        }
    }

    pub(super) const fn caused_by(&self) -> Option<&Cause> {
        match self {
            Self::Record(event) => event.caused_by.as_ref(),
            Self::Connector(_) => None,
        }
    }
}
