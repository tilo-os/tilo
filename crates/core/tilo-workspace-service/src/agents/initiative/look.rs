//! What one look finds, with no model asked: what came in for the person,
//! what changed in the workspace's documents, which tasks became due, and
//! what is open.
//!
//! A look compares the workspace with where the last look left it
//! ([`Seen`]): every document's heads, the mail it took in, the tasks it
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
    /// Each message the Mail app held when the mail was last taken in, by its key, with when it
    /// arrived: what arrived since is new, and a message a later sync rereads is not.
    mail: BTreeMap<String, i64>,
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
        mail: seen.mail.clone(),
        due: BTreeSet::new(),
    };
    let mut found = Found::default();
    let (mut changed, mut due) = (Vec::new(), Vec::new());
    let against = Against {
        person: tilo_action_policy::MemberId::person(own.person.as_str()),
        assistant: own
            .assistant
            .as_deref()
            .map(tilo_action_policy::MemberId::agent),
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
    // The workspace's tasks, as its store holds them.
    let tasks = crate::agents::task_work::tasks(workspaces, workspace, Vec::new())
        .await
        .map_err(|error| format!("the workspace's tasks do not read: {error}"))?;
    tasks_in(&tasks, &against, &mut found, &mut due, &mut next);
    // The Mail app's messages, as its connector brings them in: what arrived since the last look
    // came in for the person. Mail that cannot be read is no reason to hold the rest back.
    match crate::apps::triggers::mail::arrived(workspaces, workspace, &mut next.mail, seen.looked)
        .await
    {
        Ok(arrived) => found.came_in = u64::try_from(arrived).unwrap_or(u64::MAX),
        Err(error) => {
            tracing::warn!(%error, workspace, "the mail a look counts could not be read");
        }
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
    let events = events_since(workspace, document, &kind, doc.document(), since);
    let theirs: Vec<&DocumentEvent> = events
        .iter()
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

/// The changes of `doc` after `since` (hex heads), as the events their recorded entries
/// describe. A change that carries no entry (a genesis, a merge with no action) is none.
fn events_since(
    workspace: &str,
    record: &str,
    kind: &str,
    doc: &automerge::Automerge,
    since: &[String],
) -> Vec<DocumentEvent> {
    let since: Vec<automerge::ChangeHash> = since
        .iter()
        .filter_map(|hex| {
            tilo_failure::ignored!(
                "a head that does not parse names no change, so the changes are read from before it, none skipped",
                hex.parse().ok()
            )
        })
        .collect();
    doc.get_changes(&since)
        .into_iter()
        .filter_map(|change| {
            let context = change
                .message()
                .and_then(tilo_actions::ActionContext::from_change_message)?;
            Some(tilo_automation::dispatch::event_of(
                workspace,
                record,
                kind,
                &change.hash().to_string(),
                &context,
            ))
        })
        .collect()
}

/// What a look holds the workspace's tasks against.
struct Against<'a> {
    /// The person the look is for, as a task's assignee names them.
    person: tilo_action_policy::MemberId,
    /// The assistant, as a task's assignee names it.
    assistant: Option<tilo_action_policy::MemberId>,
    seen: &'a Seen,
    now_ms: i64,
}

/// The open tasks among `tasks`: those that are the person's or their assistant's counted, and
/// each that became due said once, and again when its date moves.
fn tasks_in(
    tasks: &[crate::agents::task_work::Held],
    against: &Against<'_>,
    found: &mut Found,
    due: &mut Vec<Value>,
    next: &mut Seen,
) {
    for task in tasks {
        if task.closed() {
            continue;
        }
        let theirs = task.assigned_to(&against.person)
            || against
                .assistant
                .as_ref()
                .is_some_and(|assistant| task.assigned_to(assistant));
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
                    "issue": task.address(),
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

#[cfg(test)]
mod tests {
    use tilo_resource_store::wire::Command;
    use tilo_resource_store::{Cell, Resource, Stamp, Value as Stored};

    use super::*;

    /// One text of a record.
    fn text(text: &str) -> Cell {
        Cell::One(Stored::Text(text.to_owned()))
    }

    /// The Mail app's messages, with the fields a look counts them by.
    const MAIL_SURFACE: &str = r#"format = 2

[[resources]]
kind = "tl.so.tilo.mail.Message"
label = "Message"
version = 1
fields = [
  { path = "key", type = "text" },
  { path = "mailbox", type = "text" },
  { path = "received", type = "text" },
]
verbs = [
  { id = "$origin", label = "Keep in step with its service", kind = "undoable" },
]
"#;

    /// Keep `id` as the Mail app's message that arrived at `received`, as its connector writes it.
    async fn keep_message(workspaces: &Workspaces, workspace: &str, id: &str, received: &str) {
        workspaces
            .own_resource_command(
                workspace,
                "so.tilo.mail",
                Command::Put {
                    kind: "tl.so.tilo.mail.Message".into(),
                    id: id.into(),
                    resource: Resource {
                        readers: "*".into(),
                        parent: None,
                        order: None,
                        fields: [
                            ("key".to_owned(), text(id)),
                            ("mailbox".to_owned(), text("inbox")),
                            ("received".to_owned(), text(received)),
                        ]
                        .into_iter()
                        .collect(),
                    },
                    stamp: Stamp {
                        verb: "$origin".into(),
                        ..Stamp::default()
                    },
                },
                Vec::new(),
            )
            .await
            .unwrap();
    }

    /// What the mail held when a look first ran is where it stands, what arrives after it is what
    /// came in, and no message is counted twice.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_look_counts_the_mail_that_arrived_since_the_last_one() {
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = crate::workspaces::fixtures::governor();
        let host = crate::workspaces::fixtures::test_host(scratch.path(), &key, &cert).await;
        let workspaces = host.workspaces().clone();
        crate::workspaces::fixtures::declare_app(&host, "so.tilo.mail", MAIL_SURFACE.as_bytes());
        let workspace = workspaces.create("Mail").await.unwrap().workspace;
        let own = Own {
            person: hex::encode(cert.user_public_key),
            ..Own::default()
        };
        let now_ms = 1_764_000_000_000;

        keep_message(&workspaces, &workspace, "old", "2026-11-23T07:00:00Z").await;
        let first = gather(&workspaces, &workspace, &own, &Seen::default(), now_ms)
            .await
            .unwrap();
        assert_eq!(first.found.came_in, 0, "what was there is not news");
        assert_eq!(first.findings["cameIn"]["mail"].as_u64(), Some(0));

        keep_message(&workspaces, &workspace, "new-1", "2026-11-23T08:10:00Z").await;
        keep_message(&workspaces, &workspace, "new-2", "2026-11-23T08:11:00Z").await;
        let second = gather(&workspaces, &workspace, &own, &first.seen, now_ms + 60_000)
            .await
            .unwrap();
        assert_eq!(second.found.came_in, 2);
        assert!(second.found.anything_new(), "mail is news");
        assert_eq!(second.findings["cameIn"]["mail"].as_u64(), Some(2));

        let third = gather(
            &workspaces,
            &workspace,
            &own,
            &second.seen,
            now_ms + 120_000,
        )
        .await
        .unwrap();
        assert_eq!(third.found.came_in, 0, "no message is counted twice");
        host.shutdown().await;
    }
}
