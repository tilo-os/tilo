//! The services a person connected by their address, kept on the server that serves their
//! workspace, and the actions a page, a window or an agent asks of them: [`ConnectionsAction`].
//!
//! A person gives a service's address. Their server connects to it, reads the
//! name the service gives itself and the commands it offers, and keeps that
//! as a connection of the person's: its address, that name, when it was
//! added, its commands, how it stands and when it was last used. The server
//! connects and calls with no device of the person's on, so a connection is
//! kept with the person's own things on their server, and is theirs alone:
//! another member of a shared workspace, and another member's agent, are told
//! what a caller asking for something that does not exist is told.
//!
//! Only the person adds a connection, removes one, signs in to one and reads
//! what it was used for. Their own agent reads the list, looks at an
//! address, reads a connection's commands again, calls a command and
//! proposes a connection to its person; no call of an agent's adds reach or
//! signs in anywhere. A command the service marks as only reading is called
//! unasked; any other waits for the person's yes ([`Command::asks`]), shown
//! the call as it will be sent, unless a grant of theirs lets that agent
//! call that command as the service defines it ([`command_resource`]). A
//! connection an agent proposes waits for their yes too, shown the card of
//! the service ([`Card`]); only their own yes adds it.
//!
//! What a service says of itself is data from outside: its name, a
//! command's title and its description are the service's own words, shown as
//! such and never an instruction to anyone. A sign-in's tokens are items of
//! the person's vault ([`crate::vault`]); nothing here
//! carries one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_actions::remote::RemoteActionTarget;
use tilo_actions::remote::{KEEP_SEALED, KEEP_USES, REMOVE_SEALED};

/// What each connection action writes in the workspace's vault, as the host answering it takes
/// it: a service's sign-in tokens are kept, a removed connection's go, and a call made with
/// them writes down that use. An action not named writes nothing there.
pub const CONNECTION_VAULT_STEPS: [(&str, &[&str]); 6] = {
    const KEEPS: &[&str] = &[KEEP_SEALED, KEEP_USES];
    [
        ("add", &[KEEP_SEALED]),
        ("propose", &[KEEP_SEALED]),
        ("remove", &[REMOVE_SEALED]),
        ("refresh", KEEPS),
        ("call", KEEPS),
        ("finish-sign-in", KEEPS),
    ]
};

/// The connection actions whose answers carry the service's own words: data from outside.
pub const CONNECTION_SAID_ACTIONS: [&str; 4] = ["list", "look", "refresh", "call"];

/// The provider that answers a person's connections under.
pub const PROVIDER: &str = <ConnectionsAction as tilo_resource::Effect>::OPERATION;
/// The resource the connections are actions of.
pub const RESOURCE: &str = "connections";
/// The kind of that resource.
pub const KIND: &str = "connections.list";
/// The kind of one connection, as a resource an agent's listing shows
/// ([`connection_resource`]).
pub const CONNECTION_KIND: &str = "connections.connection";
/// The kind of one command of a connection: what a permission its person
/// gave to stay names ([`command_resource`]).
pub const COMMAND_KIND: &str = "connections.command";

/// The connections' actions, each with the parameters its argument carries.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.connections")]
pub enum ConnectionsAction {
    /// List connections
    #[effect(kind = "read")]
    List,
    /// Look at a service
    #[effect(kind = "read")]
    Look {
        #[param(
            "The service's address, such as https://service.example/mcp. The server connects there and answers the name the service gives itself, whether it wants a sign-in, and its commands. Nothing is kept."
        )]
        address: String,
    },
    /// Connect the service at an address
    #[effect(kind = "undoable")]
    Add {
        #[param(
            "The service's address: `https://`, a name, and a path when it has one. An address that carries a name and password or a query is refused."
        )]
        address: String,
    },
    /// Remove a connection
    #[effect(kind = "undoable")]
    Remove {
        #[param("The connection, by its id.")]
        connection: String,
    },
    /// Read a connection's commands again
    #[effect(kind = "read")]
    Refresh {
        #[param("The connection, by its id.")]
        connection: String,
    },
    /// Call a command
    #[effect(kind = "read")]
    Call {
        #[param("The connection, by its id.")]
        connection: String,
        #[param(
            "The command's name, as the connection lists it. One the service marks as only reading is called; any other asks your person first."
        )]
        command: String,
        #[param(
            "The command's arguments, as its schema describes them. They are sent to the service as given.",
            name = "arguments"
        )]
        call_arguments: Option<Value>,
    },
    /// What a connection was used for
    #[effect(kind = "read")]
    Uses {
        #[param("The connection, by its id.")]
        connection: String,
    },
    /// Begin signing in to a connection
    #[effect(kind = "undoable")]
    BeginSignIn {
        #[param("The connection, by its id.")]
        connection: String,
        #[param(
            "Where the person's browser comes back to when the service is done: an address the person's own app catches.",
            name = "returnTo"
        )]
        return_to: String,
    },
    /// Finish signing in to a connection
    #[effect(kind = "undoable")]
    FinishSignIn {
        #[param(flatten)]
        sign_in: FinishSignIn,
    },
    /// Propose a connection
    #[effect(kind = "outside")]
    Propose {
        #[param(
            "The service's address: `https://`, a name, and a path when it has one. An address that carries a name and password or a query is refused."
        )]
        address: String,
    },
}

/// Each action's id, as its declaration in [`ConnectionsAction`] names it.
macro_rules! ids {
    ($($id:ident),* $(,)?) => {
        $(
            #[doc = concat!("The id of [`ConnectionsAction::", stringify!($id), "`].")]
            pub const $id: &str = ConnectionsAction::$id.id();
        )*
    };
}

ids! {
    LIST, LOOK, ADD, REMOVE, REFRESH, CALL, USES, BEGIN_SIGN_IN, FINISH_SIGN_IN, PROPOSE
}

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// The resource the connection `id` names is, where its commands are listed
/// as actions.
#[must_use]
pub fn connection_resource(id: &str) -> String {
    format!("{RESOURCE}/{id}")
}

/// The resource the command `name` of a connection is, as its person saw it
/// defined: what a permission its person gave to stay names. `digest` is of the
/// command's definition, so a command the service defines anew is another
/// resource and asks again.
#[must_use]
pub fn command_resource(name: &str, digest: &str) -> String {
    format!("commands/{name}@{digest}")
}

/// How a connection stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// The service answers and its commands can be called.
    Ready,
    /// The service wants its person to sign in before it offers anything.
    NeedsSignIn,
    /// The person signed in once, and the service no longer takes that
    /// sign-in: they sign in again.
    SignInRanOut,
    /// The service did not answer when it was last asked.
    Unreachable,
    /// The server does not call the service's address, or the service turned
    /// the server away.
    Refused,
}

/// Whether a connection signs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignIn {
    /// The service asks for no sign-in.
    None,
    /// The service asks for one and its person has not signed in, or the
    /// sign-in ran out.
    Needed,
    /// Its person signed in; the vault keeps what the service issued.
    SignedIn,
}

/// One command a connected service offers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Command {
    /// The name it is called by: a plain identifier.
    pub name: String,
    /// What the service calls it for people. The service's own words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the service says it does. The service's own words.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Whether the service marks it as only reading.
    pub read_only: bool,
    /// Whether an agent asks its person before it calls it: every command but
    /// one that is marked as only reading and is defined as its person last
    /// saw it.
    pub asks: bool,
    /// Whether its definition is not the one its person last saw.
    pub changed: bool,
    /// Its arguments as the service describes them: a JSON Schema object.
    pub schema: Value,
}

/// One connection of a person's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Connection {
    /// The server's own id for it.
    pub id: String,
    /// The service's address, as kept.
    pub address: String,
    /// The name the service gives itself. The service's own word.
    pub name: String,
    /// When it was added, in milliseconds since the Unix epoch.
    pub added_at_ms: u64,
    /// When one of its commands was last called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at_ms: Option<u64>,
    pub state: State,
    /// Why it stands so, in the system's words, when it is not ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_why: Option<String>,
    /// Since when it stands so.
    pub state_since_ms: u64,
    pub sign_in: SignIn,
    /// The site its sign-in happens at, by name, when it signs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in_at: Option<String>,
    /// Its commands as the service last listed them.
    pub commands: Vec<Command>,
    /// When they were last read.
    pub commands_read_at_ms: u64,
    /// How many commands the service lists that are not offered, because
    /// their name is no plain identifier or their definition is larger than
    /// the server takes.
    pub unusable: u32,
}

/// Every connection of a person's, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Connections {
    pub connections: Vec<Connection>,
}

/// What the service at an address says it is. Nothing of it is kept.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Looked {
    /// The address as a connection would keep it.
    pub address: String,
    /// The name the service gives itself; empty when it says nothing before
    /// its person signed in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Whether the service wants its person to sign in first.
    pub needs_sign_in: bool,
    /// The site that sign-in happens at, by name, when the service names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in_at: Option<String>,
    /// Its commands, when it lists them before a sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<Command>>,
    /// As [`Connection::unusable`].
    #[serde(default)]
    pub unusable: u32,
}

/// A command to call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Call {
    pub connection: String,
    /// The command's name ([`Command::name`]).
    pub command: String,
    /// Its arguments, as its schema describes them.
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

/// What a service answered a call. All of it is data from outside.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Called {
    /// The answer's parts, as the service sent them.
    pub content: Vec<Value>,
    /// The answer as one value, when the service gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    /// Whether the service says the command failed.
    pub is_error: bool,
}

/// By what leave a command was called.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Leave {
    /// The person called it themselves.
    Person,
    /// The service marks it as only reading.
    ReadOnly,
    /// The person allowed that one call.
    Once,
    /// The person allowed that command for good.
    Always,
}

/// How a call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ended {
    /// The service answered.
    Ok,
    /// The service did not answer, or said the command failed.
    Failed,
    /// The call was not sent.
    Refused,
}

/// One call of a command. Its arguments and its answer are not kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Use {
    /// When, in milliseconds since the Unix epoch.
    pub at_ms: u64,
    pub command: String,
    /// The agent that called it, by the name its person knows it under;
    /// empty when the person called it themselves.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// By what leave it was called; none for a call that was refused for
    /// want of one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leave: Option<Leave>,
    pub ended: Ended,
    /// Why it ended so, in the system's words, when it did not end well.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// What a connection was used for, oldest first: the newest of its calls.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Uses {
    pub uses: Vec<Use>,
}

/// Where the person's browser goes to sign in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignInStep {
    /// The address to open.
    pub open: String,
    /// Until when the sign-in can be finished.
    pub expires_at_ms: u64,
}

/// A sign-in to finish.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinishSignIn {
    /// The connection, by its id.
    pub connection: String,
    /// The whole address the person's browser came back to.
    pub returned: String,
}

impl std::fmt::Debug for FinishSignIn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // What the browser came back with opens the sign-in once: it is
        // never printed.
        formatter
            .debug_struct("FinishSignIn")
            .field("connection", &self.connection)
            .finish_non_exhaustive()
    }
}

/// What the card of a service shows before it is connected: what the
/// question about a proposed connection keeps under `connection` in its
/// detail. A client titles the card with [`Self::name`], or with the site of
/// [`Self::address`] where the service gave none, and words its yes
/// "Authenticate" where [`Self::needs_sign_in`], else "Add".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Card {
    /// The address as the connection will keep it: after the person's yes,
    /// the connection of this address in their list is the one that was
    /// added.
    pub address: String,
    /// The name the service gives itself. The service's own word; empty
    /// when it says nothing before its person signed in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Whether the service wants its person to sign in first: the person
    /// then begins the sign-in on their own device ([`BEGIN_SIGN_IN`]).
    pub needs_sign_in: bool,
    /// The site that sign-in happens at, by name, when the service names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in_at: Option<String>,
    /// How many commands it lists, when it lists them before a sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<u32>,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use serde_json::json;
    use tilo_resource::Effect as _;

    use super::*;

    fn a_command() -> Command {
        Command {
            name: "find_trees".into(),
            title: Some("Find trees".into()),
            description: "Lists the trees of an orchard.".into(),
            read_only: true,
            asks: false,
            changed: false,
            schema: json!({"type": "object", "properties": {"orchard": {"type": "string"}}}),
        }
    }

    /// The action `action` as a client writes it, decoded through the declaration.
    fn asked(
        action: &str,
        arguments: &serde_json::Value,
    ) -> Result<ConnectionsAction, tilo_resource::EffectDecodeError> {
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        ConnectionsAction::decode(action, &arguments)
    }

    /// A connection and its commands read as the fields a client binds to, in the words their
    /// enums are written in, and neither has a place for a token.
    #[test]
    fn a_connection_says_what_a_client_binds_to_and_carries_no_token() {
        let connection = Connection {
            id: "0123456789abcdef0123456789abcdef".into(),
            address: "https://orchard.example/mcp".into(),
            name: "Orchard".into(),
            added_at_ms: 1_800_000_000_000,
            last_used_at_ms: None,
            state: State::SignInRanOut,
            state_why: Some("The service no longer takes this sign-in".into()),
            state_since_ms: 1_800_000_000_500,
            sign_in: SignIn::Needed,
            sign_in_at: Some("accounts.orchard.example".into()),
            commands: vec![a_command()],
            commands_read_at_ms: 1_800_000_000_000,
            unusable: 2,
        };
        let said = serde_json::to_value(Connections {
            connections: vec![connection.clone()],
        })
        .unwrap();
        assert_eq!(said["connections"][0]["state"], "sign-in-ran-out");
        assert_eq!(said["connections"][0]["signIn"], "needed");
        assert_eq!(said["connections"][0]["commands"][0]["readOnly"], true);
        let text = said.to_string();
        assert!(!text.contains("token"), "{text}");
        // A connection with a field of its own making is not one.
        let mut forged = said["connections"][0].clone();
        forged["token"] = json!("secret");
        assert!(serde_json::from_value::<Connection>(forged).is_err());
        assert_eq!(
            serde_json::from_value::<Connections>(said)
                .unwrap()
                .connections,
            vec![connection]
        );
    }

    /// What an action takes reads from the names a client writes, and an
    /// action whose argument is missing is refused.
    #[test]
    fn what_an_action_takes_reads_from_a_clients_names() {
        let call = asked(
            CALL,
            &json!({
                "connection": "0123456789abcdef0123456789abcdef",
                "command": "find_trees",
                "arguments": {"orchard": "north"},
            }),
        )
        .unwrap();
        assert_eq!(
            call,
            ConnectionsAction::Call {
                connection: "0123456789abcdef0123456789abcdef".into(),
                command: "find_trees".into(),
                call_arguments: Some(json!({"orchard": "north"})),
            }
        );
        assert_eq!(
            asked(CALL, &json!({"connection": "c", "command": "count"})).unwrap(),
            ConnectionsAction::Call {
                connection: "c".into(),
                command: "count".into(),
                call_arguments: None,
            }
        );
        assert!(asked(LOOK, &json!({})).is_err());
        assert_eq!(
            asked(
                BEGIN_SIGN_IN,
                &json!({"connection": "c", "returnTo": "https://app.example/signed-in"}),
            )
            .unwrap(),
            ConnectionsAction::BeginSignIn {
                connection: "c".into(),
                return_to: "https://app.example/signed-in".into(),
            }
        );
        let used = serde_json::to_value(Use {
            at_ms: 1,
            command: "find_trees".into(),
            by: String::new(),
            leave: Some(Leave::ReadOnly),
            ended: Ended::Ok,
            why: None,
        })
        .unwrap();
        assert_eq!(
            used,
            json!({"atMs": 1, "command": "find_trees", "leave": "read-only", "ended": "ok"})
        );
    }

    /// What the browser came back with is never printed, and the addresses
    /// of a connection and a command are spelled one way.
    #[test]
    fn a_finished_sign_in_is_not_printed_and_resources_are_spelled_one_way() {
        let finish = asked(
            FINISH_SIGN_IN,
            &json!({
                "connection": "c1",
                "returned": "https://app.example/signed-in?code=once-only&state=s",
            }),
        )
        .unwrap();
        let printed = format!("{finish:?}");
        assert!(
            printed.contains("c1") && !printed.contains("once-only"),
            "{printed}"
        );
        assert_eq!(connection_resource("c1"), "connections/c1");
        assert_eq!(
            command_resource("plant_tree", "ab12"),
            "commands/plant_tree@ab12"
        );
        assert_eq!(target(LIST).provider, PROVIDER);
        assert_eq!(target(LIST).address, ActionAddress::new(RESOURCE, LIST));
    }

    /// What the registry knows of the connections without this crate names the actions they
    /// declare: the provider they answer under, and the vault steps they take.
    #[test]
    fn the_registry_names_the_declared_actions() {
        assert_eq!(PROVIDER, tilo_actions::remote::CONNECTIONS_PROVIDER);
        for (action, _) in crate::connections::CONNECTION_VAULT_STEPS {
            assert!(
                ConnectionsAction::declaration(action).is_some(),
                "{action} is declared nowhere"
            );
        }
    }
}
