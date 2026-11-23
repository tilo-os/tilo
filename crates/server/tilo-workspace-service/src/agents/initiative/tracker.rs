//! The workspace's Tasks as the server reads and writes them for a proposal:
//! a proposed piece of work stands there as a task, so its person finds it
//! where they look for tasks and decides it where they decide tasks.
//!
//! A proposal is a task the assistant filed for itself: assigned to the
//! assistant, given by nobody, so nothing starts. Its person's nod is them
//! giving it to the assistant, which is what starts any task; turned down,
//! it is cancelled. The server writes each of these through Tasks' own
//! actions, as whoever asked for it, and reads what its person did there
//! through the store, as that person may read it.

use serde_json::{Map, Value, json};
use tilo_actions::remote::Caller;
use tilo_actions::remote::workspace_initiative::ProposedTask;

use super::proposals::InTracker;
use crate::agents::tl_tasks::{self, Reader, Task};
use crate::agents::{EngineActions, WorkspaceActionProvider};
use crate::workspaces::Workspaces;
use tilo_server_protocol::WorkspaceId;

/// What the task at a proposal's address says about the proposal that stands as it: given to
/// the assistant by a person it is accepted, cancelled it is declined, and otherwise it waits.
fn stands(task: &Task) -> InTracker {
    let given = task
        .assigned_by
        .as_deref()
        .and_then(tl_tasks::actor_of)
        .is_some_and(
            |by| matches!(by, tilo_actions::Actor::Member(member) if member.person_id().is_some()),
        );
    match task.status.as_str() {
        "cancelled" => InTracker::Dropped,
        "in_progress" | "in_review" | "done" => InTracker::Given,
        _ if given => InTracker::Given,
        _ => InTracker::Waits,
    }
}

/// What the tasks at `addresses` say, as `person` (their root, in hex) reads them: a task gone,
/// or one they may not read, is no longer there.
///
/// # Errors
/// The store could not be read: what is not read is never taken for gone.
pub(super) async fn issues(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    addresses: &[String],
) -> Result<Vec<(String, InTracker)>, String> {
    let reader = Reader::Person(workspaces, person);
    let mut read = Vec::new();
    for address in addresses {
        let task = match tl_tasks::task_of(address) {
            Some(id) => reader.task(workspace, id).await?,
            None => None,
        };
        read.push((
            address.clone(),
            task.as_ref().map_or(InTracker::Dropped, stands),
        ));
    }
    Ok(read)
}

/// What the task at `address` says about a proposal that stands as it, of what [`issues`] read.
pub(super) fn in_tracker(issues: &[(String, InTracker)], address: &str) -> InTracker {
    issues
        .iter()
        .find(|(read, _)| read == address)
        .map_or(InTracker::Waits, |(_, stands)| *stands)
}

/// Tasks' actions, taken for one caller in one workspace.
pub(super) struct Tracker {
    pub actions: EngineActions,
    pub workspace_id: WorkspaceId,
    pub workspace: String,
    pub caller: Caller,
}

impl Tracker {
    fn service(&self) -> WorkspaceActionProvider {
        WorkspaceActionProvider::new(self.actions.clone(), self.workspace_id)
    }

    /// The task at `address`, as the caller reads it now.
    async fn task(&self, address: &str) -> Result<Task, String> {
        let id = tl_tasks::task_of(address).ok_or("the proposal stands as no task")?;
        let reads = self
            .actions
            .resource_reads()
            .ok_or("this device reads no task for an agent")?;
        let reader = Reader::Turn {
            reads,
            caller: &self.caller,
        };
        reader
            .task(&self.workspace, id)
            .await?
            .ok_or_else(|| "the task is no longer in Tasks".to_owned())
    }

    /// One of Tasks' actions on `task`, as the caller.
    async fn on(
        &self,
        task: &str,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<(), String> {
        tl_tasks::invoke(
            &self.service(),
            &self.caller,
            &self.workspace,
            task,
            action,
            arguments,
        )
        .await
        .map(drop)
    }

    /// File `title` as a task of the agent `assistant`, as the caller (an agent, so nobody gave
    /// it and nothing starts), with its one line of `why` as the task's first comment. The task
    /// as Tasks keeps it.
    ///
    /// # Errors
    /// Tasks did not take it, or does not show it.
    pub(super) async fn file(
        &self,
        assistant: &str,
        title: &str,
        why: &str,
    ) -> Result<ProposedTask, String> {
        let arguments = json!({"title": title, "assignee": {"agent": assistant}});
        let made = tl_tasks::invoke(
            &self.service(),
            &self.caller,
            &self.workspace,
            &tl_tasks::tracker(),
            "new_task",
            arguments.as_object().cloned().unwrap_or_default(),
        )
        .await?;
        let address = made
            .pointer("/resources/0")
            .and_then(Value::as_str)
            .filter(|made| tl_tasks::task_of(made).is_some())
            .ok_or_else(|| format!("Tasks made no task: {made}"))?
            .to_owned();
        if !why.trim().is_empty()
            && let Err(error) = tl_tasks::comment(
                &self.service(),
                &self.caller,
                &self.workspace,
                &address,
                why.trim(),
            )
            .await
        {
            tracing::warn!(workspace = %self.workspace, %error, "why a task was proposed is not said on it");
        }
        let task = self.task(&address).await?;
        Ok(ProposedTask {
            issue: address,
            number: u64::try_from(task.number).ok(),
        })
    }

    /// Give the task at `address` to the agent it is filed for, as the caller: its person,
    /// whose giving is what starts the work. It is taken from the agent first, since Tasks
    /// refuses to give a task to whom it is with already.
    ///
    /// # Errors
    /// The task is gone or no agent's, or Tasks refused.
    pub(super) async fn give(&self, address: &str) -> Result<(), String> {
        let task = self.task(address).await?;
        let assignee = task
            .assignee
            .as_deref()
            .and_then(tl_tasks::actor_of)
            .and_then(|actor| match actor {
                tilo_actions::Actor::Member(member) => member.agent_id().map(str::to_owned),
                _ => None,
            })
            .ok_or_else(|| "the task is no longer the assistant's in Tasks".to_owned())?;
        let mut taken = Map::new();
        taken.insert("who".to_owned(), Value::Null);
        self.on(address, "assign", taken).await?;
        let mut given = Map::new();
        given.insert("who".to_owned(), json!({"agent": assignee}));
        self.on(address, "assign", given).await
    }

    /// Cancel the task at `address`, as the caller.
    ///
    /// # Errors
    /// The task is gone, or Tasks refused.
    pub(super) async fn cancel(&self, address: &str) -> Result<(), String> {
        self.task(address).await?;
        let mut cancelled = Map::new();
        cancelled.insert("status".to_owned(), json!("cancelled"));
        self.on(address, "set_status", cancelled).await
    }
}

#[cfg(all(test, feature = "apps"))]
mod tests {
    use tilo_resource_store::Value as Stored;

    use super::*;
    use crate::agents::tl_tasks::testing::Host;

    /// A proposed piece of work is filed as a task of Tasks, by the assistant, its why said on
    /// it; its person's nod gives it to the assistant, and turning it down cancels it. What the
    /// person did there is read back through the store.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_proposal_is_filed_as_a_task_and_decided_there() {
        let host = Host::new().await;
        let broker = host.broker(&[]);
        let tracker = Tracker {
            actions: host.service(&broker).actions.clone(),
            workspace_id: WorkspaceId::parse(&host.workspace).unwrap(),
            workspace: host.workspace.clone(),
            caller: host.caller(),
        };
        // Tasks gives the new task its id and number.
        host.put(
            tl_tasks::TASK,
            "t-3",
            vec![
                ("number", Stored::Int(3)),
                ("title", Stored::Text("Tidy the shared folder".into())),
                ("status", Stored::Text("backlog".into())),
                ("assignee", Stored::Text(tl_tasks::agent("scout"))),
            ],
        )
        .await;
        broker.made("new_task", "tl:so.tilo.tasks:t-3");
        let filed = tracker
            .file(
                "scout",
                "Tidy the shared folder",
                "It holds 40 copies of one file.",
            )
            .await
            .unwrap();
        assert_eq!(filed.issue, tl_tasks::resource("t-3"));
        assert_eq!(filed.number, Some(3));
        let asked = broker.asked.lock().unwrap().clone();
        assert_eq!(asked[0].1, tl_tasks::tracker());
        assert_eq!(asked[0].2, "new_task");
        assert_eq!(asked[0].3["title"], "Tidy the shared folder");
        assert_eq!(asked[0].3["assignee"], json!({"agent": "scout"}));
        assert_eq!(asked[1].2, "comment");
        assert_eq!(asked[1].3["text"], "It holds 40 copies of one file.");

        tracker.give(&filed.issue).await.unwrap();
        tracker.cancel(&filed.issue).await.unwrap();
        let asked = broker.asked.lock().unwrap().clone();
        let tail: Vec<_> = asked[2..]
            .iter()
            .map(|(_, resource, action, arguments)| {
                (resource.as_str(), action.as_str(), arguments.clone())
            })
            .collect();
        let task = tl_tasks::resource("t-3");
        assert_eq!(
            tail,
            [
                (
                    task.as_str(),
                    "assign",
                    json!({"who": null}).as_object().cloned().unwrap()
                ),
                (
                    task.as_str(),
                    "assign",
                    json!({"who": {"agent": "scout"}})
                        .as_object()
                        .cloned()
                        .unwrap()
                ),
                (
                    task.as_str(),
                    "set_status",
                    json!({"status": "cancelled"}).as_object().cloned().unwrap()
                ),
            ]
        );

        // What its person did is read where they did it.
        let read = |status: &'static str, by: Option<String>| {
            let mut fields = vec![
                ("number", Stored::Int(3)),
                ("title", Stored::Text("Tidy the shared folder".into())),
                ("status", Stored::Text(status.into())),
                ("assignee", Stored::Text(tl_tasks::agent("scout"))),
            ];
            if let Some(by) = by {
                fields.push(("assigned_by", Stored::Text(by)));
            }
            fields
        };
        let decided = async |fields| {
            host.put(tl_tasks::TASK, "t-3", fields).await;
            let read = issues(
                &host.workspaces,
                &host.workspace,
                &host.person,
                &[task.clone()],
            )
            .await
            .unwrap();
            in_tracker(&read, &task)
        };
        assert_eq!(decided(read("backlog", None)).await, InTracker::Waits);
        assert_eq!(
            decided(read("backlog", Some(tl_tasks::agent("scout")))).await,
            InTracker::Waits,
            "the assistant giving it to itself is no nod"
        );
        assert_eq!(
            decided(read("todo", Some(tl_tasks::person(&host.person)))).await,
            InTracker::Given
        );
        assert_eq!(decided(read("cancelled", None)).await, InTracker::Dropped);
        let gone = issues(
            &host.workspaces,
            &host.workspace,
            &host.person,
            &[tl_tasks::resource("t-9")],
        )
        .await
        .unwrap();
        assert_eq!(
            in_tracker(&gone, &tl_tasks::resource("t-9")),
            InTracker::Dropped
        );
        host.end().await;
    }
}
