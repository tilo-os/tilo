//! The rooms this relay governs, as their own governance actions.
//!
//! A room's administration is its public surface like any app's: provider
//! [`ROOM_GOVERNANCE_PROVIDER`] (`tilo.workspace`) publishes it on the
//! room's governance resource (`workspace.permissions:<room>`), so an agent,
//! a script, the palette or a screen reader invites, lists and cancels
//! invitations, admits and declines join requests, sets roles, restricts and
//! unbans members and runs calls over `Envelope::RemoteAction`, without the
//! `tilo-room-admin/1` and `/rooms` doors those callers cannot speak. The
//! desktop administers the rooms it holds this way too.
//!
//! Beside it, two providers carry what is not one room's governance
//! (`tilo_relay_session::rooms` names them):
//!
//! - the host's own operations ([`HOST_ACTIONS_PROVIDER`], `host`):
//!   creating, listing and entering rooms and letting an app author one,
//!   which only devices of the relay's own person may, decided as
//!   [`Rooms::authorize`] decides every [`RoomOp::Host`]. They are asked on any workspace the device holds on
//!   the relay (its person's continuity workspace, before it holds a room),
//!   and need the device's certificate as the relay's handshake verified it,
//!   which entering admits to the room. Creating a room may issue its first
//!   invitation and entering answers the invitation the device redeems, so
//!   a desktop shares a workspace without any governance before it holds
//!   the room.
//! - a member's own business in a room ([`MEMBER_ACTIONS_PROVIDER`],
//!   `member`): claiming their topology namespace and recording their own
//!   effect receipt, decided by the room methods the wire door called.
//!
//! Nothing here decides who may. Every invocation calls the [`Rooms`] method
//! its wire door calls, and with it [`Rooms::authorize`]; the catalogue asks
//! the same decision ([`Rooms::permits`]) so an action the caller may not
//! take is absent, and invoking one is the same `UnknownAction` as invoking
//! one that does not exist.
//!
//! Removing or banning a member and ending a call are destructive: they run
//! only on a confirmation the person gave on their own device, which this
//! host cannot yet ask for. They are left out of the catalogue, and a caller
//! the room would let take one is refused with
//! [`ActionHostError::ConfirmationRequired`]; a caller's own `confirm` is
//! never taken for the person's. Until then removing a member stays on
//! `tilo-room-admin/1` and the `/rooms` pages.
//!
//! What the host does for its embedder is unchanged: [`GovernedPlane`]
//! answers this provider and hands every other request to the app-serving
//! plane the embedder supplied, if any.

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value, json};
use tilo_action_policy::roles::{self, governance};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CallerPrincipal, CataloguePage, CatalogueProvider,
    CatalogueQuery, CatalogueResource, CatalogueRow, InvocationLedger, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ROOM_GOVERNANCE_KIND,
    ROOM_GOVERNANCE_PROVIDER, RemoteActionMessage, room_governance_resource,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ActionOutput, ActionParam,
    ExecutionLocus, InputSpec, InvocationId, InvocationUpdate, OperationKind, ParamType, RiskLevel,
};
use tilo_peer_state::join::MemberRole;
use tilo_relay_protocol::voice::{CallOperation, Directions};
use tilo_relay_session::rooms::{HOST_ACTIONS_PROVIDER, MEMBER_ACTIONS_PROVIDER};
use tilo_user_identity::DeviceCert;
use tokio::sync::Mutex as AsyncMutex;

mod host;
mod member;

use super::authority::RoomOp;
use super::{
    CancelInvitationRequest, CollaborationError, GovernedRoom, IssueInvitationRequest,
    RemoveMemberRequest, Rooms, SetMemberRoleRequest,
};
use crate::app_serving::AppServingPlane;
use tilo_lock::MutexRecover as _;

/// Listing the invitations still redeemable: part of inviting.
pub(crate) const INVITATIONS: &str = "invitation.list";
/// Cancelling an unredeemed invitation: part of inviting.
pub(crate) const CANCEL_INVITATION: &str = "invitation.cancel";
/// Listing the join requests waiting for someone to admit them.
pub(crate) const JOIN_REQUESTS: &str = "join-request.list";
/// Admitting a waiting join request, with the role its invitation carries.
pub(crate) const ADMIT: &str = "join-request.admit";
/// Declining a waiting join request.
pub(crate) const DECLINE: &str = "join-request.decline";
/// Listing the room as a public workspace, with its description and the
/// role everyone who joins gets.
pub(crate) const PUBLISH: &str = "publish";
/// Taking the room out of the directory.
pub(crate) const UNPUBLISH: &str = "unpublish";
/// Whether the room is public, and with which listing.
pub(crate) const PUBLICATION: &str = "publication";
/// Asking the room's host to run a bot from the directory.
pub(crate) const REQUEST_BOT: &str = "request-bot";
/// Taking back the room's request for a bot: its host stops and
/// uninstalls it.
pub(crate) const WITHDRAW_BOT_REQUEST: &str = "withdraw-bot-request";
/// Giving the host a secret a requested bot's connector needs (a Discord
/// bot token): kept in the host's secret store for that connector, never in
/// the room's governance, a record or a log.
pub(crate) const SET_CONNECTOR_SECRET: &str = "set-connector-secret";
/// The connector secrets a room member may give its host, by connector and
/// purpose.
const CONNECTOR_SECRETS: [(&str, &str); 1] = [("so.tilo.discord", "bot-token")];
/// Most bytes a connector secret takes.
const MAX_CONNECTOR_SECRET: usize = 512;
/// The roles a public workspace hands out at its door.
const DOOR_ROLES: [&str; 3] = ["viewer", "commenter", "editor"];

/// How long an invitation stays redeemable when the caller does not say.
const DEFAULT_INVITATION_HOURS: u64 = 7 * 24;
/// How long a restriction lasts when the caller does not say.
const DEFAULT_RESTRICTION_MINUTES: u64 = 60;
const HOUR_NS: u64 = 3_600 * 1_000_000_000;
const SECOND_NS: u64 = 1_000_000_000;
const MINUTE_MS: u64 = 60 * 1_000;

/// Derives each invocation's idempotency key, so a retry of one invocation
/// is the same request to the room's request records.
const REQUEST_ID_CONTEXT: &str = "tilo-relay room governance action request v1";

const ROLES: [&str; 4] = ["viewer", "commenter", "editor", "moderator"];
const CALL_OPERATIONS: [&str; 4] = ["start", "restart", "set-directions", "set-denial"];
/// The call operation that is destructive, and so not served yet.
const END_CALL: &str = "end";

/// One action a governed room publishes.
struct Verb {
    id: &'static str,
    label: &'static str,
    /// The room governance action [`Rooms::authorize`] gates it on, and the
    /// catalogue offers it by.
    gate: &'static str,
    kind: ActionKind,
    params: fn() -> Vec<ActionParam>,
}

/// What the rooms serve, in catalogue order.
const VERBS: [Verb; 17] = [
    Verb {
        id: governance::INVITE,
        label: "Invite",
        gate: governance::INVITE,
        kind: roles::GOVERNANCE_KIND,
        params: invite_params,
    },
    Verb {
        id: INVITATIONS,
        label: "List invitations",
        gate: governance::INVITE,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: CANCEL_INVITATION,
        label: "Cancel invitation",
        gate: governance::INVITE,
        kind: roles::GOVERNANCE_KIND,
        params: invitation_params,
    },
    Verb {
        id: JOIN_REQUESTS,
        label: "List join requests",
        gate: governance::INVITE,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: ADMIT,
        label: "Admit",
        gate: governance::INVITE,
        kind: roles::GOVERNANCE_KIND,
        params: join_request_params,
    },
    Verb {
        id: DECLINE,
        label: "Decline",
        gate: governance::INVITE,
        kind: roles::GOVERNANCE_KIND,
        params: join_request_params,
    },
    Verb {
        id: governance::SET_ROLE,
        label: "Set role",
        gate: governance::SET_ROLE,
        kind: roles::GOVERNANCE_KIND,
        params: set_role_params,
    },
    Verb {
        id: governance::RESTRICT,
        label: "Restrict",
        gate: governance::RESTRICT,
        kind: roles::GOVERNANCE_KIND,
        params: restrict_params,
    },
    Verb {
        id: governance::UNRESTRICT,
        label: "Lift restriction",
        gate: governance::UNRESTRICT,
        kind: roles::GOVERNANCE_KIND,
        params: person_params,
    },
    Verb {
        id: governance::UNBAN,
        label: "Unban",
        gate: governance::UNBAN,
        kind: roles::GOVERNANCE_KIND,
        params: person_params,
    },
    Verb {
        id: governance::CALL,
        label: "Call",
        gate: governance::CALL,
        kind: roles::GOVERNANCE_KIND,
        params: call_params,
    },
    Verb {
        id: PUBLISH,
        label: "Make public",
        gate: governance::SETTINGS,
        kind: roles::GOVERNANCE_KIND,
        params: publish_params,
    },
    Verb {
        id: UNPUBLISH,
        label: "Make private",
        gate: governance::SETTINGS,
        kind: roles::GOVERNANCE_KIND,
        params: Vec::new,
    },
    Verb {
        id: PUBLICATION,
        label: "Public listing",
        gate: governance::SETTINGS,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: REQUEST_BOT,
        label: "Add a bot",
        gate: governance::INSTALL_BOT,
        kind: roles::GOVERNANCE_KIND,
        params: bot_request_params,
    },
    Verb {
        id: WITHDRAW_BOT_REQUEST,
        label: "Remove a bot",
        gate: governance::UNINSTALL_BOT,
        kind: roles::GOVERNANCE_KIND,
        params: bot_entry_params,
    },
    Verb {
        id: SET_CONNECTOR_SECRET,
        label: "Give a bot's connector its secret",
        gate: governance::INSTALL_BOT,
        kind: ActionKind::Outside,
        params: connector_secret_params,
    },
];

/// Destructive governance this provider names but does not run until the
/// person's own confirmation can be asked for.
const UNCONFIRMED: [&str; 2] = [governance::REMOVE_MEMBER, governance::BAN];

fn person_param() -> ActionParam {
    ActionParam::required(
        "person",
        ParamType::Text,
        "The member's person: their user root key, 64 hex characters.",
    )
}

fn role_param() -> ActionParam {
    ActionParam::required("role", ParamType::Text, "The role.").with_choices(ROLES)
}

fn invite_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "role",
            ParamType::Text,
            "The role the invited person gets; never above the inviter's own.",
        )
        .with_choices(ROLES),
        ActionParam::optional(
            "recipient",
            ParamType::Text,
            "Only this person (user root key, 64 hex characters) may redeem it; anyone holding it when omitted.",
        ),
        ActionParam::optional(
            "invitee_name",
            ParamType::Text,
            "How the invitation names its recipient to them.",
        ),
        ActionParam::optional(
            "inviter_name",
            ParamType::Text,
            "The name the inviting person gives, recorded in the invitation.",
        ),
        ActionParam::optional(
            "expires_in_hours",
            ParamType::Integer,
            "How long it stays redeemable: 168 hours (7 days) when omitted, at most 720.",
        ),
        ActionParam::optional(
            "expires_in_seconds",
            ParamType::Integer,
            "How long it stays redeemable, in seconds, when finer than hours; takes precedence over expires_in_hours. At most 30 days.",
        ),
    ]
}

fn invitation_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "invite",
        ParamType::Text,
        "The invitation's id, 64 hex characters, as listing invitations reports it.",
    )]
}

fn join_request_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "request",
        ParamType::Text,
        "The join request's id, 64 hex characters, as listing join requests reports it.",
    )]
}

fn set_role_params() -> Vec<ActionParam> {
    vec![person_param(), role_param()]
}

fn person_params() -> Vec<ActionParam> {
    vec![person_param()]
}

fn restrict_params() -> Vec<ActionParam> {
    vec![
        person_param(),
        ActionParam::optional(
            "minutes",
            ParamType::Integer,
            "How long they may change nothing; 60 minutes when omitted. They keep reading.",
        ),
    ]
}

fn bot_entry_param() -> ActionParam {
    ActionParam::required(
        "entry",
        ParamType::Text,
        "The bot's entry in the registry's directory, such as so.tilo.bots.standup.",
    )
}

fn bot_request_params() -> Vec<ActionParam> {
    vec![
        bot_entry_param(),
        ActionParam::required(
            "version",
            ParamType::Text,
            "The listed version the room's host runs.",
        ),
        ActionParam::optional(
            "inputs",
            ParamType::Object,
            "The bot's settings, by name, such as a bridge's guild and server; never a secret.",
        ),
    ]
}

fn connector_secret_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "app",
            ParamType::Text,
            "The connector the secret is for, such as so.tilo.discord.",
        ),
        ActionParam::required(
            "purpose",
            ParamType::Text,
            "What the secret is, such as bot-token.",
        ),
        ActionParam::required("secret", ParamType::Secret, "The secret itself."),
    ]
}

fn bot_entry_params() -> Vec<ActionParam> {
    vec![bot_entry_param()]
}

fn publish_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "default_role",
            ParamType::Text,
            "The role everyone who joins from the directory gets.",
        )
        .with_choices(DOOR_ROLES),
        ActionParam::optional(
            "description",
            ParamType::Text,
            "What the workspace is, in a sentence or two (at most 280 characters).",
        ),
    ]
}

fn call_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("operation", ParamType::Text, "What to do with the call.")
            .with_choices(CALL_OPERATIONS),
        ActionParam::optional(
            "call",
            ParamType::Text,
            "The call, 64 hex characters; every operation but start.",
        ),
        ActionParam::optional(
            "epoch",
            ParamType::Integer,
            "The call's epoch the caller last saw; every operation but start.",
        ),
        ActionParam::optional(
            "app",
            ParamType::Text,
            "Start: the chat app holding the voice channel.",
        ),
        ActionParam::optional(
            "record",
            ParamType::Text,
            "Start: the chat record holding the voice channel.",
        ),
        ActionParam::optional("channel", ParamType::Text, "Start: the voice channel."),
        ActionParam::optional(
            "publish",
            ParamType::Boolean,
            "Start, set-directions: participants may send audio. Set-denial: the person may not. False when omitted.",
        ),
        ActionParam::optional(
            "subscribe",
            ParamType::Boolean,
            "Start, set-directions: participants hear each other. Set-denial: the person may not. False when omitted.",
        ),
        ActionParam::optional(
            "person",
            ParamType::Text,
            "Set-denial: the person denied, user root key, 64 hex characters.",
        ),
    ]
}

/// Where a provider publishes its verbs: the provider and the kind of
/// resource it publishes them on.
#[derive(Clone, Copy)]
struct Publisher {
    provider: &'static str,
    kind: &'static str,
}

/// A room's own governance.
const GOVERNANCE: Publisher = Publisher {
    provider: ROOM_GOVERNANCE_PROVIDER,
    kind: ROOM_GOVERNANCE_KIND,
};

impl Publisher {
    /// The catalogue entry naming this provider, offered on `room` (empty
    /// for the host's own).
    fn entry(self, room: &str) -> CatalogueProvider {
        CatalogueProvider {
            provider: self.provider.into(),
            app_id: self.provider.into(),
            room: room.to_owned(),
            record_id: String::new(),
            browsable: false,
        }
    }
}

impl Verb {
    fn row(&self, publisher: Publisher, resource: &str, label: &str) -> CatalogueRow {
        let params = (self.params)();
        CatalogueRow {
            provider: publisher.provider.into(),
            app_id: publisher.provider.into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: resource.into(),
                kind: publisher.kind.into(),
                label: label.into(),
                presentation: None,
            },
            action: Some(ActionDescriptor {
                semantic_identity: None,
                address: ActionAddress::new(resource, self.id),
                resource_kind: publisher.kind.into(),
                resource_label: label.into(),
                action_label: self.label.into(),
                description: None,
                locus: ExecutionLocus::ServerPreferred,
                kind: OperationKind::Effect,
                contract: ActionContract {
                    kind: self.kind,
                    risk: if self.kind == ActionKind::Read {
                        RiskLevel::None
                    } else {
                        RiskLevel::Privileged
                    },
                    ..ActionContract::default()
                },
                input: InputSpec::None,
                params,
                view: None,
            }),
            has_children: false,
            visibility: None,
        }
    }
}

/// The governance action `action` is gated on, for one this provider names.
fn gate_of(action: &str) -> Option<&'static str> {
    VERBS
        .iter()
        .find(|verb| verb.id == action)
        .map(|verb| verb.gate)
        .or_else(|| UNCONFIRMED.into_iter().find(|verb| *verb == action))
}

/// Whether `action` with `args` needs the person's own confirmation.
fn destructive(action: &str, args: &Args) -> bool {
    UNCONFIRMED.contains(&action)
        || (action == governance::CALL && args.text("operation").ok().flatten() == Some(END_CALL))
}

fn invalid(reason: impl Into<String>) -> ActionHostError {
    ActionHostError::InvalidRequest {
        reason: reason.into(),
    }
}

fn refused(error: &impl ToString) -> ActionHostError {
    ActionHostError::Refused {
        reason: error.to_string(),
    }
}

/// Keep the secret a requested bot's connector needs in this host's store
/// for that connector, where its worker asks for it. Only the secrets a
/// connector is known to take are kept; the caller already holds the
/// room's right to install bots (the verb's gate).
fn connector_secret(args: &Args) -> Result<Value, ActionHostError> {
    let app = args.required_text("app")?;
    let purpose = args.required_text("purpose")?;
    let secret = args.required_text("secret")?.trim();
    let (_, purpose) = CONNECTOR_SECRETS
        .iter()
        .copied()
        .find(|(known_app, known_purpose)| *known_app == app && *known_purpose == purpose)
        .ok_or_else(|| invalid(format!("{app} takes no secret named {purpose}")))?;
    if secret.is_empty() || secret.len() > MAX_CONNECTOR_SECRET {
        return Err(invalid("the secret is empty or too long"));
    }
    #[cfg(feature = "agents")]
    {
        crate::room_bots::installed::put_connector_secret(app, purpose, secret.as_bytes())
            .map_err(|error| refused(&format!("the host could not keep the secret: {error}")))?;
        Ok(json!({ "kept": true }))
    }
    #[cfg(not(feature = "agents"))]
    {
        let _ = purpose;
        Err(refused(&"this host runs no connectors"))
    }
}

/// An invocation's typed arguments.
struct Args(Map<String, Value>);

impl Args {
    fn of(argument: &InvokeArgument) -> Result<Self, ActionHostError> {
        match argument {
            InvokeArgument::None => Ok(Self(Map::new())),
            InvokeArgument::Json(object) => Ok(Self(object.clone())),
            InvokeArgument::Text(_) => Err(invalid("room governance takes named arguments")),
        }
    }

    fn text(&self, name: &str) -> Result<Option<&str>, ActionHostError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(invalid(format!("`{name}` must be text"))),
        }
    }

    /// An object of text values, by name; empty when absent.
    fn text_map(
        &self,
        name: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, ActionHostError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(std::collections::BTreeMap::new()),
            Some(Value::Object(object)) => object
                .iter()
                .map(|(key, value)| match value {
                    Value::String(text) => Ok((key.clone(), text.clone())),
                    _ => Err(invalid(format!("`{name}.{key}` must be text"))),
                })
                .collect(),
            Some(_) => Err(invalid(format!("`{name}` must be an object of text"))),
        }
    }

    fn required_text(&self, name: &str) -> Result<&str, ActionHostError> {
        self.text(name)?
            .ok_or_else(|| invalid(format!("`{name}` is required")))
    }

    fn key(&self, name: &str) -> Result<Option<[u8; 32]>, ActionHostError> {
        self.text(name)?
            .map(|text| {
                let mut key = [0; 32];
                hex::decode_to_slice(text, &mut key)
                    .map_err(|_| invalid(format!("`{name}` must be 64 hex characters")))?;
                Ok(key)
            })
            .transpose()
    }

    fn required_key(&self, name: &str) -> Result<[u8; 32], ActionHostError> {
        self.key(name)?
            .ok_or_else(|| invalid(format!("`{name}` is required")))
    }

    fn integer(&self, name: &str) -> Result<Option<u64>, ActionHostError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .map(Some)
                .ok_or_else(|| invalid(format!("`{name}` must be a whole number"))),
        }
    }

    fn flag(&self, name: &str) -> Result<bool, ActionHostError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(_) => Err(invalid(format!("`{name}` must be true or false"))),
        }
    }

    fn role(&self) -> Result<MemberRole, ActionHostError> {
        self.role_at("role")
    }

    /// The role the argument `name` names.
    fn role_at(&self, name: &str) -> Result<MemberRole, ActionHostError> {
        match self.required_text(name)? {
            "viewer" => Ok(MemberRole::Viewer),
            "commenter" => Ok(MemberRole::Commenter),
            "editor" => Ok(MemberRole::Editor),
            "moderator" => Ok(MemberRole::Moderator),
            other => Err(invalid(format!("no role `{other}`"))),
        }
    }

    fn directions(&self) -> Result<Directions, ActionHostError> {
        Ok(Directions {
            publish: self.flag("publish")?,
            subscribe: self.flag("subscribe")?,
        })
    }

    fn call(&self) -> Result<CallOperation, ActionHostError> {
        let call = || self.required_key("call");
        let epoch = || {
            self.integer("epoch")?
                .ok_or_else(|| invalid("`epoch` is required"))
        };
        match self.required_text("operation")? {
            "start" => Ok(CallOperation::Start {
                app_id: self.required_text("app")?.to_owned(),
                record_id: self.required_text("record")?.to_owned(),
                channel_id: self.required_text("channel")?.to_owned(),
                allowed: self.directions()?,
            }),
            "restart" => Ok(CallOperation::Restart {
                call: call()?,
                epoch: epoch()?,
            }),
            "set-directions" => Ok(CallOperation::SetDirections {
                call: call()?,
                epoch: epoch()?,
                allowed: self.directions()?,
            }),
            "set-denial" => Ok(CallOperation::SetDenial {
                call: call()?,
                epoch: epoch()?,
                person: self.required_key("person")?,
                denied: self.directions()?,
            }),
            other => Err(invalid(format!("no call operation `{other}`"))),
        }
    }
}

const fn role_name(role: MemberRole) -> &'static str {
    match role {
        MemberRole::Viewer => "viewer",
        MemberRole::Commenter => "commenter",
        MemberRole::Editor => "editor",
        MemberRole::Moderator => "moderator",
    }
}

/// A pending invitation, as listing invitations reports it.
fn pending_json(invitation: &super::PendingInvitation) -> Value {
    json!({
        "invite": hex::encode(invitation.invite_id),
        "role": role_name(invitation.role),
        "recipient": invitation.recipient_user_public_key.map(hex::encode),
        "invitee_name": invitation.invitee_name,
        "expires_at_ns": invitation.expires_at_ns,
    })
}

/// A waiting join request, as listing join requests reports it.
fn join_json(join: &tilo_relay_protocol::rooms::JoinRequest) -> Value {
    json!({
        "request": hex::encode(join.request_id),
        "person": hex::encode(join.user_public_key),
        "device": hex::encode(join.device_public_key),
        "name": join.name,
        "role": role_name(super::pages::member_role(join.role)),
        "requested_at_ns": join.requested_at_ns,
    })
}

/// The idempotency key of invocation `invocation` by the person `person`:
/// a retry of it is a retry to the room's request records, which answer it
/// with what the first attempt did.
fn request_id(person: &[u8; 32], invocation: InvocationId) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(REQUEST_ID_CONTEXT);
    hasher.update(person);
    hasher.update(invocation.0.as_bytes());
    *hasher.finalize().as_bytes()
}

/// A governed room, locked.
type Governed = Arc<AsyncMutex<GovernedRoom>>;

/// Whether `caller` is a person acting themselves: an agent or a bot
/// governs nothing and holds no room's business of its own.
fn speaks_for_themselves(caller: &Caller) -> bool {
    matches!(caller.principal(), CallerPrincipal::Person(_))
}

/// What a catalogue offers its caller, before it is paged.
#[derive(Default)]
struct Offer {
    /// The governance generation of the room it is about, if any: the
    /// page's revision.
    generation: u64,
    rows: Vec<CatalogueRow>,
    providers: Vec<CatalogueProvider>,
}

impl Offer {
    /// Offer `rows`, which `publisher` publishes on `room`; a provider
    /// offering nothing is not named.
    fn add(&mut self, publisher: Publisher, room: &str, rows: Vec<CatalogueRow>) {
        if !rows.is_empty() {
            self.providers.push(publisher.entry(room));
            self.rows.extend(rows);
        }
    }

    fn page(self, query: &CatalogueQuery) -> CataloguePage {
        let mut page = CataloguePage::from_rows(self.generation.to_string(), self.rows, query);
        if query.provider.is_none() {
            page.providers = self.providers;
        }
        page
    }
}

impl Rooms {
    /// The room `workspace_id` carries, and the certificate of `caller`'s
    /// device in it: the one the room's governance certified, or the relay's
    /// own. `None` for a workspace that carries no governed room, a device
    /// the room never certified, and any caller but a person acting
    /// themselves: an agent or a bot governs nothing.
    async fn governed_by(
        &self,
        workspace_id: u64,
        caller: &Caller,
    ) -> Option<(Governed, DeviceCert)> {
        if !speaks_for_themselves(caller) {
            return None;
        }
        let (user, device) = (caller.user_public_key()?, caller.device_public_key()?);
        let rooms = self
            .inner
            .rooms
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for governed in rooms {
            let room = governed.lock().await;
            if room.workspace_id() != workspace_id {
                continue;
            }
            let host = &self.inner.host.cert;
            let cert = room
                .peer()
                .certificates()
                .into_iter()
                .find(|cert| cert.device_public_key == *device)
                .or_else(|| (host.device_public_key == *device).then(|| host.clone()))?;
            drop(room);
            return (cert.user_public_key == *user).then_some((governed, cert));
        }
        None
    }

    /// What `caller`, on the device `certificate` when the transport named
    /// it, may do here about workspace `workspace_id`: the governance of
    /// the room it carries, their own business in that room, and the host's
    /// operations when they are its person's.
    async fn catalogue(
        &self,
        workspace_id: u64,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        query: &CatalogueQuery,
    ) -> CataloguePage {
        let wants = |provider: &str| {
            query
                .provider
                .as_deref()
                .is_none_or(|wanted| wanted == provider)
        };
        let mut offer = Offer::default();
        if (wants(ROOM_GOVERNANCE_PROVIDER) || wants(MEMBER_ACTIONS_PROVIDER))
            && let Some((governed, device)) = self.governed_by(workspace_id, caller).await
        {
            let room = governed.lock().await;
            offer.generation = room.record.generation;
            if wants(ROOM_GOVERNANCE_PROVIDER) {
                let now = tilo_clock::unix_now_ns();
                let resource = room_governance_resource(room.room());
                let rows = VERBS
                    .iter()
                    .filter(|verb| {
                        self.permits(&device, Some(&room), RoomOp::Govern(verb.gate), now)
                    })
                    .map(|verb| verb.row(GOVERNANCE, &resource, &room.record.name))
                    .collect();
                offer.add(GOVERNANCE, room.room(), rows);
            }
            if wants(MEMBER_ACTIONS_PROVIDER) {
                offer.add(member::MEMBER, room.room(), member::rows(&room));
            }
        }
        if wants(HOST_ACTIONS_PROVIDER)
            && let Some(certificate) = certificate.filter(|_| speaks_for_themselves(caller))
        {
            offer.add(host::HOST, "", self.host_rows(certificate));
        }
        offer.page(query)
    }

    /// Run `request` from `caller`, on the device `certificate` when the
    /// transport named it, by the provider it names.
    async fn invoke(
        &self,
        workspace_id: u64,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        match request.target.provider.as_str() {
            ROOM_GOVERNANCE_PROVIDER => self.governance_invoke(workspace_id, caller, request).await,
            MEMBER_ACTIONS_PROVIDER => self.member_invoke(workspace_id, caller, request).await,
            HOST_ACTIONS_PROVIDER => {
                // Unknown and forbidden are the same answer.
                let certificate = certificate
                    .filter(|certificate| {
                        speaks_for_themselves(caller)
                            && caller.device_public_key() == Some(&certificate.device_public_key)
                            && caller.user_public_key() == Some(&certificate.user_public_key)
                    })
                    .ok_or(ActionHostError::UnknownAction)?;
                self.host_invoke(certificate, request).await
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }

    /// Run `request` from `caller` against the room `workspace_id` carries,
    /// through the room method its wire door calls.
    async fn governance_invoke(
        &self,
        workspace_id: u64,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        let address = &request.target.address;
        let gate = gate_of(&address.action).ok_or(ActionHostError::UnknownAction)?;
        let (governed, device) = self
            .governed_by(workspace_id, caller)
            .await
            .ok_or(ActionHostError::UnknownAction)?;
        let room = {
            let room = governed.lock().await;
            // Unknown and forbidden are the same answer.
            if address.resource != room_governance_resource(room.room())
                || !self.permits(
                    &device,
                    Some(&room),
                    RoomOp::Govern(gate),
                    tilo_clock::unix_now_ns(),
                )
            {
                return Err(ActionHostError::UnknownAction);
            }
            room.room().to_owned()
        };
        let args = Args::of(&request.argument)?;
        if destructive(&address.action, &args) {
            return Err(ActionHostError::ConfirmationRequired);
        }
        let call = GovernanceCall {
            room: &room,
            device: &device,
            request_id: request_id(&device.user_public_key, request.invocation),
            args: &args,
        };
        self.governance_run(&address.action, call).await
    }

    async fn governance_run(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        match action {
            governance::INVITE
            | INVITATIONS
            | CANCEL_INVITATION
            | JOIN_REQUESTS
            | ADMIT
            | DECLINE => self.governance_invitations(action, call).await,
            PUBLISH | UNPUBLISH | PUBLICATION => self.governance_listing(action, call).await,
            REQUEST_BOT | WITHDRAW_BOT_REQUEST => self.governance_bots(action, call).await,
            SET_CONNECTOR_SECRET => connector_secret(call.args),
            _ => self.governance_members(action, call).await,
        }
    }

    /// Making the room a public workspace, taking it back, and what it is
    /// public with: `{"public": bool, "description"?, "default_role"?}`.
    async fn governance_listing(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        let GovernanceCall {
            room, device, args, ..
        } = call;
        let listed = |listing: Option<tilo_relay_protocol::directory::Listing>| match listing {
            Some(listing) => json!({
                "public": true,
                "description": listing.description,
                "default_role": role_name(super::pages::member_role(listing.default_role)),
            }),
            None => json!({ "public": false }),
        };
        match action {
            PUBLISH => {
                let default_role = match args.required_text("default_role")? {
                    "viewer" => tilo_relay_protocol::rooms::RoomRole::Viewer,
                    "commenter" => tilo_relay_protocol::rooms::RoomRole::Commenter,
                    "editor" => tilo_relay_protocol::rooms::RoomRole::Editor,
                    other => {
                        return Err(invalid(format!(
                            "a public workspace hands out viewer, commenter or editor, not `{other}`"
                        )));
                    }
                };
                let listing = tilo_relay_protocol::directory::Listing {
                    description: args.text("description")?.unwrap_or_default().to_owned(),
                    default_role,
                };
                self.publish(room, device, listing)
                    .await
                    .map(|listing| listed(Some(listing)))
                    .map_err(|error| refused(&error))
            }
            UNPUBLISH => self
                .unpublish(room, device)
                .await
                .map(|()| listed(None))
                .map_err(|error| refused(&error)),
            _ => Ok(listed(self.listing(room))),
        }
    }

    /// The bots the room asks its host to run: asking for one, and taking
    /// the request back.
    async fn governance_bots(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        use tilo_peer_state::workspace_membership::{BotRequest, WorkspaceMembershipCommand};
        let GovernanceCall {
            room,
            device,
            request_id,
            args,
        } = call;
        let entry = args.required_text("entry")?.to_owned();
        let command = if action == REQUEST_BOT {
            WorkspaceMembershipCommand::RequestBot(BotRequest {
                entry,
                version: args.required_text("version")?.to_owned(),
                inputs: args.text_map("inputs")?,
            })
        } else {
            WorkspaceMembershipCommand::WithdrawBotRequest { entry }
        };
        command.validate().map_err(invalid)?;
        let done = self
            .ask_for_bot(room, device, request_id, command)
            .await
            .map_err(|error| refused(&error))?;
        Ok(json!({ "generation": done.generation }))
    }

    /// Inviting, and what inviting covers: the invitations and join
    /// requests.
    async fn governance_invitations(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        let GovernanceCall {
            room,
            device,
            request_id,
            args,
        } = call;
        let now = tilo_clock::unix_now_ns();
        match action {
            governance::INVITE => {
                let lifetime_ns = match args.integer("expires_in_seconds")? {
                    Some(seconds) => seconds.saturating_mul(SECOND_NS),
                    None => args
                        .integer("expires_in_hours")?
                        .unwrap_or(DEFAULT_INVITATION_HOURS)
                        .saturating_mul(HOUR_NS),
                };
                let issued = self
                    .issue(
                        room,
                        device,
                        IssueInvitationRequest {
                            request_id,
                            role: args.role()?,
                            recipient_user_public_key: args.key("recipient")?,
                            invitee_name: args.text("invitee_name")?.map(str::to_owned),
                            expires_at_ns: now.saturating_add(lifetime_ns),
                            inviter_name: args.text("inviter_name")?.map(str::to_owned),
                        },
                    )
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(json!({
                    "invite": hex::encode(issued.invite_id),
                    "invitation": hex::encode(issued.signed_invitation),
                }))
            }
            INVITATIONS => {
                let pending = self
                    .pending(room, device)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(pending.iter().map(pending_json).collect())
            }
            CANCEL_INVITATION => {
                self.cancel(
                    room,
                    device,
                    CancelInvitationRequest {
                        request_id,
                        invite_id: args.required_key("invite")?,
                    },
                )
                .await
                .map_err(|error| refused(&error))?;
                Ok(json!({}))
            }
            JOIN_REQUESTS => {
                let waiting = self
                    .join_requests(room, device, now)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(waiting.iter().map(join_json).collect())
            }
            ADMIT => {
                self.admit_page(room, device, args.required_key("request")?, now)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(json!({}))
            }
            DECLINE => {
                self.decline_page(room, device, args.required_key("request")?, now)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(json!({}))
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }

    /// Who holds which role, and the room's calls.
    async fn governance_members(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        let GovernanceCall {
            room,
            device,
            request_id,
            args,
        } = call;
        let member = |person| RemoveMemberRequest {
            request_id,
            user_public_key: person,
        };
        let generation = |done: Result<super::GovernanceCompletion, CollaborationError>| {
            done.map(|done| json!({ "generation": done.generation }))
                .map_err(|error| refused(&error))
        };
        match action {
            governance::SET_ROLE => {
                let request = SetMemberRoleRequest {
                    request_id,
                    user_public_key: args.required_key("person")?,
                    role: args.role()?,
                };
                generation(self.set_member_role(room, device, request).await)
            }
            governance::RESTRICT => {
                let minutes = args
                    .integer("minutes")?
                    .unwrap_or(DEFAULT_RESTRICTION_MINUTES);
                let until_ms =
                    tilo_clock::unix_now_ms().saturating_add(minutes.saturating_mul(MINUTE_MS));
                let person = member(args.required_key("person")?);
                generation(self.restrict_member(room, device, person, until_ms).await)
            }
            governance::UNRESTRICT => {
                let person = member(args.required_key("person")?);
                generation(self.unrestrict_member(room, device, person).await)
            }
            governance::UNBAN => {
                let person = member(args.required_key("person")?);
                generation(self.unban_member(room, device, person).await)
            }
            governance::CALL => {
                let state = self
                    .voice_call_as(room, device, &args.call()?)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(json!({
                    "call": hex::encode(state.call),
                    "epoch": state.epoch,
                    "active": state.active,
                    "publish": state.allowed.publish,
                    "subscribe": state.allowed.subscribe,
                }))
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }
}

/// One governance invocation, as its room method takes it.
struct GovernanceCall<'a> {
    room: &'a str,
    device: &'a DeviceCert,
    request_id: [u8; 32],
    args: &'a Args,
}

/// The rooms' governance as an action host, attached once the rooms open.
#[derive(Clone)]
pub(crate) struct HostRoomActions {
    shared: Arc<Shared>,
}

struct Shared {
    rooms: OnceLock<Rooms>,
    runtime: tokio::runtime::Handle,
    /// What each admitted invocation came to.
    ledger: Mutex<InvocationLedger<InvocationState>>,
}

impl HostRoomActions {
    /// Governance for rooms [`attach`](Self::attach)ed later. Call it inside
    /// the runtime the relay runs on.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                rooms: OnceLock::new(),
                runtime: tokio::runtime::Handle::current(),
                ledger: Mutex::new(InvocationLedger::default()),
            }),
        }
    }

    /// Serve `rooms`; only the first rooms attached are served.
    pub(crate) fn attach(&self, rooms: Rooms) {
        let _ = self.shared.rooms.set(rooms);
    }

    /// This host for requests about workspace `workspace_id`, from a caller
    /// whose device certificate the transport did not name: everything but
    /// the host's own operations.
    #[must_use]
    pub(crate) fn in_workspace(
        &self,
        workspace_id: u64,
    ) -> impl ActionHost + Clone + Send + 'static {
        InWorkspace {
            host: self.clone(),
            workspace_id,
            certificate: None,
        }
    }

    /// This host for requests about workspace `workspace_id`, from the
    /// device whose certificate the relay's handshake verified as
    /// `certificate`.
    #[must_use]
    pub(crate) fn in_workspace_as(
        &self,
        workspace_id: u64,
        certificate: &DeviceCert,
    ) -> impl ActionHost + Clone + Send + 'static {
        InWorkspace {
            host: self.clone(),
            workspace_id,
            certificate: Some(certificate.clone()),
        }
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, InvocationLedger<InvocationState>> {
        self.shared.ledger.lock_recover()
    }

    /// Whether `caller` started invocation `id` here.
    fn admitted(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.ledger().get(caller, id).is_some()
    }

    /// The answer `work` gives, on the relay's runtime: a room is decided
    /// under its lock, which is never held on the caller's thread.
    fn spawn<T: Send + 'static>(
        &self,
        work: impl Future<Output = T> + Send + 'static,
    ) -> Pending<T> {
        let (completer, answer) = tilo_actions::remote::pending();
        self.shared.runtime.spawn(async move {
            completer.complete(work.await);
        });
        answer
    }
}

/// [`HostRoomActions`] for the room one workspace carries, and for the
/// host's own operations asked on it.
#[derive(Clone)]
struct InWorkspace {
    host: HostRoomActions,
    workspace_id: u64,
    /// The caller's device certificate, as the transport verified it.
    certificate: Option<DeviceCert>,
}

impl ActionHost for InWorkspace {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let Some(rooms) = self.host.shared.rooms.get().cloned() else {
            return Pending::ready(Ok(CataloguePage::from_rows("0".into(), Vec::new(), &query)));
        };
        let (caller, workspace_id, certificate) =
            (caller.clone(), self.workspace_id, self.certificate.clone());
        self.host.spawn(async move {
            Ok(rooms
                .catalogue(workspace_id, &caller, certificate.as_ref(), &query)
                .await)
        })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        let Some(rooms) = self.host.shared.rooms.get().cloned() else {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        };
        let invocation = request.invocation;
        // The ledger's guard must not outlive the reservation: a retry reads
        // it again below.
        let reservation = self.host.ledger().reserve(caller, &request);
        match reservation {
            Err(error) => return Pending::ready(Err(error)),
            Ok(tilo_actions::remote::Reservation::Admitted) => {
                return Pending::ready(
                    self.host
                        .ledger()
                        .get(caller, &invocation)
                        .map(InvocationState::receipt)
                        .ok_or(ActionHostError::UnknownInvocation),
                );
            }
            Ok(tilo_actions::remote::Reservation::Reserved) => {}
        }
        let (host, caller, workspace_id, certificate) = (
            self.host.clone(),
            caller.clone(),
            self.workspace_id,
            self.certificate.clone(),
        );
        self.host.spawn(async move {
            let ran = rooms
                .invoke(workspace_id, &caller, certificate.as_ref(), &request)
                .await;
            match ran {
                Ok(value) => {
                    let state = InvocationState::new(
                        invocation,
                        InvocationUpdate::Succeeded {
                            invocation,
                            output: ActionOutput {
                                value: Some(value),
                                resources: Vec::new(),
                            },
                        },
                    );
                    let receipt = state.receipt();
                    host.ledger().complete(&invocation, state);
                    Ok(receipt)
                }
                // Refused before anything changed, or answered from the
                // room's request records on a retry: nothing to remember.
                Err(error) => {
                    host.ledger().abandon(&invocation);
                    Err(error)
                }
            }
        })
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.host.ledger().get(caller, id).cloned()
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
        if !self.host.admitted(caller, id) {
            return Err(ActionHostError::UnknownInvocation);
        }
        Err(ActionHostError::NotCancellable {
            reason: "room governance completes at once".into(),
        })
    }
}

/// The providers a relay that governs rooms answers itself.
const PROVIDERS: [&str; 3] = [
    ROOM_GOVERNANCE_PROVIDER,
    MEMBER_ACTIONS_PROVIDER,
    HOST_ACTIONS_PROVIDER,
];

/// What a relay that governs rooms answers remote actions addressed to it
/// with: the rooms' governance (provider [`ROOM_GOVERNANCE_PROVIDER`]), a
/// member's own business in them ([`MEMBER_ACTIONS_PROVIDER`]), the host's
/// own operations ([`HOST_ACTIONS_PROVIDER`]), and the app-serving plane its
/// embedder supplied for everything else.
pub(crate) struct GovernedPlane {
    pub(crate) governance: HostRoomActions,
    /// Answers everything else, when the embedder serves apps.
    pub(crate) apps: Option<Arc<dyn AppServingPlane>>,
}

impl GovernedPlane {
    /// Whether `message` from `caller` is the rooms' to answer.
    fn governs(&self, caller: &Caller, message: &RemoteActionMessage) -> bool {
        match message {
            RemoteActionMessage::CatalogueRequest { query, .. } => {
                match query.provider.as_deref() {
                    Some(provider) => PROVIDERS.contains(&provider),
                    // With no apps, the rooms are all this host publishes.
                    None => {
                        self.apps.is_none()
                            && query.view_resource.is_none()
                            && query.canonical_route.is_none()
                    }
                }
            }
            RemoteActionMessage::Invoke(request) => {
                PROVIDERS.contains(&request.target.provider.as_str())
            }
            RemoteActionMessage::Observe { invocation, .. }
            | RemoteActionMessage::Cancel { invocation } => {
                self.governance.admitted(caller, invocation)
            }
            _ => false,
        }
    }
}

impl AppServingPlane for GovernedPlane {
    fn answers(&self, caller: &Caller, message: &RemoteActionMessage) -> bool {
        self.governs(caller, message)
            || self
                .apps
                .as_ref()
                .is_some_and(|apps| apps.answers(caller, message))
    }

    fn serve(
        &self,
        workspace_id: u64,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        if self.governs(caller, &message) {
            return tilo_actions::remote::serve(
                &self.governance.in_workspace(workspace_id),
                caller,
                message,
            );
        }
        self.apps.as_ref()?.serve(workspace_id, caller, message)
    }

    fn serve_certified(
        &self,
        workspace_id: u64,
        caller: &Caller,
        certificate: &DeviceCert,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        if self.governs(caller, &message) {
            return tilo_actions::remote::serve(
                &self.governance.in_workspace_as(workspace_id, certificate),
                caller,
                message,
            );
        }
        self.apps
            .as_ref()?
            .serve_certified(workspace_id, caller, certificate, message)
    }
}

#[cfg(test)]
mod tests;
