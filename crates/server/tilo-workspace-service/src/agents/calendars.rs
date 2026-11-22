//! What a turn on this server reads of its person's calendars.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources and nothing of a person's own calendars. Those are the server's own to answer,
//! through its entry for a turn it carries out
//! ([`crate::workspaces::Workspaces::calendars_for_turn`]): the turn is one this server carries out, and
//! its person named the agent for the calendar account. [`TurnCalendars`] is that entry as one
//! turn's tools reach it, a provider of the turn's own ([`TurnProvider`]): it lists the reads
//! the turn is offered beside the workspace's resources, and takes them. So an assistant reads
//! what its person's calendars hold, and when they are free, with every device of its person
//! off. It reads only: nothing here writes to a calendar server, and what would is answered
//! as an action nobody knows.
//!
//! What an entry says was written by whoever made it, often somebody else. A read of entries
//! is answered to the turn's tools as content from outside
//! ([`TurnProvider::carries_untrusted`]), so its model is told that it is data to reason
//! about and never an instruction to follow.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeRequest,
    Pending, RemoteActionTarget, person_accounts,
};
use tilo_actions::{ActionOutput, InvocationUpdate};
use tilo_workspace_agent::Agent;

use super::turn_provider::{Ending, Rows};
use super::{Reply, Turn, TurnProvider, WorkspaceId};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{CALENDAR_CONNECT, CALENDAR_ENTRIES, CALENDARS, HostedTurn};

/// What one turn reads of its person's calendars. A turn's caller changes when its person
/// writes again while it works, so each request is taken as the caller of the tool call that
/// made it.
pub(super) struct TurnCalendars {
    workspaces: Workspaces,
    workspace: String,
    /// The workspace's id; `None` where the turn names none, which reaches nothing.
    workspace_id: Option<WorkspaceId>,
    agent: Agent,
    /// The reads this turn is offered: none for a turn the entry refuses.
    offered: Vec<CatalogueRow>,
}

impl TurnCalendars {
    /// What `turn` reads of its person's calendars, where `reply` holds what its tools begin
    /// as.
    pub(super) async fn for_turn(workspaces: &Workspaces, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = WorkspaceId::parse(&reply.workspace);
        let narrowed = turn.agent.bound_rules();
        let offered = if let Some(workspace) = workspace_id {
            let hosted = HostedTurn {
                workspace,
                caller: &reply.caller,
                bound: narrowed.as_deref(),
            };
            workspaces.calendars_offered(&hosted).await
        } else {
            Vec::new()
        };
        Arc::new(Self {
            workspaces: workspaces.clone(),
            workspace: reply.workspace.clone(),
            workspace_id,
            agent: turn.agent.clone(),
            offered,
        })
    }

    /// Take the read `request` asks as `caller`: what it answers, or the server's refusal.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let Some(workspace) = self.workspace_id else {
            return Err(ActionCallError::UnknownAction);
        };
        let narrowed = self.agent.bound_rules();
        let hosted = HostedTurn {
            workspace,
            caller,
            bound: narrowed.as_deref(),
        };
        let value = self.workspaces.calendars_for_turn(&hosted, request).await?;
        Ok(InvocationUpdate::Succeeded {
            invocation: request.invocation,
            output: ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        })
    }
}

impl TurnProvider for TurnCalendars {
    /// The provider of a person's accounts, as a listing of everything names it beside the
    /// workspace's resources, where this turn is offered anything of their calendars.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        if self.offered.is_empty() {
            return Vec::new();
        }
        vec![CatalogueProvider {
            provider: person_accounts::PROVIDER.to_owned(),
            app_id: person_accounts::PROVIDER.to_owned(),
            workspace: self.workspace.clone(),
            document_id: String::new(),
            browsable: true,
        }]
    }

    /// The calendars' rows, when `query` asks the provider of a person's accounts.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (query.provider.as_deref() == Some(person_accounts::PROVIDER) && !self.offered.is_empty())
            .then(|| Pending::ready(Ok(self.offered.clone())))
    }

    /// Whatever is asked of a person's calendars is the server's entry's to answer, also where
    /// it refuses it: a turn's request to change one reaches nothing else. Connecting one its
    /// person tells it of is another provider's ([`super::calendar_connect`]).
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == person_accounts::PROVIDER
            && target.address.resource == CALENDARS
            && target.address.action != CALENDAR_CONNECT
    }

    /// What the calendars' entries say is whoever made them's to have written.
    fn carries_untrusted(&self, target: &RemoteActionTarget) -> bool {
        target.address.action == CALENDAR_ENTRIES
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}
