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
use tilo_actions::remote::workspace_initiative::Found;
use tilo_automation::dispatch::DocumentEvent;

use crate::agents::tl_tasks::{self, Reader, Task};
use crate::workspaces::Workspaces;

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
    /// The person the look is for, by their root in hex.
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
        person: tl_tasks::person(&own.person),
        assistant: own.assistant.as_deref().map(tl_tasks::agent),
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
            && let Some(named) = changes_in(workspace, (&app, &document), &mut doc, since, own)
        {
            found.changed = found.changed.saturating_add(1);
            if changed.len() < MAX_NAMED {
                changed.push(named);
            }
        }
        next.documents.insert(key, heads);
    }
    // The tasks are read as the person may read them: what is open, and what became due.
    let open = Reader::Person(workspaces, &own.person)
        .open_tasks(workspace)
        .await
        .unwrap_or_else(|error| {
            tracing::debug!(%workspace, %error, "a look reads no tasks");
            Vec::new()
        });
    tasks_in(&open, &against, &mut found, &mut due, &mut next);
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
fn changes_in(
    workspace: &str,
    (app, document): (&str, &str),
    doc: &mut AutoCommit,
    since: &[String],
    own: &Own,
) -> Option<Value> {
    let kind = collaboration::resource::identity(doc)
        .map(|checkout| checkout.kind.as_str().to_owned())
        .unwrap_or_default();
    let events =
        tilo_agent_session::bot::events_since(workspace, document, &kind, doc.document(), since);
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
    /// The person, as a task's assignee names them.
    person: String,
    /// The assistant, as a task's assignee names it.
    assistant: Option<String>,
    seen: &'a Seen,
    now_ms: i64,
}

/// The open tasks among `tasks`: those that are the person's or their
/// assistant's counted, and each that became due said once, and again when
/// its date moves.
fn tasks_in(
    tasks: &[Task],
    against: &Against<'_>,
    found: &mut Found,
    due: &mut Vec<Value>,
    next: &mut Seen,
) {
    for task in tasks.iter().filter(|task| !task.closed()) {
        let theirs = task.assignee.as_deref().is_some_and(|assignee| {
            assignee == against.person || against.assistant.as_deref() == Some(assignee)
        });
        if theirs {
            found.open = found.open.saturating_add(1);
        }
        let Some(due_ms) = task
            .due_ms()
            .filter(|due_ms| *due_ms <= against.now_ms.saturating_add(DUE_WITHIN_MS))
        else {
            continue;
        };
        let said = format!("{}@{due_ms}", task.id);
        if against.seen.looked && !against.seen.due.contains(&said) {
            found.due = found.due.saturating_add(1);
            if due.len() < MAX_NAMED {
                due.push(json!({
                    "task": task.resource(),
                    "number": task.number,
                    "title": task.title,
                    "dueMs": due_ms,
                    "overdue": due_ms < against.now_ms,
                }));
            }
        }
        next.due.insert(said);
    }
}

#[cfg(all(test, feature = "apps"))]
mod tests {
    use super::*;
    use crate::agents::tl_tasks::testing::Host;

    /// The tasks Tasks keeps are read through the store, as the person the look is for may read
    /// them: open ones that are theirs or their assistant's are counted, one that became due is
    /// named once, and a finished one or another's is left out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_look_finds_the_due_and_open_tasks_through_the_store() {
        let host = Host::new().await;
        let person = json!({ "person": host.person });
        let now_ms = 1_795_392_000_000; // 2026-11-23, midnight
        let call = host
            .new_task(
                "Call the venue",
                json!({"status": "todo", "assignee": person, "due": "2026-11-23"}),
            )
            .await;
        host.new_task(
            "Plan the week",
            json!({"status": "in_progress", "assignee": {"agent": "scout"}}),
        )
        .await;
        host.new_task(
            "Old errand",
            json!({"status": "done", "assignee": person, "due": "2026-11-20"}),
        )
        .await;
        host.new_task(
            "Bo's task",
            json!({"status": "todo", "assignee": {"person": "b0"}, "due": "2026-12-30"}),
        )
        .await;
        let own = Own {
            person: host.person.clone(),
            assistant: Some("scout".to_owned()),
            event: "look-round".to_owned(),
            thread: None,
        };
        let seen = Seen {
            looked: true,
            ..Seen::default()
        };
        let gathered = gather(
            &host.workspaces,
            &host.workspace,
            &host.root,
            &own,
            &seen,
            now_ms,
        )
        .await
        .unwrap();
        assert_eq!(gathered.found.open, 2, "{:?}", gathered.findings);
        assert_eq!(gathered.found.due, 1, "{:?}", gathered.findings);
        assert_eq!(
            gathered.findings["due"]["named"],
            json!([{
                "task": tl_tasks::resource(&call),
                "number": 1,
                "title": "Call the venue",
                "dueMs": now_ms,
                "overdue": false,
            }])
        );
        // The next look names it no more, until its day moves.
        let again = gather(
            &host.workspaces,
            &host.workspace,
            &host.root,
            &own,
            &gathered.seen,
            now_ms,
        )
        .await
        .unwrap();
        assert_eq!(again.found.due, 0);
        assert_eq!(again.found.open, 2);
        host.end().await;
    }
}
