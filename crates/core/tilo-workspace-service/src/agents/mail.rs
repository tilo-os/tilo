//! What a turn on this server reaches of its person's mail.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources and nothing of a person's own mail. That mail is the server's own to answer,
//! through its entry for a turn it carries out
//! ([`crate::workspaces::Workspaces::mail_for_turn`]): the turn is one this server carries out, and its
//! person named the agent for the account. [`TurnMail`] is that entry as one turn's tools reach
//! it, a provider of the turn's own ([`TurnProvider`]): it lists the mail actions the turn is
//! offered beside the workspace's resources, and takes them. So an assistant lists what
//! arrived, reads a message with the files it carries by name and size, searches and keeps a
//! draft, with every device of its person off.
//!
//! A send asks, every time. The entry sets the message apart and refuses the call for want of
//! its person's yes, showing them whom a message of theirs goes to and what it says; the call
//! waits as its asked record, and nothing waits on a thread. No grant is offered. Their
//! approve admits the same call again, and the message leaves, once, and only while it still
//! shows exactly what they were shown.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeRequest,
    Pending, RemoteActionTarget, person_accounts,
};
use tilo_actions::{ActionOutput, InvocationUpdate};
use tilo_workspace_agent::Agent;

use super::turn_provider::{Ending, Rows};
use super::{Reply, ServerAgents as Agents, Turn, TurnProvider, WorkspaceId};
use crate::workspaces::hosted_use::{HostedTurn, MAIL_CONNECT};

/// What one turn reaches of its person's mail. A turn's caller changes when its person writes
/// again while it works, so each request is taken as the caller of the tool call that made it.
pub(super) struct TurnMail {
    agents: Agents,
    workspace: String,
    /// The workspace's id; `None` where the turn names none, which reaches nothing.
    workspace_id: Option<WorkspaceId>,
    agent: Agent,
    /// The mail actions this turn is offered: none for a turn the entry refuses.
    offered: Vec<CatalogueRow>,
}

impl TurnMail {
    /// What `turn` reaches of its person's mail, where `reply` holds what its tools begin as.
    pub(super) async fn for_turn(agents: &Agents, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = WorkspaceId::parse(&reply.workspace);
        let narrowed = turn.agent.bound_rules();
        let offered = if let Some(workspace) = workspace_id {
            let hosted = HostedTurn {
                workspace,
                caller: &reply.caller,
                bound: narrowed.as_deref(),
            };
            agents.workspaces.mail_offered(&hosted).await
        } else {
            Vec::new()
        };
        Arc::new(Self {
            agents: agents.clone(),
            workspace: reply.workspace.clone(),
            workspace_id,
            agent: turn.agent.clone(),
            offered,
        })
    }

    /// Take the mail request `request` as `caller`: how it ended, or the server's refusal.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let invocation = request.invocation;
        let Some(workspace) = self.workspace_id else {
            return Err(ActionCallError::UnknownAction);
        };
        let narrowed = self.agent.bound_rules();
        let hosted = HostedTurn {
            workspace,
            caller,
            bound: narrowed.as_deref(),
        };
        let value = self
            .agents
            .workspaces
            .mail_for_turn(&hosted, request)
            .await?;
        Ok(InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        })
    }
}

impl TurnProvider for TurnMail {
    /// The provider of a person's accounts, as a listing of everything names it beside the
    /// workspace's resources, where this turn is offered anything of mail.
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

    /// The mail rows, when `query` asks the provider of a person's accounts.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (query.provider.as_deref() == Some(person_accounts::PROVIDER) && !self.offered.is_empty())
            .then(|| Pending::ready(Ok(self.offered.clone())))
    }

    /// Whatever is asked of a person's mail accounts is the server's entry's to answer, also
    /// where it refuses it. A use of their vault by reference is another provider's, asked
    /// before this one, and signing an account in from its address alone is another's too
    /// ([`super::mail_connect`]).
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == person_accounts::PROVIDER
            && target.address.resource == person_accounts::RESOURCE
            && target.address.action != MAIL_CONNECT
    }

    /// What a message holds was written by whoever sent it: its words, what it is about, the
    /// name its sender goes by and the names of the files with it. Whatever answers messages
    /// (what arrived, a folder, one message, what a search found) carries them, and the turn's
    /// tools are told so with the answer. What this server says itself, of a message it began
    /// or of a question it put to its person, is no content from outside.
    fn carries_untrusted(&self, target: &RemoteActionTarget) -> bool {
        matches!(
            target.address.action.as_str(),
            person_accounts::ARRIVED
                | person_accounts::FETCH
                | person_accounts::READ
                | person_accounts::SEARCH
        )
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}
