//! A person's recent actions, read from the structured history every
//! record keeps: what the moderation agent shows the model, and what it
//! takes back.

use serde::Serialize;
use tilo_resource::history::Version;

/// The longest rendering of one action's arguments shown to the model; a
/// spam post is recognisable from its start.
const MAX_ARGS_CHARS: usize = 600;

/// One change a person made, as history records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Activity {
    pub app: String,
    pub record: String,
    /// The change, as history names it (hex of its hash).
    pub change: String,
    /// The change before it in the record's history, if any: the version an
    /// undo of everything the person did from here on goes back to.
    pub before: Option<String>,
    pub time_ms: i64,
    /// The exact invoker history names, which `undo { by }` takes.
    pub actor: String,
    /// What the change did: the verb or field edit, with its arguments.
    pub what: String,
    /// Marked destructive by the action that made it (a deletion, a
    /// clear).
    pub destructive: bool,
    /// A change to the record's content, not housekeeping such as joining,
    /// presence or a read mark; only content is ever taken back.
    pub content: bool,
}

/// One record's history, as the agent reads it.
#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    pub app: &'a str,
    pub record: &'a str,
    /// Its versions, oldest first ([`tilo_resource::history::versions`]).
    pub versions: &'a [Version],
    /// The verbs its resource publishes as destructive
    /// ([`destructive_verbs`]).
    pub destructive: &'a [String],
}

/// The ids of the actions `resource` publishes as destructive: what a
/// deletion looks like in that record's history.
#[must_use]
pub fn destructive_verbs(resource: &tilo_actions::Resource) -> Vec<String> {
    resource
        .actions
        .iter()
        .filter(|action| action.contract.risk == tilo_actions::RiskLevel::Destructive)
        .map(|action| action.id.clone())
        .collect()
}

fn is_destructive(context: &tilo_actions::ActionContext, destructive: &[String]) -> bool {
    use tilo_actions::{Did, EditOp};
    let marked = context
        .facets
        .iter()
        .any(|facet| facet.key == tilo_actions::RISK_FACET && facet.value == "destructive");
    marked
        || match &context.what {
            Some(Did::Verb(call)) => destructive.contains(&call.action.action),
            Some(Did::Edit(edit)) => matches!(
                edit.op,
                EditOp::Remove | EditOp::RemoveRow | EditOp::RemoveColumn
            ),
            _ => false,
        }
}

/// `person`'s changes in one record since `since_ms`, oldest first. A change
/// counts when the person made it or someone (an agent, a Shortcut) made it
/// for them.
#[must_use]
pub fn activity_of(source: &Source<'_>, person: &str, since_ms: i64) -> Vec<Activity> {
    let Source {
        app,
        record,
        versions,
        destructive,
    } = *source;
    let mut out = Vec::new();
    let mut before: Option<String> = None;
    for version in versions {
        let hash = version.hash.to_string();
        let Some(context) = version.context.as_ref() else {
            before = Some(hash);
            continue;
        };
        let invoker = context.invoker.as_ref();
        let by_person = invoker.is_some_and(|invoker| {
            invoker.actor == person || invoker.on_behalf_of.as_deref() == Some(person)
        });
        if by_person && version.time_ms >= since_ms {
            let what = context
                .what
                .as_ref()
                .and_then(|what| serde_json::to_string(what).ok())
                .unwrap_or_default();
            out.push(Activity {
                app: app.to_owned(),
                record: record.to_owned(),
                change: hash.clone(),
                before: before.clone(),
                time_ms: version.time_ms,
                actor: invoker
                    .map(|invoker| invoker.actor.clone())
                    .unwrap_or_default(),
                what: clip(&what, MAX_ARGS_CHARS),
                destructive: is_destructive(context, destructive),
                content: !context.is_housekeeping(),
            });
        }
        before = Some(hash);
    }
    out
}

fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", text.get(..at).unwrap_or_default()),
        None => text.to_owned(),
    }
}

/// Whether `activity` holds a burst of destructive changes: at least
/// `count` within any `window_ms`. A person deleting what others made, fast,
/// is worth a look before anyone reports it.
#[must_use]
pub fn burst(activity: &[Activity], count: usize, window_ms: i64) -> bool {
    let mut times: Vec<i64> = activity
        .iter()
        .filter(|item| item.destructive)
        .map(|item| item.time_ms)
        .collect();
    if count == 0 {
        return true;
    }
    times.sort_unstable();
    times
        .windows(count)
        .any(|run| match (run.first(), run.last()) {
            (Some(first), Some(last)) => last.saturating_sub(*first) <= window_ms,
            _ => false,
        })
}
