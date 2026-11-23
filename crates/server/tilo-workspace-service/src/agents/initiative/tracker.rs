//! The workspace's tracker as the server reads and writes it for a proposal:
//! a proposed piece of work stands there as a task, so its person finds it
//! where they look for tasks and decides it where they decide tasks.
//!
//! A proposal is a task the assistant filed for itself: assigned to the
//! assistant, given by nobody, so nothing starts. Its person's nod is them
//! giving it to the assistant, which is what starts any task; turned down,
//! it is cancelled. The server writes each of these through the tracker's own
//! actions, as whoever asked for it, and reads what its person did there
//! from the document.

use crate::agents::EngineActions;
use serde_json::{Map, Value, json};
use tilo_actions::remote::Caller;
use tilo_actions::remote::workspace_initiative::ProposedTask;
use tilo_peer_state::document_frame::DocumentRoute;

use super::proposals::InTracker;
use crate::agents::{WorkspaceActionProvider, invoke_named};
use crate::workspaces::Workspaces;
use tilo_server_protocol::WorkspaceId;

/// The app that keeps the workspace's tasks.
const TASKS: &str = "so.tilo.tasks";

/// One issue of the tracker, as far as a proposal reads it.
pub(super) struct Filed {
    /// The document of the part it is kept in.
    document: String,
    id: String,
    /// Its address, whichever part keeps it.
    address: String,
    title: String,
    number: u64,
    created_ms: i64,
    assignee: Option<String>,
    /// Whether a person gave it to the agent it is assigned to.
    given: bool,
    /// What it says about a proposal that stands as it.
    stands: InTracker,
}

impl Filed {
    fn of(document: &str, id: &tilo_resource::Id, issue: &tasks_model::Issue) -> Self {
        use tasks_model::Status;
        let given = issue.assigned_ms > 0
            && issue
                .assigned_by
                .as_ref()
                .is_some_and(tilo_resource::MemberId::is_person);
        let stands = match issue.status {
            Status::Cancelled => InTracker::Dropped,
            Status::InProgress | Status::InReview | Status::Done => InTracker::Given,
            Status::Backlog | Status::Todo if given => InTracker::Given,
            Status::Backlog | Status::Todo => InTracker::Waits,
        };
        Self {
            document: document.to_owned(),
            id: id.as_str().to_owned(),
            address: tasks_model::issue_resource(id),
            title: issue.title.clone(),
            number: issue.number,
            created_ms: issue.created_ms,
            assignee: issue.assignee.as_ref().map(|assignee| assignee.0.clone()),
            given,
            stands,
        }
    }
}

/// Every issue the tracker of `workspace` keeps, part by part.
///
/// # Errors
/// The workspace is not governed here, or a part of its tracker cannot be
/// read: what is not read is never taken for gone.
pub(super) async fn issues(workspaces: &Workspaces, workspace: &str) -> Result<Vec<Filed>, String> {
    let routes = workspaces
        .workspace_document_routes(workspace)
        .await
        .map_err(|error| error.to_string())?;
    let mut filed = Vec::new();
    for (app, document) in routes {
        let part = app == TASKS
            && collab_types::document::DocumentId::parse(&document)
                .and_then(|id| id.chain_part())
                .is_some_and(|(chain, _)| chain == tasks::issue_list::chain());
        if !part {
            continue;
        }
        let Some(bytes) = workspaces
            .resource_document(workspace, &app, &document)
            .await
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
        let part = crate::agents::opened::<tasks::issue_list::IssueList>(&mut doc)?;
        filed.extend(
            part.issues
                .iter()
                .map(|(id, issue)| Filed::of(&document, id, issue)),
        );
    }
    Ok(filed)
}

/// What the issue at `address` says about a proposal that stands as it:
/// one that is gone was dropped.
pub(super) fn in_tracker(issues: &[Filed], address: &str) -> InTracker {
    issues
        .iter()
        .find(|filed| filed.address == address)
        .map_or(InTracker::Dropped, |filed| filed.stands)
}

/// The tracker's actions, taken for one caller in one workspace.
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

    fn route(&self, document: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: TASKS.to_owned(),
            cluster_id: self.workspace.clone(),
            document: document.to_owned(),
        }
    }

    /// The issue at `address`, as the tracker keeps it now.
    async fn filed(&self, address: &str) -> Result<Filed, String> {
        issues(&self.workspaces, &self.workspace)
            .await?
            .into_iter()
            .find(|filed| filed.address == address)
            .ok_or_else(|| "the task is no longer in the tracker".to_owned())
    }

    /// One action on the issue `filed`, as the caller.
    async fn on(
        &self,
        filed: &Filed,
        action: &str,
        mut arguments: Map<String, Value>,
    ) -> Result<(), String> {
        arguments.insert("issue".to_owned(), json!(filed.id));
        invoke_named(
            &self.service(),
            &self.caller,
            &self.route(&filed.document),
            &filed.address,
            action,
            arguments,
        )
        .await
        .map(drop)
    }

    /// File `title` with its one line of `why` as a task of the agent
    /// `assistant`, as the caller: an agent, so nobody gave it and nothing
    /// starts. The task as the tracker keeps it.
    ///
    /// # Errors
    /// The tracker did not take it, or does not show it.
    pub(super) async fn file(
        &self,
        assistant: &str,
        title: &str,
        why: &str,
    ) -> Result<ProposedTask, String> {
        let assignee = tasks_model::agent_assignee(assistant).0;
        let tracker =
            crate::agents::workspace_resource(&tilo_agent_session::AppCatalogue::native(), TASKS);
        let arguments = json!({"title": title, "body": why, "assignee": assignee});
        invoke_named(
            &self.service(),
            &self.caller,
            &self.route(&tracker),
            &collab_types::document::chain_resource_id(&tasks::issue_list::chain()),
            "create",
            arguments.as_object().cloned().unwrap_or_default(),
        )
        .await?;
        // The tracker gave it its id and number: the newest task of this
        // title the assistant has that nobody gave it.
        issues(&self.workspaces, &self.workspace)
            .await?
            .into_iter()
            .filter(|filed| {
                filed.title == title && !filed.given && filed.assignee.as_deref() == Some(&assignee)
            })
            .max_by_key(|filed| filed.created_ms)
            .map(|filed| ProposedTask {
                issue: filed.address,
                number: Some(filed.number),
            })
            .ok_or_else(|| "the tracker does not show the task it was asked for".to_owned())
    }

    /// Give the task at `address` to the agent it is filed for, as the
    /// caller: its person, whose giving is what starts the work. It is
    /// taken from the agent first, because giving a task to the agent it
    /// is with already asks nothing new.
    ///
    /// # Errors
    /// The task is gone or no agent's, or the tracker refused.
    pub(super) async fn give(&self, address: &str) -> Result<(), String> {
        let filed = self.filed(address).await?;
        let assignee = filed
            .assignee
            .clone()
            .ok_or_else(|| "the task is no longer the assistant's in the tracker".to_owned())?;
        self.on(&filed, "assign", Map::new()).await?;
        let mut given = Map::new();
        given.insert("assignee".to_owned(), json!(assignee));
        self.on(&filed, "assign", given).await
    }

    /// Cancel the task at `address`, as the caller.
    ///
    /// # Errors
    /// The task is gone, or the tracker refused.
    pub(super) async fn cancel(&self, address: &str) -> Result<(), String> {
        let filed = self.filed(address).await?;
        let mut cancelled = Map::new();
        cancelled.insert("status".to_owned(), json!("cancelled"));
        self.on(&filed, "place", cancelled).await
    }
}
