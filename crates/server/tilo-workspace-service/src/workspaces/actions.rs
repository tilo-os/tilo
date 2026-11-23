//! The workspaces this holder governs, as their own governance actions.
//!
//! A workspace's administration is its public surface like any app's: provider
//! [`WORKSPACE_GOVERNANCE_PROVIDER`] (`tilo.workspace`) publishes it on the
//! workspace's governance resource (`workspace.permissions:<workspace>`), so an agent,
//! a script, the palette or a screen reader invites, lists and cancels
//! invitations, admits and declines join requests, sets roles, restricts and
//! unbans members over `Envelope::RemoteAction`, without the
//! `/workspaces` pages those callers cannot speak. The
//! desktop administers the workspaces it holds this way too.
//!
//! Beside it, two providers carry what is not one workspace's governance
//! (`tilo_server_session::workspaces` names them):
//!
//! - the holder's own operations ([`HOST_ACTIONS_PROVIDER`], `host`):
//!   creating, listing and entering workspaces and letting an app author one,
//!   which only devices of the server's own person may, decided as
//!   [`Workspaces::authorize`] decides every [`WorkspaceOp::Host`]. They are asked of
//!   the holder itself over the device's session with it, naming no workspace
//!   (`Envelope::HostAction`, [`AppActionServer::serve_host`]), or on a workspace
//!   the device holds there, and need the device's certificate as the
//!   server's handshake verified it,
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
//! Adding, placing, removing and banning a member are admitted by the
//! workspace's grants like any other action; where someone wants a change
//! confirmed, an Ask grant asks for it.
//!
//! The holder also serves its own person's MCP server
//! ([`mcp_server::PROVIDER`]): the devices they run an agent on, read live,
//! and stopping one's run. Only a device of that person's own asks it, never
//! an agent's.
//!
//! What the holder does for its embedder is unchanged: [`GovernedServer`]
//! answers this provider and hands every other request to the app-serving
//! app action server the embedder supplied, if any.

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value, json};
use tilo_action_policy::Answer;
use tilo_action_policy::roles::{self, governance};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvocationLog, InvocationReceipt, InvocationState,
    InvokeArgument, InvokeRequest, Pending, RemoteActionMessage, WORKSPACE_APPS,
    WORKSPACE_GOVERNANCE_KIND, WORKSPACE_GOVERNANCE_PROVIDER, WORKSPACE_INVITATIONS,
    WORKSPACE_MEMBER_ADDED, WORKSPACE_MEMBER_ADMIT, WORKSPACE_MEMBER_PLACED,
    WORKSPACE_MEMBER_REMOVED, WORKSPACE_MEMBERS, WORKSPACE_SERVERS, WORKSPACE_WAITING, formats,
    mcp_server, person_accounts, person_asking, person_connections, person_models, person_telling,
    person_vault, server_account, workspace_governance_address, workspace_governance_child,
    workspace_governance_operation, workspace_governance_resource, workspace_member,
    workspace_person,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ActionParam, ExecutionLocus,
    InputSpec, InvocationId, InvocationUpdate, OperationKind, ParamType, RiskLevel,
};
use tilo_peer_state::join::Role;
use tilo_server_session::workspaces::HOST_ACTIONS_PROVIDER;
use tilo_user_identity::DeviceCert;
use tokio::sync::Mutex as AsyncMutex;

pub(super) mod agents;
#[cfg(all(feature = "agents", not(target_arch = "wasm32")))]
mod calendars;
mod server_params;
use server_params::{add_server_params, copies_params, move_server_params, remove_server_params};
#[cfg(not(target_arch = "wasm32"))]
mod connections;
pub(super) mod host;
mod invocation_states;
#[cfg(not(target_arch = "wasm32"))]
mod mail;
#[cfg(not(target_arch = "wasm32"))]
mod model_asks;
mod server;
use invocation_states::{answered_within, ended, failed, still_running, succeeded};
#[cfg(not(target_arch = "wasm32"))]
mod telling;

use super::authority::WorkspaceOp;
#[cfg(not(target_arch = "wasm32"))]
use super::workspace_stores::Store;
use super::{
    CancelInvitationRequest, CollaborationError, GovernedWorkspace, IssueInvitationRequest,
    RemoveMemberRequest, Workspaces,
};
use crate::app_serving::AppActionServer;
use tilo_lock::MutexRecover as _;
use tilo_resource::Id;
use tilo_server_protocol::WorkspaceId;

/// Listing the invitations still redeemable: part of inviting.
pub const INVITATIONS: &str = "invitation.list";
/// Cancelling an unredeemed invitation: part of inviting.
pub const CANCEL_INVITATION: &str = "invitation.cancel";
/// The people in the workspace and their roles: what whoever invites sees
/// before and after inviting.
pub(crate) const MEMBERS: &str = "member.list";
/// The keys this workspace admitted for the calling person: each grant its
/// governance records, its name, the workspace it reaches and until when.
pub(crate) const KEYS: &str = "key.list";
/// Revoke a key the caller holds or one it minted: the workspace records the
/// revocation, and every holder and API server refuses the key from then on.
pub(crate) const REVOKE_KEY: &str = "key.revoke";
/// The calling person's own name in the workspace: what its member list and
/// the invitations they make show for them, from any of their devices or
/// keys. Every member names themself, so it is no governance action and no
/// grant checks it.
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
/// The servers keeping the workspace: the one asked first, then each other
/// server its owners had keep it as well.
pub(crate) const SERVERS: &str = "server.list";
/// Having another server, named by its site (`https://tilo.sh`), keep the
/// workspace as well: both keep it, a change on either reaches the other, and
/// its members reach it at either. The server takes it as its own rules say.
pub(crate) const ADD_SERVER: &str = "server.add";
/// Having a server keep the workspace no more.
pub(crate) const REMOVE_SERVER: &str = "server.remove";
/// Moving the workspace from one server to another: the new one keeps it
/// as well, then the old one keeps it no more.
pub(crate) const MOVE_SERVER: &str = "server.move";
/// How many copies the workspace wants kept: key holders, which hold its
/// keys and take its work, and stores, which keep its ciphertext.
pub(crate) const SET_COPIES: &str = "copies.set";
/// How much of the content the workspace's records name this server keeps
/// none of yet: what is still only on its members' devices, so a device that
/// held the last copy is switched off only once it says none.
pub(crate) const CONTENT_HELD: &str = "content.held";
/// What checks choosing the workspace's servers: its owners', which no grant
/// of its governance gives ([`Workspaces::chooses_servers`]).
const CHOOSES_SERVERS: &str = "choose-servers";
/// Listing the workspace as a public workspace, with its description and the
/// role everyone who joins gets.
pub(crate) const PUBLISH: &str = "publish";
/// Taking the workspace out of the directory.
pub(crate) const UNPUBLISH: &str = "unpublish";
/// Whether the workspace is public, and with which listing.
pub(crate) const PUBLICATION: &str = "publication";
/// No longer choosing which version of an app the workspace runs
/// (`install-app`, the governance action of that name, chooses one).
pub(crate) const UNINSTALL_APP: &str = "uninstall-app";
/// Hand the workspace's holder a piece of an app version's package file.
pub(crate) const UPLOAD_APP_FILE: &str = "upload-app-file";

/// How long an invitation stays redeemable when the caller does not say.
const DEFAULT_INVITATION_HOURS: u64 = 7 * 24;
const HOUR_NS: u64 = 3_600 * 1_000_000_000;
const SECOND_NS: u64 = 1_000_000_000;

/// Derives each invocation's idempotency key, so a retry of one invocation
/// is the same request to the workspace's request records.
const REQUEST_ID_CONTEXT: &str = "tilo-server workspace governance action request v1";

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

/// One action a governed workspace publishes.
struct Verb {
    id: &'static str,
    label: &'static str,
    /// The workspace governance action [`Workspaces::authorize`] checks it on, and the
    /// catalogue offers it by.
    check: &'static str,
    kind: ActionKind,
    params: fn() -> Vec<ActionParam>,
}

/// What the workspaces serve, in catalogue order.
const VERBS: [Verb; 27] = [
    Verb {
        id: governance::INVITE,
        label: "Invite",
        check: governance::INVITE,
        kind: roles::GOVERNANCE_KIND,
        params: invite_params,
    },
    Verb {
        id: INVITATIONS,
        label: "List invitations",
        check: governance::INVITE,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: CANCEL_INVITATION,
        label: "Cancel invitation",
        check: governance::INVITE,
        kind: roles::GOVERNANCE_KIND,
        params: invitation_params,
    },
    Verb {
        id: MEMBERS,
        label: "List members",
        check: governance::INVITE,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: ADD_MEMBER,
        label: "Add a person",
        check: governance::ADD_MEMBER,
        kind: roles::GOVERNANCE_KIND,
        params: add_member_params,
    },
    Verb {
        id: governance::REMOVE_MEMBER,
        label: "Remove member",
        check: governance::REMOVE_MEMBER,
        kind: roles::GOVERNANCE_KIND,
        params: person_params,
    },
    Verb {
        id: governance::BAN,
        label: "Ban",
        check: governance::BAN,
        kind: roles::GOVERNANCE_KIND,
        params: person_params,
    },
    Verb {
        id: WAIT_LIST,
        label: "List the wait list",
        check: governance::INVITE,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: UNLIST,
        label: "Take an address off the wait list",
        check: governance::INVITE,
        kind: roles::GOVERNANCE_KIND,
        params: wait_list_entry_params,
    },
    Verb {
        id: MAIL_INVITATION,
        label: "Mail an invitation to an address on the wait list",
        check: governance::INVITE,
        kind: roles::GOVERNANCE_KIND,
        params: mail_invitation_params,
    },
    Verb {
        id: SERVERS,
        label: "List the servers keeping it",
        check: CHOOSES_SERVERS,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: ADD_SERVER,
        label: "Have another server keep it",
        check: CHOOSES_SERVERS,
        kind: roles::GOVERNANCE_KIND,
        params: add_server_params,
    },
    Verb {
        id: REMOVE_SERVER,
        label: "Have a server keep it no more",
        check: CHOOSES_SERVERS,
        kind: roles::GOVERNANCE_KIND,
        params: remove_server_params,
    },
    Verb {
        id: MOVE_SERVER,
        label: "Move it to another server",
        check: CHOOSES_SERVERS,
        kind: roles::GOVERNANCE_KIND,
        params: move_server_params,
    },
    Verb {
        id: SET_COPIES,
        label: "Choose how many copies it keeps",
        check: CHOOSES_SERVERS,
        kind: roles::GOVERNANCE_KIND,
        params: copies_params,
    },
    Verb {
        id: CONTENT_HELD,
        label: "Count the content still only on members' devices",
        check: CHOOSES_SERVERS,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: NAME,
        label: "Set your name",
        // Nothing checks it: what a person is called in a workspace is theirs,
        // not a grant's. Whoever asks names themselves, or is refused.
        check: "",
        kind: roles::GOVERNANCE_KIND,
        params: name_params,
    },
    Verb {
        id: governance::UNBAN,
        label: "Unban",
        check: governance::UNBAN,
        kind: roles::GOVERNANCE_KIND,
        params: person_params,
    },
    Verb {
        id: governance::CALL,
        label: "Moderate a call",
        check: governance::CALL,
        kind: roles::GOVERNANCE_KIND,
        params: call_params,
    },
    Verb {
        id: PUBLISH,
        label: "Make public",
        check: governance::SETTINGS,
        kind: roles::GOVERNANCE_KIND,
        params: publish_params,
    },
    Verb {
        id: UNPUBLISH,
        label: "Make private",
        check: governance::SETTINGS,
        kind: roles::GOVERNANCE_KIND,
        params: Vec::new,
    },
    Verb {
        id: PUBLICATION,
        label: "Public listing",
        check: governance::SETTINGS,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: governance::INSTALL_APP,
        label: "Run a version of an app",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: app_release_params,
    },
    Verb {
        id: UPLOAD_APP_FILE,
        label: "Hand the workspace an app version's package",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: app_file_params,
    },
    Verb {
        id: UNINSTALL_APP,
        label: "Let everyone run their own version of an app",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: app_params,
    },
    Verb {
        id: KEYS,
        label: "List the keys this workspace admitted",
        // Nothing checks it: a key lists the grants of its own person's, and
        // the API server shows it only the key itself and the keys it minted.
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: REVOKE_KEY,
        label: "Revoke a key",
        // As listing: its own person's grant, and only the key itself or one
        // it minted, which the handler checks.
        check: "",
        kind: roles::GOVERNANCE_KIND,
        params: key_params,
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

/// What a call's moderator does to it.
const CALL_OPERATIONS: [&str; 7] = [
    "start",
    "end",
    "restart",
    "set-directions",
    "deny",
    "allow",
    "show",
];

fn call_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("operation", ParamType::Text, "What to do with the call.")
            .with_choices(CALL_OPERATIONS),
        ActionParam::required(
            "channel",
            ParamType::Text,
            "The voice channel whose call it is.",
        ),
        ActionParam::optional(
            "publish",
            ParamType::Boolean,
            "Start and set-directions: whether everyone may speak; deny: whether the person may no longer speak.",
        ),
        ActionParam::optional(
            "subscribe",
            ParamType::Boolean,
            "Start and set-directions: whether everyone may listen; deny: whether the person may no longer listen.",
        ),
        ActionParam::optional(
            "person",
            ParamType::Text,
            "Deny and allow: the person, their user public key as 64 hex characters.",
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

fn publish_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "default_role",
            ParamType::Text,
            "The role everyone who joins from the directory gets.",
        )
        .with_choices(
            Role::ALL
                .into_iter()
                .filter(|role| Role::Editor.includes(*role))
                .map(Role::key),
        ),
        ActionParam::optional(
            "description",
            ParamType::Text,
            "What the workspace is, in a sentence or two (at most 280 characters).",
        ),
    ]
}

/// Who asks one of this device's own providers: the workspace the request
/// came through, its caller, and the device the transport certified for it.
type Asked<'a> = (Option<WorkspaceId>, &'a Caller, Option<&'a DeviceCert>);

#[cfg(target_arch = "wasm32")]
impl Workspaces {
    /// A page keeps no connections of its own: the server of its person's
    /// workspace connects for them.
    fn connections_invoke(
        _workspaces: &Self,
        _asked: Asked<'_>,
        _request: &InvokeRequest,
    ) -> std::future::Ready<Result<Value, ActionCallError>> {
        std::future::ready(Err(ActionCallError::UnknownAction))
    }
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
    /// for the holder's own).
    fn entry(self, workspace: &str) -> CatalogueProvider {
        CatalogueProvider {
            provider: self.provider.into(),
            app_id: self.provider.into(),
            workspace: workspace.to_owned(),
            document_id: String::new(),
            // A workspace's governance is a published tree from its root
            // (`workspace`), listed beside its apps; the others are reached
            // by name.
            browsable: self.provider == WORKSPACE_GOVERNANCE_PROVIDER,
        }
    }
}

/// Whether `provider` publishes what decides a workspace or this device
/// itself: a workspace's governance, or the device's own operations. A
/// button a thing told offers never invokes either.
pub(super) fn governs(provider: &str) -> bool {
    provider == WORKSPACE_GOVERNANCE_PROVIDER || provider == HOST_ACTIONS_PROVIDER
}

impl Verb {
    /// This verb as the grants deciding an agent's call of it name it.
    const fn operation(&self, publisher: Publisher) -> agents::Operation<'static> {
        agents::Operation {
            provider: publisher.provider,
            kind: publisher.kind,
            action: self.id,
            effect: self.kind,
            label: self.label,
        }
    }

    fn row(&self, publisher: Publisher, resource: &str, label: &str) -> CatalogueRow {
        let params = (self.params)();
        CatalogueRow {
            offered: None,
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

impl Verb {
    /// This verb's row in `workspace`'s governance tree, the workspace named
    /// `name`: at the address the tree publishes it
    /// ([`tilo_actions::remote::workspace_governance_address`]), on the root or
    /// on the resource under it that holds it.
    fn governance_row(&self, workspace: &str, name: &str) -> CatalogueRow {
        let address = workspace_governance_address(workspace, self.id);
        let root = workspace_governance_resource(workspace);
        let child = address
            .resource
            .strip_prefix(root.as_str())
            .and_then(|rest| rest.strip_prefix('/'));
        let label = match child {
            Some(WORKSPACE_SERVERS) => "Servers",
            Some(WORKSPACE_APPS) => "Apps",
            Some(WORKSPACE_MEMBERS) => "People",
            Some(WORKSPACE_INVITATIONS) => "Invitations",
            Some(WORKSPACE_WAITING) => "Waiting to be invited",
            Some(other) => other,
            None => name,
        };
        let mut row = self.row(GOVERNANCE, &address.resource, label);
        if child.is_some() {
            row.parent_resource.clone_from(&root);
        }
        // The workspace's people each hold what changes their place.
        row.has_children = child == Some(WORKSPACE_MEMBERS);
        if let Some(action) = row.action.as_mut() {
            // Reading what the tree holds (`tilo_read workspace/members`) lists it.
            if child.is_some() && address.action == "list" && self.kind == ActionKind::Read {
                action
                    .contract
                    .tags
                    .push(tilo_actions::DOCUMENT_READ_TAG.to_owned());
            }
            action.address = address;
        }
        row
    }
}

/// `rows` of a workspace's governance tree, the workspace named `name`, its
/// root saying it holds what is offered under it: listed on its own when it
/// offers nothing itself, so what is under it is found by browsing.
fn governance_tree(mut rows: Vec<CatalogueRow>, name: &str) -> Vec<CatalogueRow> {
    let under = rows
        .iter()
        .find(|row| !row.parent_resource.is_empty())
        .cloned();
    if let Some(mut root) = under.filter(|_| rows.iter().all(|row| !row.parent_resource.is_empty()))
    {
        root.resource.id = std::mem::take(&mut root.parent_resource);
        name.clone_into(&mut root.resource.label);
        root.action = None;
        rows.insert(0, root);
    }
    let holds = rows.iter().any(|row| !row.parent_resource.is_empty());
    for row in &mut rows {
        if row.parent_resource.is_empty() {
            row.has_children = holds;
        }
    }
    rows
}

/// `request` as the workspace's governance runs it: its `operation` on `workspace`'s root, asked
/// of the person its address names when it names one.
fn governance_request(request: &InvokeRequest, workspace: &str, operation: &str) -> InvokeRequest {
    let mut asked = request.clone();
    asked.target.address = ActionAddress::new(workspace_governance_resource(workspace), operation);
    if let Some(person) = workspace_person(&request.target.address) {
        let mut arguments = match asked.argument {
            InvokeArgument::Json(arguments) => arguments,
            _ => Map::new(),
        };
        arguments.insert("person".into(), person.into());
        asked.argument = InvokeArgument::Json(arguments);
    }
    asked
}

/// The operation `change` is granted by, and the action the tree publishes it at, for a device
/// asking whether it is its to make: `add` and `role` are the `member.add` operation, and a
/// removal is `remove-member`.
fn admitted_change(change: &str) -> Option<(&'static str, &'static str)> {
    let published = [
        WORKSPACE_MEMBER_ADDED,
        WORKSPACE_MEMBER_PLACED,
        WORKSPACE_MEMBER_REMOVED,
    ]
    .into_iter()
    .find(|action| *action == change)?;
    let action = if published == WORKSPACE_MEMBER_REMOVED {
        governance::REMOVE_MEMBER
    } else {
        ADD_MEMBER
    };
    Some((action, published))
}

/// The governance action `action` depends on, for one this provider names.
fn check_of(action: &str) -> Option<&'static str> {
    VERBS
        .iter()
        .find(|verb| verb.id == action)
        .map(|verb| verb.check)
}

/// The certificate `device` is in `cert`'s chain of issuers: the key that
/// minted `cert`, or one that stood in for the person when it was minted.
fn minting_issuer(cert: &DeviceCert, device: &[u8; 32]) -> Option<DeviceCert> {
    let mut issuer = cert.issuer.as_deref();
    while let Some(cert) = issuer {
        if cert.device_public_key == *device {
            return Some(cert.clone());
        }
        issuer = cert.issuer.as_deref();
    }
    None
}

/// The arguments `request` names, to show the person asked about it.
fn arguments_of(request: &InvokeRequest) -> Map<String, Value> {
    match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        _ => Map::new(),
    }
}

/// An invocation's typed arguments.
struct Args(Map<String, Value>);

impl Args {
    fn of(argument: &InvokeArgument) -> Result<Self, ActionCallError> {
        match argument {
            InvokeArgument::None => Ok(Self(Map::new())),
            InvokeArgument::Json(object) => Ok(Self(object.clone())),
            InvokeArgument::Text(_) => Err(ActionCallError::invalid_request(
                "workspace governance takes named arguments",
            )),
        }
    }

    fn text(&self, name: &str) -> Result<Option<&str>, ActionCallError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(ActionCallError::invalid_request(format!(
                "`{name}` must be text"
            ))),
        }
    }

    /// The argument `name`, an object read as `T`.
    fn object<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<Option<T>, ActionCallError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|error| ActionCallError::invalid_request(format!("`{name}`: {error}"))),
        }
    }

    fn required_text(&self, name: &str) -> Result<&str, ActionCallError> {
        self.text(name)?
            .ok_or_else(|| ActionCallError::invalid_request(format!("`{name}` is required")))
    }

    fn key(&self, name: &str) -> Result<Option<[u8; 32]>, ActionCallError> {
        self.text(name)?
            .map(|text| {
                let mut key = [0; 32];
                hex::decode_to_slice(text, &mut key).map_err(|_| {
                    ActionCallError::invalid_request(format!("`{name}` must be 64 hex characters"))
                })?;
                Ok(key)
            })
            .transpose()
    }

    fn required_key(&self, name: &str) -> Result<[u8; 32], ActionCallError> {
        self.key(name)?
            .ok_or_else(|| ActionCallError::invalid_request(format!("`{name}` is required")))
    }

    fn integer(&self, name: &str) -> Result<Option<u64>, ActionCallError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value.as_u64().map(Some).ok_or_else(|| {
                ActionCallError::invalid_request(format!("`{name}` must be a whole number"))
            }),
        }
    }

    fn texts(&self, name: &str) -> Result<std::collections::BTreeSet<String>, ActionCallError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(std::collections::BTreeSet::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str().map(str::to_owned).ok_or_else(|| {
                        ActionCallError::invalid_request(format!("`{name}` must list text"))
                    })
                })
                .collect(),
            Some(_) => Err(ActionCallError::invalid_request(format!(
                "`{name}` must be a list of text"
            ))),
        }
    }

    /// The change a call's moderator asks for, or `None` for `show`.
    fn call_change(&self) -> Result<Option<collab_types::calls::CallChange>, ActionCallError> {
        use collab_types::calls::{CallChange, Directions};
        let directions = || -> Result<Directions, ActionCallError> {
            Ok(Directions {
                publish: self.flag_or("publish", true)?,
                subscribe: self.flag_or("subscribe", true)?,
            })
        };
        Ok(Some(match self.required_text("operation")? {
            "start" => CallChange::Start {
                allowed: directions()?,
            },
            "end" => CallChange::End,
            "restart" => CallChange::Restart,
            "set-directions" => CallChange::SetDirections {
                allowed: directions()?,
            },
            "deny" => CallChange::Deny {
                person: self.required_key("person")?,
                denied: directions()?,
            },
            "allow" => CallChange::Allow {
                person: self.required_key("person")?,
            },
            "show" => return Ok(None),
            other => {
                return Err(ActionCallError::invalid_request(format!(
                    "no call operation `{other}`"
                )));
            }
        }))
    }

    /// The flag `name`, or `otherwise` when it was not given.
    fn flag_or(&self, name: &str, otherwise: bool) -> Result<bool, ActionCallError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(otherwise),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(_) => Err(ActionCallError::invalid_request(format!(
                "`{name}` must be true or false"
            ))),
        }
    }

    fn flag(&self, name: &str) -> Result<bool, ActionCallError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(_) => Err(ActionCallError::invalid_request(format!(
                "`{name}` must be true or false"
            ))),
        }
    }

    /// Where the argument `name` puts a person: a role, an administrator's
    /// place or an owner's.
    fn place_at(&self, name: &str) -> Result<super::MemberPlace, ActionCallError> {
        let text = self.required_text(name)?;
        super::MemberPlace::parse(text)
            .ok_or_else(|| ActionCallError::invalid_request(format!("no role `{text}`")))
    }

    /// The part an invitation the argument `name` names hands out, and
    /// whether it also makes its holder an administrator.
    fn invitation_role_at(&self, name: &str) -> Result<(Role, bool), ActionCallError> {
        let text = self.required_text(name)?;
        if text == ADMINISTRATOR_ROLE {
            // An administrator joins as an Editor and is made one by the
            // invitation's own grant, never by a role above the inviter's.
            return Ok((Role::Editor, true));
        }
        self.role_at(name).map(|role| (role, false))
    }

    /// The role the argument `name` names.
    fn role_at(&self, name: &str) -> Result<Role, ActionCallError> {
        let text = self.required_text(name)?;
        Role::parse(text)
            .filter(|role| role.assignable())
            .ok_or_else(|| ActionCallError::invalid_request(format!("no role `{text}`")))
    }
}

/// A pending invitation, as listing invitations reports it.
/// The one place a person holds in the workspace, or an invitation gives: an owner is an
/// administrator and holds a role beneath, and is named by the highest of them alone.
fn place(owner: bool, administrator: bool, role: Option<&str>) -> Option<&str> {
    if owner {
        Some("owner")
    } else if administrator {
        Some("administrator")
    } else {
        role
    }
}

/// An open invitation as the workspace lists it; `owner` when redeeming it makes an owner
/// ([`super::Workspaces::owner_invitation`]), which only this server knows.
fn pending_json(invitation: &super::PendingInvitation, owner: bool) -> Value {
    json!({
        "invite": hex::encode(invitation.invite_id),
        "role": invitation.role.key(),
        "administrator": invitation.administrator,
        "owner": owner,
        "place": place(owner, invitation.administrator, Some(invitation.role.key())),
        "recipient": invitation.recipient_user_public_key.map(hex::encode),
        "invitee_name": invitation.invitee_name,
        "expires_at_ns": invitation.expires_at_ns,
    })
}

/// How long an invitation issued now stays redeemable, as its arguments
/// say it: seconds when given, else hours, else the default.
fn invitation_lifetime(args: &Args) -> Result<u64, ActionCallError> {
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

/// Whether `caller` is the person's server key `certificate` certifies,
/// reaching every workspace of its person: what may read that person's
/// account. An agent the person connected through consent does not.
fn reaches_every_workspace(caller: &Caller, certificate: &DeviceCert) -> bool {
    certificate.capabilities.agent == Some(tilo_user_identity::AgentScope::AllWorkspaces)
        && agents::is_the_person(caller, certificate, None)
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
    /// device in it: the one the workspace's governance certified, or the server's
    /// own. `None` for a workspace that carries no governed workspace, a device
    /// the workspace never certified, and a caller acting for nobody: a bot.
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
        (agents::acts_for_its_person(caller) && cert.user_public_key == *user)
            .then_some((governed, cert))
    }

    /// The workspace `workspace_id` carries and the certificate of `caller`'s
    /// device in it, for the key that reads and revokes its own grants: the
    /// device must be one the workspace admitted, or one whose key it admitted
    /// (the key that minted it), and the grant may be any of the person's, a
    /// key for one workspace included.
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
            .find_map(|cert| {
                (cert.device_public_key == *device)
                    .then_some(cert.clone())
                    .or_else(|| minting_issuer(&cert, device))
            })
            .or_else(|| (host.device_public_key == *device).then(|| host.clone()))?;
        drop(workspace);
        (cert.user_public_key == *user).then_some((governed, cert))
    }

    /// The actions of the workspace `workspace_id` this server runs itself (its
    /// governance, a person's own business in it) whose words hold `needle`,
    /// as `caller` may run them: what a page's search lists beside the
    /// actions of the workspace's records.
    pub async fn page_actions(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        needle: &str,
    ) -> CataloguePage {
        let query = CatalogueQuery {
            needle: needle.to_owned(),
            provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.to_owned()),
            max_results: 40,
            ..CatalogueQuery::default()
        };
        self.catalogue(Some(workspace_id), caller, None, &query)
            .await
    }

    /// Run `request`, one of the actions [`Self::page_actions`] offered, for
    /// `caller` on a page of the workspace `workspace_id`: decided as the
    /// same call from any other caller is.
    ///
    /// # Errors
    ///
    /// The action refused, or is not one `caller` may run.
    pub async fn page_invoke(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        if request.target.provider != WORKSPACE_GOVERNANCE_PROVIDER {
            return Err(ActionCallError::UnknownAction);
        }
        Box::pin(self.invoke(Some(workspace_id), caller, None, request)).await
    }

    /// Whether `caller` may invite into the workspace `workspace_id`, as its
    /// governance decides: what a page asks before it offers inviting at all,
    /// a link handed over or an address on the wait list invited.
    pub async fn page_may_invite(&self, workspace_id: WorkspaceId, caller: &Caller) -> bool {
        let Some(workspace) = self.workspace_identity(workspace_id).await else {
            return false;
        };
        let wanted = workspace_governance_address(&workspace, governance::INVITE);
        let query = CatalogueQuery {
            needle: "invite".to_owned(),
            provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.to_owned()),
            max_results: 200,
            ..CatalogueQuery::default()
        };
        self.catalogue(Some(workspace_id), caller, None, &query)
            .await
            .rows
            .iter()
            .any(|row| {
                row.action
                    .as_ref()
                    .is_some_and(|action| action.address == wanted)
            })
    }

    /// The addresses waiting at the entry of the workspace `workspace_id`, and
    /// whether the entry mails an invitation to one, for whoever may invite:
    /// what a page shows as the addresses waiting to join, and invites from.
    ///
    /// # Errors
    ///
    /// `caller` may not invite into the workspace.
    pub async fn page_waiting_to_join(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
    ) -> Result<Value, ActionCallError> {
        let workspace = self
            .workspace_identity(workspace_id)
            .await
            .ok_or(ActionCallError::UnknownAction)?;
        let request = Self::governance_ask(&workspace, WAIT_LIST, Map::new());
        let listed = self.page_invoke(workspace_id, caller, &request).await?;
        let entries: Vec<tilo_server_protocol::page::WaitingAddress> = listed
            .get("entries")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        Some(tilo_server_protocol::page::WaitingAddress {
                            entry: entry.get("entry")?.as_str()?.to_owned(),
                            contact: entry.get("contact")?.as_str()?.to_owned(),
                            invited: entry.get("invited_at_ns").is_some_and(|at| !at.is_null()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({
            "mails": listed.get("mails").and_then(Value::as_bool).unwrap_or(false),
            "entries": entries,
        }))
    }

    /// Invite the address `entry` waiting at the workspace `workspace_id`'s
    /// entry, as whoever may invite does: the invitation is issued for it,
    /// with the short link mailed to the address where the entry sends mail.
    /// Answers what [`governance::INVITE`] answered.
    ///
    /// # Errors
    ///
    /// `caller` may not invite, or the address is no longer waiting.
    pub async fn page_invite_waiting(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        entry: &str,
    ) -> Result<Value, ActionCallError> {
        let workspace = self
            .workspace_identity(workspace_id)
            .await
            .ok_or(ActionCallError::UnknownAction)?;
        let mut arguments = Map::new();
        arguments.insert("role".to_owned(), Value::from("editor"));
        arguments.insert("wait_list_entry".to_owned(), Value::from(entry));
        let request = Self::governance_ask(&workspace, governance::INVITE, arguments);
        self.page_invoke(workspace_id, caller, &request).await
    }

    /// A short invite link to the workspace `workspace_id`, for whoever may
    /// invite into it: an editor's invitation, issued as the workspace issues
    /// every one, which the link names there. A page keeps one ready for a
    /// press, since a clipboard write has to happen inside the press, and
    /// asks for the next after each is handed over.
    ///
    /// # Errors
    ///
    /// `caller` may not invite into the workspace.
    pub async fn page_invite_link(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
    ) -> Result<Value, ActionCallError> {
        let workspace = self
            .workspace_identity(workspace_id)
            .await
            .ok_or(ActionCallError::UnknownAction)?;
        let mut arguments = Map::new();
        arguments.insert("role".to_owned(), Value::from("editor"));
        let request = Self::governance_ask(&workspace, governance::INVITE, arguments);
        let issued = self.page_invoke(workspace_id, caller, &request).await?;
        // The short link where this host has a web origin to ask; the
        // invitation's own where it has none, which joins just the same.
        let link = ["short_link", "link"]
            .into_iter()
            .find_map(|key| issued.get(key).filter(|link| link.is_string()).cloned())
            .unwrap_or(Value::Null);
        Ok(json!({"link": link}))
    }

    /// The request `operation` of the workspace `workspace`'s governance,
    /// asked by its tree's address, with `arguments`.
    fn governance_ask(
        workspace: &str,
        operation: &str,
        arguments: Map<String, Value>,
    ) -> InvokeRequest {
        InvokeRequest {
            invocation: InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                address: workspace_governance_address(workspace, operation),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// What `caller`, on the device `certificate` when the transport named
    /// it, may do here about workspace `workspace_id` (`None` when asked of
    /// the serving device itself): the governance of the workspace it carries, their own
    /// business in that workspace, and the serving device's operations when they are its
    /// person's.
    pub(super) async fn catalogue(
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
        {
            let operating = self
                .server_operator(workspace_id, caller, certificate)
                .await;
            if let Some((governed, device)) = self.governed_by(workspace_id, caller).await {
                // What an agent's grants here refuse it is not offered to it.
                let person = agents::is_the_person(caller, &device, Some(workspace_id));
                let grants = if person {
                    None
                } else {
                    Some(self.agent_grants(Some(workspace_id), caller).await)
                };
                let workspace = governed.lock().await;
                offer.generation = workspace.record.generation;
                let now = tilo_clock::unix_now_ns();
                let offered = |verb: &Verb| {
                    self.offers(verb, (caller, &device), &workspace, grants.as_ref(), now)
                };
                // Removing is published on each person, under the workspace's people.
                let mut rows: Vec<CatalogueRow> = VERBS
                    .iter()
                    .filter(|verb| verb.id != governance::REMOVE_MEMBER && offered(verb))
                    .map(|verb| verb.governance_row(workspace.workspace(), &workspace.name()))
                    .collect();
                let own = device.user_public_key;
                rows.extend(self.member_rows(&workspace, (query, own), offered));
                if operating.is_some() {
                    rows.extend(server::rows(&workspace));
                }
                let id = workspace.workspace().to_owned();
                let name = workspace.name();
                drop(workspace);
                // The workspace's mail accounts, as its policy on its mail
                // lets the caller ask them.
                #[cfg(not(target_arch = "wasm32"))]
                rows.extend(
                    self.mail_account_rows((Some(workspace_id), caller, certificate), &id)
                        .await,
                );
                offer.add(GOVERNANCE, &id, governance_tree(rows, &name));
            } else if let Some((governed, _device)) = operating {
                // An agent of the person's asks what they ask of their
                // servers: the workspace's governance is not theirs.
                let workspace = governed.lock().await;
                offer.add(GOVERNANCE, workspace.workspace(), server::rows(&workspace));
            }
        }
        if wants(HOST_ACTIONS_PROVIDER)
            && let Some(certificate) = certificate
            && agents::acts_for_its_person(caller)
        {
            let rows = self.account_rows(workspace_id, caller, certificate).await;
            offer.add(host::ACCOUNT, "", rows);
        }
        // An agent or a key the workspace's policy lets read its mail or its
        // calendars finds what it may do with them here. A page asks
        // directly, and is offered no row.
        #[cfg(not(target_arch = "wasm32"))]
        if wants(person_accounts::PROVIDER) && caller.acting().is_agent() {
            let rows = self.mail_rows((workspace_id, caller, certificate)).await;
            if !rows.is_empty() {
                offer.add(mail::MAIL, "", rows);
            }
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
        // An agent finds what it may read of the services the workspace
        // connected. Adding one is no row.
        #[cfg(not(target_arch = "wasm32"))]
        if wants(person_connections::PROVIDER)
            && self
                .connects_as_agent((workspace_id, caller, certificate))
                .await
        {
            offer.add(connections::CONNECTIONS, "", connections::agent_rows());
        }
        offer.page(query)
    }

    /// Run `request` from `caller`, on the device `certificate` when the
    /// transport named it, by the provider it names: a workspace's on the
    /// workspace carrying it, the serving device's own on any workspace or asked of
    /// the serving device itself (`None`).
    /// The certificate a host action is admitted on: the caller's own device's, acting for its
    /// person; anything else is unknown, as a refusal is.
    fn host_actions_certificate<'a>(
        caller: &Caller,
        certificate: Option<&'a DeviceCert>,
    ) -> Result<&'a DeviceCert, ActionCallError> {
        certificate
            .filter(|certificate| {
                agents::acts_for_its_person(caller)
                    && caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
            })
            .ok_or(ActionCallError::UnknownAction)
    }

    /// Stop an agent session, for a caller that keeps the server: unknown to anyone else, and to
    /// a request that names anything but a stop.
    fn mcp_stop(
        &self,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        self.mcp_keeper(caller, certificate)?;
        if request.target != mcp_server::target(mcp_server::STOP) {
            return Err(ActionCallError::UnknownAction);
        }
        let InvokeArgument::Json(argument) = &request.argument else {
            return Err(ActionCallError::invalid_request(
                "a stop names its grant and session",
            ));
        };
        let stop: mcp_server::AgentSessionRef =
            serde_json::from_value(Value::Object(argument.clone()))
                .map_err(|error| ActionCallError::invalid_request(error.to_string()))?;
        if self.inner.mcp_server.stop(&stop.grant, &stop.session) {
            Ok(Value::Null)
        } else {
            Err(ActionCallError::Refused {
                reason: "That agent session has ended already".into(),
            })
        }
    }

    async fn invoke(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        match (request.target.provider.as_str(), workspace_id) {
            (WORKSPACE_GOVERNANCE_PROVIDER, Some(workspace_id)) => {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(asked) = mail::account_action(&request.target.address) {
                    return Box::pin(self.mail_account_invoke(
                        (Some(workspace_id), caller, certificate),
                        asked,
                        request,
                    ))
                    .await;
                }
                // The tree publishes each verb where a person looks for it

                // (`workspace/servers#add`); the verb is what it is granted and
                // checked as. An address the tree does not publish is unknown.
                let (workspace, operation) =
                    workspace_governance_operation(&request.target.address)
                        .ok_or(ActionCallError::UnknownAction)?;
                let asked = governance_request(request, workspace, operation);
                if operation == tilo_server_session::workspaces::RUN_UNATTENDED {
                    self.unattended_invoke(workspace_id, caller, certificate, &asked)
                        .await
                } else {
                    // The action the tree publishes the operation at, which says more than
                    // the operation does where one publishes several (`role` and `add` are
                    // both `member.add`).
                    let published = request.target.address.action.clone();
                    self.governance_invoke(workspace_id, caller, certificate, &asked, &published)
                        .await
                }
            }
            (HOST_ACTIONS_PROVIDER, _) => {
                let certificate = Self::host_actions_certificate(caller, certificate)?;
                Box::pin(self.account_invoke(workspace_id, caller, certificate, request, None))
                    .await
            }
            (mcp_server::PROVIDER, _) => self.mcp_stop(caller, certificate, request),
            // A request that names its secret by reference carries none, so
            // an agent acting for the person may ask it too: the vault decides
            // whether the item belongs where it would be used.
            #[cfg(not(target_arch = "wasm32"))]
            (provider, _)
                if matches!(
                    (provider, request.target.address.action.as_str()),
                    (
                        person_accounts::PROVIDER,
                        person_accounts::ADD_MAIL_WITH_ITEM
                    ) | (person_models::PROVIDER, person_models::SET_KEY_FROM_ITEM)
                        | (person_vault::PROVIDER, person_vault::ASK)
                ) =>
            {
                self.vault_use_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            // The person's own mail accounts and model providers, kept here
            // when their workspace runs here: a page has no connection to a
            // mail server and no vault of its own.
            #[cfg(not(target_arch = "wasm32"))]
            (person_accounts::PROVIDER, _) => {
                Box::pin(self.person_accounts_invoke(workspace_id, caller, certificate, request))
                    .await
            }
            #[cfg(not(target_arch = "wasm32"))]
            (person_models::PROVIDER, _) => {
                self.person_models_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            // What the person keeps to sign in with, in that same vault:
            // nothing of it is read back, and no agent asks.
            // The same holds for what their agents no longer ask them, in
            // the same store: read and taken back by the person alone, and
            // written by no request.
            #[cfg(not(target_arch = "wasm32"))]
            (person_vault::PROVIDER | person_asking::PROVIDER, _) => {
                self.person_alone_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            // The formats and zone the person reads dates in: theirs alone,
            // kept in their own workspace on their own devices.
            #[cfg(not(target_arch = "wasm32"))]
            (formats::PROVIDER, _) => {
                self.formats_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            // What the person calls important, and what may interrupt
            // them: theirs, and their own agent's to read and to keep.
            #[cfg(not(target_arch = "wasm32"))]
            (person_telling::PROVIDER, _) => {
                self.person_telling_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            // What the agents on this server learned about the person: theirs to read
            // and to change, and no agent's. A turn keeps a fact through its own tools.
            #[cfg(not(target_arch = "wasm32"))]
            (LEARNED, _) => {
                self.person_learned_invoke(workspace_id, caller, certificate, request)
                    .await
            }
            (server_account::PROVIDER, _)
                if request.target == server_account::target(server_account::ACCOUNT) =>
            {
                // The person's own account, read by their own device or by
                // a key of theirs that reaches all their workspaces (a
                // backend's server key, metering what it runs).
                let person = caller
                    .user_public_key()
                    .filter(|_| {
                        caller.acting().is_person()
                            || certificate.is_some_and(|certificate| {
                                reaches_every_workspace(caller, certificate)
                            })
                    })
                    .ok_or(ActionCallError::UnknownAction)?;
                let account = self
                    .own_account(person)
                    .ok_or(ActionCallError::UnknownAction)?;
                serde_json::to_value(account).map_err(|error| ActionCallError::Refused {
                    reason: error.to_string(),
                })
            }
            _ => Self::connections_invoke(self, (workspace_id, caller, certificate), request).await,
        }
    }

    /// Whether `caller`, on the device `certificate` certifies, keeps this
    /// host's MCP server: a device of the host's own person speaking for
    /// themselves, and no agent's. Unknown and forbidden are the same answer.
    fn mcp_keeper(
        &self,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
    ) -> Result<(), ActionCallError> {
        let host = &self.inner.host.cert;
        certificate
            .filter(|certificate| {
                caller.acting().is_person()
                    && caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
                    && certificate.user_public_key == host.user_public_key
                    && !self
                        .inner
                        .mcp_server
                        .is_agent(&certificate.device_public_key)
            })
            .map(|_| ())
            .ok_or(ActionCallError::UnknownAction)
    }

    /// Run `request` from `caller` against the workspace `workspace_id` carries,
    /// through the workspace method its wire API calls.
    async fn governance_invoke(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
        published: &str,
    ) -> Result<Value, ActionCallError> {
        let address = &request.target.address;
        if let Some(verb) = server::verb(&address.action) {
            return self
                .server_operation_invoke(workspace_id, caller, certificate, verb, request)
                .await;
        }
        let args = Args::of(&request.argument)?;
        // A device of a person's asks whether a change to who is in the workspace is its to make
        // before it signs the change itself: decided here as the change would be, and nothing
        // done. This round trip is temporary: the device will admit the call itself, through
        // `admit_invocation` over its synced grants, asking the person through the Confirm
        // frame, and that replaces this.
        let admitting = address.action == WORKSPACE_MEMBER_ADMIT;
        let (action, published) = if admitting {
            let (action, published) = admitted_change(args.required_text("change")?)
                .ok_or(ActionCallError::UnknownAction)?;
            (action, published)
        } else {
            (address.action.as_str(), published)
        };
        let check = server::check_of(action).ok_or(ActionCallError::UnknownAction)?;
        // A key's own record is read by the key itself, whatever its grant may
        // govern: the workspace lists the grants of its person's, and only the
        // key or the key that minted it revokes one.
        let listed_key = matches!(action, KEYS | REVOKE_KEY);
        let (governed, device) = match self.governed_by(workspace_id, caller).await {
            Some(found) => found,
            None if listed_key => self
                .grant_device(workspace_id, caller)
                .await
                .ok_or(ActionCallError::UnknownAction)?,
            None => return Err(ActionCallError::UnknownAction),
        };
        if check == CHOOSES_SERVERS {
            let mut workspace = governed.lock().await;
            self.hand_kept_workspace_to(&mut workspace, device.user_public_key)
                .await;
        }
        let workspace = {
            let workspace = governed.lock().await;
            // A key its person revoked acts no longer: the workspace's record
            // of the revocation refuses every call it would make here, from
            // whichever API server it comes.
            let revoked_ms = workspace
                .peer()
                .governance()
                .workspace_grants()
                .into_iter()
                .find(|grant| grant.device == hex::encode(device.device_public_key))
                .and_then(|grant| grant.revoked_ms);
            if let Some(revoked_ms) = revoked_ms {
                return Err(ActionCallError::Refused {
                    reason: format!("key_revoked: this key was revoked at {revoked_ms}"),
                });
            }
            // Unknown and forbidden are the same answer. A verb with no check
            // is the caller's own business, which its run decides: naming
            // themselves, in [`NAME`]'s case, and nobody else.
            // Anyone may leave: removing themselves is decided by the change itself.
            let leaving = action == governance::REMOVE_MEMBER
                && args.key("person")? == Some(device.user_public_key);
            let permitted = match check {
                "" => true,
                _ if leaving => true,
                CHOOSES_SERVERS => Self::chooses_servers(&workspace, &device),
                check => self.permits(
                    &device,
                    Some(&workspace),
                    WorkspaceOp::Govern(check),
                    tilo_clock::unix_now_ns(),
                ),
            };
            if address.resource != workspace_governance_resource(workspace.workspace())
                || !permitted
            {
                return Err(ActionCallError::UnknownAction);
            }
            workspace.workspace().to_owned()
        };
        // A key's own grant is its own business; anything else an agent asks
        // is decided by its grants here, and by its person's own checks, before
        // its person is asked: last, so their yes is never refused after.
        if !listed_key && let Some(verb) = VERBS.iter().find(|verb| verb.id == action) {
            self.check_governance(&device, &workspace, verb.id, &args, admitting, published)
                .await?;
            self.admit_operation(
                Some(workspace_id),
                caller,
                &device,
                &verb.operation(GOVERNANCE),
                &arguments_of(request),
            )
            .await?;
        }
        if admitting {
            return Ok(json!({}));
        }
        let call = GovernanceCall {
            workspace: &workspace,
            device: &device,
            request_id: request_id(&device.user_public_key, request.invocation),
            args: &args,
        };
        self.governance_run(action, call).await
    }

    /// The person's own checks of the governance action `action` with `args`
    /// by `device`, beyond the action itself, as carrying it out makes them: the place
    /// a person is given or invited to, and whom a removal or a ban reaches. A change the device
    /// `signs` itself is checked as its own, which this host only relays.
    async fn check_governance(
        &self,
        device: &DeviceCert,
        workspace: &str,
        action: &str,
        args: &Args,
        signs: bool,
        published: &str,
    ) -> Result<(), ActionCallError> {
        let change;
        let op = match action {
            ADD_MEMBER => {
                let person = args.required_key("person")?;
                let place = args.place_at("role")?;
                // A role is changed on a member; `add` is what gives a person a place.
                if published == WORKSPACE_MEMBER_PLACED {
                    WorkspaceOp::Role(person, place)
                } else {
                    WorkspaceOp::Place(person, place)
                }
            }
            governance::INVITE => match args.place_at("role")? {
                super::MemberPlace::Role(role) => WorkspaceOp::Invite(role),
                super::MemberPlace::Administrator => WorkspaceOp::InviteAdministrator,
                super::MemberPlace::Owner => WorkspaceOp::MakeOwner,
            },
            governance::REMOVE_MEMBER | governance::BAN | governance::UNBAN => {
                let membership = match action {
                    governance::REMOVE_MEMBER => super::collaboration::Membership::Remove,
                    governance::BAN => super::collaboration::Membership::Ban,
                    _ => super::collaboration::Membership::Unban,
                };
                change = membership.change(args.required_key("person")?);
                WorkspaceOp::Change(&change)
            }
            _ => return Ok(()),
        };
        if signs {
            return self
                .check_signed_now(device, workspace, op)
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()));
        }
        self.check_now(device, Some(workspace), op)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))
    }

    async fn governance_run(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionCallError> {
        match action {
            governance::INVITE | INVITATIONS | CANCEL_INVITATION | MEMBERS | NAME => {
                self.governance_invitations(action, call).await
            }
            ADD_MEMBER => self.governance_add_member(call).await,
            CONTENT_HELD => Ok(self.content_held(call.workspace).await),
            SERVERS | ADD_SERVER | REMOVE_SERVER | MOVE_SERVER | SET_COPIES => {
                self.governance_hosts(action, call).await
            }
            server::MACHINE_ADMIT => Box::pin(self.machine_admit(call)).await,
            WAIT_LIST | UNLIST | MAIL_INVITATION => self.governance_wait_list(action, call).await,
            PUBLISH | UNPUBLISH | PUBLICATION => self.governance_listing(action, call).await,
            governance::INSTALL_APP | UNINSTALL_APP => self.governance_apps(action, call).await,
            governance::CALL => self.call_action(call).await,
            UPLOAD_APP_FILE => self.upload_app_piece(call.workspace, call.args),
            KEYS => self.governance_keys(call).await,
            REVOKE_KEY => self.governance_revoke_key(call).await,
            _ => self.governance_members(action, call).await,
        }
    }

    /// Moderate one of the workspace's calls, or `show` what it allows: end or restart it, deny a
    /// person or let them back, or set what everyone in it may do. The change is the caller's,
    /// decided by their grants on every device that receives it. Answers what the call allows now.
    async fn call_action(&self, call: GovernanceCall<'_>) -> Result<Value, ActionCallError> {
        let GovernanceCall {
            workspace,
            device,
            request_id,
            args,
        } = call;
        let channel = args.required_text("channel")?.to_owned();
        collab_types::calls::validate_channel(&channel)
            .map_err(ActionCallError::invalid_request)?;
        if let Some(change) = args.call_change()? {
            self.moderate_call(workspace, device, request_id, &channel, change)
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()))?;
        }
        let rule = self
            .call_rule(workspace, &channel)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        Ok(json!({ "channel": channel, "call": rule }))
    }

    /// The grants this workspace records of the calling person's: each key it
    /// admitted, with the grant that minted it when one did.
    async fn governance_keys(&self, call: GovernanceCall<'_>) -> Result<Value, ActionCallError> {
        let grants = self
            .grants_of(call.workspace, &call.device.user_public_key)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        Ok(json!({"grants": grants}))
    }

    /// Revoke the key `grant` names: the caller's own or one it minted, or, for a device of the
    /// person's or a server key of theirs, any key of the person's. The workspace keeps its
    /// person's revocation, and every holder and API server refuses it from then on. An agent's
    /// grant takes back no key but its own and the ones it minted.
    async fn governance_revoke_key(
        &self,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionCallError> {
        let id = call.args.required_text("grant")?.to_owned();
        let mine =
            collab_types::workspace_grant::WorkspaceGrant::id_of(&call.device.device_public_key);
        let grants = self
            .grants_of(call.workspace, &call.device.user_public_key)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        let persons_own =
            call.device.capabilities.agent.is_none() || call.device.capabilities.server_key;
        let own = id == mine
            || grants.iter().any(|grant| {
                grant.id == id && (persons_own || grant.parent.as_deref() == Some(mine.as_str()))
            });
        if !own {
            return Err(ActionCallError::UnknownAction);
        }
        let now_ms = tilo_clock::unix_now_ns() / 1_000_000;
        let changed = self
            .revoke_grant(call.workspace, &id, now_ms)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        if !changed {
            return Err(ActionCallError::Refused {
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
    ) -> Result<Value, ActionCallError> {
        let GovernanceCall {
            workspace,
            device,
            args,
            ..
        } = call;
        let listed = |listing: Option<tilo_server_protocol::directory::Listing>| match listing {
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
                        ActionCallError::invalid_request(format!(
                            "a public workspace hands out viewer, commenter or editor, not `{text}`"
                        ))
                    })?;
                let listing = tilo_server_protocol::directory::Listing {
                    description: args.text("description")?.unwrap_or_default().to_owned(),
                    default_role,
                };
                self.publish(workspace, device, listing)
                    .await
                    .map(|listing| listed(Some(listing)))
                    .map_err(|error| ActionCallError::refused(error.to_string()))
            }
            UNPUBLISH => self
                .unpublish(workspace, device)
                .await
                .map(|()| listed(None))
                .map_err(|error| ActionCallError::refused(error.to_string())),
            _ => Ok(listed(self.listing(workspace).await)),
        }
    }

    /// The version of an app the workspace runs: choosing one, and taking the
    /// choice back. Answers what the workspace runs now.
    async fn governance_apps(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionCallError> {
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
            // Members fetch the file from this holder: it must be here first.
            if let Some(file) = &file
                && !self.keeps_app_file(workspace, file)
            {
                return Err(ActionCallError::refused(
                    "the workspace's host does not have this package file: hand it over with `upload-app-file` first",
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
        command
            .validate()
            .map_err(ActionCallError::invalid_request)?;
        let done = self
            .govern_app(workspace, device, request_id, command)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        let running = self
            .app_versions(workspace)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?
            .into_iter()
            .find(|installed| installed.release.app == app);
        Ok(json!({ "generation": done.generation, "running": running }))
    }

    /// A piece of an app version's package file, for this holder to keep and
    /// answer members with. Answers how many bytes are in, or that the
    /// whole file is.
    fn upload_app_piece(&self, workspace: &str, args: &Args) -> Result<Value, ActionCallError> {
        let file = args.required_text("file")?;
        let offset = args
            .integer("offset")?
            .ok_or_else(|| ActionCallError::invalid_request("`offset` is required"))?;
        let total = args
            .integer("total")?
            .ok_or_else(|| ActionCallError::invalid_request("`total` is required"))?;
        let data = hex::decode(args.required_text("data")?)
            .map_err(|_| ActionCallError::invalid_request("`data` must be hex"))?;
        if data.len() > super::app_files::APP_FILE_PIECE_BYTES {
            return Err(ActionCallError::invalid_request(
                "a piece is at most 256 KiB",
            ));
        }
        match self
            .upload_app_file(workspace, file, offset, total, &data)
            .map_err(ActionCallError::refused)?
        {
            super::app_files::Uploaded::Partial(received) => Ok(json!({ "received": received })),
            super::app_files::Uploaded::Complete => Ok(json!({ "received": total, "kept": true })),
        }
    }

    /// Whether the catalogue offers `verb` to `caller` on `device` in `workspace` at `now`, as
    /// the `grants` of the agent it is, if it is one, let it.
    fn offers(
        &self,
        verb: &Verb,
        (caller, device): (&Caller, &DeviceCert),
        workspace: &super::GovernedWorkspace,
        grants: Option<&agents::AgentGrants>,
        now: u64,
    ) -> bool {
        // A verb with no check is the caller's own business. Naming
        // themselves is every person's, and an agent's grant has it
        // not. Its own grant is a key's: the key lists and revokes
        // it by asking for the verb by name, and no person is
        // offered what only a key or the key that minted it can do.
        if verb.check.is_empty() {
            return verb.id == NAME && caller.acting().is_person();
        }
        if grants
            .is_some_and(|grants| grants.answer(&verb.operation(GOVERNANCE), now) == Answer::Deny)
        {
            return false;
        }
        if verb.check == CHOOSES_SERVERS {
            return self.may_choose_servers(workspace, device);
        }
        self.permits(
            device,
            Some(workspace),
            WorkspaceOp::Govern(verb.check),
            now,
        )
    }

    /// What each person among `workspace`'s people publishes (`workspace/members/<person>`) that
    /// `offered` lets the caller ask, when `query` browses the people: moving them to another
    /// place, and removing them. Every member, `own`, is offered removing themselves, leaving,
    /// the server's person as anyone; the owner is removed by nobody, themselves included.
    fn member_rows(
        &self,
        workspace: &super::GovernedWorkspace,
        (query, own): (&CatalogueQuery, [u8; 32]),
        offered: impl Fn(&Verb) -> bool,
    ) -> Vec<CatalogueRow> {
        let members = workspace_governance_child(workspace.workspace(), Some(WORKSPACE_MEMBERS));
        if query.parent_resource.as_deref() != Some(members.as_str()) {
            return Vec::new();
        }
        let verbs: Vec<(&Verb, bool)> = VERBS
            .iter()
            .filter(|verb| matches!(verb.id, ADD_MEMBER | governance::REMOVE_MEMBER))
            .map(|verb| (verb, offered(verb)))
            .collect();
        let owner = workspace.peer().owner();
        let mut rows = Vec::new();
        for member in self.people(workspace).members {
            let Some(root) = member.root().filter(|_| !member.removed) else {
                continue;
            };
            let person = hex::encode(root);
            let resource = workspace_member(workspace.workspace(), &person);
            let label = if member.name.is_empty() {
                &person
            } else {
                &member.name
            };
            let removing = |verb: &Verb| verb.id == governance::REMOVE_MEMBER;
            for (verb, _) in verbs.iter().filter(|(verb, offered)| {
                (*offered || (removing(verb) && root == own)) && !(removing(verb) && root == owner)
            }) {
                let mut row = verb.row(GOVERNANCE, &resource, label);
                row.parent_resource.clone_from(&members);
                if let Some(action) = row.action.as_mut() {
                    let (name, said) = if verb.id == ADD_MEMBER {
                        (WORKSPACE_MEMBER_PLACED, "Change role")
                    } else {
                        (WORKSPACE_MEMBER_REMOVED, verb.label)
                    };
                    action.address = ActionAddress::new(resource.as_str(), name);
                    action.action_label = said.into();
                    action.params.retain(|param| param.name != "person");
                }
                rows.push(row);
            }
        }
        rows
    }

    /// The people in `workspace`, one row each: who they are, their role,
    /// and whether they own or administer it.
    pub(super) async fn members(&self, workspace: &str) -> Result<Vec<Value>, CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let governed = governed.lock().await;
        // The workspace's governance names its one owner, on every device.
        let owner = governed.peer().owner();
        let owns = |root: [u8; 32]| root == owner;
        let people = self.people(&governed);
        drop(governed);
        Ok(people
            .members
            .iter()
            .filter(|member| !member.removed)
            .map(|member| {
                let owner = member.root().is_some_and(owns);
                json!({
                    "member": member.id,
                    "name": (!member.name.is_empty()).then_some(&member.name),
                    "role": member.role.map(Role::key),
                    "owner": owner,
                    "administrator": member.administrator,
                    "place": place(owner, member.administrator, member.role.map(Role::key)),
                })
            })
            .collect())
    }

    /// An invitation, answered with the link that hands it over.
    async fn invite_answered(&self, call: GovernanceCall<'_>) -> Result<Value, ActionCallError> {
        let GovernanceCall {
            workspace,
            device,
            request_id,
            args,
        } = call;
        let now = tilo_clock::unix_now_ns();
        let expires_at_ns = now.saturating_add(invitation_lifetime(args)?);
        let place = args.place_at("role")?;
        let wait_list_entry = args.key("wait_list_entry")?;
        let (role, makes_administrator) = place.invited_as();
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
        .map_err(|error| ActionCallError::refused(error.to_string()))?;
        let native_link = tilo_peer_state::join_transport::retained_invitation_link(
            &issued.signed_invitation,
            now,
        );
        // The invitation's own link, as every build hands it over, and the
        // short one that names it in this workspace at this host: what a
        // person's page hands over in one press, and what the mail to an
        // address carries.
        let link = self.invitation_link(workspace, &issued.signed_invitation);
        let short_link = self.short_invitation_link(workspace, &issued.signed_invitation);
        let mut answered = json!({
            "workspace": workspace,
            "invite": hex::encode(issued.invite_id),
            "invitation": hex::encode(&issued.signed_invitation),
            "link": link,
            "short_link": short_link.clone(),
            "native_link": native_link,
            "role": args.required_text("role")?,
            "administrator": makes_administrator,
            "owner": place == super::MemberPlace::Owner,
            "expires_at_ns": expires_at_ns,
        });
        // An invitation for an address on the wait list is mailed to it where the
        // entry sends mail; where it sends none, the link is handed over.
        if let (Some(entry), Some(fields)) = (wait_list_entry, answered.as_object_mut()) {
            // The mail carries the short link where there is one: the same
            // link a person hands over.
            let mailed_link = short_link.unwrap_or_else(|| link.clone());
            let mailed = self
                .mail_invite(workspace, device, entry, mailed_link, now)
                .await;
            fields.insert("sent".into(), json!(mailed.is_ok()));
            if let Err(error) = mailed {
                fields.insert(
                    "next".into(),
                    json!(format!(
                        "the invitation was not mailed ({error}): hand the link over yourself"
                    )),
                );
            }
        }
        Ok(answered)
    }

    /// Inviting, and what inviting covers: the invitations.
    async fn governance_invitations(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionCallError> {
        if action == governance::INVITE {
            return self.invite_answered(call).await;
        }
        let GovernanceCall {
            workspace,
            device,
            request_id,
            args,
        } = call;
        match action {
            INVITATIONS => {
                let pending = self
                    .pending(workspace, device)
                    .await
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                Ok(pending
                    .iter()
                    .map(|invitation| {
                        pending_json(
                            invitation,
                            self.owner_invitation(invitation.invite_id, workspace),
                        )
                    })
                    .collect())
            }
            MEMBERS => Ok(self
                .members(workspace)
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()))?
                .into()),
            NAME => {
                self.name_self(
                    workspace,
                    device,
                    args.required_text("name")?,
                    tilo_clock::unix_now_ns(),
                )
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()))?;
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
                .map_err(|error| ActionCallError::refused(error.to_string()))?;
                Ok(json!({}))
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Holding a person in the workspace by their key: answers the people as
    /// the workspace holds them now.
    async fn governance_add_member(
        &self,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionCallError> {
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
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        Ok(self
            .members(workspace)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?
            .into())
    }

    /// The workspace's servers, having another keep it as well, and having one
    /// keep it no more: each answers the servers as they are then. Choosing how
    /// many copies it keeps answers the same, and each says where its copies
    /// stand.
    async fn governance_hosts(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionCallError> {
        let GovernanceCall {
            workspace, args, ..
        } = call;
        let confirmed = match action {
            ADD_SERVER => {
                let role = args
                    .text("role")?
                    .map(|role| serde_json::from_value(Value::String(role.to_owned())))
                    .transpose()
                    .map_err(|_| {
                        ActionCallError::invalid_request("`role` is `key_holder` or `store`")
                    })?
                    .unwrap_or(tilo_peer_state::CopyRole::KeyHolder);
                self.add_server(workspace, args.required_text("host")?, role)
                    .await
                    .map_err(ActionCallError::refused)?;
                None
            }
            REMOVE_SERVER => Some(
                self.remove_server(workspace, args.required_text("server")?)
                    .await
                    .map_err(ActionCallError::refused)?,
            ),
            MOVE_SERVER => Some(
                self.move_server(
                    workspace,
                    args.required_text("server")?,
                    args.required_text("host")?,
                )
                .await
                .map_err(ActionCallError::refused)?,
            ),
            SET_COPIES => {
                let count = |name| {
                    args.integer(name)?
                        .map(u32::try_from)
                        .transpose()
                        .map_err(|_| {
                            ActionCallError::invalid_request(format!("`{name}` is too large"))
                        })
                };
                let (key_holders, stores) = (count("key_holders")?, count("stores")?);
                let policy = (key_holders.is_some() || stores.is_some()).then(|| {
                    tilo_peer_state::CopyPolicy {
                        key_holders: key_holders.unwrap_or(0),
                        stores: stores.unwrap_or(0),
                    }
                });
                self.choose_copies(workspace, policy)
                    .await
                    .map_err(ActionCallError::refused)?;
                None
            }
            _ => None,
        };
        let hosts = self
            .keeping_devices(workspace)
            .await
            .map_err(ActionCallError::refused)?;
        let copies = self
            .copies(workspace)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        let mut answered = json!({ "workspace": workspace, "servers": hosts });
        if let Some(fields) = answered.as_object_mut() {
            fields.insert("copies".into(), json!(copies));
        }
        if let (Some(confirmed), Some(fields)) = (confirmed, answered.as_object_mut()) {
            // A host that could not be reached is no longer named to anyone,
            // and lets the workspace go once it hears so.
            fields.insert("confirmed".into(), json!(confirmed));
        }
        Ok(answered)
    }

    /// How many blobs of the content `workspace`'s records name this server
    /// keeps none of yet (`lacking`), among how many its records name
    /// (`named`). A record still waiting for the keys that open it counts as
    /// one blob lacked: what it names is not known here yet.
    async fn content_held(&self, workspace: &str) -> Value {
        let lacking = self
            .content_lacking(workspace, true, &std::collections::BTreeSet::new())
            .await;
        let named = self.content_named(workspace).await;
        json!({ "workspace": workspace, "lacking": lacking, "named": named })
    }

    /// The workspace's wait list, taking an address off it, and mailing an
    /// address the invitation issued for it.
    async fn governance_wait_list(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionCallError> {
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
                .map_err(|error| ActionCallError::refused(error.to_string()))?,
            MAIL_INVITATION => self
                .mail_invite(
                    workspace,
                    device,
                    args.required_key("entry")?,
                    args.required_text("link")?.to_owned(),
                    now,
                )
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()))?,
            _ => {}
        }
        let entries = self
            .wait_list(workspace, device, now)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
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

    /// Removing, banning and unbanning members.
    async fn governance_members(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionCallError> {
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
                .map_err(|error| ActionCallError::refused(error.to_string()))
        };
        match action {
            governance::UNBAN => {
                let person = member(args.required_key("person")?);
                generation(self.unban_member(workspace, device, person).await)
            }
            governance::REMOVE_MEMBER => {
                let person = member(args.required_key("person")?);
                self.remove_member(workspace, device, person)
                    .await
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                Ok(self
                    .members(workspace)
                    .await
                    .map_err(|error| ActionCallError::refused(error.to_string()))?
                    .into())
            }
            governance::BAN => {
                let person = member(args.required_key("person")?);
                generation(self.ban_member(workspace, device, person).await)
            }
            _ => Err(ActionCallError::UnknownAction),
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
pub(crate) struct ServiceWorkspaceActions {
    pub(crate) shared: Arc<Shared>,
}

pub(crate) struct Shared {
    workspaces: OnceLock<Workspaces>,
    runtime: tokio::runtime::Handle,
    /// What each admitted invocation came to.
    log: Mutex<InvocationLog<InvocationState>>,
    /// The admitted invocations that follow the MCP server: live reads,
    /// moved on whenever the MCP server does.
    mcp_follows: Mutex<std::collections::HashSet<InvocationId>>,
    /// The admitted invocations that follow the person's mail accounts: live
    /// reads, each with the person whose accounts it reads, moved on whenever
    /// those settings change.
    account_follows: Mutex<std::collections::HashMap<InvocationId, String>>,
    /// The running asks of people's models, each with the signal that moves
    /// whenever it wrote more or ended.
    model_asks: Mutex<std::collections::HashMap<InvocationId, tokio::sync::watch::Receiver<u64>>>,
}

impl ServiceWorkspaceActions {
    /// Governance for workspaces [`attach`](Self::attach)ed later. Call it inside
    /// the runtime the server runs on.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                workspaces: OnceLock::new(),
                runtime: tokio::runtime::Handle::current(),
                log: Mutex::new(InvocationLog::default()),
                mcp_follows: Mutex::default(),
                account_follows: Mutex::default(),
                model_asks: Mutex::default(),
            }),
        }
    }

    /// Serve `workspaces`; only the first workspaces attached are served. Their members' devices
    /// ask this governance on the device action service too.
    pub(crate) fn attach(&self, workspaces: Workspaces) {
        workspaces.govern_device_actions_with(Arc::downgrade(&self.shared));
        let _ = self.shared.workspaces.set(workspaces);
    }

    /// This host for requests about workspace `workspace_id`, from a caller
    /// whose device certificate the transport did not name.
    #[must_use]
    pub(crate) fn in_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> impl ActionProvider + Clone + Send + 'static {
        InWorkspace {
            server: self.clone(),
            workspace_id: Some(workspace_id),
            certificate: None,
        }
    }

    /// This host for requests about workspace `workspace_id`, from the
    /// device whose certificate the server's handshake verified as
    /// `certificate`.
    #[must_use]
    pub(crate) fn in_workspace_as(
        &self,
        workspace_id: WorkspaceId,
        certificate: &DeviceCert,
    ) -> impl ActionProvider + Clone + Send + 'static {
        InWorkspace {
            server: self.clone(),
            workspace_id: Some(workspace_id),
            certificate: Some(certificate.clone()),
        }
    }

    /// This host for its own operations, asked of it directly by the device
    /// whose certificate the server's handshake verified as `certificate`.
    #[must_use]
    pub(crate) fn as_host(
        &self,
        certificate: &DeviceCert,
    ) -> impl ActionProvider + Clone + Send + 'static {
        InWorkspace {
            server: self.clone(),
            workspace_id: None,
            certificate: Some(certificate.clone()),
        }
    }

    fn log(&self) -> std::sync::MutexGuard<'_, InvocationLog<InvocationState>> {
        self.shared.log.lock_recover()
    }

    /// Whether `target` is the account read of a host that keeps workspaces
    /// of people's own, which it answers itself.
    fn answers_own_account(&self, target: &tilo_actions::remote::RemoteActionTarget) -> bool {
        *target == server_account::target(server_account::ACCOUNT)
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
    ) -> Result<InvocationReceipt, ActionCallError> {
        workspaces.mcp_keeper(caller, certificate)?;
        let invocation = request.invocation;
        let mut log = self.log();
        if log.reserve(caller, request)? == tilo_actions::remote::Reservation::Admitted {
            return log
                .get(caller, &invocation)
                .map(InvocationState::receipt)
                .ok_or(ActionCallError::UnknownInvocation);
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
        log.complete(&invocation, state);
        drop(log);
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
        let mut log = self.log();
        let Some(entry) = log.get_mut(caller, id) else {
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
        drop(log);
        true
    }

    /// Whether `caller` started invocation `id` here.
    fn admitted(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.log().get(caller, id).is_some()
    }

    /// Follow `workspace`'s mail accounts for `caller`, whom its policy let
    /// read them, as invocation `request` asks ([`person_accounts::WATCH`]):
    /// the accounts now, as its first item; [`Self::accounts_moved`] moves it
    /// on.
    async fn follow_accounts(
        &self,
        workspaces: &Workspaces,
        caller: &Caller,
        workspace: String,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let invocation = request.invocation;
        // Read before the log is taken: the log is never held across a wait.
        let value = workspaces.accounts_listing(&workspace).await?;
        let mut log = self.log();
        if log.reserve(caller, request)? == tilo_actions::remote::Reservation::Admitted {
            return log
                .get(caller, &invocation)
                .map(InvocationState::receipt)
                .ok_or(ActionCallError::UnknownInvocation);
        }
        let state = InvocationState::new(
            invocation,
            InvocationUpdate::Item {
                invocation,
                sequence: 1,
                value,
            },
        );
        let receipt = state.receipt();
        log.complete(&invocation, state);
        drop(log);
        self.shared
            .account_follows
            .lock_recover()
            .insert(invocation, workspace);
        Ok(receipt)
    }

    /// Whether `id` is a live read of the accounts `caller` follows.
    fn follows_accounts(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.shared.account_follows.lock_recover().contains_key(id) && self.admitted(caller, id)
    }

    /// Read the person's accounts again for the follow `id`; whether they
    /// moved.
    async fn accounts_moved(
        &self,
        workspaces: &Workspaces,
        caller: &Caller,
        id: &InvocationId,
    ) -> bool {
        let Some(workspace) = self.shared.account_follows.lock_recover().get(id).cloned() else {
            return false;
        };
        let Ok(value) = workspaces.accounts_listing(&workspace).await else {
            return false;
        };
        let mut log = self.log();
        let Some(entry) = log.get_mut(caller, id) else {
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
        drop(log);
        true
    }

    /// The answer `work` gives, on the server's runtime: a workspace is decided
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

/// [`ServiceWorkspaceActions`] for the workspace one workspace carries, or (with no
/// workspace) for the host's own operations asked of it directly.
#[derive(Clone)]
struct InWorkspace {
    server: ServiceWorkspaceActions,
    workspace_id: Option<WorkspaceId>,
    /// The caller's device certificate, as the transport verified it.
    certificate: Option<DeviceCert>,
}

/// How long a caller waits for the answer in its receipt; past it the receipt says the invocation
/// runs, and the caller follows it. A burst of slow operations (a backend making hundreds of
/// workspaces at once) is answered as each finishes, never left unanswered past the caller's
/// deadline while it still runs.
fn caller_wait(request: &InvokeRequest) -> Option<std::time::Duration> {
    request
        .wait_ms
        .map(|wait_ms| {
            std::time::Duration::from_millis(u64::from(wait_ms))
                .min(tilo_actions::remote::MAX_REMOTE_WAIT)
        })
        .or_else(|| super::unattended::asks_first(request))
}

impl ActionProvider for InWorkspace {
    /// The surface the workspace's package `app` publishes, from the
    /// workspace's own content: the version its governance keeps names the
    /// hash, and a member that has the bytes answers for them. No app has to
    /// run for its types to be readable.
    fn installed_surface(&self, app: &str) -> tilo_actions::remote::Pending<Option<Vec<u8>>> {
        let (Some(workspaces), Some(workspace_id)) = (
            self.server.shared.workspaces.get().cloned(),
            self.workspace_id,
        ) else {
            return Pending::ready(None);
        };
        let app = app.to_owned();
        self.server.spawn(async move {
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
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let Some(workspaces) = self.server.shared.workspaces.get().cloned() else {
            return Pending::ready(Ok(CataloguePage::from_rows("0".into(), Vec::new(), &query)));
        };
        let (caller, workspace_id, certificate) =
            (caller.clone(), self.workspace_id, self.certificate.clone());
        self.server.spawn(async move {
            Ok(workspaces
                .catalogue(workspace_id, &caller, certificate.as_ref(), &query)
                .await)
        })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        let Some(workspaces) = self.server.shared.workspaces.get().cloned() else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        if request.target == mcp_server::target(mcp_server::SERVER) {
            return Pending::ready(self.server.follow_mcp(
                &workspaces,
                caller,
                self.certificate.as_ref(),
                &request,
            ));
        }
        if request.target == person_accounts::target(person_accounts::WATCH) {
            let (host, caller, workspace_id, certificate) = (
                self.server.clone(),
                caller.clone(),
                self.workspace_id,
                self.certificate.clone(),
            );
            return self.server.spawn(async move {
                let reach = workspaces
                    .reaching_allowed(
                        (workspace_id, &caller, certificate.as_ref()),
                        Store::MAIL,
                        person_accounts::WATCH,
                        tilo_action_policy::ActionKind::Read,
                    )
                    .await?;
                host.follow_accounts(&workspaces, &caller, reach.workspace.to_string(), &request)
                    .await
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        if model_asks::is_model_ask(&request) {
            return self.ask_model(workspaces, caller, request);
        }
        let invocation = request.invocation;
        // The log's guard must not outlive the reservation: a retry reads
        // it again below.
        let reservation = self.server.log().reserve(caller, &request);
        match reservation {
            Err(error) => return Pending::ready(Err(error)),
            Ok(tilo_actions::remote::Reservation::Admitted) => {
                return Pending::ready(
                    self.server
                        .log()
                        .get(caller, &invocation)
                        .map(InvocationState::receipt)
                        .ok_or(ActionCallError::UnknownInvocation),
                );
            }
            Ok(tilo_actions::remote::Reservation::Reserved) => {}
        }
        let (host, caller, workspace_id, certificate) = (
            self.server.clone(),
            caller.clone(),
            self.workspace_id,
            self.certificate.clone(),
        );
        let wait = caller_wait(&request);
        let runtime = self.server.shared.runtime.clone();
        self.server.spawn(async move {
            // An invocation that puts a question to its person answers its caller at once that
            // it waits for their approval, rather than at the end of the wait.
            let asking = std::sync::Arc::new(tokio::sync::Notify::new());
            let told = std::sync::Arc::clone(&asking);
            let mut work = runtime.spawn(agents::ASKING.scope(told, async move {
                Box::pin(workspaces.invoke(workspace_id, &caller, certificate.as_ref(), &request))
                    .await
            }));
            let (ran, asked) = match answered_within(&mut work, wait, &asking).await {
                Ok(ran) => ran,
                Err(asked) => {
                    let receipt = still_running(&host, invocation, work, asked);
                    if asked {
                        return Err(ActionCallError::Refused {
                            reason: tilo_actions::remote::AWAITING_APPROVAL_REASON.to_owned(),
                        });
                    }
                    return Ok(receipt);
                }
            };
            ended(&host, invocation, ran, asked)
        })
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        // A follow reads the MCP server as it is now.
        if self.server.follows_mcp(caller, id)
            && let Some(workspaces) = self.server.shared.workspaces.get()
        {
            self.server.mcp_moved(workspaces, caller, id);
        }
        // A follow of the workspace's accounts is read again as they change
        // ([`Self::next_change`]): reading the record waits on it.
        self.server.log().get(caller, id).cloned()
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(moved) = self.server.next_ask_change(caller, id) {
            return Some(moved);
        }
        if self.server.follows_accounts(caller, id) {
            let workspaces = self.server.shared.workspaces.get()?.clone();
            let mut changes = workspaces.person_settings_changes();
            let (host, caller, id) = (self.server.clone(), caller.clone(), *id);
            return Some(self.server.spawn(async move {
                if host.accounts_moved(&workspaces, &caller, &id).await {
                    return;
                }
                // Bounded, as a waiter's deadline is only checked when woken.
                let _ = tokio::time::timeout(FOLLOW_WAKE, changes.changed()).await;
                host.accounts_moved(&workspaces, &caller, &id).await;
            }));
        }
        if !self.server.follows_mcp(caller, id) {
            return None;
        }
        let workspaces = self.server.shared.workspaces.get()?.clone();
        let mut changes = workspaces.inner.mcp_server.changes();
        if self.server.mcp_moved(&workspaces, caller, id) {
            return Some(Pending::ready(()));
        }
        let (host, caller, id) = (self.server.clone(), caller.clone(), *id);
        Some(self.server.spawn(async move {
            // Bounded, as a waiter's deadline is only checked when woken.
            let _ = tokio::time::timeout(FOLLOW_WAKE, changes.changed()).await;
            host.mcp_moved(&workspaces, &caller, &id);
        }))
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        if self.server.follows_mcp(caller, id) {
            self.server.shared.mcp_follows.lock_recover().remove(id);
            self.server.log().complete(
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
        if self.server.follows_accounts(caller, id) {
            self.server.shared.account_follows.lock_recover().remove(id);
            self.server.log().complete(
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
        tilo_actions::remote::Pending::ready(if self.server.admitted(caller, id) {
            Err(ActionCallError::NotCancellable {
                reason: "workspace governance completes at once".into(),
            })
        } else {
            Err(ActionCallError::UnknownInvocation)
        })
    }
}

/// The longest a live follow here waits before it reads again.
const FOLLOW_WAKE: std::time::Duration = std::time::Duration::from_secs(1);

/// The MCP server as its read answers it.
fn mcp_value(workspaces: &Workspaces) -> Value {
    serde_json::to_value(workspaces.inner.mcp_server.server()).unwrap_or(Value::Null)
}

/// The provider of what this server's agents learned about the person who asks
/// ([`super::person_learned`]).
pub(super) const LEARNED: &str = "tilo.learned";

/// The providers a server that governs workspaces answers itself.
const PROVIDERS: [&str; 11] = [
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
    // What the person's agents no longer ask them, in the same store.
    person_asking::PROVIDER,
    // The services the person connected by their address.
    person_connections::PROVIDER,
    // What the agents on this server learned about the person.
    LEARNED,
    // The formats and the zone the person reads dates in.
    formats::PROVIDER,
];

/// What a server that governs workspaces answers remote actions addressed to it
/// with: the workspaces' governance (provider [`WORKSPACE_GOVERNANCE_PROVIDER`]), the
/// host's own operations ([`HOST_ACTIONS_PROVIDER`]), and the app action server its
/// embedder supplied for everything else.
pub(crate) struct GovernedServer {
    pub(crate) governance: ServiceWorkspaceActions,
    /// Answers everything else, when the embedder serves apps.
    pub(crate) apps: Option<Arc<dyn AppActionServer>>,
}

impl GovernedServer {
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

impl GovernedServer {
    /// Whether `message` browses everything the workspace publishes: a
    /// catalogue page naming no provider, which the apps answer and the
    /// governance's own publishers join.
    fn browses(message: &RemoteActionMessage) -> bool {
        matches!(
            message,
            RemoteActionMessage::CatalogueRequest { query, .. }
                if query.provider.is_none()
                    && query.view_resource.is_none()
                    && query.canonical_route.is_none()
                    && query.target.is_none()
                    && query.action.is_none()
        )
    }

    /// `answered`, the apps' page `caller` browsed, with the browsable
    /// publishers of `governance` beside the apps': the workspace's own tree
    /// (`workspace`) is listed among its apps, and its rows are its own to
    /// list when asked by provider.
    fn beside_apps(
        &self,
        governance: &impl ActionProvider,
        caller: &Caller,
        answered: Pending<RemoteActionMessage>,
    ) -> Pending<RemoteActionMessage> {
        let publishers = governance.query(caller, CatalogueQuery::default());
        let (completer, merged) = tilo_actions::remote::pending();
        self.governance.shared.runtime.spawn(async move {
            // An answer the apps abandoned is abandoned here too.
            let Some(mut answer) = answered.await else {
                return;
            };
            if let RemoteActionMessage::CatalogueResponse {
                result: Ok(page), ..
            } = &mut answer
                && let Some(Ok(governed)) = publishers.await
            {
                for publisher in governed.providers {
                    if publisher.browsable
                        && !page.providers.iter().any(|listed| {
                            listed.provider == publisher.provider
                                && listed.workspace == publisher.workspace
                        })
                    {
                        page.providers.push(publisher);
                    }
                }
            }
            completer.complete(answer);
        });
        merged
    }

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

impl AppActionServer for GovernedServer {
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
        let browses = Self::browses(&message);
        let answered = self.apps.as_ref()?.serve(workspace_id, caller, message)?;
        Some(if browses {
            self.beside_apps(
                &self.governance.in_workspace(workspace_id),
                caller,
                answered,
            )
        } else {
            answered
        })
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
        let browses = Self::browses(&message);
        let answered =
            self.apps
                .as_ref()?
                .serve_certified(workspace_id, caller, certificate, message)?;
        Some(if browses {
            self.beside_apps(
                &self.governance.in_workspace_as(workspace_id, certificate),
                caller,
                answered,
            )
        } else {
            answered
        })
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

    fn pages(&self) -> Option<&dyn crate::app_serving::AppPages> {
        self.apps.as_deref()?.pages()
    }
}

#[cfg(all(test, feature = "agents"))]
mod openai_sign_in_tests;
#[cfg(test)]
mod person_learned_tests;
#[cfg(test)]
mod person_model_tests;
#[cfg(test)]
mod share_tests;
#[cfg(test)]
mod tests;
