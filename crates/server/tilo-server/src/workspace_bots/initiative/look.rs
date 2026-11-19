//! What one look finds, with no model asked: what came in for the person,
//! what changed in the workspace's documents, which tasks became due, and
//! what is open.
//!
//! A look compares the workspace with where the last look left it
//! ([`Seen`]): every document's heads, the newest mail arrival, the tasks it
//! already said were due. The first look only notes where everything
//! stands, as a bot starts at a document's heads: the workspace's past is
//! not news. What the assistant did itself is not news to it either: its
//! own changes, the look-round's own asks, what is said in its own thread
//! with its person, and what the server keeps about the workspace's agents
//! (what each spent, how each stands).

use std::collections::{BTreeMap, BTreeSet};

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_actions::entry::Did;
use tilo_actions::remote::person_initiative::Found;
use tilo_automation::dispatch::DocumentEvent;

use super::screening;
use crate::workspaces::Workspaces;

/// The app that keeps the workspace's tasks.
const TASKS: &str = "so.tilo.tasks";
/// The app that keeps the workspace's list of its agents.
const CHAT: &str = "so.tilo.chat";
/// How far ahead a task counts as due: a day.
const DUE_WITHIN_MS: i64 = 24 * 60 * 60 * 1000;
/// The most documents and the most tasks a look names; the rest it counts.
const MAX_NAMED: usize = 20;
/// What Chat calls the conversation a message is in, as a verb's argument and on the
/// message itself.
const CONVERSATION: &str = "channel";

/// Where a look left the workspace, for the next to compare with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Seen {
    /// Whether a look was taken at all: the first only notes where
    /// everything stands.
    looked: bool,
    /// Each document's heads, by `app/document`.
    documents: BTreeMap<String, Vec<String>>,
    /// The newest place of the person's arrival log a look has counted.
    mail_seq: u64,
    /// The tasks a look already said were due, each by its id and its due
    /// time, so a task is said once and again when its date moves.
    due: BTreeSet<String>,
}

/// What is the assistant's own in a workspace, which a look leaves out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Own {
    /// The person the look is for, as a task's assignee names them.
    pub person: String,
    /// The assistant's id among the workspace's agents, where the person
    /// keeps one.
    pub assistant: Option<String>,
    /// The look-round's own event, which asks as itself.
    pub event: String,
    /// The conversation of the person and their assistant.
    pub thread: Option<String>,
}

impl Own {
    /// Whether `event` is the assistant's own doing: a change it or its
    /// look-round made, or something said in its own thread.
    fn made(&self, event: &DocumentEvent) -> bool {
        let by_own = event
            .actor
            .agent_id()
            .is_some_and(|agent| self.assistant.as_deref() == Some(agent) || agent == self.event);
        by_own || self.in_thread(event)
    }

    /// Whether `event` is something said in the assistant's own thread: the
    /// conversation its verb names (what a post or an ask carries), or the
    /// one the message it changed is in.
    fn in_thread(&self, event: &DocumentEvent) -> bool {
        let Some(thread) = &self.thread else {
            return false;
        };
        let names = |value: Option<&Value>| value.and_then(Value::as_str) == Some(thread.as_str());
        names(event.arguments.get(CONVERSATION))
            || [&event.before, &event.after]
                .into_iter()
                .flatten()
                .any(|item| names(item.get(CONVERSATION)))
    }
}

/// What a look found.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Gathered {
    /// In numbers.
    pub found: Found,
    /// As what the assistant is handed: data, each list cut to its first
    /// [`MAX_NAMED`] with the rest counted.
    pub findings: Value,
    /// Structured batch of workspace changes, arrivals, and due tasks for
    /// wake screening and compact ~1,000-token formatting.
    pub batch: screening::LookBatch,
    /// Compact formatted change lines (~1,000 tokens) produced by
    /// [`screening::format_look_material`].
    pub formatted: screening::FormattedChanges,
    /// Where this look leaves the workspace.
    pub seen: Seen,
}

/// Who changed something, as a look says it.
fn who(event: &DocumentEvent) -> &'static str {
    let actor = &event.actor;
    if actor.is_person() {
        "a person"
    } else if actor.is_agent() {
        "an agent"
    } else if actor.is_bot() {
        "a bot"
    } else {
        "software"
    }
}

/// Classify who originated `event` relative to `own` for wake screening.
fn origin_of(event: &DocumentEvent, own: &Own) -> screening::ChangeOrigin {
    if own.in_thread(event) && event.actor.person_id() == Some(own.person.as_str()) {
        return screening::ChangeOrigin::OwnerSelf;
    }
    if own.made(event) {
        return screening::ChangeOrigin::OwnAssistant;
    }
    if event.actor.is_person() {
        screening::ChangeOrigin::OtherPerson
    } else if event.actor.is_agent() || event.actor.is_bot() || event.actor.is_automation() {
        screening::ChangeOrigin::OtherAgent
    } else {
        screening::ChangeOrigin::Software
    }
}

/// Short structural verb label extracted from `did`, never item body text.
fn verb_of(did: &Did) -> String {
    match did {
        Did::Verb(call) if !call.action.action.trim().is_empty() => call.action.action.clone(),
        Did::Verb(_) => "changed".to_owned(),
        Did::Edit(edit) if !edit.field.trim().is_empty() => {
            format!("{}-{}", edit.op.as_str(), edit.field)
        }
        Did::Edit(edit) => edit.op.as_str().to_owned(),
        Did::Group(group) => group
            .shared()
            .map_or_else(|| format!("group({})", group.count), verb_of),
        Did::History(_) => "history".to_owned(),
    }
}

/// Build one compact structural [`screening::ChangeLine`] from `event`, without
/// copying message or document body text.
fn change_line_of(
    app: &str,
    document: &str,
    kind: &str,
    event: &DocumentEvent,
    own: &Own,
) -> screening::ChangeLine {
    let item = event
        .item
        .as_ref()
        .map(|item| item.kind.trim())
        .filter(|item_kind| !item_kind.is_empty())
        .or_else(|| {
            let trimmed = kind.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .unwrap_or(document);
    screening::ChangeLine::new(who(event), verb_of(&event.did), app, item, None, None)
        .with_origin(origin_of(event, own))
        .with_housekeeping(event.housekeeping)
}

/// Look at `workspace` for `own.person` against what `seen` says the last
/// look left, at `now_ms`.
///
/// A document that cannot be read here is left out, as a bot leaves it out:
/// what the server does hold is still looked at.
///
/// # Errors
/// The workspace is not governed here.
pub(super) async fn gather(
    workspaces: &Workspaces,
    workspace: &str,
    person: &[u8; 32],
    own: &Own,
    seen: &Seen,
    now_ms: i64,
) -> Result<Gathered, String> {
    let routes = workspaces
        .workspace_document_routes(workspace)
        .await
        .map_err(|error| error.to_string())?;
    let mut next = Seen {
        looked: true,
        documents: seen.documents.clone(),
        mail_seq: seen.mail_seq,
        due: BTreeSet::new(),
    };
    let mut found = Found::default();
    let (mut changed, mut due) = (Vec::new(), Vec::new());
    let (mut change_lines, mut due_lines) = (Vec::new(), Vec::new());
    let against = Against {
        own,
        assistant: own
            .assistant
            .as_deref()
            .map(|agent| tasks_model::agent_assignee(agent).0),
        seen,
        now_ms,
    };
    // The workspace's list of its agents is where the server counts what a
    // turn cost and where agents say how they stand: bookkeeping about the
    // assistant and its helpers, written after every turn, and never news.
    let roster = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    for (app, document) in routes {
        if app == CHAT && document == roster {
            continue;
        }
        let Ok(Some(bytes)) = workspaces
            .resource_document(workspace, &app, &document)
            .await
        else {
            continue;
        };
        let Ok(mut doc) = AutoCommit::load(&bytes) else {
            continue;
        };
        let heads: Vec<String> = doc.get_heads().iter().map(ToString::to_string).collect();
        let key = format!("{app}/{document}");
        // A document the last look did not hold is new since then, whole; on
        // the first look it is where the workspace stands.
        let since = match seen.documents.get(&key) {
            Some(since) => Some(since.as_slice()),
            None if seen.looked => Some([].as_slice()),
            None => None,
        };
        if let Some(since) = since.filter(|since| *since != heads.as_slice()) {
            let (named, lines) = changes_in(workspace, (&app, &document), &mut doc, since, own);
            change_lines.extend(lines);
            if let Some(named) = named {
                found.changed = found.changed.saturating_add(1);
                if changed.len() < MAX_NAMED {
                    changed.push(named);
                }
            }
        }
        next.documents.insert(key, heads);
        let tasks_part = app == TASKS
            && collab_types::document::DocumentId::parse(&document)
                .and_then(|id| id.chain_part())
                .is_some_and(|(chain, _)| chain == tasks::issue_list::chain());
        if tasks_part
            && let Ok(part) =
                crate::workspace_bots::opened::<tasks::issue_list::IssueList>(&mut doc)
        {
            tasks_in(
                part.issues.iter(),
                &against,
                &mut found,
                &mut due,
                &mut due_lines,
                &mut next,
            );
        }
    }
    // The first look starts at the newest arrival: mail from before it is
    // where the inbox stands.
    let arrivals = workspaces.mail_arrivals(person, seen.mail_seq);
    next.mail_seq = arrivals
        .iter()
        .map(|arrival| arrival.seq)
        .max()
        .unwrap_or(seen.mail_seq);
    if seen.looked {
        found.came_in = u64::try_from(arrivals.len()).unwrap_or(u64::MAX);
    }
    let batch = screening::LookBatch {
        workspace: workspace.to_owned(),
        at_ms: now_ms,
        changes: change_lines,
        incoming_mail_count: found.came_in,
        mail_snippets: Vec::new(),
        due_tasks: due_lines,
        bypass_signals: Vec::new(),
        active_context: None,
    };
    let formatted = screening::format_look_material(
        &batch,
        screening::DEFAULT_MAX_LINES,
        screening::DEFAULT_MAX_MATERIAL_CHARS,
    );
    let findings = json!({
        "cameIn": { "mail": found.came_in },
        "changed": { "documents": found.changed, "named": changed },
        "due": { "tasks": found.due, "named": due },
        "open": { "tasks": found.open },
    });
    Ok(Gathered {
        found,
        findings,
        batch,
        formatted,
        seen: next,
    })
}

/// What the changes of one document since `since` say, where anybody but the
/// assistant made one: where it changed, how often and by whom, never what
/// the change says, plus typed [`screening::ChangeLine`] entries for screening.
fn changes_in(
    workspace: &str,
    (app, document): (&str, &str),
    doc: &mut AutoCommit,
    since: &[String],
    own: &Own,
) -> (Option<Value>, Vec<screening::ChangeLine>) {
    let kind = collaboration::resource::identity(doc)
        .map(|checkout| checkout.kind.as_str().to_owned())
        .unwrap_or_default();
    let events =
        tilo_agent_session::bot::events_since(workspace, document, &kind, doc.document(), since);
    let mut lines = Vec::with_capacity(events.len());
    let mut by = BTreeSet::new();
    let mut relevant_count = 0usize;
    for (event, _) in &events {
        let line = change_line_of(app, document, &kind, event, own);
        if line.is_relevant_for_look() {
            relevant_count = relevant_count.saturating_add(1);
            by.insert(who(event));
        }
        lines.push(line);
    }
    let named = (relevant_count > 0).then(|| {
        json!({
            "app": app,
            "document": document,
            "kind": kind,
            "changes": relevant_count,
            "by": by,
        })
    });
    (named, lines)
}

/// What a look holds a tracker's tasks against.
struct Against<'a> {
    own: &'a Own,
    /// The assistant, as a task's assignee names it.
    assistant: Option<String>,
    seen: &'a Seen,
    now_ms: i64,
}

/// The open tasks among `issues`: those that are the person's or their
/// assistant's counted, and each that became due said once, and again when
/// its date moves.
fn tasks_in<'a>(
    issues: impl IntoIterator<Item = (&'a tilo_resource::Id, &'a tasks_model::Issue)>,
    against: &Against<'_>,
    found: &mut Found,
    due: &mut Vec<Value>,
    due_lines: &mut Vec<screening::DueTaskLine>,
    next: &mut Seen,
) {
    for (id, issue) in issues {
        if issue.status.closed() {
            continue;
        }
        let theirs = issue.assignee.as_ref().is_some_and(|assignee| {
            assignee.0 == against.own.person
                || against.assistant.as_deref() == Some(assignee.0.as_str())
        });
        if theirs {
            found.open = found.open.saturating_add(1);
        }
        let Some(due_ms) = issue
            .due_ms
            .filter(|due_ms| *due_ms <= against.now_ms.saturating_add(DUE_WITHIN_MS))
        else {
            continue;
        };
        let said = format!("{id}@{due_ms}");
        if against.seen.looked && !against.seen.due.contains(&said) {
            found.due = found.due.saturating_add(1);
            let issue_id = tasks_model::issue_resource(id);
            due_lines.push(screening::DueTaskLine {
                id: issue_id.clone(),
                title: issue.title.clone(),
                due_ms,
                overdue: due_ms < against.now_ms,
            });
            if due.len() < MAX_NAMED {
                due.push(json!({
                    "issue": issue_id,
                    "number": issue.number,
                    "title": issue.title,
                    "dueMs": due_ms,
                    "overdue": due_ms < against.now_ms,
                }));
            }
        }
        next.due.insert(said);
    }
}
