//! Who a workspace's connections are answered to on this server, and what
//! of them an agent finds in its listing.
//!
//! The services a workspace connected are its own, reached as the
//! workspace's policy says ([`Store::CONNECTIONS`]): a member adds one,
//! removes one and reads them as their role lets them, an agent or a key as
//! its grants do. A command that changes something asks the agent's person
//! first unless a grant says otherwise. A caller the workspace does not have
//! is told what a caller asking for something that does not exist is told,
//! before the request is read.
//!
//! An agent whose turn this server carries out itself, with no device of its
//! person's on, is answered by an entry of its own ([`for_turn`]): there a
//! command that changes something asks its person, and a connection is
//! proposed to them.

use serde_json::{Map, Value};
use tilo_actions::ActionKind;
use tilo_actions::remote::{
    ActionCallError as Refused, CatalogueRow, InvokeArgument, InvokeRequest,
};
use tilo_resource::{Effect as _, EffectDecl};

use super::{Asked, DeclaredRow as _, Publisher};
use crate::connections::{self, Calling, Reach};
use crate::person_connections::{self, ConnectionsAction};
use crate::workspaces::Workspaces;
use crate::workspaces::workspace_stores::Store;

mod for_turn;

/// A workspace's connections, as its server publishes them.
pub(super) const CONNECTIONS: Publisher = Publisher {
    provider: person_connections::PROVIDER,
    kind: person_connections::KIND,
};

/// Whether an agent is offered the action `id` of the workspace's connections in its listing:
/// the reads an agent does, and a proposal its person answers. Adding, removing, signing in and
/// what a connection was used for are the person's own.
pub(super) fn offered_to_agents(id: &str) -> bool {
    matches!(
        id,
        person_connections::LIST
            | person_connections::LOOK
            | person_connections::REFRESH
            | person_connections::CALL
    )
}

/// What an agent is offered of the workspace's connections.
pub(super) fn agent_rows() -> Vec<CatalogueRow> {
    ConnectionsAction::declarations()
        .iter()
        .filter(|decl| offered_to_agents(decl.id))
        .map(|decl| decl.row(CONNECTIONS, person_connections::RESOURCE, "Connections"))
        .collect()
}

/// Whether this server answers the declared action `decl` for the person: every connection
/// action but a proposal, which only an agent's turn makes to its person.
fn answered(decl: &EffectDecl) -> bool {
    decl.id != person_connections::PROPOSE
}

/// The action `request` asks, its arguments read as the declaration names them.
fn asked_of(request: &InvokeRequest) -> Result<ConnectionsAction, Refused> {
    let arguments = match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        _ => Map::new(),
    };
    ConnectionsAction::decode(&request.target.address.action, &arguments).map_err(|error| {
        match error {
            tilo_resource::EffectDecodeError::Unknown(_) => Refused::UnknownAction,
            tilo_resource::EffectDecodeError::Argument { name, .. } => {
                Refused::invalid_request(format!("The `{name}` argument is not readable"))
            }
            other => Refused::invalid_request(other.to_string()),
        }
    })
}

/// The call the declaration's `arguments` make of `command` of `connection`, or the refusal of
/// arguments that are no object.
fn call_made(
    connection: String,
    command: String,
    arguments: Option<Value>,
) -> Result<person_connections::Call, Refused> {
    let arguments = match arguments {
        None => Map::new(),
        Some(Value::Object(arguments)) => arguments,
        Some(_) => {
            return Err(Refused::invalid_request(
                "The `arguments` argument is not readable",
            ));
        }
    };
    Ok(person_connections::Call {
        connection,
        command,
        arguments,
    })
}

/// What the caller reads of an outcome, or the reason it was refused.
fn said<T: serde::Serialize>(outcome: Result<T, String>) -> Result<Value, Refused> {
    let refused = |reason: String| Refused::Refused { reason };
    serde_json::to_value(outcome.map_err(refused)?).map_err(|error| refused(error.to_string()))
}

impl Workspaces {
    /// Whether `caller` is an agent the workspace's policy lets read its
    /// connections: whom they are listed for.
    pub(super) async fn connects_as_agent(&self, asked: Asked<'_>) -> bool {
        asked.1.acting().is_agent()
            && self
                .reaching(
                    asked,
                    Store::CONNECTIONS,
                    person_connections::LIST,
                    ActionKind::Read,
                )
                .await
                .is_ok()
    }

    /// What this server calls for its workspaces' connections: addresses on the
    /// internet, and its own network too where its operator lets it call
    /// the addresses people name. Read anew for every request.
    fn connections_reach(&self) -> Reach {
        Reach::of(self.inner.models.private_addresses())
    }

    /// Answer what no other provider of the server's own answered: a request
    /// for a workspace's connections ([`person_connections::PROVIDER`]).
    /// Anything else is unknown.
    pub(super) async fn connections_invoke(
        workspaces: &Self,
        (workspace_id, caller, certificate): Asked<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, Refused> {
        let target = &request.target;
        if target.provider != person_connections::PROVIDER
            || target.address.resource != person_connections::RESOURCE
        {
            return Err(Refused::UnknownAction);
        }
        let action = target.address.action.as_str();
        // Decided before the request is read. A command the service marks as only reading is
        // called as one; any other asks the agent's person first ([`connections::call`]).
        let Some(decl) = ConnectionsAction::declaration(action).filter(answered) else {
            return Err(Refused::UnknownAction);
        };
        let reach = workspaces
            .reaching_allowed(
                (workspace_id, caller, certificate),
                Store::CONNECTIONS,
                action,
                decl.action_kind,
                request,
            )
            .await?;
        let by = reach.by();
        let calling = if reach.agent.is_none() {
            Calling::Person
        } else {
            Calling::Agent(&by)
        };
        let workspace = reach.workspace.to_string();
        let root = workspaces.workspace_root(&workspace);
        let vault = workspaces
            .vault_for(&workspace, &reach.acting)
            .serving(request);
        let reach = workspaces.connections_reach();
        let asked = asked_of(request)?;
        match asked {
            ConnectionsAction::List => said(connections::list(&root)),
            ConnectionsAction::Look { address } => {
                said(connections::look(&root, &reach, &address).await)
            }
            ConnectionsAction::Add { address } => {
                said(connections::add(&root, &reach, &address).await)
            }
            ConnectionsAction::Remove { connection } => {
                said(connections::remove((&root, vault), &connection).await)
            }
            ConnectionsAction::Uses { connection } => said(connections::uses(&root, &connection)),
            ConnectionsAction::BeginSignIn {
                connection,
                return_to,
            } => said(connections::begin_sign_in(&root, &reach, &connection, &return_to).await),
            ConnectionsAction::FinishSignIn { sign_in } => said(
                connections::finish_sign_in(
                    (&root, vault),
                    &reach,
                    &sign_in.connection,
                    &sign_in.returned,
                )
                .await,
            ),
            ConnectionsAction::Call {
                connection,
                command,
                call_arguments,
            } => {
                let call = call_made(connection, command, call_arguments)?;
                said(connections::call((&root, vault), &reach, calling, call).await)
            }
            ConnectionsAction::Refresh { connection } => {
                said(connections::refresh((&root, vault), &reach, &connection, calling).await)
            }
            // A proposal is an agent's turn's, answered by [`for_turn`].
            ConnectionsAction::Propose { .. } => Err(Refused::UnknownAction),
        }
    }
}
