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

use serde_json::Value;
use tilo_actions::remote::person_connections::{
    self, AtAddress, BeginSignIn, Call, FinishSignIn, Named,
};
use tilo_actions::remote::{
    ActionCallError as Refused, CatalogueRow, InvokeArgument, InvokeRequest,
};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::{Asked, Publisher, Verb};
use crate::connections::{self, Calling, Reach};
use crate::workspaces::Workspaces;
use crate::workspaces::workspace_stores::Store;

mod for_turn;

/// A workspace's connections, as its server publishes them.
pub(super) const CONNECTIONS: Publisher = Publisher {
    provider: person_connections::PROVIDER,
    kind: person_connections::KIND,
};

/// What an agent is offered of the workspace's connections. Each reads:
/// none of them adds a connection or changes what a service keeps.
const AGENT_VERBS: [Verb; 4] = [
    reads(person_connections::LIST, "List connections", Vec::new),
    reads(
        person_connections::LOOK,
        "Look at a service",
        address_params,
    ),
    reads(
        person_connections::REFRESH,
        "Read a connection's commands again",
        connection_params,
    ),
    reads(person_connections::CALL, "Call a command", call_params),
];

/// The action `id` as one that reads, offered by who is asking and by no
/// governance action of a workspace.
const fn reads(id: &'static str, label: &'static str, params: fn() -> Vec<ActionParam>) -> Verb {
    Verb {
        id,
        label,
        check: "",
        kind: ActionKind::Read,
        params,
    }
}

fn address_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "address",
        ParamType::Text,
        "The service's address, such as https://service.example/mcp. The server connects there and answers the name the service gives itself, whether it wants a sign-in, and its commands. Nothing is kept.",
    )]
}

fn connection_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "connection",
        ParamType::Text,
        "The connection, by its id.",
    )]
}

fn call_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("connection", ParamType::Text, "The connection, by its id."),
        ActionParam::required(
            "command",
            ParamType::Text,
            "The command's name, as the connection lists it. One the service marks as only reading is called; any other asks your person first.",
        ),
        ActionParam::optional(
            "arguments",
            ParamType::Object,
            "The command's arguments, as its schema describes them. They are sent to the service as given.",
        ),
    ]
}

/// What an agent is offered of the workspace's connections.
pub(super) fn agent_rows() -> Vec<CatalogueRow> {
    AGENT_VERBS
        .iter()
        .map(|verb| verb.row(CONNECTIONS, person_connections::RESOURCE, "Connections"))
        .collect()
}

/// What a request's arguments name, or `what` they should have named. The
/// reason never quotes the arguments.
fn argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, Refused> {
    let invalid = || Refused::InvalidRequest {
        reason: what.to_owned(),
    };
    let InvokeArgument::Json(argument) = &request.argument else {
        return Err(invalid());
    };
    serde_json::from_value(Value::Object(argument.clone())).map_err(|_| invalid())
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
        let effect = match action {
            person_connections::LIST
            | person_connections::LOOK
            | person_connections::REFRESH
            | person_connections::USES
            | person_connections::CALL => ActionKind::Read,
            person_connections::ADD
            | person_connections::REMOVE
            | person_connections::BEGIN_SIGN_IN
            | person_connections::FINISH_SIGN_IN => ActionKind::Undoable,
            _ => return Err(Refused::UnknownAction),
        };
        let reach = workspaces
            .reaching_allowed(
                (workspace_id, caller, certificate),
                Store::CONNECTIONS,
                action,
                effect,
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
        let vault = workspaces.vault_at(&workspace);
        let reach = workspaces.connections_reach();
        let by_address = "a service is named by its address";
        let by_id = "a connection is named by its id";
        match action {
            person_connections::LIST => said(connections::list(&root)),
            person_connections::LOOK => {
                let at: AtAddress = argument(request, by_address)?;
                said(connections::look(&root, &reach, &at.address).await)
            }
            person_connections::ADD => {
                let at: AtAddress = argument(request, by_address)?;
                said(connections::add(&root, &reach, &at.address).await)
            }
            person_connections::REMOVE => {
                let named: Named = argument(request, by_id)?;
                said(connections::remove((&root, vault), &named.connection).await)
            }
            person_connections::USES => {
                let named: Named = argument(request, by_id)?;
                said(connections::uses(&root, &named.connection))
            }
            person_connections::BEGIN_SIGN_IN => {
                let begin: BeginSignIn = argument(
                    request,
                    "a sign-in to begin names its connection and where the browser comes back to",
                )?;
                let (id, back) = (&begin.connection, &begin.return_to);
                said(connections::begin_sign_in(&root, &reach, id, back).await)
            }
            person_connections::FINISH_SIGN_IN => {
                let finish: FinishSignIn = argument(
                    request,
                    "a sign-in to finish names its connection and the address the browser came back to",
                )?;
                let (id, back) = (&finish.connection, &finish.returned);
                said(connections::finish_sign_in((&root, vault), &reach, id, back).await)
            }
            person_connections::CALL => {
                let call: Call = argument(
                    request,
                    "a call names its connection, a command of it and the command's arguments",
                )?;
                said(connections::call((&root, vault), &reach, calling, call).await)
            }
            _ => {
                let named: Named = argument(request, by_id)?;
                said(connections::refresh((&root, vault), &reach, &named.connection, calling).await)
            }
        }
    }
}
