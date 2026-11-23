//! What a turn on this server reaches of the services its person connected.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources and nothing of a person's own connections. Those are the server's own to answer,
//! through its entry for a turn it carries out
//! ([`crate::workspaces::Workspaces::connections_for_turn`]): the turn is one this server carries out, and
//! an agent that anything narrows reaches only a connection a rule of its own names.
//! [`TurnConnections`] is that entry as one turn's tools reach it, a provider of the turn's own
//! ([`TurnProvider`]): at every listing it names what an agent does with its person's
//! connections and each command of each connection, with whether it asks first, and it takes
//! their calls. So an assistant lists its person's services and calls a command that only
//! reads, with every device of its person off.
//!
//! A command that changes something waits for its person's yes, unless a grant of the
//! workspace's lets this agent call exactly that command of that connection: the entry
//! refuses it for want of their yes, and the call waits as its asked record until their
//! approve admits it again. A connection an agent proposes waits the same way, and is added
//! on their approve.

use std::future::Future;
use std::sync::Arc;

use serde_json::Value;
use tilo_actions::remote::person_connections;
use tilo_actions::remote::{
    Caller, CatalogueProvider, CatalogueQuery, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_workspace_agent::Agent;

use super::turn_provider::{Ending, Rows};
use super::{Reply, Turn, TurnProvider, WorkspaceId};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::HostedTurn;

/// What one turn reaches of its person's connections. A turn's caller changes when its person
/// writes again while it works, so each request is taken as the caller of the tool call that
/// made it.
/// How a call a turn's provider took ends: what the turn's tools are told, or the server's
/// refusal.
type Ended = <Ending as Future>::Output;

pub(super) struct TurnConnections {
    workspaces: Workspaces,
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    /// Whether the turn is one this server carries out: no other turn is told that a person's
    /// connections exist.
    theirs: bool,
    /// Where a listing's rows are read: a tool may list from outside any task.
    runtime: tokio::runtime::Handle,
}

/// What `turn` reaches of its person's connections in the workspace `workspace_id`, where
/// `reply` holds what its tools begin as.
pub(super) async fn of(
    workspaces: Workspaces,
    turn: &Turn,
    reply: &Reply,
    workspace_id: WorkspaceId,
) -> Arc<TurnConnections> {
    let narrowed = turn.agent.bound_rules();
    let hosted = HostedTurn {
        workspace: workspace_id,
        caller: &reply.caller,
        bound: narrowed.as_deref(),
    };
    let theirs = workspaces.asked_turn(&hosted).await.is_ok();
    Arc::new(TurnConnections {
        workspaces,
        workspace: reply.workspace.clone(),
        workspace_id,
        agent: turn.agent.clone(),
        theirs,
        runtime: tokio::runtime::Handle::current(),
    })
}

impl TurnConnections {
    /// Take the request `request` as `caller`: how it ended, or the server's refusal.
    async fn take(&self, caller: &Caller, request: &InvokeRequest) -> Ended {
        let invocation = request.invocation;
        let workspace = self.workspace_id;
        let narrowed = self.agent.bound_rules();
        let hosted = HostedTurn {
            workspace,
            caller,
            bound: narrowed.as_deref(),
        };
        let value = self
            .workspaces
            .connections_for_turn(&hosted, request)
            .await?;
        Ok(succeeded(invocation, value))
    }
}

impl TurnProvider for TurnConnections {
    /// The provider of a person's connections, as a listing of everything names it beside the
    /// workspace's resources, for a turn that is one this server carries out.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        if !self.theirs {
            return Vec::new();
        }
        vec![CatalogueProvider {
            provider: person_connections::PROVIDER.to_owned(),
            app_id: person_connections::PROVIDER.to_owned(),
            workspace: self.workspace.clone(),
            document_id: String::new(),
            browsable: true,
        }]
    }

    /// The rows of a person's connections, when `query` asks their provider: read anew at
    /// every listing, so a connection its person added a moment ago is in it.
    fn rows(&self, caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        if !self.theirs || query.provider.as_deref() != Some(person_connections::PROVIDER) {
            return None;
        }
        let workspace = self.workspace_id;
        let (completer, rows) = tilo_actions::remote::pending();
        let workspaces = self.workspaces.clone();
        let (caller, agent) = (caller.clone(), self.agent.clone());
        self.runtime.spawn(async move {
            let narrowed = agent.bound_rules();
            let hosted = HostedTurn {
                workspace,
                caller: &caller,
                bound: narrowed.as_deref(),
            };
            completer.complete(Ok(workspaces.connection_rows_for_turn(&hosted).await));
        });
        Some(rows)
    }

    /// Whatever is asked of a person's connections is the server's entry's to answer, also
    /// where it refuses it.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == person_connections::PROVIDER
    }

    /// What a service its person connected says was written by that service: what a command
    /// of it answers, and what it says of itself and of its commands where a turn lists its
    /// person's connections, looks at an address or reads a connection's commands again. The
    /// turn's tools are told so with the answer.
    fn carries_untrusted(&self, target: &RemoteActionTarget) -> bool {
        matches!(
            target.address.action.as_str(),
            person_connections::CALL
                | person_connections::LIST
                | person_connections::LOOK
                | person_connections::REFRESH
        )
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}

fn succeeded(invocation: InvocationId, value: Value) -> InvocationUpdate {
    InvocationUpdate::Succeeded {
        invocation,
        output: ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        },
    }
}
