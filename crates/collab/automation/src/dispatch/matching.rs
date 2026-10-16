use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tilo_actions::entry::{Did, PathStep};
use tilo_actions::interaction::ActorKind;
use tilo_actions::params::ParamType;

use super::event::{ConnectorEvent, Event};
use super::{Cause, MAX_CHAIN_DEPTH};
use crate::RunId;
use crate::shortcut::{ActorFilter, EVENT_INPUT, Shortcut, TriggerSpec};

/// Who a firing runs as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Firer<'a> {
    /// An installed bot, by installation id.
    Bot { installation: &'a str },
    /// A person's saved Shortcut, by id, and the person who saved it.
    Shortcut { shortcut: &'a str, saver: &'a str },
}

impl Firer<'_> {
    /// `bot:<installation>` or `shortcut:<id>`: the key its firings are
    /// queued under, and for a bot the actor id its changes carry.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Bot { installation } => format!("bot:{installation}"),
            Self::Shortcut { shortcut, .. } => format!("shortcut:{shortcut}"),
        }
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
    pub run: RunId,
    pub inputs: Map<String, Value>,
    /// What the run records on each change it makes (bots only).
    pub cause: Option<Cause>,
    /// The event's identity ([`Event::identity`]).
    pub event: String,
}

/// Why a matching trigger did not fire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skipped {
    SelfTriggered,
    /// The run would have been this deep.
    TooDeep {
        depth: u8,
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
            if !matches(trigger, armed, event) {
                continue;
            }
            let key = armed.firer.key();
            if self_triggered(&armed.firer, event) {
                out.skipped.push((key, index, Skipped::SelfTriggered));
                continue;
            }
            let cause = cause_for(&armed.firer, event);
            if let Some(cause) = &cause
                && cause.depth > MAX_CHAIN_DEPTH
            {
                out.skipped
                    .push((key, index, Skipped::TooDeep { depth: cause.depth }));
                continue;
            }
            out.firings.push(Firing {
                firer: key,
                trigger_index: index,
                trigger: trigger.clone(),
                run: run_id(&armed.firer, index, event),
                inputs: inputs(armed.shortcut, event),
                cause,
                event: event.identity(),
            });
        }
    }
    out
}

/// The run a firing starts, derived from the firer, the trigger and the
/// event, so the same event firing twice (a restart, a redelivery) reuses
/// every invocation id and acts once.
#[must_use]
pub fn run_id(firer: &Firer<'_>, trigger_index: usize, event: Event<'_>) -> RunId {
    let mut hasher = Sha256::new();
    hasher.update(b"tilo.automation.run\0");
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
    RunId(uuid::Builder::from_custom_bytes(bytes).into_uuid())
}

fn matches(trigger: &TriggerSpec, armed: &Armed<'_>, event: Event<'_>) -> bool {
    match (trigger, event) {
        (
            TriggerSpec::RecordChanged {
                kind,
                actions,
                fields,
                by,
            },
            Event::Record(change),
        ) => {
            change.kind == *kind
                && actor_counts(*by, change.actor.kind)
                && ((actions.is_empty() && fields.is_empty())
                    || verbs_of(&change.did)
                        .iter()
                        .any(|verb| actions.contains(verb))
                    || edited_paths(&change.did)
                        .iter()
                        .any(|path| fields.iter().any(|pattern| field_matches(pattern, path))))
        }
        (TriggerSpec::IncomingMessage { app_id, from }, Event::Record(change)) => {
            let saver = match armed.firer {
                Firer::Shortcut { saver, .. } => Some(saver),
                Firer::Bot { .. } => None,
            };
            change.incoming_message
                && app_id
                    .as_ref()
                    .is_none_or(|app| app_of(&change.did).as_deref() == Some(app.as_str()))
                && from.as_ref().is_none_or(|from| *from == change.actor.id)
                && saver.is_none_or(|saver| saver != change.actor.id)
        }
        (
            TriggerSpec::Connector {
                app_id,
                source,
                filter,
            },
            Event::Connector(fired),
        ) => {
            matches!(armed.firer, Firer::Bot { .. })
                && connector_matches(app_id, source, filter, fired)
        }
        _ => false,
    }
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

const fn actor_counts(filter: ActorFilter, kind: ActorKind) -> bool {
    match filter {
        ActorFilter::Anyone => true,
        ActorFilter::People => matches!(kind, ActorKind::Person),
        ActorFilter::NotBots => !matches!(kind, ActorKind::Bot),
    }
}

/// A bot never fires on a change it made, and a person's Shortcut never
/// on a change one of its runs made.
fn self_triggered(firer: &Firer<'_>, event: Event<'_>) -> bool {
    let Event::Record(change) = event else {
        return false;
    };
    match firer {
        Firer::Bot { .. } => change.actor.id == firer.key(),
        Firer::Shortcut { shortcut, .. } => change.shortcut.as_deref() == Some(*shortcut),
    }
}

/// A bot run continues the chain of the change that fired it when that
/// change came from a bot run, else starts one at depth 1. A person's
/// Shortcut records no cause.
fn cause_for(firer: &Firer<'_>, event: Event<'_>) -> Option<Cause> {
    let Firer::Bot { installation } = firer else {
        return None;
    };
    let parent = match event {
        // Only a bot's own change can continue a chain: a depth any other
        // actor claims is ignored, so nobody can lengthen or shorten one.
        Event::Record(change) if change.actor.kind == ActorKind::Bot => {
            event.caused_by().map_or(0, |cause| cause.depth)
        }
        _ => 0,
    };
    Some(Cause {
        installation: (*installation).to_owned(),
        event: event.identity(),
        depth: parent.saturating_add(1),
    })
}

fn inputs(shortcut: &Shortcut, event: Event<'_>) -> Map<String, Value> {
    let declares = shortcut
        .inputs
        .iter()
        .any(|input| input.name.as_ref() == EVENT_INPUT && input.ty == ParamType::Object);
    let mut inputs = Map::new();
    if declares {
        inputs.insert(EVENT_INPUT.to_owned(), event.to_json());
    }
    inputs
}

/// Whether field-edit `path` (`channels.c1.name`) matches `pattern`
/// (`channels.*.name`, or a prefix such as `channels`).
#[must_use]
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
