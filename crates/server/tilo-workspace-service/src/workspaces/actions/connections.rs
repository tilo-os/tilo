//! Who a person's connections are answered to on this server, and what of
//! them an agent of theirs finds in its listing.
//!
//! The person, on a device of theirs, adds a connection, removes one and
//! reads them. An agent acting for them, with a device of theirs behind it,
//! reads the list, looks at an address and reads a connection's commands
//! again. It adds nothing and removes nothing, so no call of an agent's
//! gives it reach it did not have. Another member, their agent, a bot and an
//! app are told what a caller asking for something that does not exist is
//! told, before the request is read.
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

mod for_turn;

/// A person's connections, as their server publishes them.
pub(super) const CONNECTIONS: Publisher = Publisher {
    provider: person_connections::PROVIDER,
    kind: person_connections::KIND,
};

/// What an agent does with its person's connections. Each reads: none of
/// them adds a connection or changes what a service keeps.
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
        "The service's address, such as https://service.example/mcp. The server connects there and answers the name the service gives itself, whether it wants a sign-in, and its commands. Nothing is kept: only the person adds a connection.",
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

/// What an agent of a person's is offered of that person's connections.
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
    /// Whether `caller` is an agent acting for a person with a device of
    /// theirs behind it: whom that person's connections are listed for.
    pub(super) fn connects_as_agent((workspace_id, caller, certificate): Asked<'_>) -> bool {
        caller.acting().is_agent()
            && Self::asking_for_person(caller, certificate, workspace_id).is_ok()
    }

    /// What this server calls for its people's connections: addresses on the
    /// internet, and its own network too where its operator lets it call
    /// the addresses people name. Read anew for every request.
    fn connections_reach(&self) -> Reach {
        Reach::of(self.inner.models.private_addresses())
    }

    /// Answer what no other provider of the server's own answered: a request
    /// for a person's connections ([`person_connections::PROVIDER`]).
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
        // Unknown and forbidden are the same answer, given before the
        // request is read. `by` names the agent that asks for the person;
        // it is empty where they ask themselves.
        let (person, by) = match action {
            person_connections::ADD
            | person_connections::REMOVE
            | person_connections::USES
            | person_connections::BEGIN_SIGN_IN
            | person_connections::FINISH_SIGN_IN => (
                Self::asking_person(caller, certificate, workspace_id)?,
                String::new(),
            ),
            person_connections::LIST
            | person_connections::LOOK
            | person_connections::REFRESH
            | person_connections::CALL => {
                Self::asking_for_person(caller, certificate, workspace_id)?
            }
            _ => return Err(Refused::UnknownAction),
        };
        let calling = if caller.acting().is_person() {
            Calling::Person
        } else {
            Calling::Agent(&by)
        };
        let root = workspaces.person_settings_root(&person);
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
                said(connections::remove(&root, &named.connection))
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
                said(connections::finish_sign_in(&root, &reach, id, back).await)
            }
            person_connections::CALL => {
                let call: Call = argument(
                    request,
                    "a call names its connection, a command of it and the command's arguments",
                )?;
                said(connections::call(&root, &reach, calling, call).await)
            }
            _ => {
                let named: Named = argument(request, by_id)?;
                said(connections::refresh(&root, &reach, &named.connection, calling).await)
            }
        }
    }
}
