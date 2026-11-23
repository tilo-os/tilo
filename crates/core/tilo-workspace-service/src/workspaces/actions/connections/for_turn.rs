//! A workspace's connections as a turn this server carries out itself
//! reaches them.
//!
//! An agent this server runs works with every device of whoever asked it off, so no transport
//! verified a device behind what it asks. This is the server's own entry for such a turn, reached
//! from no transport: only the code that carries the turn out calls it, with the very caller the
//! turn's tools act as ([`HostedTurn`]). The turn is one this server carries out or it is answered
//! as if the provider were not there ([`Workspaces::asked_turn`]), and the workspace's policy lets
//! its agent reach the workspace's connections ([`Store::CONNECTIONS`]). Then the connections' own
//! rule:
//!
//! - an agent that anything narrows (what its entry bounds it to; a
//!   sub-agent) reaches only a connection a rule of its own names, and looks
//!   at an address or proposes one only where a rule names that action;
//! - listing, looking, reading a connection's commands again and calling a
//!   command that only reads are done at once;
//! - a command that changes something is called unasked only where a grant
//!   of the workspace's lets this agent call that command of that connection,
//!   as the service defines it now. Otherwise it waits for the person's yes,
//!   shown the service by the site its address names, the command and its
//!   arguments whole, and their approve admits the same call again, held to
//!   every rule above once more;
//! - a connection is never added by a turn alone. An agent proposes one, and
//!   it is added once its person approved the proposal.

#[cfg(feature = "agents")]
use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};
use tilo_action_policy::{Answer, Level, Segment, TreePath};
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::person_connections::{self, AtAddress, Call, Leave, Named};
#[cfg(feature = "agents")]
use tilo_actions::remote::person_connections::{Command, Connection, Connections};
#[cfg(feature = "agents")]
use tilo_actions::remote::{CatalogueResource, CatalogueRow};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
#[cfg(feature = "agents")]
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ActionParam, ExecutionLocus,
    InputSpec, OperationKind, ParamType, RiskLevel,
};

#[cfg(feature = "agents")]
use super::super::Verb;
#[cfg(feature = "agents")]
use super::{AGENT_VERBS, CONNECTIONS, address_params};
use super::{Refused, argument};
use crate::connections::{self, Calling, Reach};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{AskedTurn, HostedTurn};
use crate::workspaces::workspace_stores::Store;

/// The most arguments a question shows whole, as JSON: a call with more is
/// refused before anybody is asked.
const MAX_ASKED_BYTES: usize = 2 * 1024;
#[cfg(feature = "agents")]
/// The most of a service's own words about a command a listing repeats.
const MAX_SAID_CHARS: usize = 240;
#[cfg(feature = "agents")]
/// The longest name of an argument a listing spells out.
const MAX_ARGUMENT_NAME_BYTES: usize = 64;

const TOO_LARGE_TO_ASK: &str = "Those arguments are larger than a question shows whole, so your person cannot be asked about this call";
const CONNECTED: &str = "That service is connected already: it is in the list of connections";

#[cfg(feature = "agents")]
/// Proposing a connection: its person is asked, and nothing is added by the
/// call.
const PROPOSES: Verb = Verb {
    id: person_connections::PROPOSE,
    label: "Propose a connection",
    check: "",
    kind: ActionKind::Outside,
    params: address_params,
};

fn refused(reason: String) -> Refused {
    Refused::Refused { reason }
}

fn done<T: Serialize>(answered: T) -> Result<Value, Refused> {
    serde_json::to_value(answered).map_err(|error| refused(error.to_string()))
}

/// Where a grant that lets `using`'s agent call the command `name` of `connection`, as the
/// service defines it now (`digest`), stands in its workspace's policy.
fn call_node(using: &AskedTurn, connection: &str, (name, digest): (&str, &str)) -> TreePath {
    TreePath::workspace(&using.workspace)
        .app(person_connections::PROVIDER)
        .kind(person_connections::COMMAND_KIND)
        .resource(format!(
            "{connection}/{}",
            person_connections::command_resource(name, digest)
        ))
        .action(person_connections::CALL)
}

/// The arguments `request` carries, whole.
fn arguments_of(request: &InvokeRequest) -> Map<String, Value> {
    match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        _ => Map::new(),
    }
}

/// Whether the turn reaches the connection `id`: an agent nothing narrows
/// reaches every connection of its person's, a narrowed one only a
/// connection that a rule of its own names and lets it call.
fn names(turn: &HostedTurn<'_>, workspace: &str, id: &str) -> bool {
    let Some(bound) = turn.bound else {
        return true;
    };
    let resource = person_connections::connection_resource(id);
    let named = bound.iter().any(|rule| {
        rule.answer == Answer::Allow
            && rule.node.segments().iter().any(
                |segment| matches!(segment, Segment::Name(Level::Resource, name) if *name == resource),
            )
    });
    let node = TreePath::workspace(workspace)
        .app(person_connections::PROVIDER)
        .kind(person_connections::CONNECTION_KIND)
        .resource(resource)
        .action(person_connections::CALL);
    named
        && tilo_automation::grant_answer(bound, &node, tilo_action_policy::ActionKind::Outside)
            .answer
            == Answer::Allow
}

#[cfg(feature = "agents")]
/// `said` on one line, as far as a listing repeats a service's own words.
fn one_line(said: &str) -> String {
    let mut line = String::new();
    for word in said.split_whitespace() {
        if !line.is_empty() {
            line.push(' ');
        }
        line.extend(word.chars().filter(|character| !character.is_control()));
    }
    if line.chars().count() > MAX_SAID_CHARS {
        line = line.chars().take(MAX_SAID_CHARS).collect();
        line.push('\u{2026}');
    }
    line
}

#[cfg(feature = "agents")]
/// The arguments `schema` describes, as a listing spells them out: each by
/// its name, its kind and whether a call must give it. A description this
/// listing cannot spell out whole names none: the command's own schema is
/// in its connection's list.
fn params_of(schema: &Value) -> Vec<ActionParam> {
    let Some(described) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut params = Vec::new();
    for (name, argument) in described {
        let plain = !name.is_empty()
            && name.len() <= MAX_ARGUMENT_NAME_BYTES
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
        let ty = match argument.get("type").and_then(Value::as_str) {
            Some("string") => ParamType::Text,
            Some("integer") => ParamType::Integer,
            Some("number") => ParamType::Number,
            Some("boolean") => ParamType::Boolean,
            Some("array") => ParamType::Array,
            Some("object") => ParamType::Object,
            _ => return Vec::new(),
        };
        if !plain {
            return Vec::new();
        }
        let says = argument
            .get("description")
            .and_then(Value::as_str)
            .map(one_line)
            .unwrap_or_default();
        params.push(if required.contains(&name.as_str()) {
            ActionParam::required_owned(name.clone(), ty, says)
        } else {
            ActionParam::optional_owned(name.clone(), ty, says)
        });
    }
    params
}

#[cfg(feature = "agents")]
/// The row of one command of `connection`, whose service is at `site`: what
/// an agent's listing shows of it, with whether it asks first. What the
/// service says of the command follows the system's own words, marked as
/// the service's.
fn command_row(connection: &Connection, site: &str, command: &Command) -> CatalogueRow {
    let resource = person_connections::connection_resource(&connection.id);
    let label = if connection.name.is_empty() {
        site.to_owned()
    } else {
        connection.name.clone()
    };
    let leave = if command.asks {
        "Asks your person first"
    } else {
        "Only reads"
    };
    let mut description = format!("{leave}. A command of the service at {site}.");
    if !command.description.is_empty() {
        let _ = write!(
            description,
            " The service says of it: {}",
            one_line(&command.description)
        );
    }
    CatalogueRow {
        offered: None,
        provider: person_connections::PROVIDER.into(),
        app_id: person_connections::PROVIDER.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.clone(),
            kind: person_connections::CONNECTION_KIND.into(),
            label: label.clone(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(resource, command.name.clone()),
            resource_kind: person_connections::CONNECTION_KIND.into(),
            resource_label: label,
            action_label: command
                .title
                .clone()
                .unwrap_or_else(|| command.name.clone()),
            keywords: Vec::new(),
            icon: None,
            description: Some(description),
            locus: ExecutionLocus::ServerPreferred,
            kind: OperationKind::Effect,
            contract: ActionContract {
                kind: if command.asks {
                    ActionKind::Outside
                } else {
                    ActionKind::Read
                },
                risk: if command.asks {
                    RiskLevel::Privileged
                } else {
                    RiskLevel::None
                },
                ..ActionContract::default()
            },
            input: InputSpec::None,
            params: params_of(&command.schema),
            view: None,
        }),
        has_children: false,
        visibility: None,
        surface: None,
    }
}

impl Workspaces {
    /// Whether the agent `turn` carries out reaches the workspace's
    /// connections, as the workspace's policy decides for it.
    async fn reaches_connections(&self, turn: &HostedTurn<'_>) -> bool {
        self.turn_reaches(
            turn,
            Store::CONNECTIONS,
            person_connections::LIST,
            tilo_action_policy::ActionKind::Read,
        )
        .await
    }

    /// Take what `request` asks of its person's connections, for `turn`.
    ///
    /// # Errors
    /// The turn is none this server carries out, or
    /// the request names nothing an agent does with a connection
    /// ([`Refused::UnknownAction`]: adding, removing, signing in and
    /// reading what a connection was used for are the person's own); its
    /// agent is narrowed and holds no rule for the connection or the action
    /// ([`Refused::NotPermitted`]); the arguments do not read; or the
    /// connections refuse it, in their own words.
    pub(crate) async fn connections_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, Refused> {
        let using = self.asked_turn(turn).await?;
        let asker = using.acting();
        let target = &request.target;
        if target.provider != person_connections::PROVIDER {
            return Err(Refused::UnknownAction);
        }
        if !self.reaches_connections(turn).await {
            return Err(Refused::UnknownAction);
        }
        let root = self.workspace_root(&using.workspace);
        let reach = self.connections_reach();
        let at = (root.as_path(), &reach);
        let resource: &str = &target.address.resource;
        let action: &str = &target.address.action;
        // One command of one connection, called by its own row: its
        // arguments are the request's, whole.
        if let Some(connection) = resource
            .strip_prefix(person_connections::RESOURCE)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            let call = Call {
                connection: connection.to_owned(),
                command: action.to_owned(),
                arguments: arguments_of(request),
            };
            return self.call_for_turn((turn, request), &using, at, call).await;
        }
        if resource != person_connections::RESOURCE {
            return Err(Refused::UnknownAction);
        }
        let by_address = "a service is named by its address";
        let narrowed = |target: &RemoteActionTarget| {
            if turn.may_take(&using.workspace, target) {
                Ok(())
            } else {
                Err(Refused::NotPermitted)
            }
        };
        match action {
            person_connections::LIST => {
                let mut kept = connections::list(&root).map_err(refused)?;
                kept.connections
                    .retain(|kept| names(turn, &using.workspace, &kept.id));
                done(kept)
            }
            person_connections::LOOK => {
                narrowed(target)?;
                let looked: AtAddress = argument(request, by_address)?;
                done(
                    connections::look(&root, &reach, &looked.address)
                        .await
                        .map_err(refused)?,
                )
            }
            person_connections::REFRESH => {
                let named: Named = argument(request, "a connection is named by its id")?;
                if !names(turn, &using.workspace, &named.connection) {
                    return Err(Refused::NotPermitted);
                }
                let by = Calling::Agent(&using.name);
                done(
                    connections::refresh(
                        (&root, self.vault_for(&using.workspace, &asker)),
                        &reach,
                        &named.connection,
                        by,
                    )
                    .await
                    .map_err(refused)?,
                )
            }
            person_connections::CALL => {
                let call: Call = argument(
                    request,
                    "a call names its connection, a command of it and the command's arguments",
                )?;
                self.call_for_turn((turn, request), &using, at, call).await
            }
            person_connections::PROPOSE => {
                narrowed(target)?;
                let proposed: AtAddress = argument(request, by_address)?;
                Self::propose_for_turn((turn, request), at, &proposed.address).await
            }
            _ => Err(Refused::UnknownAction),
        }
    }

    /// Call the command `call` names for the turn of `using`, where it only reads, a grant
    /// lets this agent call it, or its person approved this very call; otherwise it waits for
    /// their yes.
    async fn call_for_turn(
        &self,
        (turn, request): (&HostedTurn<'_>, &InvokeRequest),
        using: &AskedTurn,
        (root, reach): (&Path, &Reach),
        call: Call,
    ) -> Result<Value, Refused> {
        if !names(turn, &using.workspace, &call.connection) {
            return Err(Refused::NotPermitted);
        }
        let found =
            connections::callable(root, reach, &call.connection, &call.command).map_err(refused)?;
        if !found.asks {
            let by = Calling::Agent(&using.name);
            return done(
                connections::call(
                    (root, self.vault_for(&using.workspace, &asker)),
                    reach,
                    by,
                    call,
                )
                .await
                .map_err(refused)?,
            );
        }
        // What a grant lets stand is read at every call, so a grant taken back covers nothing
        // from that moment.
        let node = call_node(using, &call.connection, (&call.command, &found.digest));
        let leave = if self.granted(&using.workspace, &using.agent, &node).await {
            Leave::Always
        } else {
            // What a question could not show whole is refused before anybody is asked.
            let sent = serde_json::to_string(&call.arguments)
                .map_err(|error| refused(error.to_string()))?;
            if sent.len() > MAX_ASKED_BYTES {
                return Err(refused(TOO_LARGE_TO_ASK.to_owned()));
            }
            crate::workspaces::asking::approved_or_asks(request, || AskedFacts {
                on: Some(format!("{} at {}", call.command, found.site)),
                facts: vec![Fact::new("Arguments", sent)],
                verb: "call".to_owned(),
                plural: Some("commands".to_owned()),
                ..AskedFacts::by(crate::workspaces::asking::asked_by(turn.caller))
            })?;
            Leave::Once
        };
        let allowed = Calling::Allowed {
            by: &using.name,
            leave,
            digest: &found.digest,
        };
        done(
            connections::call(
                (root, self.vault_at(&using.workspace)),
                reach,
                allowed,
                call,
            )
            .await
            .map_err(refused)?,
        )
    }

    /// Connect the service at `address` for `turn`'s person once they approved the proposal
    /// `request` makes; until then it waits for their yes, shown the service.
    async fn propose_for_turn(
        (turn, request): (&HostedTurn<'_>, &InvokeRequest),
        (root, reach): (&Path, &Reach),
        address: &str,
    ) -> Result<Value, Refused> {
        let looked = connections::look(root, reach, address)
            .await
            .map_err(refused)?;
        let kept = connections::list(root).map_err(refused)?;
        if kept
            .connections
            .iter()
            .any(|kept| kept.address == looked.address)
        {
            return Err(refused(CONNECTED.to_owned()));
        }
        let site = connections::site(reach, &looked.address).map_err(refused)?;
        crate::workspaces::asking::approved_or_asks(request, || {
            // The system's words alone: what the service calls itself is its own word.
            let mut facts = vec![Fact::new("Address", looked.address.clone())];
            if !looked.name.is_empty() {
                facts.push(Fact::new("It calls itself", looked.name.clone()));
            }
            match (&looked.commands, &looked.sign_in_at) {
                (Some(commands), _) => {
                    facts.push(Fact::new("Commands", commands.len().to_string()));
                }
                (None, Some(issuer)) if looked.needs_sign_in => {
                    facts.push(Fact::new("Sign in at", issuer.clone()));
                }
                (None, _) => {}
            }
            AskedFacts {
                on: Some(site),
                facts,
                verb: "connect".to_owned(),
                ..AskedFacts::by(crate::workspaces::asking::asked_by(turn.caller))
            }
        })?;
        // Their own yes is what adds it: the address looked at now.
        done(
            connections::add(root, reach, &looked.address)
                .await
                .map_err(refused)?,
        )
    }

    #[cfg(feature = "agents")]
    /// What `turn` finds of its person's connections in its listing: what an
    /// agent does with them, and each command of each connection it
    /// reaches, with whether it asks first. Read anew at every listing.
    /// Nothing for a turn this server does not carry out.
    pub(crate) async fn connection_rows_for_turn(
        &self,
        turn: &HostedTurn<'_>,
    ) -> Vec<CatalogueRow> {
        let Ok(using) = self.asked_turn(turn).await else {
            return Vec::new();
        };
        if !self.reaches_connections(turn).await {
            return Vec::new();
        }
        let root = self.workspace_root(&using.workspace);
        let reach = self.connections_reach();
        let reached: Vec<Connection> = connections::list(&root)
            .map(|kept: Connections| kept.connections)
            .unwrap_or_default()
            .into_iter()
            .filter(|kept| names(turn, &using.workspace, &kept.id))
            .collect();
        let offered = |verb: &&Verb| {
            let target = person_connections::target(verb.id);
            match verb.id {
                person_connections::LOOK | person_connections::PROPOSE => {
                    turn.may_take(&using.workspace, &target)
                }
                // What is done with a connection is offered where the
                // turn reaches one.
                _ => turn.bound.is_none() || !reached.is_empty(),
            }
        };
        let mut rows: Vec<CatalogueRow> = AGENT_VERBS
            .iter()
            .chain(std::iter::once(&PROPOSES))
            .filter(offered)
            .map(|verb| verb.row(CONNECTIONS, person_connections::RESOURCE, "Connections"))
            .collect();
        for connection in &reached {
            let site = connections::site(&reach, &connection.address).unwrap_or_default();
            rows.extend(
                connection
                    .commands
                    .iter()
                    .map(|command| command_row(connection, &site, command)),
            );
        }
        rows
    }
}
