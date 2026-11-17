//! The workspaces this relay governs, as their own governance actions.
//!
//! A workspace's administration is its public surface like any app's: provider
//! [`WORKSPACE_GOVERNANCE_PROVIDER`] (`tilo.workspace`) publishes it on the
//! workspace's governance resource (`workspace.permissions:<workspace>`), so an agent,
//! a script, the palette or a screen reader invites, lists and cancels
//! invitations, admits and declines join requests, sets roles, restricts and
//! unbans members and runs calls over `Envelope::RemoteAction`, without the
//! `/workspaces` pages those callers cannot speak. The
//! desktop administers the workspaces it holds this way too.
//!
//! Beside it, two providers carry what is not one workspace's governance
//! (`tilo_relay_session::workspaces` names them):
//!
//! - the host's own operations ([`HOST_ACTIONS_PROVIDER`], `host`):
//!   creating, listing and entering workspaces and letting an app author one,
//!   which only devices of the relay's own person may, decided as
//!   [`Workspaces::authorize`] decides every [`WorkspaceOp::Host`]. They are asked of
//!   the host itself over the device's session with it, naming no workspace
//!   (`Envelope::HostAction`, [`AppServingPlane::serve_host`]), or on a workspace
//!   the device holds there, and need the device's certificate as the
//!   relay's handshake verified it,
//!   which entering admits to the workspace. Creating a workspace may issue its first
//!   invitation and entering answers the invitation the device redeems, so
//!   a desktop shares a workspace without any governance before it holds
//!   the workspace.
//!
//! Nothing here decides who may. Every invocation calls the [`Workspaces`] method
//! its wire API calls, and with it [`Workspaces::authorize`]; the catalogue asks
//! the same decision ([`Workspaces::permits`]) so an action the caller may not
//! take is absent, and invoking one is the same `UnknownAction` as invoking
//! one that does not exist.
//!
//! Removing or banning a member and ending a call are destructive: they run
//! only on a confirmation the person gave on their own device. The workspace's
//! owner removes and bans by asking from a device of their own: their
//! request is that confirmation. For anyone else this host cannot ask yet:
//! the action is left out of their catalogue, and a caller the workspace would
//! let take one is refused with [`ActionHostError::ConfirmationRequired`]; a
//! caller's own `confirm` is never taken for the person's. Ending a call
//! waits for everyone.
//!
//! The host also serves its own person's MCP server
//! ([`mcp_server::PROVIDER`]): the devices they run an agent on, read live,
//! and stopping one's run. Only a device of that person's own asks it, never
//! an agent's.
//!
//! What the host does for its embedder is unchanged: [`GovernedPlane`]
//! answers this provider and hands every other request to the app-serving
//! plane the embedder supplied, if any.

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value, json};
use tilo_action_policy::roles::{self, governance};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvocationLedger, InvocationReceipt, InvocationState,
    InvokeArgument, InvokeRequest, Pending, RemoteActionMessage, WORKSPACE_GOVERNANCE_KIND,
    WORKSPACE_GOVERNANCE_PROVIDER, host_account, mcp_server, person_accounts, person_models,
    person_telling, person_vault, workspace_governance_resource,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ActionOutput, ActionParam,
    ExecutionLocus, InputSpec, InvocationId, InvocationUpdate, OperationKind, ParamType, RiskLevel,
};
use tilo_peer_state::join::Role;
use tilo_relay_protocol::voice::{CallOperation, Directions};
use tilo_relay_session::workspaces::HOST_ACTIONS_PROVIDER;
use tilo_user_identity::DeviceCert;
use tokio::sync::Mutex as AsyncMutex;

pub(super) mod host;
#[cfg(not(target_arch = "wasm32"))]
mod mail;
#[cfg(not(target_arch = "wasm32"))]
mod telling;

use super::authority::WorkspaceOp;
use super::{
    CancelInvitationRequest, CollaborationError, GovernedWorkspace, IssueInvitationRequest,
    RemoveMemberRequest, Workspaces,
};
use crate::app_serving::AppServingPlane;
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::WorkspaceId;

/// Listing the invitations still redeemable: part of inviting.
pub(crate) const INVITATIONS: &str = "invitation.list";
/// Cancelling an unredeemed invitation: part of inviting.
pub(crate) const CANCEL_INVITATION: &str = "invitation.cancel";
/// The people in the workspace and their roles: what whoever invites sees
/// before and after inviting.
pub(crate) const MEMBERS: &str = "member.list";
/// The keys this workspace admitted for the calling person: each grant its
/// governance records, its name, the workspace it reaches and until when.
pub(crate) const KEYS: &str = "key.list";
/// Revoke a key the caller holds or one it minted: the workspace records the
/// revocation, and every holder and front door refuses the key from then on.
pub(crate) const REVOKE_KEY: &str = "key.revoke";
/// The calling person's own name in the workspace: what its member list and
/// the invitations they make show for them, from any of their devices or
/// keys. Every member names themself, so it is no governance action and no
/// grant gates it.
pub(crate) const NAME: &str = "member.name";
/// Holding a person in the workspace by their key, at a role, as an
/// administrator or as an owner: what an administrator does when they know
/// the person's key rather than handing them an invitation.
pub(crate) const ADD_MEMBER: &str = "member.add";
/// The addresses people left at the entry while it takes people by
/// invitation: part of inviting.
pub(crate) const WAIT_LIST: &str = "wait-list.list";
/// Taking an address off the wait list: part of inviting.
pub(crate) const UNLIST: &str = "wait-list.remove";
/// Mailing the invitation issued for an address on the wait list to it,
/// where the entry sends mail: part of inviting.
pub(crate) const MAIL_INVITATION: &str = "wait-list.mail";
/// Listing the workspace as a public workspace, with its description and the
/// role everyone who joins gets.
pub(crate) const PUBLISH: &str = "publish";
/// Taking the workspace out of the directory.
pub(crate) const UNPUBLISH: &str = "unpublish";
/// Whether the workspace is public, and with which listing.
pub(crate) const PUBLICATION: &str = "publication";
/// Asking the workspace's host to run a bot from the directory.
pub(crate) const REQUEST_BOT: &str = "request-bot";
/// Taking back the workspace's request for a bot: its host stops and
/// uninstalls it.
pub(crate) const WITHDRAW_BOT_REQUEST: &str = "withdraw-bot-request";
/// No longer choosing which version of an app the workspace runs
/// (`install-app`, the governance action of that name, chooses one).
pub(crate) const UNINSTALL_APP: &str = "uninstall-app";
/// Hand the workspace's host a piece of an app version's package file.
pub(crate) const UPLOAD_APP_FILE: &str = "upload-app-file";
/// Giving the host a secret a requested bot's connector needs (a Discord
/// bot token): kept in the host's secret store for that connector, never in
/// the workspace's governance, a record or a log.
pub(crate) const SET_CONNECTOR_SECRET: &str = "set-connector-secret";
/// The connector secrets a workspace member may give its host, by connector and
/// purpose.
const CONNECTOR_SECRETS: [(&str, &str); 1] = [("so.tilo.discord", "bot-token")];
/// Most bytes a connector secret takes.
const MAX_CONNECTOR_SECRET: usize = 512;
/// The roles a public workspace hands out at its entry.
const ENTRY_ROLES: [&str; 3] = ["viewer", "commenter", "editor"];

/// How long an invitation stays redeemable when the caller does not say.
const DEFAULT_INVITATION_HOURS: u64 = 7 * 24;
const HOUR_NS: u64 = 3_600 * 1_000_000_000;
const SECOND_NS: u64 = 1_000_000_000;

/// Derives each invocation's idempotency key, so a retry of one invocation
/// is the same request to the workspace's request records.
const REQUEST_ID_CONTEXT: &str = "tilo-relay workspace governance action request v1";

const ROLES: [&str; 6] = [
    "viewer",
    "commenter",
    "editor",
    "moderator",
    ADMINISTRATOR_ROLE,
    OWNER_ROLE,
];
/// The invitation choice that makes its holder an administrator: an Editor
/// with the administrator grants, which no role of its own names.
const ADMINISTRATOR_ROLE: &str = "administrator";
/// The place that makes its holder an owner: an administrator the workspace
/// is kept for, who makes and unmakes its administrators and owners.
const OWNER_ROLE: &str = "owner";
const CALL_OPERATIONS: [&str; 4] = ["start", "restart", "set-directions", "set-denial"];
/// The call operation that is destructive, and so not served yet.
const END_CALL: &str = "end";

/// One action a governed workspace publishes.
struct Verb {
    id: &'static str,
    label: &'static str,
    /// The workspace governance action [`Workspaces::authorize`] gates it on, and the
    /// catalogue offers it by.
    gate: &'static str,
    kind: ActionKind,
    params: fn() -> Vec<ActionParam>,
}

/// What the workspaces serve, in catalogue order.
const VERBS: [Verb; 22] = [
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
        id: MEMBERS,
        label: "List members",
        gate: governance::INVITE,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: ADD_MEMBER,
        label: "Add a person",
        gate: governance::ADD_MEMBER,
        kind: roles::GOVERNANCE_KIND,
        params: add_member_params,
    },
    Verb {
        id: WAIT_LIST,
        label: "List the wait list",
        gate: governance::INVITE,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: UNLIST,
        label: "Take an address off the wait list",
        gate: governance::INVITE,
        kind: roles::GOVERNANCE_KIND,
        params: wait_list_entry_params,
    },
    Verb {
        id: MAIL_INVITATION,
        label: "Mail an invitation to an address on the wait list",
        gate: governance::INVITE,
        kind: roles::GOVERNANCE_KIND,
        params: mail_invitation_params,
    },
    Verb {
        id: NAME,
        label: "Name yourself",
        // Nothing gates it: what a person is called in a workspace is theirs,
        // not a grant's. Whoever asks names themselves, or is refused.
        gate: "",
        kind: roles::GOVERNANCE_KIND,
        params: name_params,
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
    Verb {
        id: governance::INSTALL_APP,
        label: "Run a version of an app",
        gate: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: app_release_params,
    },
    Verb {
        id: UPLOAD_APP_FILE,
        label: "Hand the workspace an app version's package",
        gate: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: app_file_params,
    },
    Verb {
        id: UNINSTALL_APP,
        label: "Let everyone run their own version of an app",
        gate: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: app_params,
    },
    Verb {
        id: KEYS,
        label: "List the keys this workspace admitted",
        // Nothing gates it: a key lists the grants of its own person's, and
        // the front door shows it only the key itself and the keys it minted.
        gate: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: REVOKE_KEY,
        label: "Revoke a key",
        // As listing: its own person's grant, and only the key itself or one
        // it minted, which the handler checks.
        gate: "",
        kind: roles::GOVERNANCE_KIND,
        params: key_params,
    },
];

/// Destructive governance: run for the workspace's owner, whose own request is
/// their confirmation, and for nobody else until the person's own
/// confirmation can be asked for.
const OWNER_CONFIRMED: [Verb; 2] = [
    Verb {
        id: governance::REMOVE_MEMBER,
        label: "Remove member",
        gate: governance::REMOVE_MEMBER,
        kind: roles::GOVERNANCE_KIND,
        params: person_params,
    },
    Verb {
        id: governance::BAN,
        label: "Ban",
        gate: governance::BAN,
        kind: roles::GOVERNANCE_KIND,
        params: person_params,
    },
];

fn person_param() -> ActionParam {
    ActionParam::required(
        "person",
        ParamType::Text,
        "The member's person: their user root key, 64 hex characters.",
    )
}

fn invite_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "role",
            ParamType::Text,
            "The role the invited person gets; `administrator` gives them an editor's part \
             and the administrator grants, `owner` makes them an owner as well; never above \
             the inviter's own.",
        )
        .with_choices(ROLES),
        ActionParam::optional(
            "wait_list_entry",
            ParamType::Text,
            "The address on the wait list it is for (its id, 64 hex characters): it shows as \
             invited, and leaves the list once someone joins with it.",
        ),
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

fn add_member_params() -> Vec<ActionParam> {
    vec![
        person_param(),
        ActionParam::required(
            "role",
            ParamType::Text,
            "Where they are held: a role, `administrator` (an editor holding every governance \
             action) or `owner` (an administrator the workspace is kept for); never more than \
             yours to give. A member already is moved there.",
        )
        .with_choices(ROLES),
        ActionParam::optional(
            "name",
            ParamType::Text,
            "How the workspace names them until they name themselves.",
        ),
    ]
}

fn wait_list_entry_param() -> ActionParam {
    ActionParam::required(
        "entry",
        ParamType::Text,
        "The address's id on the wait list, 64 hex characters, as listing the wait list reports it.",
    )
}

fn wait_list_entry_params() -> Vec<ActionParam> {
    vec![wait_list_entry_param()]
}

fn mail_invitation_params() -> Vec<ActionParam> {
    vec![
        wait_list_entry_param(),
        ActionParam::required(
            "link",
            ParamType::Text,
            "The invitation's link on this workspace's entry, issued for the address.",
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

fn name_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "name",
        ParamType::Text,
        "How the workspace shows you from now on, in its member list and on the invitations you make.",
    )]
}

fn person_params() -> Vec<ActionParam> {
    vec![person_param()]
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
            "The listed version the workspace's host runs.",
        ),
        ActionParam::optional(
            "inputs",
            ParamType::Object,
            "The bot's settings, by name, such as a bridge's guild and server; never a secret.",
        ),
        ActionParam::optional(
            "program",
            ParamType::Text,
            "A developer's own program for the entry, run instead of the listed one; `version` names the build.",
        ),
    ]
}

fn app_param() -> ActionParam {
    ActionParam::required("app", ParamType::Text, "The app, such as so.tilo.notes.")
}

fn app_release_params() -> Vec<ActionParam> {
    vec![
        app_param(),
        ActionParam::required("version", ParamType::Text, "The version its package names."),
        ActionParam::required(
            "surface",
            ParamType::Text,
            "The BLAKE3 hash of the package's surface.toml, 64 hex characters.",
        ),
        ActionParam::required(
            "package",
            ParamType::Text,
            "The package's tapp: hash, 64 hex characters.",
        ),
        ActionParam::required(
            "publisher",
            ParamType::Text,
            "The publisher key that signed the package, 64 hex characters.",
        ),
        ActionParam::optional(
            "dev",
            ParamType::Boolean,
            "A developer's own build, never reviewed by a registry.",
        ),
        ActionParam::optional(
            "file",
            ParamType::Text,
            "The BLAKE3 hash of the .tapp file the host keeps (`upload-app-file`), 64 hex characters: members fetch it from the workspace.",
        ),
    ]
}

fn app_file_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "file",
            ParamType::Text,
            "The BLAKE3 hash of the whole .tapp file, 64 hex characters.",
        ),
        ActionParam::required("offset", ParamType::Integer, "Where this piece starts."),
        ActionParam::required("total", ParamType::Integer, "The file's size in bytes."),
        ActionParam::required(
            "data",
            ParamType::Text,
            "The piece, as hex: at most 256 KiB of the file.",
        ),
    ]
}

fn app_params() -> Vec<ActionParam> {
    vec![app_param()]
}

fn key_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "grant",
        ParamType::Text,
        "The key's grant, as the workspace lists it: the key itself or one it minted.",
    )]
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
        .with_choices(ENTRY_ROLES),
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
            "Start, set-directions: members may send audio. Set-denial: the person may not. False when omitted.",
        ),
        ActionParam::optional(
            "subscribe",
            ParamType::Boolean,
            "Start, set-directions: members hear each other. Set-denial: the person may not. False when omitted.",
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

/// A workspace's own governance.
const GOVERNANCE: Publisher = Publisher {
    provider: WORKSPACE_GOVERNANCE_PROVIDER,
    kind: WORKSPACE_GOVERNANCE_KIND,
};

impl Publisher {
    /// The catalogue entry naming this provider, offered on `workspace` (empty
    /// for the host's own).
    fn entry(self, workspace: &str) -> CatalogueProvider {
        CatalogueProvider {
            provider: self.provider.into(),
            app_id: self.provider.into(),
            workspace: workspace.to_owned(),
            document_id: String::new(),
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
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: ActionAddress::new(resource, self.id),
                resource_kind: publisher.kind.into(),
                resource_label: label.into(),
                action_label: self.label.into(),
                keywords: Vec::new(),
                icon: None,
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
            surface: None,
        }
    }
}

/// The governance action `action` is gated on, for one this provider names.
fn gate_of(action: &str) -> Option<&'static str> {
    VERBS
        .iter()
        .chain(&OWNER_CONFIRMED)
        .find(|verb| verb.id == action)
        .map(|verb| verb.gate)
}

/// Whether `action` with `args` by a device of `owner`'s person or not needs
/// the person's own confirmation, which this host cannot ask for yet. The
/// workspace's owner confirms removing and banning by asking.
fn unconfirmed(action: &str, args: &Args, owner: bool) -> bool {
    (OWNER_CONFIRMED.iter().any(|verb| verb.id == action) && !owner)
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
/// workspace's right to install bots (the verb's gate).
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
        // Where the host's connectors read it.
        tilo_secrets::SecretStore::for_app(app)
            .item(purpose, "default")
            .put_blocking(secret.as_bytes())
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
            InvokeArgument::Text(_) => Err(invalid("workspace governance takes named arguments")),
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

    /// The argument `name`, an object read as `T`.
    fn object<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<Option<T>, ActionHostError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|error| invalid(format!("`{name}`: {error}"))),
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

    /// Where the argument `name` puts a person: a role, an administrator's
    /// place or an owner's.
    fn place_at(&self, name: &str) -> Result<super::MemberPlace, ActionHostError> {
        let text = self.required_text(name)?;
        super::MemberPlace::parse(text).ok_or_else(|| invalid(format!("no role `{text}`")))
    }

    /// The part an invitation the argument `name` names hands out, and
    /// whether it also makes its holder an administrator.
    fn invitation_role_at(&self, name: &str) -> Result<(Role, bool), ActionHostError> {
        let text = self.required_text(name)?;
        if text == ADMINISTRATOR_ROLE {
            // An administrator joins as an Editor and is made one by the
            // invitation's own grant, never by a role above the inviter's.
            return Ok((Role::Editor, true));
        }
        self.role_at(name).map(|role| (role, false))
    }

    /// The role the argument `name` names.
    fn role_at(&self, name: &str) -> Result<Role, ActionHostError> {
        let text = self.required_text(name)?;
        Role::parse(text)
            .filter(|role| role.assignable())
            .ok_or_else(|| invalid(format!("no role `{text}`")))
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
                document_id: self.required_text("record")?.to_owned(),
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

/// A pending invitation, as listing invitations reports it.
fn pending_json(invitation: &super::PendingInvitation) -> Value {
    json!({
        "invite": hex::encode(invitation.invite_id),
        "role": invitation.role.key(),
        "administrator": invitation.administrator,
        "recipient": invitation.recipient_user_public_key.map(hex::encode),
        "invitee_name": invitation.invitee_name,
        "expires_at_ns": invitation.expires_at_ns,
    })
}

/// How long an invitation issued now stays redeemable, as its arguments
/// say it: seconds when given, else hours, else the default.
fn invitation_lifetime(args: &Args) -> Result<u64, ActionHostError> {
    Ok(match args.integer("expires_in_seconds")? {
        Some(seconds) => seconds.saturating_mul(SECOND_NS),
        None => args
            .integer("expires_in_hours")?
            .unwrap_or(DEFAULT_INVITATION_HOURS)
            .saturating_mul(HOUR_NS),
    })
}

/// The idempotency key of invocation `invocation` by the person `person`:
/// a retry of it is a retry to the workspace's request records, which answer it
/// with what the first attempt did.
fn request_id(person: &[u8; 32], invocation: InvocationId) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(REQUEST_ID_CONTEXT);
    hasher.update(person);
    hasher.update(invocation.0.as_bytes());
    *hasher.finalize().as_bytes()
}

/// A governed workspace, locked.
type Governed = Arc<AsyncMutex<GovernedWorkspace>>;

/// Whether `caller` is a person acting themselves: an agent or a bot
/// governs nothing and holds no workspace's business of its own.
fn speaks_for_themselves(caller: &Caller) -> bool {
    caller.acting().is_person()
}

/// Whether `caller` asks the host's own operations as the person
/// `certificate` names: the person themselves, or an agent's grant, which
/// the host's operations treat as its person within its scope
/// ([`crate::host_authority`]) while every workspace takes it as their agent.
fn asks_as_person(caller: &Caller, certificate: &DeviceCert) -> bool {
    speaks_for_themselves(caller)
        || (certificate.capabilities.agent.is_some()
            && caller.is_session_delegated()
            && caller.acting().is_agent())
}

/// How `caller`, on the device `certificate` names, asks the host's own
/// operations: as its person ([`asks_as_person`]), every operation its
/// certificate allows; as an agent's run on a person's own device (a
/// computer the person logged in, whose session says it is their agent's
/// run), only what reads what the host keeps for that person; `None`: not at
/// all.
fn asks_host(caller: &Caller, certificate: &DeviceCert) -> Option<host::Asking> {
    if asks_as_person(caller, certificate) {
        Some(host::Asking::AsPerson)
    } else if certificate.capabilities.agent.is_none()
        && caller.is_session_delegated()
        && caller.acting().is_agent()
    {
        Some(host::Asking::Reading)
    } else {
        None
    }
}

/// Whether `caller` is the person's server key `certificate` certifies,
/// reaching every workspace of its person: what may read that person's
/// account. An agent the person connected through consent does not.
fn reaches_every_workspace(caller: &Caller, certificate: &DeviceCert) -> bool {
    certificate.capabilities.agent == Some(tilo_user_identity::AgentScope::AllWorkspaces)
        && certificate.capabilities.server_key
        && asks_as_person(caller, certificate)
        && caller.device_public_key() == Some(&certificate.device_public_key)
        && caller.user_public_key() == Some(&certificate.user_public_key)
}

/// What a catalogue offers its caller, before it is paged.
#[derive(Default)]
struct Offer {
    /// The governance generation of the workspace it is about, if any: the
    /// page's revision.
    generation: u64,
    rows: Vec<CatalogueRow>,
    providers: Vec<CatalogueProvider>,
}

impl Offer {
    /// Offer `rows`, which `publisher` publishes on `workspace`; a provider
    /// offering nothing is not named.
    fn add(&mut self, publisher: Publisher, workspace: &str, rows: Vec<CatalogueRow>) {
        if !rows.is_empty() {
            self.providers.push(publisher.entry(workspace));
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

impl Workspaces {
    /// The workspace `workspace_id` carries, and the certificate of `caller`'s
    /// device in it: the one the workspace's governance certified, or the relay's
    /// own. `None` for a workspace that carries no governed workspace, a device
    /// the workspace never certified, and any caller but a person acting
    /// themselves or their server key: an agent or a bot governs nothing,
    /// and a server key only invites (`authority::server_key_invites`).
    async fn governed_by(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
    ) -> Option<(Governed, DeviceCert)> {
        let (user, device) = (caller.user_public_key()?, caller.device_public_key()?);
        let governed = self.held(workspace_id).await?;
        let workspace = governed.lock().await;
        let host = &self.inner.host.cert;
        let cert = workspace
            .peer()
            .certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == *device)
            .or_else(|| (host.device_public_key == *device).then(|| host.clone()))?;
        drop(workspace);
        let asking = speaks_for_themselves(caller)
            || (cert.capabilities.server_key && asks_as_person(caller, &cert));
        (asking && cert.user_public_key == *user).then_some((governed, cert))
    }

    /// The workspace `workspace_id` carries and the certificate of `caller`'s
    /// device in it, for the key that reads and revokes its own grants: the
    /// device must be one the workspace admitted, and the grant may be any of
    /// the person's, a key for one workspace included.
    async fn grant_device(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
    ) -> Option<(Governed, DeviceCert)> {
        let (user, device) = (caller.user_public_key()?, caller.device_public_key()?);
        let governed = self.held(workspace_id).await?;
        let workspace = governed.lock().await;
        let host = &self.inner.host.cert;
        let cert = workspace
            .peer()
            .certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == *device)
            .or_else(|| (host.device_public_key == *device).then(|| host.clone()))?;
        drop(workspace);
        (cert.user_public_key == *user).then_some((governed, cert))
    }

    /// What `caller`, on the device `certificate` when the transport named
    /// it, may do here about workspace `workspace_id` (`None` when asked of
    /// the host itself): the governance of the workspace it carries, their own
    /// business in that workspace, and the host's operations when they are its
    /// person's.
    async fn catalogue(
        &self,
        workspace_id: Option<WorkspaceId>,
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
        if wants(WORKSPACE_GOVERNANCE_PROVIDER)
            && let Some(workspace_id) = workspace_id
            && let Some((governed, device)) = self.governed_by(workspace_id, caller).await
        {
            let workspace = governed.lock().await;
            offer.generation = workspace.record.generation;
            let now = tilo_clock::unix_now_ns();
            let resource = workspace_governance_resource(workspace.workspace());
            let owner = device.user_public_key == self.inner.host.cert.user_public_key;
            let confirmed: &[Verb] = if owner { &OWNER_CONFIRMED } else { &[] };
            let rows: Vec<CatalogueRow> = VERBS
                .iter()
                .chain(confirmed)
                .filter(|verb| {
                    // A verb with no gate is the caller's own business (naming
                    // themselves), which every person has and an agent's grant
                    // does not.
                    if verb.gate.is_empty() {
                        return speaks_for_themselves(caller);
                    }
                    self.permits(
                        &device,
                        Some(&workspace),
                        WorkspaceOp::Govern(verb.gate),
                        now,
                    )
                })
                .map(|verb| verb.row(GOVERNANCE, &resource, &workspace.name()))
                .collect();
            offer.add(GOVERNANCE, workspace.workspace(), rows);
        }
        if wants(HOST_ACTIONS_PROVIDER)
            && let Some(certificate) = certificate
            && let Some(asking) = asks_host(caller, certificate)
        {
            offer.add(host::HOST, "", self.host_rows(certificate, asking));
        }
        // An agent its person named for a mail account finds what it may
        // do with that mail here. A page asks the host directly, and is
        // offered no row.
        #[cfg(not(target_arch = "wasm32"))]
        if wants(person_accounts::PROVIDER)
            && self.reads_mail_as_assistant(caller, certificate, workspace_id)
        {
            offer.add(mail::MAIL, "", mail::assistant_rows());
        }
        // An agent of a person's finds that person's rule for what is
        // important here: to read wherever it works for them, to change
        // in a workspace of theirs.
        #[cfg(not(target_arch = "wasm32"))]
        if wants(person_telling::PROVIDER)
            && let Some(own) = self.tells_as_agent(caller, certificate, workspace_id)
        {
            offer.add(telling::TELLING, "", telling::agent_rows(own));
        }
        offer.page(query)
    }

    /// Run `request` from `caller`, on the device `certificate` when the
    /// transport named it, by the provider it names: a workspace's on the
    /// workspace carrying it, the host's own on any workspace or asked of
    /// the host itself (`None`).
    async fn invoke(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        match (request.target.provider.as_str(), workspace_id) {
            (WORKSPACE_GOVERNANCE_PROVIDER, Some(workspace_id))
                if request.target.address.action
                    == tilo_relay_session::workspaces::RUN_UNATTENDED =>
            {
                self.unattended_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            (WORKSPACE_GOVERNANCE_PROVIDER, Some(workspace_id)) => {
                self.governance_invoke(workspace_id, caller, request).await
            }
            (HOST_ACTIONS_PROVIDER, _) => {
                // Unknown and forbidden are the same answer.
                let certificate = certificate
                    .filter(|certificate| {
                        caller.device_public_key() == Some(&certificate.device_public_key)
                            && caller.user_public_key() == Some(&certificate.user_public_key)
                    })
                    .ok_or(ActionHostError::UnknownAction)?;
                let asking =
                    asks_host(caller, certificate).ok_or(ActionHostError::UnknownAction)?;
                self.host_invoke(certificate, asking, request).await
            }
            (mcp_server::PROVIDER, _) => {
                self.mcp_keeper(caller, certificate)?;
                if request.target != mcp_server::target(mcp_server::STOP) {
                    return Err(ActionHostError::UnknownAction);
                }
                let InvokeArgument::Json(argument) = &request.argument else {
                    return Err(invalid("a stop names its grant and session"));
                };
                let stop: mcp_server::AgentSessionRef =
                    serde_json::from_value(Value::Object(argument.clone()))
                        .map_err(|error| invalid(error.to_string()))?;
                if self.inner.mcp_server.stop(&stop.grant, &stop.session) {
                    Ok(Value::Null)
                } else {
                    Err(ActionHostError::Refused {
                        reason: "That agent session has ended already".into(),
                    })
                }
            }
            // A request that names its secret by reference carries none, so
            // an agent acting for the person may ask it too: the vault decides
            // whether the item belongs where it would be used.
            #[cfg(not(target_arch = "wasm32"))]
            (person_accounts::PROVIDER, _)
                if request.target.address.action == person_accounts::ADD_MAIL_WITH_ITEM =>
            {
                self.vault_use_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            #[cfg(not(target_arch = "wasm32"))]
            (person_models::PROVIDER, _)
                if request.target.address.action == person_models::SET_KEY_FROM_ITEM =>
            {
                self.vault_use_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            // The person's own mail accounts and model providers, kept here
            // when their workspace runs here: a page has no connection to a
            // mail server and no vault of its own.
            #[cfg(not(target_arch = "wasm32"))]
            (person_accounts::PROVIDER, _) => {
                self.person_accounts_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            #[cfg(not(target_arch = "wasm32"))]
            (person_models::PROVIDER, _) => {
                self.person_models_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            // What the person keeps to sign in with, in that same vault:
            // nothing of it is read back, and no agent asks.
            #[cfg(not(target_arch = "wasm32"))]
            (person_vault::PROVIDER, _) => {
                self.person_vault_invoke(workspace_id, caller, certificate, request)
            }
            // What the person calls important, and what may interrupt
            // them: theirs, and their own agent's to read and to keep.
            #[cfg(not(target_arch = "wasm32"))]
            (person_telling::PROVIDER, _) => {
                self.person_telling_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            (host_account::PROVIDER, _)
                if request.target == host_account::target(host_account::ACCOUNT) =>
            {
                // The person's own account, read by their own device or by
                // a key of theirs that reaches all their workspaces (a
                // backend's server key, metering what it runs).
                let person = caller
                    .user_public_key()
                    .filter(|_| {
                        speaks_for_themselves(caller)
                            || certificate.is_some_and(|certificate| {
                                reaches_every_workspace(caller, certificate)
                            })
                    })
                    .ok_or(ActionHostError::UnknownAction)?;
                let account = self
                    .own_account(person)
                    .ok_or(ActionHostError::UnknownAction)?;
                serde_json::to_value(account).map_err(|error| ActionHostError::Refused {
                    reason: error.to_string(),
                })
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }

    /// Whether `caller`, on the device `certificate` certifies, keeps this
    /// host's MCP server: a device of the host's own person speaking for
    /// themselves, and no agent's. Unknown and forbidden are the same answer.
    fn mcp_keeper(
        &self,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
    ) -> Result<(), ActionHostError> {
        let host = &self.inner.host.cert;
        certificate
            .filter(|certificate| {
                speaks_for_themselves(caller)
                    && caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
                    && certificate.user_public_key == host.user_public_key
                    && !self
                        .inner
                        .mcp_server
                        .is_agent(&certificate.device_public_key)
            })
            .map(|_| ())
            .ok_or(ActionHostError::UnknownAction)
    }

    /// Run `request` from `caller` against the workspace `workspace_id` carries,
    /// through the workspace method its wire API calls.
    async fn governance_invoke(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        let address = &request.target.address;
        let gate = gate_of(&address.action).ok_or(ActionHostError::UnknownAction)?;
        // A key's own record is read by the key itself, whatever its grant may
        // govern: the workspace lists the grants of its person's, and only the
        // key or the key that minted it revokes one.
        let listed_key = matches!(address.action.as_str(), KEYS | REVOKE_KEY);
        let (governed, device) = match self.governed_by(workspace_id, caller).await {
            Some(found) => found,
            None if listed_key => self
                .grant_device(workspace_id, caller)
                .await
                .ok_or(ActionHostError::UnknownAction)?,
            None => return Err(ActionHostError::UnknownAction),
        };
        let workspace = {
            let workspace = governed.lock().await;
            // A key its person revoked acts no longer: the workspace's record
            // of the revocation refuses every call it would make here, from
            // whichever front door it comes.
            let revoked_ms = workspace
                .peer()
                .governance()
                .workspace_grants()
                .into_iter()
                .find(|grant| grant.device == hex::encode(device.device_public_key))
                .and_then(|grant| grant.revoked_ms);
            if let Some(revoked_ms) = revoked_ms {
                return Err(ActionHostError::Refused {
                    reason: format!("key_revoked: this key was revoked at {revoked_ms}"),
                });
            }
            // Unknown and forbidden are the same answer. A verb with no gate
            // is the caller's own business, which its run decides: naming
            // themselves, in [`NAME`]'s case, and nobody else.
            if address.resource != workspace_governance_resource(workspace.workspace())
                || (!gate.is_empty()
                    && !self.permits(
                        &device,
                        Some(&workspace),
                        WorkspaceOp::Govern(gate),
                        tilo_clock::unix_now_ns(),
                    ))
            {
                return Err(ActionHostError::UnknownAction);
            }
            workspace.workspace().to_owned()
        };
        let args = Args::of(&request.argument)?;
        let owner = device.user_public_key == self.inner.host.cert.user_public_key;
        if unconfirmed(&address.action, &args, owner) {
            return Err(ActionHostError::confirmation_required());
        }
        let call = GovernanceCall {
            workspace: &workspace,
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
            governance::INVITE | INVITATIONS | CANCEL_INVITATION | MEMBERS | NAME => {
                self.governance_invitations(action, call).await
            }
            ADD_MEMBER => self.governance_add_member(call).await,
            WAIT_LIST | UNLIST | MAIL_INVITATION => self.governance_wait_list(action, call).await,
            PUBLISH | UNPUBLISH | PUBLICATION => self.governance_listing(action, call).await,
            REQUEST_BOT | WITHDRAW_BOT_REQUEST => self.governance_bots(action, call).await,
            governance::INSTALL_APP | UNINSTALL_APP => self.governance_apps(action, call).await,
            UPLOAD_APP_FILE => self.upload_app_piece(call.workspace, call.args),
            SET_CONNECTOR_SECRET => connector_secret(call.args),
            KEYS => self.governance_keys(call).await,
            REVOKE_KEY => self.governance_revoke_key(call).await,
            _ => self.governance_members(action, call).await,
        }
    }

    /// The grants this workspace records of the calling person's: each key it
    /// admitted, with the grant that minted it when one did.
    async fn governance_keys(&self, call: GovernanceCall<'_>) -> Result<Value, ActionHostError> {
        let grants = self
            .grants_of(call.workspace, &call.device.user_public_key)
            .await
            .map_err(|error| refused(&error))?;
        Ok(json!({"grants": grants}))
    }

    /// Revoke the key `grant` names, the caller's own or one it minted: the
    /// workspace records its person's revocation, and every holder and front
    /// door refuses it from then on.
    async fn governance_revoke_key(
        &self,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        let id = call.args.required_text("grant")?.to_owned();
        let mine = tilo_peer_state::WorkspaceGrant::id_of(&call.device.device_public_key);
        let grants = self
            .grants_of(call.workspace, &call.device.user_public_key)
            .await
            .map_err(|error| refused(&error))?;
        let own = id == mine
            || grants
                .iter()
                .any(|grant| grant.id == id && grant.parent.as_deref() == Some(mine.as_str()));
        if !own {
            return Err(ActionHostError::UnknownAction);
        }
        let now_ms = tilo_clock::unix_now_ns() / 1_000_000;
        let changed = self
            .revoke_grant(call.workspace, &id, now_ms)
            .await
            .map_err(|error| refused(&error))?;
        if !changed {
            return Err(ActionHostError::Refused {
                reason: "that key is revoked already, or this workspace never admitted it".into(),
            });
        }
        Ok(json!({"revoked": id}))
    }

    /// Making the workspace a public workspace, taking it back, and what it is
    /// public with: `{"public": bool, "description"?, "default_role"?}`.
    async fn governance_listing(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        let GovernanceCall {
            workspace,
            device,
            args,
            ..
        } = call;
        let listed = |listing: Option<tilo_relay_protocol::directory::Listing>| match listing {
            Some(listing) => json!({
                "public": true,
                "description": listing.description,
                "default_role": listing.default_role.key(),
            }),
            None => json!({ "public": false }),
        };
        match action {
            PUBLISH => {
                let text = args.required_text("default_role")?;
                let default_role = Role::parse(text)
                    .filter(|role| matches!(role, Role::Viewer | Role::Commenter | Role::Editor))
                    .ok_or_else(|| {
                        invalid(format!(
                            "a public workspace hands out viewer, commenter or editor, not `{text}`"
                        ))
                    })?;
                let listing = tilo_relay_protocol::directory::Listing {
                    description: args.text("description")?.unwrap_or_default().to_owned(),
                    default_role,
                };
                self.publish(workspace, device, listing)
                    .await
                    .map(|listing| listed(Some(listing)))
                    .map_err(|error| refused(&error))
            }
            UNPUBLISH => self
                .unpublish(workspace, device)
                .await
                .map(|()| listed(None))
                .map_err(|error| refused(&error)),
            _ => Ok(listed(self.listing(workspace).await)),
        }
    }

    /// The bots the workspace asks its host to run: asking for one, and taking
    /// the request back.
    async fn governance_bots(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        use tilo_peer_state::workspace_membership::{BotRequest, WorkspaceMembershipCommand};
        let GovernanceCall {
            workspace,
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
                program: args.text("program")?.map(str::to_owned),
            })
        } else {
            WorkspaceMembershipCommand::WithdrawBotRequest { entry }
        };
        command.validate().map_err(invalid)?;
        let done = self
            .ask_for_bot(workspace, device, request_id, command)
            .await
            .map_err(|error| refused(&error))?;
        Ok(json!({ "generation": done.generation }))
    }

    /// The version of an app the workspace runs: choosing one, and taking the
    /// choice back. Answers what the workspace runs now.
    async fn governance_apps(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        use tilo_peer_state::workspace_membership::{AppRelease, WorkspaceMembershipCommand};
        let GovernanceCall {
            workspace,
            device,
            request_id,
            args,
        } = call;
        let app = args.required_text("app")?.to_owned();
        let command = if action == governance::INSTALL_APP {
            let file = args.text("file")?.map(str::to_owned);
            // Members fetch the file from this host: it must be here first.
            if let Some(file) = &file
                && !self.keeps_app_file(workspace, file)
            {
                return Err(refused(
                    &"the workspace's host does not have this package file: hand it over with `upload-app-file` first",
                ));
            }
            WorkspaceMembershipCommand::InstallApp(AppRelease {
                app: app.clone(),
                version: args.required_text("version")?.to_owned(),
                surface: args.required_text("surface")?.to_owned(),
                package: args.required_text("package")?.to_owned(),
                publisher: args.required_text("publisher")?.to_owned(),
                file,
                dev: args.flag("dev")?,
            })
        } else {
            WorkspaceMembershipCommand::UninstallApp { app: app.clone() }
        };
        command.validate().map_err(invalid)?;
        let done = self
            .govern_app(workspace, device, request_id, command)
            .await
            .map_err(|error| refused(&error))?;
        let running = self
            .app_versions(workspace)
            .await
            .map_err(|error| refused(&error))?
            .into_iter()
            .find(|installed| installed.release.app == app);
        Ok(json!({ "generation": done.generation, "running": running }))
    }

    /// A piece of an app version's package file, for this host to keep and
    /// answer members with. Answers how many bytes are in, or that the
    /// whole file is.
    fn upload_app_piece(&self, workspace: &str, args: &Args) -> Result<Value, ActionHostError> {
        let file = args.required_text("file")?;
        let offset = args
            .integer("offset")?
            .ok_or_else(|| invalid("`offset` is required"))?;
        let total = args
            .integer("total")?
            .ok_or_else(|| invalid("`total` is required"))?;
        let data =
            hex::decode(args.required_text("data")?).map_err(|_| invalid("`data` must be hex"))?;
        if data.len() > super::app_files::APP_FILE_PIECE_BYTES {
            return Err(invalid("a piece is at most 256 KiB"));
        }
        match self
            .upload_app_file(workspace, file, offset, total, &data)
            .map_err(|error| refused(&error))?
        {
            super::app_files::Uploaded::Partial(received) => Ok(json!({ "received": received })),
            super::app_files::Uploaded::Complete => Ok(json!({ "received": total, "kept": true })),
        }
    }

    /// The people in `workspace`, one row each: who they are, their role,
    /// and whether they own or administer it.
    pub(super) async fn members(&self, workspace: &str) -> Result<Vec<Value>, CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let governed = governed.lock().await;
        // Whoever made a workspace of their own holds it, though its
        // governance names this host's device as the structural owner
        // that runs it: every other workspace here is its governor's.
        let owner = self
            .own()
            .and_then(|own| own.owner_of(workspace))
            .unwrap_or_else(|| governed.peer().owner());
        // An owner the host declared (its owner link, or the operator's
        // `owner`) is an administrator the workspace is kept for: as much an
        // owner as whoever made it.
        let policy = governed
            .peer()
            .permissions(self.inner.host.cert.device_public_key, tilo_clock::wall())
            .ok()
            .map(|permissions| permissions.policy);
        let owns = |root: [u8; 32]| {
            root == owner
                || policy
                    .as_ref()
                    .is_some_and(|policy| policy.is_keeper(&hex::encode(root)))
        };
        let people = self.people(&governed);
        drop(governed);
        Ok(people
            .members
            .iter()
            .filter(|member| !member.removed)
            .map(|member| {
                json!({
                    "member": member.id,
                    "name": (!member.name.is_empty()).then_some(&member.name),
                    "role": member.role.map(Role::key),
                    "owner": member.root().is_some_and(owns),
                    "administrator": member.administrator,
                })
            })
            .collect())
    }

    /// Inviting, and what inviting covers: the invitations.
    async fn governance_invitations(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        let GovernanceCall {
            workspace,
            device,
            request_id,
            args,
        } = call;
        let now = tilo_clock::unix_now_ns();
        match action {
            governance::INVITE => {
                let expires_at_ns = now.saturating_add(invitation_lifetime(args)?);
                let place = args.place_at("role")?;
                let wait_list_entry = args.key("wait_list_entry")?;
                let (role, makes_administrator) = match place {
                    super::MemberPlace::Role(role) => (role, false),
                    // An administrator joins as an Editor and is made one by
                    // the invitation's own grant, never by a role above the
                    // inviter's; an owner is an administrator as well.
                    super::MemberPlace::Administrator | super::MemberPlace::Owner => {
                        (Role::Editor, true)
                    }
                };
                let request = IssueInvitationRequest {
                    request_id,
                    role,
                    recipient_user_public_key: args.key("recipient")?,
                    invitee_name: args.text("invitee_name")?.map(str::to_owned),
                    expires_at_ns,
                    inviter_name: args.text("inviter_name")?.map(str::to_owned),
                };
                let issued = if place == super::MemberPlace::Owner {
                    self.issue_owner_invitation(workspace, device, request, wait_list_entry)
                        .await
                } else {
                    self.issue_for(
                        workspace,
                        device,
                        request,
                        makes_administrator,
                        wait_list_entry,
                    )
                    .await
                }
                .map_err(|error| refused(&error))?;
                Ok(json!({
                    "invite": hex::encode(issued.invite_id),
                    "invitation": hex::encode(issued.signed_invitation),
                    "administrator": makes_administrator,
                    "owner": place == super::MemberPlace::Owner,
                    "expires_at_ns": expires_at_ns,
                }))
            }
            INVITATIONS => {
                let pending = self
                    .pending(workspace, device)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(pending.iter().map(pending_json).collect())
            }
            MEMBERS => Ok(self
                .members(workspace)
                .await
                .map_err(|error| refused(&error))?
                .into()),
            NAME => {
                self.name_self(
                    workspace,
                    device,
                    args.required_text("name")?,
                    tilo_clock::unix_now_ns(),
                )
                .await
                .map_err(|error| refused(&error))?;
                Ok(json!({}))
            }
            CANCEL_INVITATION => {
                self.cancel(
                    workspace,
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
            _ => Err(ActionHostError::UnknownAction),
        }
    }

    /// Holding a person in the workspace by their key: answers the people as
    /// the workspace holds them now.
    async fn governance_add_member(
        &self,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        let GovernanceCall {
            workspace,
            device,
            request_id,
            args,
        } = call;
        let request = super::AddMemberRequest {
            request_id,
            user_public_key: args.required_key("person")?,
            place: args.place_at("role")?,
            name: args
                .text("name")?
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        };
        self.add_member(workspace, device, request)
            .await
            .map_err(|error| refused(&error))?;
        Ok(self
            .members(workspace)
            .await
            .map_err(|error| refused(&error))?
            .into())
    }

    /// The workspace's wait list, taking an address off it, and mailing an
    /// address the invitation issued for it.
    async fn governance_wait_list(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        let GovernanceCall {
            workspace,
            device,
            args,
            ..
        } = call;
        let now = tilo_clock::unix_now_ns();
        match action {
            UNLIST => self
                .unlist(workspace, device, args.required_key("entry")?, now)
                .await
                .map_err(|error| refused(&error))?,
            MAIL_INVITATION => self
                .mail_invite(
                    workspace,
                    device,
                    args.required_key("entry")?,
                    args.required_text("link")?.to_owned(),
                    now,
                )
                .await
                .map_err(|error| refused(&error))?,
            _ => {}
        }
        let entries = self
            .wait_list(workspace, device, now)
            .await
            .map_err(|error| refused(&error))?;
        Ok(json!({
            "mails": self.mail_entry().is_some(),
            "entries": entries
                .iter()
                .map(|entry| json!({
                    "entry": hex::encode(entry.request_id),
                    "contact": entry.contact,
                    "added_at_ns": entry.added_at_ns,
                    "invited_at_ns": entry.invited_at_ns,
                }))
                .collect::<Vec<_>>(),
        }))
    }

    /// Removing, banning and unbanning members, and the workspace's calls.
    async fn governance_members(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionHostError> {
        let GovernanceCall {
            workspace,
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
            governance::UNBAN => {
                let person = member(args.required_key("person")?);
                generation(self.unban_member(workspace, device, person).await)
            }
            governance::REMOVE_MEMBER => {
                let person = member(args.required_key("person")?);
                generation(self.remove_member(workspace, device, person).await)
            }
            governance::BAN => {
                let person = member(args.required_key("person")?);
                generation(self.ban_member(workspace, device, person).await)
            }
            governance::CALL => {
                let state = self
                    .voice_call_as(workspace, device, &args.call()?)
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

/// One governance invocation, as its workspace method takes it.
#[derive(Clone, Copy)]
struct GovernanceCall<'a> {
    workspace: &'a str,
    device: &'a DeviceCert,
    request_id: [u8; 32],
    args: &'a Args,
}

/// The workspaces' governance as an action host, attached once the workspaces open.
#[derive(Clone)]
pub(crate) struct HostWorkspaceActions {
    shared: Arc<Shared>,
}

struct Shared {
    workspaces: OnceLock<Workspaces>,
    runtime: tokio::runtime::Handle,
    /// What each admitted invocation came to.
    ledger: Mutex<InvocationLedger<InvocationState>>,
    /// The admitted invocations that follow the MCP server: live reads,
    /// moved on whenever the MCP server does.
    mcp_follows: Mutex<std::collections::HashSet<InvocationId>>,
    /// The admitted invocations that follow the person's mail accounts: live
    /// reads, each with the person whose accounts it reads, moved on whenever
    /// those settings change.
    account_follows: Mutex<std::collections::HashMap<InvocationId, [u8; 32]>>,
}

impl HostWorkspaceActions {
    /// Governance for workspaces [`attach`](Self::attach)ed later. Call it inside
    /// the runtime the relay runs on.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                workspaces: OnceLock::new(),
                runtime: tokio::runtime::Handle::current(),
                ledger: Mutex::new(InvocationLedger::default()),
                mcp_follows: Mutex::default(),
                account_follows: Mutex::default(),
            }),
        }
    }

    /// Serve `workspaces`; only the first workspaces attached are served.
    pub(crate) fn attach(&self, workspaces: Workspaces) {
        let _ = self.shared.workspaces.set(workspaces);
    }

    /// This host for requests about workspace `workspace_id`, from a caller
    /// whose device certificate the transport did not name.
    #[must_use]
    pub(crate) fn in_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> impl ActionHost + Clone + Send + 'static {
        InWorkspace {
            host: self.clone(),
            workspace_id: Some(workspace_id),
            certificate: None,
        }
    }

    /// This host for requests about workspace `workspace_id`, from the
    /// device whose certificate the relay's handshake verified as
    /// `certificate`.
    #[must_use]
    pub(crate) fn in_workspace_as(
        &self,
        workspace_id: WorkspaceId,
        certificate: &DeviceCert,
    ) -> impl ActionHost + Clone + Send + 'static {
        InWorkspace {
            host: self.clone(),
            workspace_id: Some(workspace_id),
            certificate: Some(certificate.clone()),
        }
    }

    /// This host for its own operations, asked of it directly by the device
    /// whose certificate the relay's handshake verified as `certificate`.
    #[must_use]
    pub(crate) fn as_host(
        &self,
        certificate: &DeviceCert,
    ) -> impl ActionHost + Clone + Send + 'static {
        InWorkspace {
            host: self.clone(),
            workspace_id: None,
            certificate: Some(certificate.clone()),
        }
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, InvocationLedger<InvocationState>> {
        self.shared.ledger.lock_recover()
    }

    /// Whether `target` is the account read of a host that keeps workspaces
    /// of people's own, which it answers itself.
    fn answers_own_account(&self, target: &tilo_actions::remote::RemoteActionTarget) -> bool {
        *target == host_account::target(host_account::ACCOUNT)
            && self
                .shared
                .workspaces
                .get()
                .is_some_and(|workspaces| workspaces.own().is_some())
    }

    /// Follow the MCP server for `caller` as invocation `request` asks: the
    /// MCP server now, as its first item; [`Self::mcp_moved`] moves it on.
    fn follow_mcp(
        &self,
        workspaces: &Workspaces,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        workspaces.mcp_keeper(caller, certificate)?;
        let invocation = request.invocation;
        let mut ledger = self.ledger();
        if ledger.reserve(caller, request)? == tilo_actions::remote::Reservation::Admitted {
            return ledger
                .get(caller, &invocation)
                .map(InvocationState::receipt)
                .ok_or(ActionHostError::UnknownInvocation);
        }
        let state = InvocationState::new(
            invocation,
            InvocationUpdate::Item {
                invocation,
                sequence: 1,
                value: mcp_value(workspaces),
            },
        );
        let receipt = state.receipt();
        ledger.complete(&invocation, state);
        drop(ledger);
        self.shared.mcp_follows.lock_recover().insert(invocation);
        Ok(receipt)
    }

    /// Whether `id` is a live read of the MCP server `caller` follows.
    fn follows_mcp(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.shared.mcp_follows.lock_recover().contains(id) && self.admitted(caller, id)
    }

    /// Read the MCP server again for the follow `id`; whether it moved.
    fn mcp_moved(&self, workspaces: &Workspaces, caller: &Caller, id: &InvocationId) -> bool {
        let value = mcp_value(workspaces);
        let mut ledger = self.ledger();
        let Some(entry) = ledger.get_mut(caller, id) else {
            return false;
        };
        let sequence = match &entry.update {
            InvocationUpdate::Item {
                sequence,
                value: read,
                ..
            } if *read != value => *sequence,
            _ => return false,
        };
        *entry = InvocationState::new(
            *id,
            InvocationUpdate::Item {
                invocation: *id,
                sequence: sequence.saturating_add(1),
                value,
            },
        );
        drop(ledger);
        true
    }

    /// Whether `caller` started invocation `id` here.
    fn admitted(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.ledger().get(caller, id).is_some()
    }

    /// Follow the person's mail accounts for `caller` as invocation
    /// `request` asks ([`person_accounts::WATCH`]): the accounts now, as its
    /// first item; [`Self::accounts_moved`] moves it on.
    fn follow_accounts(
        &self,
        workspaces: &Workspaces,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<WorkspaceId>,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let person = Workspaces::asking_person(caller, certificate, workspace_id)?;
        let invocation = request.invocation;
        let mut ledger = self.ledger();
        if ledger.reserve(caller, request)? == tilo_actions::remote::Reservation::Admitted {
            return ledger
                .get(caller, &invocation)
                .map(InvocationState::receipt)
                .ok_or(ActionHostError::UnknownInvocation);
        }
        let state = InvocationState::new(
            invocation,
            InvocationUpdate::Item {
                invocation,
                sequence: 1,
                value: workspaces.accounts_listing(&person)?,
            },
        );
        let receipt = state.receipt();
        ledger.complete(&invocation, state);
        drop(ledger);
        self.shared
            .account_follows
            .lock_recover()
            .insert(invocation, person);
        Ok(receipt)
    }

    /// Whether `id` is a live read of the accounts `caller` follows.
    fn follows_accounts(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.shared.account_follows.lock_recover().contains_key(id) && self.admitted(caller, id)
    }

    /// Read the person's accounts again for the follow `id`; whether they
    /// moved.
    fn accounts_moved(&self, workspaces: &Workspaces, caller: &Caller, id: &InvocationId) -> bool {
        let Some(person) = self.shared.account_follows.lock_recover().get(id).copied() else {
            return false;
        };
        let Ok(value) = workspaces.accounts_listing(&person) else {
            return false;
        };
        let mut ledger = self.ledger();
        let Some(entry) = ledger.get_mut(caller, id) else {
            return false;
        };
        let sequence = match &entry.update {
            InvocationUpdate::Item {
                sequence,
                value: read,
                ..
            } if *read != value => *sequence,
            _ => return false,
        };
        *entry = InvocationState::new(
            *id,
            InvocationUpdate::Item {
                invocation: *id,
                sequence: sequence.saturating_add(1),
                value,
            },
        );
        drop(ledger);
        true
    }

    /// The answer `work` gives, on the relay's runtime: a workspace is decided
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

/// The state of an invocation that answered `value`.
fn succeeded(invocation: InvocationId, value: serde_json::Value) -> InvocationState {
    InvocationState::new(
        invocation,
        InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        },
    )
}

/// Answer that `invocation` runs, record it so, and record how it ends once
/// `work` does: the caller follows it there.
fn still_running(
    host: &HostWorkspaceActions,
    invocation: InvocationId,
    work: tokio::task::JoinHandle<Result<serde_json::Value, ActionHostError>>,
) -> InvocationReceipt {
    let accepted = InvocationState::new(invocation, InvocationUpdate::Accepted { invocation });
    let receipt = accepted.receipt();
    host.ledger().complete(&invocation, accepted);
    let host = host.clone();
    host.shared.runtime.clone().spawn(async move {
        let state = match work.await {
            Ok(Ok(value)) => succeeded(invocation, value),
            Ok(Err(error)) => failed(invocation, &error.to_string()),
            Err(stopped) => failed(
                invocation,
                &format!("the host's operation stopped: {stopped}"),
            ),
        };
        host.ledger().complete(&invocation, state);
    });
    receipt
}

/// The state of an invocation that ended in `message`.
fn failed(invocation: InvocationId, message: &str) -> InvocationState {
    InvocationState::new(
        invocation,
        InvocationUpdate::Failed {
            invocation,
            error: tilo_actions::ActionFailure {
                code: "refused".into(),
                message: message.to_owned(),
                details: None,
                retryable: false,
            },
        },
    )
}

/// [`HostWorkspaceActions`] for the workspace one workspace carries, or (with no
/// workspace) for the host's own operations asked of it directly.
#[derive(Clone)]
struct InWorkspace {
    host: HostWorkspaceActions,
    workspace_id: Option<WorkspaceId>,
    /// The caller's device certificate, as the transport verified it.
    certificate: Option<DeviceCert>,
}

impl ActionHost for InWorkspace {
    /// The surface the workspace's package `app` publishes, from the
    /// workspace's own content: the version its governance keeps names the
    /// hash, and a member that has the bytes answers for them. No app has to
    /// run for its types to be readable.
    fn installed_surface(&self, app: &str) -> tilo_actions::remote::Pending<Option<Vec<u8>>> {
        let (Some(workspaces), Some(workspace_id)) = (
            self.host.shared.workspaces.get().cloned(),
            self.workspace_id,
        ) else {
            return Pending::ready(None);
        };
        let app = app.to_owned();
        self.host.spawn(async move {
            let workspace = workspace_id.as_uuid().to_string();
            let installed = workspaces
                .app_versions(&workspace)
                .await
                .ok()?
                .into_iter()
                .find(|installed| installed.release.app == app)?;
            let hash = installed.release.surface;
            if let Some(bytes) = crate::surfaces::read(&hash) {
                return Some(bytes);
            }
            // A member that has the package answers with its surface; the
            // wait is bounded, and a surface that does not come leaves the
            // app one whose types are not readable here yet.
            let _ = workspaces.ask_surface(&workspace, &app, &hash).await;
            for _ in 0..40 {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                if let Some(bytes) = crate::surfaces::read(&hash) {
                    return Some(bytes);
                }
            }
            None
        })
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let Some(workspaces) = self.host.shared.workspaces.get().cloned() else {
            return Pending::ready(Ok(CataloguePage::from_rows("0".into(), Vec::new(), &query)));
        };
        let (caller, workspace_id, certificate) =
            (caller.clone(), self.workspace_id, self.certificate.clone());
        self.host.spawn(async move {
            Ok(workspaces
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
        let Some(workspaces) = self.host.shared.workspaces.get().cloned() else {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        };
        if request.target == mcp_server::target(mcp_server::SERVER) {
            return Pending::ready(self.host.follow_mcp(
                &workspaces,
                caller,
                self.certificate.as_ref(),
                &request,
            ));
        }
        if request.target == person_accounts::target(person_accounts::WATCH) {
            return Pending::ready(self.host.follow_accounts(
                &workspaces,
                caller,
                self.certificate.as_ref(),
                self.workspace_id,
                &request,
            ));
        }
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
        // How long the caller waits for the answer in its receipt; past it the
        // receipt says the invocation runs, and the caller follows it. A burst
        // of slow operations (a backend making hundreds of workspaces at once)
        // is answered as each finishes, never left unanswered past the
        // caller's deadline while it still runs.
        let wait = request
            .wait_ms
            .map(|wait_ms| {
                std::time::Duration::from_millis(u64::from(wait_ms))
                    .min(tilo_actions::remote::MAX_REMOTE_WAIT)
            })
            .or_else(|| super::unattended::asks_first(&request));
        let runtime = self.host.shared.runtime.clone();
        self.host.spawn(async move {
            let mut work = runtime.spawn(async move {
                workspaces
                    .invoke(workspace_id, &caller, certificate.as_ref(), &request)
                    .await
            });
            let ran = match wait {
                Some(wait) => match tokio::time::timeout(wait, &mut work).await {
                    Ok(ran) => ran,
                    Err(_) => return Ok(still_running(&host, invocation, work)),
                },
                None => work.await,
            };
            match ran {
                Ok(Ok(value)) => {
                    let state = succeeded(invocation, value);
                    let receipt = state.receipt();
                    host.ledger().complete(&invocation, state);
                    Ok(receipt)
                }
                // Refused before anything changed, or answered from the
                // workspace's request records on a retry: nothing to remember.
                Ok(Err(error)) => {
                    host.ledger().abandon(&invocation);
                    Err(error)
                }
                Err(stopped) => {
                    host.ledger().abandon(&invocation);
                    Err(ActionHostError::Unavailable {
                        reason: format!("the host's operation stopped: {stopped}"),
                    })
                }
            }
        })
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        // A follow reads the MCP server as it is now.
        if self.host.follows_mcp(caller, id)
            && let Some(workspaces) = self.host.shared.workspaces.get()
        {
            self.host.mcp_moved(workspaces, caller, id);
        }
        // As does a follow of the person's accounts.
        if self.host.follows_accounts(caller, id)
            && let Some(workspaces) = self.host.shared.workspaces.get()
        {
            self.host.accounts_moved(workspaces, caller, id);
        }
        self.host.ledger().get(caller, id).cloned()
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        if self.host.follows_accounts(caller, id) {
            let workspaces = self.host.shared.workspaces.get()?.clone();
            let mut changes = workspaces.person_settings_changes();
            if self.host.accounts_moved(&workspaces, caller, id) {
                return Some(Pending::ready(()));
            }
            let (host, caller, id) = (self.host.clone(), caller.clone(), *id);
            return Some(self.host.spawn(async move {
                // Bounded, as a waiter's deadline is only checked when woken.
                let _ = tokio::time::timeout(FOLLOW_WAKE, changes.changed()).await;
                host.accounts_moved(&workspaces, &caller, &id);
            }));
        }
        if !self.host.follows_mcp(caller, id) {
            return None;
        }
        let workspaces = self.host.shared.workspaces.get()?.clone();
        let mut changes = workspaces.inner.mcp_server.changes();
        if self.host.mcp_moved(&workspaces, caller, id) {
            return Some(Pending::ready(()));
        }
        let (host, caller, id) = (self.host.clone(), caller.clone(), *id);
        Some(self.host.spawn(async move {
            // Bounded, as a waiter's deadline is only checked when woken.
            let _ = tokio::time::timeout(FOLLOW_WAKE, changes.changed()).await;
            host.mcp_moved(&workspaces, &caller, &id);
        }))
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        if self.host.follows_mcp(caller, id) {
            self.host.shared.mcp_follows.lock_recover().remove(id);
            self.host.ledger().complete(
                id,
                InvocationState::new(
                    *id,
                    InvocationUpdate::Cancelled {
                        invocation: *id,
                        message: None,
                    },
                ),
            );
            return Pending::ready(Ok(()));
        }
        if self.host.follows_accounts(caller, id) {
            self.host.shared.account_follows.lock_recover().remove(id);
            self.host.ledger().complete(
                id,
                InvocationState::new(
                    *id,
                    InvocationUpdate::Cancelled {
                        invocation: *id,
                        message: None,
                    },
                ),
            );
            return Pending::ready(Ok(()));
        }
        tilo_actions::remote::Pending::ready(if self.host.admitted(caller, id) {
            Err(ActionHostError::NotCancellable {
                reason: "workspace governance completes at once".into(),
            })
        } else {
            Err(ActionHostError::UnknownInvocation)
        })
    }
}

/// The longest a live follow here waits before it reads again.
const FOLLOW_WAKE: std::time::Duration = std::time::Duration::from_secs(1);

/// The MCP server as its read answers it.
fn mcp_value(workspaces: &Workspaces) -> Value {
    serde_json::to_value(workspaces.inner.mcp_server.server()).unwrap_or(Value::Null)
}

/// The providers a relay that governs workspaces answers itself.
const PROVIDERS: [&str; 7] = [
    WORKSPACE_GOVERNANCE_PROVIDER,
    HOST_ACTIONS_PROVIDER,
    mcp_server::PROVIDER,
    // The person's own mail accounts and model providers: a page asks here
    // because it has no mail connection and no vault of its own.
    person_accounts::PROVIDER,
    person_models::PROVIDER,
    // What the person keeps to sign in with, in the same vault.
    person_vault::PROVIDER,
    // What the person calls important, and what may interrupt them.
    person_telling::PROVIDER,
];

/// What a relay that governs workspaces answers remote actions addressed to it
/// with: the workspaces' governance (provider [`WORKSPACE_GOVERNANCE_PROVIDER`]), the
/// host's own operations ([`HOST_ACTIONS_PROVIDER`]), and the app-serving plane its
/// embedder supplied for everything else.
pub(crate) struct GovernedPlane {
    pub(crate) governance: HostWorkspaceActions,
    /// Answers everything else, when the embedder serves apps.
    pub(crate) apps: Option<Arc<dyn AppServingPlane>>,
}

impl GovernedPlane {
    /// Whether `message` from `caller` is the workspaces' to answer.
    fn governs(&self, caller: &Caller, message: &RemoteActionMessage) -> bool {
        match message {
            RemoteActionMessage::CatalogueRequest { query, .. } => {
                match query.provider.as_deref() {
                    Some(provider) => PROVIDERS.contains(&provider),
                    // With no apps, the workspaces are all this host publishes.
                    None => {
                        self.apps.is_none()
                            && query.view_resource.is_none()
                            && query.canonical_route.is_none()
                    }
                }
            }
            RemoteActionMessage::Invoke(request) => {
                PROVIDERS.contains(&request.target.provider.as_str())
                    || self.governance.answers_own_account(&request.target)
            }
            RemoteActionMessage::Observe { invocation, .. }
            | RemoteActionMessage::Cancel { invocation } => {
                self.governance.admitted(caller, invocation)
            }
            _ => false,
        }
    }
}

impl GovernedPlane {
    /// Where a frame of `workspace_id`'s goes: this host's own governance
    /// stays here, and what its embedder's apps place elsewhere (a Browser
    /// window on a VM host) is theirs, with the caller's certificate where
    /// one was verified.
    fn route_certified_by(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        // Governance this host answers itself stays here; what its embedder's
        // apps place elsewhere (a Browser window on a VM host) is theirs.
        if let RemoteActionMessage::Invoke(request) = message
            && (PROVIDERS.contains(&request.target.provider.as_str())
                || self.governance.answers_own_account(&request.target))
        {
            return None;
        }
        let apps = self.apps.as_ref()?;
        match certificate {
            Some(certificate) => apps.route_certified(workspace_id, caller, certificate, message),
            None => apps.route(workspace_id, caller, message),
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
        workspace_id: WorkspaceId,
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
        workspace_id: WorkspaceId,
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

    fn serve_host(
        &self,
        caller: &Caller,
        certificate: &DeviceCert,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        if !self.governs(caller, &message) {
            return None;
        }
        tilo_actions::remote::serve(&self.governance.as_host(certificate), caller, message)
    }

    fn route(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        self.route_certified_by(workspace_id, caller, None, message)
    }

    fn route_certified(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: &DeviceCert,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        self.route_certified_by(workspace_id, caller, Some(certificate), message)
    }

    fn relayed(
        &self,
        workspace_id: WorkspaceId,
        from: [u8; 32],
        message: &RemoteActionMessage,
    ) -> Option<RemoteActionMessage> {
        self.apps.as_ref()?.relayed(workspace_id, from, message)
    }

    fn session_arrived(&self, workspace_id: WorkspaceId, caller: &Caller) {
        if let Some(apps) = self.apps.as_ref() {
            apps.session_arrived(workspace_id, caller);
        }
    }

    fn tracks(&self, workspace_id: WorkspaceId) -> bool {
        self.apps
            .as_ref()
            .is_some_and(|apps| apps.tracks(workspace_id))
    }

    fn answers_for_host(&self, workspace_id: WorkspaceId, from: [u8; 32], to: [u8; 32]) -> bool {
        self.apps
            .as_ref()
            .is_some_and(|apps| apps.answers_for_host(workspace_id, from, to))
    }
}

#[cfg(test)]
mod person_model_tests;
#[cfg(test)]
mod share_tests;
#[cfg(test)]
mod tests;
