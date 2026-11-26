//! What a turn reaches of what its workspace keeps for the person who asked it: their vault's
//! uses by reference and the ask for a secret, the workspace's mail and calendars and the
//! connecting of them, and the services they connected.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources and nothing of these. They are the device's own to answer: the turn asks them of
//! the device's dispatch, signed with its agent's key, as every other call, and the dispatch
//! decides them for the agent on that turn
//! ([`tilo_workspace_service::workspaces::Workspaces::answer_turn`]). What the turn is offered is what the
//! device would not refuse it, read anew at every listing
//! ([`tilo_workspace_service::workspaces::Workspaces::turn_rows`]). [`TurnKept`] is that as one turn's tools reach
//! it, a provider of the turn's own ([`TurnProvider`]).
//!
//! What a mail, a calendar entry or a connected service says was written by somebody else.
//! Whatever answers with it is told to the turn's tools as content from outside
//! ([`TurnProvider::carries_untrusted`]): data to reason about, never an instruction.

use std::sync::Arc;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    ActionCallError, CONNECTION_SAID_ACTIONS, CONNECTIONS_PROVIDER, Caller, CatalogueProvider,
    CatalogueQuery, InvokeRequest, RemoteActionTarget,
};

use super::turn_provider::{Ending, Rows};
use super::{Reply, ServerAgents, TurnProvider, WorkspaceId};
use tilo_workspace_service::workspaces::hosted_use::{self, TURN_PROVIDERS};

/// What one turn reaches of what its workspace keeps for the person who asked it. A turn's
/// caller changes when its person writes again while it works, so each call and each listing
/// is taken as the caller of the tool call that made it.
pub(super) struct TurnKept {
    agents: ServerAgents,
    workspace: String,
    workspace_id: WorkspaceId,
    /// The providers the turn was offered anything of when it began: what a listing of
    /// everything names.
    named: Vec<&'static str>,
    /// Where a listing's rows are read: a tool may list from outside any task.
    runtime: tokio::runtime::Handle,
}

impl TurnKept {
    /// What the turn `reply` begins reaches of what its workspace keeps.
    pub(super) async fn for_turn(agents: &ServerAgents, reply: &Reply) -> Arc<Self> {
        let mut named = Vec::new();
        for provider in TURN_PROVIDERS {
            if !agents
                .workspaces
                .turn_rows(&reply.caller, provider)
                .await
                .is_empty()
            {
                named.push(provider);
            }
        }
        Arc::new(Self {
            agents: agents.clone(),
            workspace: reply.workspace.clone(),
            workspace_id: reply.provider.workspace,
            named,
            runtime: tokio::runtime::Handle::current(),
        })
    }
}

impl TurnProvider for TurnKept {
    /// The providers the turn is offered anything of, as a listing of everything names them
    /// beside the workspace's resources.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        self.named
            .iter()
            .map(|provider| CatalogueProvider {
                provider: (*provider).to_owned(),
                app_id: (*provider).to_owned(),
                workspace: self.workspace.clone(),
                document_id: String::new(),
                browsable: true,
            })
            .collect()
    }

    /// The rows `query` asks for, when it asks one of those providers: what the device offers
    /// `caller`'s turn now.
    fn rows(&self, caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        let provider = query.provider.clone()?;
        if !TURN_PROVIDERS.contains(&provider.as_str()) {
            return None;
        }
        let (completer, rows) = tilo_actions::remote::pending();
        let (workspaces, caller) = (self.agents.workspaces.clone(), caller.clone());
        self.runtime.spawn(async move {
            completer.complete(Ok(workspaces.turn_rows(&caller, &provider).await));
        });
        Some(rows)
    }

    /// Whatever a turn asks of these is the device's to answer, also where it refuses it.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        hosted_use::takes(target)
    }

    /// What a connected service answers or says of itself was written by that service.
    fn carries_untrusted(&self, target: &RemoteActionTarget) -> bool {
        target.provider == CONNECTIONS_PROVIDER
            && CONNECTION_SAID_ACTIONS.contains(&target.address.action.as_str())
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}

impl TurnKept {
    /// Ask the device's dispatch for `request` as `caller`: how it ended, or its refusal.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        self.agents
            .ask_device(self.workspace_id, caller, request)
            .await
    }
}
