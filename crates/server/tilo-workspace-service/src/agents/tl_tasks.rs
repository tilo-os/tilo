//! The workspace's Tasks as the agent engine reads and writes them: through the store's typed
//! reads and Tasks' own actions, never by opening its documents.
//!
//! A read is decided by who reads: a turn's agent for the person it acts for reads what both
//! may ([`Reader::Turn`]), and a server step done for one person reads what that person may
//! ([`Reader::Person`]). Every field read here is one the store indexes, as Tasks declares it:
//! a task's scalars, and the scalars of the record its `work` names. What Tasks decides (whether
//! a session may begin, when a task's pieces are through, what a wait ends with) is never
//! decided here: the engine invokes the action, and Tasks refuses or carries it out.

use std::sync::Arc;

use serde_json::{Map, Value as Json};
use tilo_actions::remote::{Caller, ResourceAddress, ResourceReads};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_resource_store::wire::{Command, Reply};
use tilo_resource_store::{Filter, Indexed, Query, Value};

use crate::workspaces::Workspaces;

/// The app that keeps the workspace's tasks.
pub(super) const APP: &str = "so.tilo.tasks";
/// The kind of a task, as the store holds it.
pub(super) const TASK: &str = "tl.so.tilo.tasks.Task";
/// The kind of what a task keeps while an agent works it.
const WORK: &str = "tl.so.tilo.tasks.Work";
/// The kind of what made a task go on after a wait.
const RESUMED: &str = "tl.so.tilo.tasks.Resumed";
/// How many tasks one read of the store takes.
const PAGE: usize = 500;

/// The resource a task is published as: `tl:so.tilo.tasks:<id>`.
pub(super) fn resource(task: &str) -> String {
    format!("tl:{APP}:{task}")
}

/// The id of the task a published resource names, when it names one.
pub(super) fn task_of(resource: &str) -> Option<&str> {
    resource
        .strip_prefix("tl:")?
        .strip_prefix(APP)?
        .strip_prefix(':')
        .filter(|id| !id.is_empty())
}

/// How the store holds an actor in a task's `assignee` or `assigned_by`.
pub(super) fn actor_text(actor: &tilo_actions::Actor) -> String {
    serde_json::to_string(actor).unwrap_or_default()
}

/// The actor [`actor_text`] wrote.
pub(super) fn actor_of(text: &str) -> Option<tilo_actions::Actor> {
    tilo_failure::ignored!(
        "a field that holds no actor names nobody",
        serde_json::from_str(text).ok()
    )
}

/// An agent as a task's assignee names it.
pub(super) fn agent(agent: &str) -> String {
    actor_text(&tilo_actions::Actor::Member(tilo_actions::MemberId::agent(
        agent,
    )))
}

/// A person, by their root, as a task's assignee names them.
pub(super) fn person(person: &str) -> String {
    actor_text(&tilo_actions::Actor::Member(
        tilo_actions::MemberId::person(person.to_owned()),
    ))
}

/// Who reads the workspace's tasks.
pub(super) enum Reader<'a> {
    /// A turn's agent, for the person it acts for: what both may read.
    Turn {
        reads: Arc<dyn ResourceReads>,
        caller: &'a Caller,
    },
    /// A person, by their root in hex: what their reader sets name.
    Person(&'a Workspaces, &'a str),
}

impl Reader<'_> {
    async fn read(&self, workspace: &str, command: Command) -> Result<Reply, String> {
        match self {
            Self::Turn { reads, caller } => {
                let (reads, caller) = (Arc::clone(reads), (*caller).clone());
                let command = serde_json::to_value(command).map_err(|error| error.to_string())?;
                let address = ResourceAddress {
                    app: APP.to_owned(),
                    record: String::new(),
                };
                let reply = tokio::task::spawn_blocking(move || {
                    reads.store_read(&caller, &address, command)
                })
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
                serde_json::from_value(reply).map_err(|error| error.to_string())
            }
            Self::Person(workspaces, person) => {
                workspaces.read_store_as(workspace, person, &command).await
            }
        }
    }

    /// The resource `id` of `kind`, as the store indexes it; none where there is none this
    /// reader may read.
    async fn get(&self, workspace: &str, kind: &str, id: &str) -> Result<Option<Indexed>, String> {
        let command = Command::Get {
            kind: kind.to_owned(),
            id: id.to_owned(),
        };
        match self.read(workspace, command).await? {
            Reply::Resource(found) => Ok(found),
            other => Err(format!(
                "the store answered a read of a task with {other:?}"
            )),
        }
    }

    /// Every task of `workspace` that holds to `filters`, as this reader may read them.
    pub(super) async fn tasks(
        &self,
        workspace: &str,
        filters: Vec<Filter>,
    ) -> Result<Vec<Task>, String> {
        let mut tasks = Vec::new();
        let mut cursor = None;
        loop {
            let mut query = Query::of(TASK, PAGE).after(cursor);
            for filter in &filters {
                query = query.filter(filter.clone());
            }
            let page = match self.read(workspace, Command::Query(query)).await? {
                Reply::Page(page) => page,
                other => {
                    return Err(format!(
                        "the store answered a query of tasks with {other:?}"
                    ));
                }
            };
            tasks.extend(page.resources.iter().map(|(id, task)| Task::of(id, task)));
            match page.next {
                Some(next) => cursor = Some(next),
                None => return Ok(tasks),
            }
        }
    }

    /// The tasks of `workspace` that are not finished, as this reader may read them.
    pub(super) async fn open_tasks(&self, workspace: &str) -> Result<Vec<Task>, String> {
        let closed = ["done", "cancelled"]
            .map(|status| Filter::Is("status".to_owned(), Value::Text(status.to_owned())));
        self.tasks(
            workspace,
            vec![Filter::Not(Box::new(Filter::Any(closed.into())))],
        )
        .await
    }

    /// The task `id`; none where it is gone or this reader may not read it.
    pub(super) async fn task(&self, workspace: &str, id: &str) -> Result<Option<Task>, String> {
        Ok(self
            .get(workspace, TASK, id)
            .await?
            .map(|task| Task::of(id, &task)))
    }

    /// What `task` keeps of its work; the default where nothing worked it yet.
    pub(super) async fn work(&self, workspace: &str, task: &Task) -> Result<Work, String> {
        let Some(id) = &task.work else {
            return Ok(Work::default());
        };
        let Some(work) = self.get(workspace, WORK, id).await? else {
            return Ok(Work::default());
        };
        let resumed = match text(&work, "resumed") {
            Some(resumed) => self
                .get(workspace, RESUMED, &resumed)
                .await?
                .and_then(|resumed| text(&resumed, "cause")),
            None => None,
        };
        Ok(Work {
            folder: text(&work, "folder").unwrap_or_default(),
            note: text(&work, "note").unwrap_or_default(),
            budget_micros: int(&work, "budget_micros"),
            spent_micros: int(&work, "spent_micros").unwrap_or_default(),
            sessions: int(&work, "sessions").unwrap_or_default(),
            interrupted: text(&work, "session_since").is_some(),
            resumed,
        })
    }
}

/// The text a field holds; none for an absent or empty one.
fn text(resource: &Indexed, field: &str) -> Option<String> {
    resource
        .scalar(field)
        .as_text()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The whole number a field holds; none for an absent one.
fn int(resource: &Indexed, field: &str) -> Option<i64> {
    match resource.scalar(field) {
        Value::Int(number) => Some(number),
        _ => None,
    }
}

/// One task, as far as the engine reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Task {
    pub id: String,
    pub number: i64,
    pub title: String,
    /// Its status, by its case's name (`todo`, `in_progress`, `cancelled`).
    pub status: String,
    /// Who is doing it, as [`actor_text`] writes an actor.
    pub assignee: Option<String>,
    /// Who gave it to the agent it is assigned to, as [`actor_text`] writes an actor.
    pub assigned_by: Option<String>,
    /// The day it is due by (`2026-11-23`), and the moment on that day, if it has them.
    pub due: Option<String>,
    pub at: Option<String>,
    /// The automation that ends its wait early, by its id; empty when none does.
    pub waiting_on: String,
    /// The record of its work, by its id; none until something worked it.
    work: Option<String>,
}

impl Task {
    fn of(id: &str, task: &Indexed) -> Self {
        Self {
            id: id.to_owned(),
            number: int(task, "number").unwrap_or_default(),
            title: text(task, "title").unwrap_or_default(),
            status: text(task, "status").unwrap_or_default(),
            assignee: text(task, "assignee"),
            assigned_by: text(task, "assigned_by"),
            due: text(task, "due"),
            at: text(task, "at"),
            waiting_on: text(task, "waiting_on").unwrap_or_default(),
            work: text(task, "work"),
        }
    }

    /// Whether it is finished, one way or the other.
    pub(super) fn closed(&self) -> bool {
        matches!(self.status.as_str(), "done" | "cancelled")
    }

    /// Its published resource.
    pub(super) fn resource(&self) -> String {
        resource(&self.id)
    }

    /// The moment it is due, in milliseconds since the epoch: its moment when it has one, else
    /// the start of its day. None for a task due on no day.
    pub(super) fn due_ms(&self) -> Option<i64> {
        if let Some(at) = &self.at
            && let Ok(at) = at.parse::<jiff::Timestamp>()
        {
            return Some(at.as_millisecond());
        }
        let day = self.due.as_deref()?.parse::<jiff::civil::Date>().ok()?;
        day.to_zoned(jiff::tz::TimeZone::UTC)
            .ok()
            .map(|day| day.timestamp().as_millisecond())
    }
}

/// What a task keeps of its work, as far as the engine reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Work {
    /// Where its working material is: the folder by its path, the note by its resource.
    pub folder: String,
    pub note: String,
    pub budget_micros: Option<i64>,
    pub spent_micros: i64,
    /// How many sessions worked it.
    pub sessions: i64,
    /// Whether a session began on it and was not said to have ended.
    pub interrupted: bool,
    /// What made it go on after its latest wait, when something did.
    pub resumed: Option<String>,
}

impl Work {
    /// What is left of its budget in money, in millionths; none for no limit.
    pub(super) fn money_left(&self) -> Option<u64> {
        self.budget_micros.map(|budget| {
            u64::try_from(budget.saturating_sub(self.spent_micros)).unwrap_or_default()
        })
    }
}

/// Invoke Tasks' action `action` on `resource` (a task's, or Tasks' own for an action on the
/// whole tracker) as `caller`, with its other arguments by name: what the action answered.
///
/// # Errors
/// Tasks refused it, in the words its rule says, or it could not be asked.
pub(super) async fn invoke(
    provider: &super::WorkspaceActionProvider,
    caller: &Caller,
    workspace: &str,
    resource: &str,
    action: &str,
    arguments: Map<String, Json>,
) -> Result<Json, String> {
    let route = DocumentRoute {
        app_id: APP.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    };
    super::invoke_named(provider, caller, &route, resource, action, arguments).await
}

/// Comment `text` on the task at `task` (its published resource) as `caller`: a comment of
/// Discussion made from the task, which its page shows under it.
///
/// # Errors
/// Discussion refused it.
pub(super) async fn comment(
    provider: &super::WorkspaceActionProvider,
    caller: &Caller,
    workspace: &str,
    task: &str,
    text: &str,
) -> Result<(), String> {
    const DISCUSSION: &str = "so.tilo.lang.discussion";
    let route = DocumentRoute {
        app_id: DISCUSSION.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    };
    let mut arguments = Map::new();
    arguments.insert("text".into(), Json::from(text));
    arguments.insert(
        tilo_actions::params::FROM_ARGUMENT.into(),
        tilo_actions::params::MadeFrom {
            resource: task.to_owned(),
            kind: String::new(),
        }
        .to_json(),
    );
    let discussion = format!("tl:{DISCUSSION}");
    super::invoke_named(provider, caller, &route, &discussion, "comment", arguments)
        .await
        .map(drop)
}

/// Tasks' own resource, which an action on the whole tracker is invoked on.
pub(super) fn tracker() -> String {
    format!("tl:{APP}")
}

/// What the engine's tests of Tasks stand on: a workspace whose store they fill, and a broker
/// standing where the device's would.
#[cfg(all(test, feature = "apps"))]
pub(super) mod testing {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use serde_json::{Map, Value};
    use tilo_actions::remote::{
        ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
        InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, ResourceReads,
    };
    use tilo_resource_store::wire::Command;
    use tilo_resource_store::{Cell, Resource, Stamp, Value as Stored};
    use tilo_server_protocol::WorkspaceId;

    use crate::workspaces::Workspaces;
    use crate::workspaces::fixtures::{TestGroupKeys, TestHost, governor, test_host_holding};

    /// The device's broker as a turn's calls reach it: it decides each call, Tasks' rules
    /// standing behind it (`refuses` names the actions it refuses, with Tasks' words), and keeps
    /// who asked what. Reads are the workspace's store, as the turn's person may read it.
    pub(in crate::agents) struct Broker {
        workspaces: Workspaces,
        workspace: String,
        person: String,
        refuses: BTreeMap<&'static str, &'static str>,
        pub asked: Mutex<Vec<(Caller, String, String, Map<String, Value>)>>,
        /// The resource each action answers it made, by the action.
        makes: Mutex<BTreeMap<String, String>>,
    }

    impl Broker {
        /// Answer `action` with `resource` as what it made.
        pub fn made(&self, action: &str, resource: &str) {
            self.makes
                .lock()
                .unwrap()
                .insert(action.to_owned(), resource.to_owned());
        }
    }

    impl ResourceReads for Broker {
        fn answer_read(
            self: Arc<Self>,
            caller: &Caller,
            correlation: u64,
            read: tilo_actions::remote::ResourceReadRequest,
        ) -> Pending<tilo_actions::remote::RemoteActionMessage> {
            tilo_actions::remote::answer_read(self, caller, correlation, read)
        }

        fn read(
            &self,
            _: &Caller,
            _: &tilo_actions::remote::ResourceAddress,
        ) -> Result<tilo_actions::remote::ResourceCopy, ActionCallError> {
            Err(ActionCallError::UnknownAction)
        }

        fn store_read(
            &self,
            _: &Caller,
            address: &tilo_actions::remote::ResourceAddress,
            read: Value,
        ) -> Result<Value, ActionCallError> {
            assert_eq!(address.app, super::APP);
            let command: Command = serde_json::from_value(read).unwrap();
            let reply = tokio::runtime::Handle::current()
                .block_on(
                    self.workspaces
                        .read_store_as(&self.workspace, &self.person, &command),
                )
                .unwrap();
            Ok(serde_json::to_value(reply).unwrap())
        }

        fn watch(
            &self,
            _: &Caller,
            _: &tilo_actions::remote::ResourceAddress,
            _: &tilo_actions::remote::ResourceWake,
        ) -> Result<(), ActionCallError> {
            Err(ActionCallError::UnknownAction)
        }
    }

    struct Provider(Arc<Broker>);

    impl ActionProvider for Provider {
        fn query(
            &self,
            _: &Caller,
            _: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Err(ActionCallError::UnknownAction))
        }

        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            let action = request.target.address.action.clone();
            let arguments = match &request.argument {
                InvokeArgument::Json(arguments) => arguments.clone(),
                _ => Map::new(),
            };
            self.0.asked.lock().unwrap().push((
                caller.clone(),
                request.target.address.resource.clone(),
                action.clone(),
                arguments,
            ));
            if let Some(words) = self.0.refuses.get(action.as_str()) {
                return Pending::ready(Err(ActionCallError::Refused {
                    reason: (*words).to_owned(),
                }));
            }
            let resources = self
                .0
                .makes
                .lock()
                .unwrap()
                .get(action.as_str())
                .cloned()
                .into_iter()
                .collect();
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: None,
                        resources,
                    },
                },
            }))
        }

        fn observe(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<InvocationState> {
            None
        }

        fn next_change(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<Pending<()>> {
            None
        }

        fn cancel(
            &self,
            _: &Caller,
            _: &tilo_actions::InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Ok(()))
        }

        fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
            Some(Arc::clone(&self.0) as Arc<dyn ResourceReads>)
        }
    }

    struct Keys;

    impl super::super::agent_keys::AgentKeys for Keys {
        fn grant(&self, _: WorkspaceId, _: &str, _: &str, _: [u8; 32]) -> Result<[u8; 32], String> {
            Ok([7; 32])
        }

        fn sign(&self, _: WorkspaceId, _: &[u8; 32], _: &mut InvokeRequest) -> Result<(), String> {
            Ok(())
        }
    }

    /// A workspace of a test host, its person and the store Tasks keeps there.
    pub(in crate::agents) struct Host {
        host: TestHost,
        _dir: tempfile::TempDir,
        pub workspaces: Workspaces,
        pub workspace: String,
        /// The person, by their root in hex.
        pub person: String,
    }

    impl Host {
        pub async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (key, cert) = governor();
            let host =
                test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default()))
                    .await;
            let workspaces = host.workspaces().clone();
            workspaces.set_member_device(Arc::new(crate::workspaces::MemberDevice {
                cert: cert.clone(),
                signer: Arc::new(key.clone()),
            }));
            let workspace = workspaces.create("Tasks").await.unwrap().workspace;
            let route = tilo_peer_state::document_frame::DocumentRoute {
                app_id: super::APP.into(),
                cluster_id: workspace.clone(),
                document: String::new(),
            };
            let _ = workspaces.own_route_keys(&route, true).await.unwrap();
            Self {
                host,
                _dir: dir,
                workspaces,
                workspace,
                person: hex::encode(cert.user_public_key),
            }
        }

        /// Put the resource `id` of `kind`, every member reading it, with `fields`.
        pub async fn put(&self, kind: &str, id: &str, fields: Vec<(&str, Stored)>) {
            self.put_for(kind, id, "*", fields).await;
        }

        /// Put the resource `id` of `kind`, read by `readers`, with `fields`.
        pub async fn put_for(
            &self,
            kind: &str,
            id: &str,
            readers: &str,
            fields: Vec<(&str, Stored)>,
        ) {
            self.workspaces
                .own_resource_command(
                    &self.workspace,
                    super::APP,
                    Command::Put {
                        kind: kind.into(),
                        id: id.into(),
                        resource: Resource {
                            readers: readers.into(),
                            parent: None,
                            order: None,
                            fields: fields
                                .into_iter()
                                .map(|(name, value)| (name.to_owned(), Cell::One(value)))
                                .collect(),
                        },
                        stamp: Stamp {
                            verb: "new_task".into(),
                            invoker: self.person.clone(),
                            invocation: id.into(),
                            ..Stamp::default()
                        },
                    },
                )
                .await
                .unwrap();
        }

        /// A broker over this workspace that refuses `refuses`, with Tasks' words.
        pub fn broker(&self, refuses: &[(&'static str, &'static str)]) -> Arc<Broker> {
            Arc::new(Broker {
                workspaces: self.workspaces.clone(),
                workspace: self.workspace.clone(),
                person: self.person.clone(),
                refuses: refuses.iter().copied().collect(),
                asked: Mutex::default(),
                makes: Mutex::new(BTreeMap::from([(
                    "write_note".to_owned(),
                    "tl:so.tilo.notes:n-1".to_owned(),
                )])),
            })
        }

        /// The engine's actions through `broker`.
        pub fn service(&self, broker: &Arc<Broker>) -> super::super::WorkspaceActionProvider {
            let provider: Arc<dyn ActionProvider + Send + Sync> =
                Arc::new(Provider(Arc::clone(broker)));
            super::super::WorkspaceActionProvider::new(
                super::super::EngineActions::Device(provider, Arc::new(Keys)),
                WorkspaceId::parse(&self.workspace).unwrap(),
            )
        }

        /// The turn's agent, as its calls name it.
        pub fn caller(&self) -> Caller {
            Caller::authenticated([9; 32], [7; 32]).in_workspace(self.workspace.clone())
        }

        pub async fn end(self) {
            self.host.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_is_due_at_its_moment_or_else_at_the_start_of_its_day() {
        let mut task = Task {
            due: Some("2026-11-23".into()),
            ..Task::default()
        };
        assert_eq!(task.due_ms(), Some(1_795_392_000_000));
        task.at = Some("2026-11-23T10:00:00Z".into());
        assert_eq!(task.due_ms(), Some(1_795_428_000_000));
        assert_eq!(Task::default().due_ms(), None);
    }

    #[test]
    fn a_published_task_names_its_id() {
        assert_eq!(task_of(&resource("t-1")), Some("t-1"));
        assert_eq!(task_of("tl:so.tilo.notes:t-1"), None);
        assert_eq!(task_of("tl:so.tilo.tasks:"), None);
    }
}
