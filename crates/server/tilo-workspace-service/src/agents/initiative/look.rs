//! What one look finds, with no model asked: what came in for the person,
//! what changed in the workspace's documents, which tasks became due, and
//! what is open.
//!
//! A look compares the workspace with where the last look left it
//! ([`Seen`]): every document's heads, the newest mail arrival, the tasks it
//! already said were due. The first look only notes where everything
//! stands, as a bot starts at a document's heads: the workspace's past is
//! not news. What the assistant did itself is not news to it either: its
//! own changes, the look-round's own asks, and what the server
//! keeps about the workspace's agents (what each spent, how each stands).

use std::collections::{BTreeMap, BTreeSet};

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_actions::remote::workspace_initiative::Found;
use tilo_automation::dispatch::DocumentEvent;

use crate::workspaces::Workspaces;

/// The app that keeps the workspace's tasks.
const TASKS: &str = "so.tilo.tasks";
/// How far ahead a task counts as due: a day.
const DUE_WITHIN_MS: i64 = 24 * 60 * 60 * 1000;
/// The most documents and the most tasks a look names; the rest it counts.
const MAX_NAMED: usize = 20;

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
    /// The look-round's own event.
    pub event: String,
}

impl Own {
    /// Whether `event` is the assistant's own doing: a change it or its
    /// look-round made. What a look asks is written as its person, in a run
    /// of the look-round, which the change carries.
    fn made(&self, event: &DocumentEvent) -> bool {
        let by_own = event
            .actor
            .agent_id()
            .is_some_and(|agent| self.assistant.as_deref() == Some(agent) || agent == self.event);
        by_own || event.shortcut.as_deref() == Some(self.event.as_str())
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
        if app == tilo_workspace_agent::OWNER && document == roster {
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
        if let Some(since) = since.filter(|since| *since != heads.as_slice())
            && let Some(named) = changes_in(
                workspaces,
                workspace,
                (&app, &document),
                &mut doc,
                since,
                own,
            )
            .await
        {
            found.changed = found.changed.saturating_add(1);
            if changed.len() < MAX_NAMED {
                changed.push(named);
            }
        }
        next.documents.insert(key, heads);
        let tasks_part = app == TASKS
            && collab_types::document::DocumentId::parse(&document)
                .and_then(|id| id.chain_part())
                .is_some_and(|(chain, _)| chain == tasks::issue_list::chain());
        if tasks_part
            && let Ok(part) = crate::agents::opened::<tasks::issue_list::IssueList>(&mut doc)
        {
            tasks_in(
                part.issues.iter(),
                &against,
                &mut found,
                &mut due,
                &mut next,
            );
        }
    }
    // The first look starts at the newest arrival: mail from before it is
    // where the inbox stands.
    let arrivals = match collab_types::WorkspaceId::parse(workspace) {
        Some(id) => {
            workspaces
                .mail_arrivals_for(id, person, seen.mail_seq)
                .await
                .arrivals
        }
        None => Vec::new(),
    };
    next.mail_seq = arrivals
        .iter()
        .map(|arrival| arrival.seq)
        .max()
        .unwrap_or(seen.mail_seq);
    if seen.looked {
        found.came_in = u64::try_from(arrivals.len()).unwrap_or(u64::MAX);
    }
    let findings = json!({
        "cameIn": { "mail": found.came_in },
        "changed": { "documents": found.changed, "named": changed },
        "due": { "tasks": found.due, "named": due },
        "open": { "tasks": found.open },
    });
    Ok(Gathered {
        found,
        findings,
        seen: next,
    })
}

/// What the changes of one document since `since` say, where anybody but the
/// assistant made one: where it changed, how often and by whom, never what
/// the change says.
async fn changes_in(
    workspaces: &Workspaces,
    workspace: &str,
    (app, document): (&str, &str),
    doc: &mut AutoCommit,
    since: &[String],
    own: &Own,
) -> Option<Value> {
    let (kind, events) = match crate::tl_records::built_in(app) {
        // A tilolang record is a document of the workspace's resource store: what changed in
        // it is read from the store's versions, through its app's package.
        Ok(Some(packages)) => {
            let (at, record) = (workspace.to_owned(), document.to_owned());
            let since = since.to_vec();
            let mut held = doc.clone();
            let viewer = crate::tl_records::runtime::Actor::App(app.into());
            let events = workspaces
                .with_tl_record(workspace, &packages, viewer, move |opened| {
                    Ok(opened.events_since(&at, &record, held.document(), &since))
                })
                .await;
            let events = tilo_failure::ignored!(
                "a record its app's package cannot open is left out, as one not read here is",
                events
            )
            .ok()?;
            // What it is, is what its changes were of.
            let kind = events
                .first()
                .map(|(event, _)| event.kind.clone())
                .unwrap_or_default();
            (kind, events)
        }
        Err(error) => {
            tracing::warn!(
                app,
                %error,
                "the app's package does not compile; its record is left out"
            );
            return None;
        }
        Ok(None) => {
            let kind = collaboration::resource::identity(doc)
                .map(|checkout| checkout.kind.as_str().to_owned())
                .unwrap_or_default();
            let events = tilo_agent_session::bot::events_since(
                workspace,
                document,
                &kind,
                doc.document(),
                since,
            );
            (kind, events)
        }
    };
    let theirs: Vec<&DocumentEvent> = events
        .iter()
        .map(|(event, _)| event)
        .filter(|event| !event.housekeeping && !own.made(event))
        .collect();
    if theirs.is_empty() {
        return None;
    }
    let by: BTreeSet<&str> = theirs.iter().map(|event| who(event)).collect();
    Some(json!({
        "app": app,
        "document": document,
        "kind": kind,
        "changes": theirs.len(),
        "by": by,
    }))
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
            if due.len() < MAX_NAMED {
                due.push(json!({
                    "issue": tasks_model::issue_resource(id),
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
