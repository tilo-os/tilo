//! A calendar or a mail account its person tells their assistant of, connected on this server.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources and nothing of a person's own accounts. Connecting one is the server's own to
//! answer, through its entries for a turn it carries out
//! ([`crate::workspaces::Workspaces::connect_calendar_for_turn`],
//! [`crate::workspaces::Workspaces::connect_mail_for_turn`]). [`TurnConnects`] is those entries
//! as one turn's tools reach them, a provider of the turn's own ([`TurnProvider`]): it lists the
//! actions beside the workspace's resources, and takes them. Each takes what the person said (a
//! calendar server's address or a mail address, the user name there, a name for the account)
//! and nothing else.
//!
//! The password is its person's to type where no agent reads it. So the call waits for their
//! yes, and the asking view opens the vault's own add for them, in the system's protected
//! password field. Their approve admits the call again, and the entry signs in and keeps the
//! account in the workspace: the call answers what was connected, or the server's refusal.
//! Neither the password nor the item's reference passes a model on this way.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeRequest,
    Pending, RemoteActionTarget, person_accounts,
};
use tilo_actions::{ActionOutput, InvocationUpdate};
use tilo_workspace_agent::Agent;

use super::turn_provider::{Ending, Rows};
use super::{Reply, ServerAgents, Turn, TurnProvider, WorkspaceId};
use crate::workspaces::hosted_use::{CALENDAR_CONNECT, CALENDARS, HostedTurn, MAIL_CONNECT};

/// What one turn connects of its person's calendars and mail. A turn's caller changes when its person
/// writes again while it works, so each telling is taken as the caller of the tool call that
/// made it.
pub(super) struct TurnConnects {
    agents: ServerAgents,
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    /// The connecting this turn is offered: none for a turn the entries refuse.
    offered: Vec<CatalogueRow>,
}

impl TurnConnects {
    /// What `turn` connects of its person's accounts, where `reply` holds what its tools begin
    /// as and act through.
    pub(super) async fn for_turn(agents: &ServerAgents, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = reply.provider.workspace;
        let narrowed = turn.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &reply.caller,
            bound: narrowed.as_deref(),
        };
        let mut offered = agents.workspaces.calendar_connect_offered(&hosted).await;
        offered.extend(agents.workspaces.mail_connect_offered(&hosted).await);
        Arc::new(Self {
            agents: agents.clone(),
            workspace: reply.workspace.clone(),
            workspace_id,
            agent: turn.agent.clone(),
            offered,
        })
    }

    /// Take the telling `request` makes as `caller`: the account it connected, or the
    /// server's refusal, which waits for its person where it asks for their password.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let narrowed = self.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
            bound: narrowed.as_deref(),
        };
        let workspaces = &self.agents.workspaces;
        let connected = if request.target.address.action == MAIL_CONNECT {
            serde_json::to_value(workspaces.connect_mail_for_turn(&hosted, request).await?)
        } else {
            serde_json::to_value(
                workspaces
                    .connect_calendar_for_turn(&hosted, request)
                    .await?,
            )
        };
        Ok(InvocationUpdate::Succeeded {
            invocation: request.invocation,
            output: ActionOutput {
                value: connected.ok(),
                resources: Vec::new(),
            },
        })
    }
}

impl TurnProvider for TurnConnects {
    /// The provider of a person's accounts, as a listing of everything names it beside the
    /// workspace's resources, where this turn is offered the connecting.
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

    /// The connecting's row, when `query` asks the provider of a person's accounts.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (query.provider.as_deref() == Some(person_accounts::PROVIDER) && !self.offered.is_empty())
            .then(|| Pending::ready(Ok(self.offered.clone())))
    }

    /// The connecting of a calendar or a mail account is the server's entries' to answer, also
    /// for a turn it is not offered to, which the entry then refuses.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        let address = &target.address;
        target.provider == person_accounts::PROVIDER
            && ((address.resource == CALENDARS && address.action == CALENDAR_CONNECT)
                || (address.resource == person_accounts::RESOURCE
                    && address.action == MAIL_CONNECT))
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}
