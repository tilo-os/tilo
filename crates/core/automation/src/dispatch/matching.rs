use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tilo_actions::Acting;
use tilo_actions::entry::{Did, PathStep};
use tilo_actions::params::ParamType;

use super::event::{ConnectorEvent, Event};
use crate::AutomationInvocationId;
use crate::shortcut::{ActorFilter, ChangeFilter, EVENT_INPUT, Shortcut, TriggerSpec};

/// Who a firing runs as: a saved shortcut, by id, and the person who saved it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Firer<'a> {
    pub shortcut: &'a str,
    pub saver: &'a str,
}

impl Firer<'_> {
    /// `shortcut:<id>`: what its firings' invocation ids are derived under.
    #[must_use]
    pub fn key(&self) -> String {
        format!("shortcut:{}", self.shortcut)
    }
}

/// A program a host may start, with whom it runs as.
#[derive(Clone, Copy, Debug)]
pub struct Armed<'a> {
    pub firer: Firer<'a>,
    pub shortcut: &'a Shortcut,
}

/// One trigger that fired.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Firing {
    pub firer: String,
    pub trigger_index: usize,
    pub trigger: TriggerSpec,
    pub invocation: AutomationInvocationId,
    pub inputs: Map<String, Value>,
    /// The event's identity ([`Event::identity`]).
    pub event: String,
}

/// Why a matching trigger did not fire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skipped {
    SelfTriggered,
    /// The change was an invocation's, and the shortcut that made it would fire on
    /// its own work: these two shortcuts would run each other forever.
    Loop {
        /// The shortcut whose run made the change, which this one would run.
        through: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dispatched {
    pub firings: Vec<Firing>,
    /// Firer key, trigger index and why it did not fire.
    pub skipped: Vec<(String, usize, Skipped)>,
}

/// Every firing `event` causes among `armed`, in armed and trigger order.
#[must_use]
pub fn dispatch(armed: &[Armed<'_>], event: Event<'_>) -> Dispatched {
    let mut out = Dispatched::default();
    for armed in armed {
        for (index, trigger) in armed.shortcut.triggers.iter().enumerate() {
            if !matches(trigger, index, armed, event) {
                continue;
            }
            let key = armed.firer.key();
            if self_triggered(&armed.firer, event) {
                out.skipped.push((key, index, Skipped::SelfTriggered));
                continue;
            }
            if let Some(through) = looped(&armed.firer, event) {
                out.skipped.push((key, index, Skipped::Loop { through }));
                continue;
            }
            out.firings.push(Firing {
                firer: key,
                trigger_index: index,
                trigger: trigger.clone(),
                invocation: invocation_id(&armed.firer, index, event),
                inputs: inputs(armed.shortcut, index, event),
                event: event.identity(),
            });
        }
    }
    out
}

/// The invocation a firing starts, derived from the firer, the trigger and the
/// event, so the same event firing twice (a restart, a redelivery) reuses
/// every invocation id and acts once.
#[must_use]
pub fn invocation_id(
    firer: &Firer<'_>,
    trigger_index: usize,
    event: Event<'_>,
) -> AutomationInvocationId {
    let mut hasher = Sha256::new();
    hasher.update(b"tilo.automation.invocation\0");
    hasher.update(firer.key().as_bytes());
    hasher.update([0]);
    hasher.update(
        u64::try_from(trigger_index)
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(event.identity().as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    for (byte, digested) in bytes.iter_mut().zip(digest.iter()) {
        *byte = *digested;
    }
    AutomationInvocationId(uuid::Builder::from_custom_bytes(bytes).into_uuid())
}

fn matches(trigger: &TriggerSpec, index: usize, armed: &Armed<'_>, event: Event<'_>) -> bool {
    match (trigger, event) {
        (TriggerSpec::Schedule { .. } | TriggerSpec::Once { .. }, Event::Schedule(fired)) => {
            fired.trigger == index
        }
        (
            TriggerSpec::ResourceChanged {
                kind,
                actions,
                fields,
                by,
                filter,
            },
            Event::Record(change),
        ) => {
            // Housekeeping fires only a trigger that names its action: a
            // record's readers marking it read change nothing a trigger on
            // the record's changes acts on.
            fires_on_kind(change.kind.as_str(), kind)
                && actor_counts(*by, &change.actor)
                && (filter.is_empty() || filter.holds(&ChangeFilter::subject(change)))
                && ((actions.is_empty() && fields.is_empty() && !change.housekeeping)
                    || verbs_of(&change.did)
                        .iter()
                        .chain(&change.invoked)
                        .any(|verb| actions.contains(verb))
                    || edited_paths(&change.did)
                        .iter()
                        .any(|path| fields.iter().any(|pattern| field_matches(pattern, path))))
        }
        (TriggerSpec::IncomingMessage { app_id, from }, Event::Record(change)) => {
            let saver = armed.firer.saver;
            change.incoming_message
                && app_id
                    .as_ref()
                    .is_none_or(|app| app_of(&change.did).as_deref() == Some(app.as_str()))
                && from
                    .as_ref()
                    .is_none_or(|from| change.actor.person_id() == Some(from.as_str()))
                && change.actor.person_id() != Some(saver)
        }
        (
            TriggerSpec::Connector {
                app_id,
                source,
                filter,
            },
            Event::Connector(fired),
        ) => connector_matches(app_id, source, filter, fired),
        (TriggerSpec::AppEvent { app_id, kind }, Event::AppEvent(declared)) => {
            declared.app_id == *app_id && declared.kind == *kind
        }
        // Whoever reads the times already held them to the trigger's filter.
        (TriggerSpec::ResourceTime { .. }, Event::Time(fired)) => fired.trigger == index,
        // Whoever looked at the source already held what it found to the
        // trigger's filter.
        (TriggerSpec::SourceChanged { .. }, Event::Source(found)) => found.trigger == index,
        (TriggerSpec::Webhook { url, .. }, _) if url.trim().is_empty() => false,
        (TriggerSpec::Webhook { events, path, .. }, Event::Record(change)) => {
            webhook_wants(events, "change")
                && path_matches(path.as_deref(), change)
                && !webhook_delivery(change)
        }
        (
            TriggerSpec::Webhook {
                events, since_ms, ..
            },
            Event::Invocation(fired),
        ) => {
            webhook_wants(events, "invocation")
                && i64::try_from(fired.started_ms).unwrap_or(i64::MAX) >= *since_ms
                && !fired
                    .path
                    .starts_with(&format!("{}/", crate::WEBHOOK_PROVIDER))
        }
        (TriggerSpec::Webhook { events, .. }, Event::Presence(_)) => {
            webhook_wants(events, "presence")
        }
        _ => false,
    }
}

/// Whether a webhook whose `events` names these kinds posts `kind`: every kind when it names
/// none.
fn webhook_wants(events: &[String], kind: &str) -> bool {
    events.is_empty() || events.iter().any(|wanted| wanted == kind)
}

/// Whether `change` is a webhook delivery's own call, which a webhook never posts: a delivery
/// that posted itself would post for ever.
fn webhook_delivery(change: &super::event::DocumentEvent) -> bool {
    change.kind.starts_with(crate::WEBHOOK_PROVIDER)
        || change.record.starts_with(crate::WEBHOOK_PROVIDER)
}

/// Whether a webhook narrowed to `path` posts `change`: one that names none posts every change,
/// and one that names a record posts the changes of that record, or of its kind.
fn path_matches(path: Option<&str>, change: &super::event::DocumentEvent) -> bool {
    let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) else {
        return true;
    };
    path == change.record
        || path == change.kind
        || path
            .rsplit_once('/')
            .is_some_and(|(_, last)| last == change.record || last == change.kind)
}

fn connector_matches(
    app_id: &str,
    source: &str,
    filter: &Map<String, Value>,
    event: &ConnectorEvent,
) -> bool {
    event.app_id == app_id
        && event.source == source
        && filter
            .iter()
            .all(|(field, wanted)| event.payload.get(field) == Some(wanted))
}

fn actor_counts(filter: ActorFilter, actor: &Acting) -> bool {
    match filter {
        ActorFilter::Anyone => true,
        ActorFilter::People => actor.is_person(),
        ActorFilter::NotBots => !actor.is_bot(),
    }
}

/// A shortcut never fires on a change one of its invocations made.
fn self_triggered(firer: &Firer<'_>, event: Event<'_>) -> bool {
    let Event::Record(change) = event else {
        return false;
    };
    change.shortcut.as_deref() == Some(firer.shortcut)
}

/// The shortcut a change came from when firing on it would run this
/// shortcut into that one's run and back: the change a shortcut made is
/// not one its own invocation is watching for. Only a shortcut's own save-time
/// check sees a chain between shortcuts it can look up; a shortcut calling
/// another is caught here, at the invocation.
fn looped(firer: &Firer<'_>, event: Event<'_>) -> Option<String> {
    let Event::Record(change) = event else {
        return None;
    };
    change
        .shortcut
        .clone()
        .filter(|through| through != firer.shortcut)
}

fn inputs(shortcut: &Shortcut, trigger: usize, event: Event<'_>) -> Map<String, Value> {
    let declares = shortcut
        .inputs
        .iter()
        .any(|input| input.name.as_ref() == EVENT_INPUT && input.ty == ParamType::Object);
    let mut inputs = Map::new();
    if declares {
        // A webhook's receiver reads the same event the API's own stream carried, under the
        // type that names its kind.
        let webhook = shortcut
            .triggers
            .get(trigger)
            .is_some_and(TriggerSpec::is_webhook);
        let value = if webhook {
            event.webhook_json()
        } else {
            event.to_json()
        };
        inputs.insert(EVENT_INPUT.to_owned(), value);
    }
    inputs
}

/// Whether field-edit `path` (`channels.c1.name`) matches `pattern`
/// (`channels.*.name`, or a prefix such as `channels`).
#[must_use]
/// Whether a change to `kind` is a change to the kind a trigger saved as `saved` names: the
/// same kind, or the kind it is now when the app was renamed since the trigger was saved.
fn fires_on_kind(kind: &str, saved: &str) -> bool {
    kind == saved
        || tilo_action_policy::earlier::held_current_kind(saved).is_some_and(|now| now == kind)
}

pub fn field_matches(pattern: &str, path: &str) -> bool {
    let mut path = path.split('.');
    pattern.split('.').all(|wanted| {
        path.next()
            .is_some_and(|part| wanted == "*" || wanted == part)
    })
}

/// The public ids of every verb a change ran.
#[must_use]
pub fn verbs_of(did: &Did) -> Vec<String> {
    let mut verbs = Vec::new();
    collect(did, &mut verbs, &|did| match did {
        Did::Verb(call) => Some(call.action.action.clone()),
        _ => None,
    });
    verbs
}

/// The dotted paths of every field a change edited, from the record's
/// root (`channels.c1.name`).
#[must_use]
pub fn edited_paths(did: &Did) -> Vec<String> {
    let mut paths = Vec::new();
    collect(did, &mut paths, &|did| match did {
        Did::Edit(edit) if edit.path.is_empty() => Some(edit.field.clone()),
        Did::Edit(edit) => Some(
            edit.path
                .iter()
                .map(|step| match step {
                    PathStep::Field(name) | PathStep::Item(name) | PathStep::Key(name) => {
                        name.clone()
                    }
                    PathStep::Cell(row, column) => format!("{row}.{column}"),
                })
                .collect::<Vec<_>>()
                .join("."),
        ),
        _ => None,
    });
    paths
}

/// What `leaf` gives for `did` and every member of a group, members first
/// then the distinct actions a truncated group still names, without
/// repeats.
fn collect(did: &Did, out: &mut Vec<String>, leaf: &dyn Fn(&Did) -> Option<String>) {
    match did {
        Did::Group(group) => {
            for member in group.members.iter().chain(&group.distinct) {
                collect(member, out, leaf);
            }
        }
        other => {
            if let Some(value) = leaf(other)
                && !out.contains(&value)
            {
                out.push(value);
            }
        }
    }
}

fn app_of(did: &Did) -> Option<String> {
    match did {
        Did::Verb(call) => Some(call.action.app.clone()),
        Did::Group(group) => group.members.iter().find_map(app_of),
        Did::Edit(_) | Did::History(_) => None,
    }
}
