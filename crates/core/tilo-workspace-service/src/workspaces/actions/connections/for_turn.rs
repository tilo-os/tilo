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
//! - a command that changes something is called unasked only where its
//!   person let that command of that connection stand for this agent, as
//!   the service defined it then. Otherwise nothing
//!   is called: the person is asked ([`Taken::Asks`]), shown the service by
//!   the site its address names, the command and its arguments whole;
//! - a connection is never added by a turn. An agent proposes one, and its
//!   person is asked.
//!
//! Whoever carries the turn out puts the question to the person and hands
//! their own answer back ([`Workspaces::connection_answered`]). What is done
//! then is what was kept when they were asked, for the person and the agent
//! that asked, held to every rule above again.

use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_action_policy::{Answer, Level, Segment, TreePath};
use tilo_actions::remote::person_asking::{Asked, Words};
use tilo_actions::remote::person_connections::{
    self, AskedCall, Asks, AtAddress, Call, CalledAt, Card, Command, Connection, Connections,
    Leave, Named, Proposed, Taken,
};
use tilo_actions::remote::{
    CatalogueResource, CatalogueRow, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ActionParam, ExecutionLocus,
    InputSpec, OperationKind, ParamType, RiskLevel,
};

use super::super::Verb;
use super::{AGENT_VERBS, CONNECTIONS, Refused, address_params, argument};
use crate::connections::{self, Calling, Reach};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{AskedTurn, HostedTurn};
use crate::workspaces::person_asking::kept_asks::KeptAsk;
use crate::workspaces::workspace_stores::Store;

/// The most arguments a question shows whole, as JSON: a call with more is
/// refused before anybody is asked.
const MAX_ASKED_BYTES: usize = 2 * 1024;
/// The most of a service's own words about a command a listing repeats.
const MAX_SAID_CHARS: usize = 240;
/// The longest name of an argument a listing spells out.
const MAX_ARGUMENT_NAME_BYTES: usize = 64;

const TOO_LARGE_TO_ASK: &str = "Those arguments are larger than a question shows whole, so your person cannot be asked about this call";
const CONNECTED: &str = "That service is connected already: it is in the list of connections";
/// What an agent that proposed a connection is told.
const PROPOSED: &str = "Your person is asked in your conversation with them whether to connect this service. Nothing is connected before they say yes there: tell them it waits for their answer.";
/// What an answer nothing waits under any more is refused with.
const NO_LONGER_WAITS: &str = "Nothing waits for that answer any more";
/// What an answer is refused with where the turn it would be carried out
/// for is not the asking agent's own for its person.
const NOT_THEIRS: &str =
    "That question is not this agent's to its own person any more, so nothing was done";
const NOT_NAMED: &str =
    "The agent that asked is no longer let near that connection, so nothing was called";

/// Proposing a connection: its person is asked, and nothing is added by the
/// call.
const PROPOSES: Verb = Verb {
    id: person_connections::PROPOSE,
    label: "Propose a connection",
    check: "",
    kind: ActionKind::Outside,
    params: address_params,
};

/// What a question about a connection shows beside its words, as the
/// question's detail spells it: a call, or the card of a service.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Shown {
    Call(AskedCall),
    Connection(Card),
}

/// What this server keeps while a person is asked about a connection.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Waits {
    shown: Shown,
    /// The words the question was put with.
    words: String,
    /// What named the command's definition when its person was asked; empty
    /// for a proposal.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    digest: String,
}

fn refused(reason: String) -> Refused {
    Refused::Refused { reason }
}

fn done<T: Serialize>(answered: T) -> Result<Taken, Refused> {
    serde_json::to_value(answered)
        .map(Taken::Done)
        .map_err(|error| refused(error.to_string()))
}

/// What `waits` asks the person of `using`.
fn asks(
    using: &AskedTurn,
    waits: &Waits,
    always: Option<Asked>,
    named: Words,
    answers: Option<Value>,
) -> Result<Taken, Refused> {
    let kept = serde_json::to_value(waits).map_err(|error| refused(error.to_string()))?;
    Ok(Taken::Asks(Box::new(Asks {
        person: using.person,
        words: waits.words.clone(),
        kept,
        always,
        named,
        answers,
    })))
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
    pub async fn connections_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Taken, Refused> {
        let using = self.asked_turn(turn).await?;
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
            return self.call_for_turn(turn, &using, at, call).await;
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
                        (&root, self.vault_at(&using.workspace)),
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
                self.call_for_turn(turn, &using, at, call).await
            }
            person_connections::PROPOSE => {
                narrowed(target)?;
                let proposed: AtAddress = argument(request, by_address)?;
                Self::propose_for_turn(&using, at, &proposed.address).await
            }
            _ => Err(Refused::UnknownAction),
        }
    }

    /// Call the command `call` names for the turn of `using`, or answer
    /// that its person is asked first.
    async fn call_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        using: &AskedTurn,
        (root, reach): (&Path, &Reach),
        call: Call,
    ) -> Result<Taken, Refused> {
        if !names(turn, &using.workspace, &call.connection) {
            return Err(Refused::NotPermitted);
        }
        let found =
            connections::callable(root, reach, &call.connection, &call.command).map_err(refused)?;
        if !found.asks {
            let by = Calling::Agent(&using.name);
            return done(
                connections::call((root, self.vault_at(&using.workspace)), reach, by, call)
                    .await
                    .map_err(refused)?,
            );
        }
        let always = person_connections::always_call(
            &using.workspace,
            &using.agent,
            &call.connection,
            (&call.command, &found.digest),
        );
        // What its person let stand is read at every call, so a permission
        // taken back covers nothing from that moment.
        if self
            .governed_permission_stands(&using.person, &always)
            .await
            .map_err(refused)?
        {
            let allowed = Calling::Allowed {
                by: &using.name,
                leave: Leave::Always,
                digest: &found.digest,
            };
            let called = connections::call(
                (root, self.vault_at(&using.workspace)),
                reach,
                allowed,
                call,
            )
            .await
            .map_err(refused)?;
            // What let it through is counted where its person reads it.
            if let Some(permission) = self.stands(&using.person, &always) {
                self.permission_used(&using.person, &permission);
            }
            return done(called);
        }
        // What a question could not show whole is refused before anybody
        // is asked.
        let sent =
            serde_json::to_string(&call.arguments).map_err(|error| refused(error.to_string()))?;
        if sent.len() > MAX_ASKED_BYTES {
            return Err(refused(TOO_LARGE_TO_ASK.to_owned()));
        }
        let words = format!(
            "May I call {} at {}? It changes something there. It is sent these arguments: {sent}",
            call.command, found.site
        );
        let named = Words {
            action: "Call a command of a connected service".to_owned(),
            on: format!("{} at {}", call.command, found.site),
            place: String::new(),
        };
        let shown = Shown::Call(AskedCall {
            connection: CalledAt {
                id: call.connection,
                address: found.address,
                name: found.name,
            },
            command: call.command,
            arguments: call.arguments,
        });
        let waits = Waits {
            shown,
            words,
            digest: found.digest,
        };
        asks(using, &waits, Some(always), named, None)
    }

    /// Look at the service at `address` and answer what the person of
    /// `using` is asked about connecting it. Nothing is kept.
    async fn propose_for_turn(
        using: &AskedTurn,
        (root, reach): (&Path, &Reach),
        address: &str,
    ) -> Result<Taken, Refused> {
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
        let card = Card {
            address: looked.address,
            name: looked.name,
            needs_sign_in: looked.needs_sign_in,
            sign_in_at: looked.sign_in_at,
            commands: looked
                .commands
                .as_ref()
                .map(|commands| u32::try_from(commands.len()).unwrap_or(u32::MAX)),
        };
        // The system's words alone: what the service calls itself is on
        // the card, as its own word.
        let mut words = format!("Connect the service at {site}?");
        match (card.commands, &card.sign_in_at) {
            (Some(commands), _) => {
                let _ = write!(words, " It lists {commands} commands.");
            }
            (None, Some(issuer)) if card.needs_sign_in => {
                let _ = write!(
                    words,
                    " It wants you to sign in at {issuer} before it lists its commands."
                );
            }
            (None, _) => words.push_str(" It wants you to sign in before it lists its commands."),
        }
        let named = Words {
            action: "Connect a service".to_owned(),
            on: site,
            place: String::new(),
        };
        let answers = serde_json::to_value(Proposed {
            connection: card.clone(),
            told: PROPOSED.to_owned(),
        })
        .map_err(|error| refused(error.to_string()))?;
        let waits = Waits {
            shown: Shown::Connection(card),
            words,
            digest: String::new(),
        };
        asks(using, &waits, None, named, Some(answers))
    }

    /// What `turn` finds of its person's connections in its listing: what an
    /// agent does with them, and each command of each connection it
    /// reaches, with whether it asks first. Read anew at every listing.
    /// Nothing for a turn this server does not carry out.
    pub async fn connection_rows_for_turn(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
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

    /// Carry out the person's answer to the question about `ask`, which
    /// this server kept for a connection: `leave` is how they allowed it,
    /// `None` where they did not. `turn` is the asking agent's turn as this
    /// server reads it now.
    ///
    /// The caller read the answer from what the person wrote themselves, to
    /// a question this server put, and claimed the ask first, so an answer
    /// is carried out once. What is done is what was kept under the ask: the
    /// call as it was shown, sent only while the service still defines the
    /// command as it did then, or the connection that was proposed, added
    /// as its person's own.
    ///
    /// # Errors
    /// Nothing of a connection is kept under that ask, the turn is not the
    /// asking agent's own for the person asked, the agent is no longer let
    /// near the connection, or the connections refuse it, in their own
    /// words.
    pub async fn connection_answered(
        &self,
        turn: &HostedTurn<'_>,
        ask: &KeptAsk,
        leave: Option<Leave>,
    ) -> Result<Option<Value>, String> {
        if ask.carrier != person_connections::CARRIER {
            return Err(NO_LONGER_WAITS.to_owned());
        }
        let waits: Waits =
            serde_json::from_value(ask.kept.clone()).map_err(|_| NO_LONGER_WAITS.to_owned())?;
        let using = self
            .asked_turn(turn)
            .await
            .map_err(|_| NOT_THEIRS.to_owned())?;
        if hex::encode(using.person) != ask.person
            || using.agent != ask.agent
            || using.workspace != ask.workspace
        {
            return Err(NOT_THEIRS.to_owned());
        }
        let Some(leave) = leave.filter(|leave| matches!(leave, Leave::Once | Leave::Always)) else {
            return Ok(None);
        };
        let root = self.workspace_root(&using.workspace);
        let reach = self.connections_reach();
        match waits.shown {
            Shown::Call(asked) => {
                if !names(turn, &using.workspace, &asked.connection.id) {
                    return Err(NOT_NAMED.to_owned());
                }
                let allowed = Calling::Allowed {
                    by: &using.name,
                    leave,
                    digest: &waits.digest,
                };
                let call = Call {
                    connection: asked.connection.id,
                    command: asked.command,
                    arguments: asked.arguments,
                };
                let expected = person_connections::always_call(
                    &using.workspace,
                    &using.agent,
                    &call.connection,
                    (&call.command, &waits.digest),
                );
                if leave == Leave::Always {
                    if ask.always.as_ref() != Some(&expected) {
                        return Err(NO_LONGER_WAITS.to_owned());
                    }
                    self.grant_always_in_workspace(&using.person, &expected)
                        .await?;
                } else {
                    self.governed_permission_stands(&using.person, &expected)
                        .await?;
                }
                let called = connections::call(
                    (&root, self.vault_at(&using.workspace)),
                    &reach,
                    allowed,
                    call,
                )
                .await?;
                serde_json::to_value(called)
                    .map(Some)
                    .map_err(|err| err.to_string())
            }
            // Their own yes is what adds it: the address this server looked
            // at when it asked, read again now.
            Shown::Connection(card) => connections::add(&root, &reach, &card.address)
                .await
                .map(|_| None),
        }
    }
}
