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

use serde_json::{Map, Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::workspace_initiative::{Proposal, ProposedTask};
use tilo_actions::remote::{Caller, InvokeArgument};
use tilo_agent_session::workspace_actions::WorkspaceActions;
use tilo_peer_state::document_frame::DocumentRoute;

use super::proposals::InTracker;
use crate::agents::{
    WorkspaceActionProvider, invocation_of, invoke_named, invoke_request, target_named,
};
use crate::workspaces::Workspaces;
use tilo_server_protocol::WorkspaceId;

/// The app that keeps the workspace's tasks.
const TASKS: &str = "so.tilo.tasks";
const ASSISTANT_SOURCE: &str = "Tilo Assistant";

/// One issue of the tracker, as far as a proposal reads it.
pub(super) struct Filed {
    /// The document of the part it is kept in.
    document: String,
    id: String,
    /// Its address, whichever part keeps it.
    address: String,
    number: u64,
    /// The proposal this issue came from, paired with its authenticated Tasks author.
    proposal: Option<(String, String)>,
    proposing_agent: Option<String>,
    given_by: Option<String>,
    /// What it says about a proposal that stands as it.
    stands: InTracker,
}

impl Filed {
    fn of(document: &str, id: &tilo_resource::Id, issue: &tasks_model::Issue) -> Self {
        use tasks_model::Status;
        let given_by = issue
            .assigned_by
            .as_ref()
            .and_then(tilo_resource::MemberId::person_id)
            .filter(|_| issue.assigned_ms > 0)
            .map(str::to_owned);
        let stands = match issue.status {
            Status::Cancelled => InTracker::Dropped,
            Status::InProgress | Status::InReview | Status::Done => InTracker::Given,
            Status::Backlog | Status::Todo if given_by.is_some() => InTracker::Given,
            Status::Backlog | Status::Todo => InTracker::Waits,
        };
        Self {
            document: document.to_owned(),
            id: id.as_str().to_owned(),
            address: tasks_model::issue_resource(id),
            number: issue.number,
            proposal: issue.origin.as_ref().and_then(|origin| {
                if origin.source != ASSISTANT_SOURCE {
                    return None;
                }
                let author = issue.author.as_ref()?.agent_id()?;
                Some((origin.reference.clone()?, author.to_owned()))
            }),
            proposing_agent: issue
                .author
                .as_ref()
                .filter(|author| issue.assigned_member().as_ref() == Some(*author))
                .and_then(tilo_resource::MemberId::agent_id)
                .map(str::to_owned),
            given_by,
            stands,
        }
    }

    fn task(self) -> ProposedTask {
        ProposedTask {
            issue: self.address,
            number: Some(self.number),
        }
    }
}

/// Every issue the tracker of `workspace` keeps, part by part.
///
/// # Errors
/// The workspace is not governed here, or a part of its tracker cannot be
/// read: what is not read is never taken for gone.
pub(super) async fn issues(
    workspaces: &Workspaces,
    workspace: &str,
    reader: (&[u8; 32], Option<&Caller>),
) -> Result<Vec<Filed>, String> {
    let routes = workspaces
        .workspace_document_reads(workspace, (reader.0, reader.1.into()))
        .await?;
    let mut filed = Vec::new();
    let mut read = Vec::new();
    for ((app, document), allowed) in routes {
        let part = app == TASKS
            && collab_types::document::DocumentId::parse(&document)
                .and_then(|id| id.chain_part())
                .is_some_and(|(chain, _)| chain == tasks::issue_list::chain());
        if !part {
            continue;
        }
        if !allowed {
            return Err("a tracker part cannot be read by this person".into());
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
        read.push((app, document.clone()));
        filed.extend(
            part.issues
                .iter()
                .map(|(id, issue)| Filed::of(&document, id, issue)),
        );
    }
    let current = workspaces
        .workspace_document_reads(workspace, (reader.0, reader.1.into()))
        .await?;
    if read.iter().any(|route| current.get(route) != Some(&true)) {
        return Err("access to the tracker changed while reading it".into());
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
    pub actions: WorkspaceActions,
    pub workspace_id: WorkspaceId,
    pub workspace: String,
    pub caller: Caller,
    pub person: [u8; 32],
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
        issues(
            &self.workspaces,
            &self.workspace,
            (&self.person, Some(&self.caller)),
        )
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

    /// File the proposal once as its assistant, using the issue address from Tasks' receipt.
    ///
    /// # Errors
    /// Tasks refused the create or did not identify the issue it created.
    pub(super) async fn file(
        &self,
        assistant: &str,
        proposal: &Proposal,
    ) -> Result<ProposedTask, String> {
        // The host's action receipt can expire after Tasks commits and before the proposal
        // keeps its link. The immutable origin and the signed author survive in Tasks.
        let filed = issues(
            &self.workspaces,
            &self.workspace,
            (&self.person, Some(&self.caller)),
        )
        .await?;
        let mut prior = filed.into_iter().filter(|issue| {
            issue
                .proposal
                .as_ref()
                .is_some_and(|(reference, author)| reference == &proposal.id && author == assistant)
        });
        if let Some(existing) = prior.next() {
            if prior.next().is_some() {
                return Err("more than one task refers to this proposal".into());
            }
            return Ok(existing.task());
        }
        let assignee = tasks_model::agent_assignee(assistant).0;
        let tracker =
            crate::agents::workspace_resource(&tilo_agent_session::AppCatalogue::native(), TASKS);
        let resource = collab_types::document::chain_resource_id(&tasks::issue_list::chain());
        let args = json!({
            "title": proposal.title,
            "body": proposal.why,
            "assignee": assignee,
            "origin": {
                "source": ASSISTANT_SOURCE,
                "reference": proposal.id,
                "author": assistant,
                "created_ms": proposal.proposed_at_ms,
            },
        });
        let mut request = invocation_of(
            target_named(&self.route(&tracker), &resource, "create"),
            InvokeArgument::Json(args.as_object().cloned().unwrap_or_default()),
        );
        request.invocation = InvocationId(uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("tilo.initiative/proposals/{}/task", proposal.id).as_bytes(),
        ));
        let answer = invoke_request(&self.service(), &self.caller, request).await?;
        let [made] = answer["value"]["made"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        else {
            return Err("Tasks did not name the issue it created".into());
        };
        let id = made["id"]
            .as_str()
            .ok_or("Tasks did not name the issue it created")?;
        let address = tasks_model::issue_resource(&tilo_resource::Id::from_raw(id));
        if made["address"].as_str() != Some(address.as_str()) {
            return Err("Tasks returned a different issue address".into());
        }
        Ok(self.filed(&address).await?.task())
    }

    /// Give the task at `address` to the agent it is filed for, as the
    /// caller. The native assignment records the first person who gave it to the proposing agent.
    ///
    /// # Errors
    /// The task is gone or no agent's, or the tracker refused.
    pub(super) async fn give(&self, address: &str) -> Result<(), String> {
        let filed = self.filed(address).await?;
        let agent = filed.proposing_agent.as_deref().ok_or_else(|| {
            "the task is no longer assigned to the agent that proposed it".to_owned()
        })?;
        let person = hex::encode(self.person);
        if filed.stands == InTracker::Dropped {
            return Err("the task was cancelled".into());
        }
        if let Some(giver) = &filed.given_by {
            return if giver == &person {
                Ok(())
            } else {
                Err("another person already gave the task".into())
            };
        }
        let mut given = Map::new();
        given.insert(
            "assignee".to_owned(),
            json!(tasks_model::agent_assignee(agent).0),
        );
        self.on(&filed, "assign", given).await?;
        if self.filed(address).await?.given_by.as_deref() != Some(person.as_str()) {
            return Err("Tasks did not record the proposal's person as its giver".into());
        }
        Ok(())
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
