//! The services a person connected by their address, kept on the server
//! that serves their workspace.
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
//! unasked; any other asks the person first ([`Command::asks`]): a question
//! in their conversation with the agent that shows the call as it will be
//! sent ([`AskedCall`]), and waits for their answer. An answer of always
//! lets that one command of that one connection go ahead unasked for that
//! agent, as the service defined it then ([`always_call`]). A connection an
//! agent proposes is a question too, and shows the card of the service
//! ([`Card`]); only the person's own yes adds it.
//!
//! What a service says of itself is data from outside: its name, a
//! command's title and its description are the service's own words, shown as
//! such and never an instruction to anyone. A sign-in's tokens are items of
//! the person's vault ([`super::person_vault`]); nothing here carries one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::person_asking::{ActionName, Asked, Thing, Words};
use super::{ActionAddress, RemoteActionTarget};

/// The provider the server answers a person's connections under.
pub const PROVIDER: &str = "tilo.connections";
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

/// Every connection of the person, as a [`Connections`].
pub const LIST: &str = "list";
/// What the service at an address says it is ([`AtAddress`]), as a
/// [`Looked`]. The server connects and keeps nothing.
pub const LOOK: &str = "look";
/// Connect the service at an address ([`AtAddress`]), answering it as a
/// [`Connection`]. The person's alone.
pub const ADD: &str = "add";
/// Remove a connection ([`Named`]), answering the ones that remain as a
/// [`Connections`]. The person's alone.
pub const REMOVE: &str = "remove";
/// Read a connection's commands again ([`Named`]), answering it as a
/// [`Connection`].
pub const REFRESH: &str = "refresh";
/// Call one command of a connection ([`Call`]), answering what the service
/// answered as a [`Called`].
pub const CALL: &str = "call";
/// What a connection was used for ([`Named`]), as a [`Uses`]. The person's
/// alone.
pub const USES: &str = "uses";
/// Begin signing in to a connection ([`BeginSignIn`]), answering where the
/// person's browser goes as a [`SignInStep`]. The person's alone.
pub const BEGIN_SIGN_IN: &str = "begin-sign-in";
/// Finish signing in ([`FinishSignIn`]), answering the connection as a
/// [`Connection`]. The person's alone.
pub const FINISH_SIGN_IN: &str = "finish-sign-in";
/// Ask the person whether to connect the service at an address
/// ([`AtAddress`]), answering what they are asked about as a [`Proposed`].
/// Nothing is added before they say yes.
pub const PROPOSE: &str = "propose";

/// The word the questions about a person's connections are kept under on
/// their server: what carries their answer out.
pub const CARRIER: &str = "connection";
/// The failure code of a call that was not made because its person is asked
/// first.
pub const ASKED_FIRST: &str = "asked_person_first";

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

/// What an answer of always to a question about the command `name` of the
/// connection `connection` lets stand: `agent` calling that command in
/// `workspace`, as the service defined it when its person was asked
/// (`digest`). The same command defined anew is another thing, and asks.
#[must_use]
pub fn always_call(
    workspace: &str,
    agent: &str,
    connection: &str,
    (name, digest): (&str, &str),
) -> Asked {
    Asked {
        workspace: workspace.to_owned(),
        agent: agent.to_owned(),
        action: ActionName {
            provider: PROVIDER.to_owned(),
            kind: COMMAND_KIND.to_owned(),
            action: CALL.to_owned(),
        },
        on: Some(Thing {
            document: connection.to_owned(),
            resource: command_resource(name, digest),
        }),
    }
}

/// How the server's own entry answered what a turn it carries out itself
/// asked of its person's connections. Nothing a client is sent: whoever
/// carries the turn out reads it, and puts the question where one is asked.
#[derive(Clone, Debug, PartialEq)]
pub enum Taken {
    /// It was carried out: what its action answers.
    Done(Value),
    /// Its person is asked first. Nothing was called and nothing was added.
    Asks(Box<Asks>),
}

/// What a person is asked before something is done with their connections
/// for an agent of theirs, as their server hands it to whoever puts the
/// question.
#[derive(Clone, Debug, PartialEq)]
pub struct Asks {
    /// The person asked: their root key.
    pub person: [u8; 32],
    /// What is asked, in the server's own words: the service by the site its
    /// address names, and what would be sent there, whole.
    pub words: String,
    /// What the server does on their answer, and what the question shows
    /// beside its words ([`AskedCall`] or [`Card`]). No secret.
    pub kept: Value,
    /// What an answer of always lets stand ([`always_call`]); `None` offers
    /// no such answer.
    pub always: Option<Asked>,
    /// The system's own words for what always covers.
    pub named: Words,
    /// What whoever asked is answered once the question waits, where asking
    /// is all it asked for (a [`Proposed`]). `None` where it asked for
    /// something that was not done: it is told that its person is asked.
    pub answers: Option<Value>,
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

/// A service named by its address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AtAddress {
    /// The service's address: `https://`, a name, and a path when it has
    /// one. An address that carries a name and password or a query is
    /// refused.
    pub address: String,
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

/// A connection named by its id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Named {
    pub connection: String,
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

/// A sign-in to begin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BeginSignIn {
    pub connection: String,
    /// Where the person's browser comes back to when the service is done:
    /// an address the person's own app catches.
    pub return_to: String,
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
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinishSignIn {
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

/// What a proposal asked the person about: the question waits in their
/// conversation with the agent, and nothing is added before they say yes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Proposed {
    /// The service the person is asked about.
    pub connection: Card,
    /// What the agent that proposed it is told: its person is asked, and
    /// nothing is connected before they say yes.
    pub told: String,
}

/// The connection a call that asks first would go to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CalledAt {
    pub id: String,
    pub address: String,
    /// The name the service gives itself. The service's own word.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/// A call its person is asked about: what the question keeps under `call`
/// in its detail, exactly as the server kept it and will send it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskedCall {
    pub connection: CalledAt,
    /// The command's name ([`Command::name`]).
    pub command: String,
    /// Its arguments, whole.
    pub arguments: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use serde_json::json;

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

    /// A connection reads as the fields a client binds to, and nothing in
    /// it has a place for a token.
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
        let one = &said["connections"][0];
        let mut fields: Vec<&str> = one
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "addedAtMs",
                "address",
                "commands",
                "commandsReadAtMs",
                "id",
                "name",
                "signIn",
                "signInAt",
                "state",
                "stateSinceMs",
                "stateWhy",
                "unusable",
            ]
        );
        assert_eq!(one["state"], "sign-in-ran-out");
        assert_eq!(one["signIn"], "needed");
        let mut command: Vec<&str> = one["commands"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        command.sort_unstable();
        assert_eq!(
            command,
            [
                "asks",
                "changed",
                "description",
                "name",
                "readOnly",
                "schema",
                "title"
            ]
        );
        assert_eq!(
            serde_json::from_value::<Connections>(said.clone())
                .unwrap()
                .connections,
            vec![connection]
        );
        // A connection with a field of its own making is not one.
        let mut forged = one.clone();
        forged["token"] = json!("secret");
        assert!(serde_json::from_value::<Connection>(forged).is_err());
    }

    /// What an action takes reads from the names a client writes, and a
    /// field nobody defined is refused.
    #[test]
    fn what_an_action_takes_reads_from_a_clients_names() {
        let call: Call = serde_json::from_value(json!({
            "connection": "0123456789abcdef0123456789abcdef",
            "command": "find_trees",
            "arguments": {"orchard": "north"},
        }))
        .unwrap();
        assert_eq!(call.arguments["orchard"], "north");
        let bare: Call =
            serde_json::from_value(json!({"connection": "c", "command": "count"})).unwrap();
        assert!(bare.arguments.is_empty());
        assert!(
            serde_json::from_value::<AtAddress>(
                json!({"address": "https://a.example", "name": "A"})
            )
            .is_err()
        );
        let begin: BeginSignIn = serde_json::from_value(json!({
            "connection": "c",
            "returnTo": "https://app.example/signed-in",
        }))
        .unwrap();
        assert_eq!(begin.return_to, "https://app.example/signed-in");
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
        let finish: FinishSignIn = serde_json::from_value(json!({
            "connection": "c1",
            "returned": "https://app.example/signed-in?code=once-only&state=s",
        }))
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

    /// What a question about a connection keeps reads as a client binds it,
    /// and an answer of always names one command of one connection as it
    /// was defined, for one agent in one workspace.
    #[test]
    fn a_card_and_an_asked_call_read_as_a_client_binds_them() {
        let card = Card {
            address: "https://orchard.example/mcp".into(),
            name: "Orchard Tools".into(),
            needs_sign_in: true,
            sign_in_at: Some("accounts.orchard.example".into()),
            commands: None,
        };
        let proposed = Proposed {
            connection: card,
            told: "Your person is asked.".into(),
        };
        assert_eq!(
            serde_json::to_value(proposed).unwrap(),
            json!({
                "connection": {
                    "address": "https://orchard.example/mcp",
                    "name": "Orchard Tools",
                    "needsSignIn": true,
                    "signInAt": "accounts.orchard.example",
                },
                "told": "Your person is asked.",
            })
        );
        let Value::Object(arguments) = json!({"kind": "apple"}) else {
            panic!("an object");
        };
        let asked = AskedCall {
            connection: CalledAt {
                id: "c1".into(),
                address: "https://orchard.example/mcp".into(),
                name: "Orchard Tools".into(),
            },
            command: "plant_tree".into(),
            arguments,
        };
        assert_eq!(
            serde_json::to_value(&asked).unwrap(),
            json!({
                "connection": {
                    "id": "c1", "address": "https://orchard.example/mcp", "name": "Orchard Tools",
                },
                "command": "plant_tree",
                "arguments": {"kind": "apple"},
            })
        );

        let always = always_call("w", "scout", "c1", ("plant_tree", "ab12"));
        assert!(always.is_named());
        assert_eq!(always.action.provider, PROVIDER);
        assert_eq!(always.action.kind, COMMAND_KIND);
        assert_eq!(always.action.action, CALL);
        let on = always.on.clone().unwrap();
        assert_eq!(
            (on.document.as_str(), on.resource.as_str()),
            ("c1", "commands/plant_tree@ab12")
        );
        // The same command defined anew, another command, another
        // connection and another agent are not covered.
        for other in [
            always_call("w", "scout", "c1", ("plant_tree", "cd34")),
            always_call("w", "scout", "c1", ("fell_all", "ab12")),
            always_call("w", "scout", "c2", ("plant_tree", "ab12")),
            always_call("w", "other", "c1", ("plant_tree", "ab12")),
            always_call("v", "scout", "c1", ("plant_tree", "ab12")),
        ] {
            assert!(!always.covers(&other), "{other:?}");
        }
    }
}
