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

use crate::work::AgentAccess;
use crate::workspaces::{ReadFor, Store, Workspaces};

/// The assistant, current signing grant and roster rules under which a look reads and reports.
#[derive(PartialEq, Eq)]
pub(super) struct Access {
    agent: String,
    created_at: i64,
    rules: Vec<tilo_action_policy::Rule>,
    pub(super) signing: AgentAccess,
}

impl Access {
    pub(super) async fn current(
        workspaces: &Workspaces,
        owner: &super::Owner,
    ) -> Result<Self, String> {
        let agent = super::assistant_of(workspaces, &owner.workspace, &owner.person)
            .await?
            .ok_or("this person keeps no active assistant in the workspace")?;
        let signing = workspaces
            .agent_access(&owner.workspace, &agent.id.0, &owner.person)
            .await
            .map_err(|error| error.to_string())?;
        if !signing.permits_app(Store::INITIATIVE.app) || !signing.permits_app(crate::agents::CHAT)
        {
            return Err("the assistant needs access to Assistant and Chat for a look-round".into());
        }
        let rules = agent.bound_rules().unwrap_or_default();
        Ok(Self {
            agent: agent.id.0,
            created_at: agent.created_at,
            rules,
            signing,
        })
    }

    pub(super) async fn check(
        &self,
        workspaces: &Workspaces,
        owner: &super::Owner,
    ) -> Result<(), String> {
        if Self::current(workspaces, owner).await? != *self {
            return Err("the assistant's access changed during the look".into());
        }
        Ok(())
    }

    pub(super) fn reader(&self) -> ReadFor<'_> {
        ReadFor::Assistant(&self.agent, &self.rules)
    }

    /// A queued look belongs to exactly the assistant and signing grant that read its sources.
    pub(super) fn fingerprint(&self, owner: &super::Owner) -> Result<String, String> {
        let hosted = match &self.signing {
            AgentAccess::Device => None,
            AgentAccess::Hosted {
                device,
                created_at,
                scope,
            } => Some((device, created_at, scope)),
        };
        let identity = (
            &owner.workspace,
            &owner.person,
            &self.agent,
            self.created_at,
            &self.rules,
            hosted,
        );
        let bytes = postcard::to_allocvec(&identity).map_err(|error| error.to_string())?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }

    pub(super) fn names(&self, owner: &super::Owner, agent: &str, fingerprint: &str) -> bool {
        self.agent == agent
            && self
                .fingerprint(owner)
                .is_ok_and(|grant| grant == fingerprint)
    }
}

/// The app that keeps the workspace's tasks.
const TASKS: &str = "so.tilo.tasks";
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
    /// The active assistant's id among the workspace's agents.
    pub assistant: String,
    /// The look-round's own event, which asks as itself.
    pub event: String,
    /// The conversation of the person and their assistant.
    pub thread: String,
}

impl Own {
    /// Whether `event` is the assistant's own doing: a change it or its
    /// look-round made, or something said in its own thread.
    fn made(&self, event: &DocumentEvent) -> bool {
        let by_own = event
            .actor
            .agent_id()
            .is_some_and(|agent| self.assistant == agent || agent == self.event);
        by_own || self.in_thread(event)
    }

    /// Whether `event` is something said in the assistant's own thread: the
    /// conversation its verb names (what a post or an ask carries), or the
    /// one the message it changed is in.
    fn in_thread(&self, event: &DocumentEvent) -> bool {
        let names =
            |value: Option<&Value>| value.and_then(Value::as_str) == Some(self.thread.as_str());
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
    owner: &super::Owner,
    access: &Access,
    caller: Option<&tilo_actions::remote::Caller>,
    event: &str,
    seen: &Seen,
    now_ms: i64,
) -> Result<Gathered, String> {
    let workspace = owner.workspace.as_str();
    let person = hex::encode(owner.person);
    let reader = caller.map_or_else(|| access.reader(), ReadFor::Caller);
    let reader = (&owner.person, reader);
    let own = Own {
        thread: super::thread(&person, &access.agent).to_string(),
        person,
        assistant: access.agent.clone(),
        event: event.to_owned(),
    };
    let mut routes = workspaces
        .workspace_document_reads(workspace, reader)
        .await?;
    routes.retain(|(app, _), allowed| *allowed && access.signing.permits_app(app));
    let mut next = Seen {
        looked: true,
        documents: BTreeMap::new(),
        mail_seq: seen.mail_seq,
        due: BTreeSet::new(),
    };
    let mut found = Found::default();
    let (mut changed, mut due) = (Vec::new(), Vec::new());
    let against = Against {
        own: &own,
        assistant: tasks_model::agent_assignee(&own.assistant).0,
        seen,
        now_ms,
    };
    // The workspace's list of its agents is where the server counts what a
    // turn cost and where agents say how they stand: bookkeeping about the
    // assistant and its helpers, written after every turn, and never news.
    let roster = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    for (app, document) in routes.keys() {
        if app == tilo_workspace_agent::OWNER && document == &roster {
            continue;
        }
        let Ok(Some(bytes)) = workspaces.resource_document(workspace, app, document).await else {
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
            && let Some(named) = changes_in(workspace, (app, document), &mut doc, since, &own)
        {
            found.changed = found.changed.saturating_add(1);
            if changed.len() < MAX_NAMED {
                changed.push(named);
            }
        }
        next.documents.insert(key, heads);
        let tasks_part = app == TASKS
            && collab_types::document::DocumentId::parse(document)
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
    let id =
        collab_types::WorkspaceId::parse(workspace).ok_or("the workspace has no valid identity")?;
    if access.signing.permits_app(Store::MAIL.app) {
        let arrived = workspaces.mail_arrivals_as(id, reader, seen.mail_seq).await;
        next.mail_seq = arrived.newest;
        if seen.looked {
            found.came_in = u64::try_from(arrived.arrivals.len()).unwrap_or(u64::MAX);
        }
    }
    // Reads and decoding ran outside governance's lock. Discard this look if a
    // previously readable route was withdrawn before handing over its metadata.
    let current = workspaces
        .workspace_document_reads(workspace, reader)
        .await?;
    if routes.keys().any(|route| current.get(route) != Some(&true)) {
        return Err("record access changed during the look; take a new look".into());
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
    own: &'a Own,
    /// The assistant, as a task's assignee names it.
    assistant: String,
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
            assignee.0 == against.own.person || assignee.0 == against.assistant
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
