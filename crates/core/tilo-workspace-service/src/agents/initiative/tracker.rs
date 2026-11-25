//! The workspace's tasks as the server reads and writes them for a proposal: a proposed piece
//! of work stands there as a task of Tasks, so its person finds it where they look for tasks
//! and decides it where they decide tasks.
//!
//! A proposal is a task the assistant filed for itself: assigned to the assistant, given by
//! nobody, so nothing starts. Its person's nod is them giving it to the assistant, which is
//! what starts any task (the assistant's own trigger fires on it); turned down, it is
//! cancelled. The server writes each of these through Tasks' own actions, as whoever asked for
//! it, and reads what its person did there from the workspace's store.

use crate::agents::EngineActions;
use serde_json::{Map, Value, json};
use tilo_actions::remote::Caller;
use tilo_actions::remote::workspace_initiative::ProposedTask;
use tilo_peer_state::document_frame::DocumentRoute;

use super::proposals::InTracker;
use crate::agent_shortcuts::TASKS;
use crate::agents::task_work::{self, Held};
use crate::agents::triggered;
use crate::agents::{WorkspaceActionProvider, invoke_named};
use crate::workspaces::Workspaces;
use tilo_server_protocol::WorkspaceId;

/// tilolang Discussion, whose comments on a task say why it was proposed.
const DISCUSSION: &str = "so.tilo.lang.discussion";

/// What a task says about a proposal that stands as it.
fn stands(task: &Held) -> InTracker {
    match task.status.as_str() {
        "cancelled" => InTracker::Dropped,
        "in_progress" | "in_review" | "done" => InTracker::Given,
        _ if task.given_by_a_person() => InTracker::Given,
        _ => InTracker::Waits,
    }
}

/// Every task of `workspace`.
///
/// # Errors
/// The workspace is not kept here: what is not read is never taken for gone.
pub(super) async fn issues(workspaces: &Workspaces, workspace: &str) -> Result<Vec<Held>, String> {
    task_work::tasks(workspaces, workspace, Vec::new()).await
}

/// What the task at `address` says about a proposal that stands as it: one that is gone was
/// dropped.
pub(super) fn in_tracker(issues: &[Held], address: &str) -> InTracker {
    issues
        .iter()
        .find(|task| task.address() == address)
        .map_or(InTracker::Dropped, stands)
}

/// Tasks' actions, taken for one caller in one workspace.
pub(super) struct Tracker {
    pub workspaces: Workspaces,
    pub actions: EngineActions,
    pub workspace_id: WorkspaceId,
    pub workspace: String,
    pub caller: Caller,
}

impl Tracker {
    fn service(&self) -> WorkspaceActionProvider {
        WorkspaceActionProvider::new(self.actions.clone(), self.workspace_id)
    }

    fn route(&self, app: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: self.workspace.clone(),
            document: String::new(),
        }
    }

    /// The task at `address`, as the workspace keeps it now.
    async fn filed(&self, address: &str) -> Result<Held, String> {
        let gone = || "the task is no longer in Tasks".to_owned();
        let id = task_work::task_id(address).ok_or_else(gone)?;
        task_work::task(&self.workspaces, &self.workspace, id)
            .await
            .ok_or_else(gone)
    }

    /// One action of Tasks on the task `task`, as the caller.
    async fn on(
        &self,
        task: &Held,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<(), String> {
        invoke_named(
            &self.service(),
            &self.caller,
            &self.route(TASKS),
            &task.address(),
            action,
            arguments,
        )
        .await
        .map(drop)
    }

    /// File `title` as a task of the agent `assistant`, with its one line of `why` said in a
    /// comment on it, as the caller: the agent, so nobody gave it and nothing starts. The task
    /// as Tasks keeps it.
    ///
    /// # Errors
    /// Tasks did not take it, or does not show it.
    pub(super) async fn file(
        &self,
        assistant: &tilo_action_policy::MemberId,
        title: &str,
        why: &str,
    ) -> Result<ProposedTask, String> {
        let assignee = serde_json::to_value(assistant).map_err(|error| error.to_string())?;
        let mut arguments = Map::new();
        arguments.insert("title".to_owned(), json!(title));
        arguments.insert("assignee".to_owned(), assignee);
        let made = invoke_named(
            &self.service(),
            &self.caller,
            &self.route(TASKS),
            &format!("tl:{TASKS}"),
            "new_task",
            arguments,
        )
        .await?;
        let address = made
            .get("value")
            .and_then(task_work::made_task)
            .map(|id| triggered::published(TASKS, id))
            .ok_or_else(|| format!("Tasks made no task: {made}"))?;
        let task = self.filed(&address).await?;
        // Why it is proposed is said on the task, where its person decides it.
        let mut comment = Map::new();
        comment.insert("text".to_owned(), json!(why));
        comment.insert(
            tilo_actions::params::FROM_ARGUMENT.to_owned(),
            tilo_actions::params::MadeFrom {
                resource: address.clone(),
                kind: String::new(),
            }
            .to_json(),
        );
        if let Err(error) = invoke_named(
            &self.service(),
            &self.caller,
            &self.route(DISCUSSION),
            &format!("tl:{DISCUSSION}"),
            "comment",
            comment,
        )
        .await
        {
            tracing::warn!(workspace = %self.workspace, %error, "why a task was proposed is not said on it");
        }
        Ok(ProposedTask {
            issue: address,
            number: Some(task.number),
        })
    }

    /// Give the task at `address` to the agent it is filed for, as the caller: its person,
    /// whose giving is what starts the work. It is taken from the agent first, because giving
    /// a task to the agent it is with already asks nothing new.
    ///
    /// # Errors
    /// The task is gone or no agent's, or Tasks refused.
    pub(super) async fn give(&self, address: &str) -> Result<(), String> {
        let task = self.filed(address).await?;
        let agent = task
            .agent()
            .ok_or_else(|| "the task is no longer the assistant's in Tasks".to_owned())?;
        let assignee = serde_json::to_value(agent).map_err(|error| error.to_string())?;
        let mut nobody = Map::new();
        nobody.insert("who".to_owned(), Value::Null);
        self.on(&task, "assign", nobody).await?;
        let mut given = Map::new();
        given.insert("who".to_owned(), assignee);
        self.on(&task, "assign", given).await
    }

    /// Cancel the task at `address`, as the caller.
    ///
    /// # Errors
    /// The task is gone, or Tasks refused.
    pub(super) async fn cancel(&self, address: &str) -> Result<(), String> {
        let task = self.filed(address).await?;
        let mut cancelled = Map::new();
        cancelled.insert("status".to_owned(), json!("cancelled"));
        self.on(&task, "set_status", cancelled).await
    }
}
