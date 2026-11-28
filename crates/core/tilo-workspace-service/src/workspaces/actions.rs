//! The workspaces this device hosts, as their own governance actions.
//!
//! A workspace's administration is its public surface like any app's: provider
//! [`WORKSPACE_GOVERNANCE_PROVIDER`] (`tilo.workspace`) publishes it on the
//! workspace's governance resource (`workspace.permissions:<workspace>`), so an agent,
//! a script, the palette or a screen reader invites, lists and cancels
//! invitations, admits and declines join requests, sets roles, restricts and
//! unbans members over the device link's actions, without the
//! `/workspaces` pages those callers cannot speak. The
//! desktop administers the workspaces it holds this way too.
//!
//! Beside it, two providers carry what is not one workspace's governance
//! (`tilo_server_protocol::workspaces` names them):
//!
//! - the holder's own operations ([`HOST_ACTIONS_PROVIDER`], `host`):
//!   creating, listing and entering workspaces and letting an app author one,
//!   which only devices of the server's own person may, decided as
//!   [`Workspaces::authorize`] decides every [`WorkspaceOp::Host`]. They are asked of
//!   the holder itself over the device's link to it, naming no workspace
//!   ([`AppActionServer::serve_host`]), or on a workspace the device holds
//!   there, and need the device's certificate as the server's handshake
//!   verified it, which entering admits to the workspace. Creating a workspace may issue its first
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
//! What the holder does for its embedder is unchanged: [`HostActionServer`]
//! answers this provider and hands every other request to the app-serving
//! app action server the embedder supplied, if any.

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value, json};
use tilo_action_calls::InvocationLog;
use tilo_action_policy::roles::{self, governance};
use tilo_action_policy::{Answer, EntryPolicy, TreePath};
use tilo_action_services::{formats, mcp_server, server_account};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, Pending, RemoteActionMessage, WORKSPACE_APPS, WORKSPACE_GOVERNANCE_KIND,
    WORKSPACE_GOVERNANCE_PROVIDER, WORKSPACE_GRANT_REVOKE, WORKSPACE_GRANT_SET,
    WORKSPACE_INVITATIONS, WORKSPACE_INVITE, WORKSPACE_MEMBER_PLACED, WORKSPACE_MEMBER_REMOVED,
    WORKSPACE_MEMBERS, WORKSPACE_READ_GRANT, WORKSPACE_READ_REVOKE, WORKSPACE_REGISTRIES,
    WORKSPACE_SERVERS, WORKSPACE_TRUST, WORKSPACE_WAITING, workspace_governance_address,
    workspace_governance_child, workspace_governance_operation, workspace_governance_resource,
    workspace_member, workspace_named_argument,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ActionParam, ExecutionLocus,
    InputSpec, InvocationId, InvocationUpdate, OperationKind, ParamType, RiskLevel,
};
use tilo_peer_state::join::Role;
use tilo_peer_state::signed_actions::Arguments;
use tilo_server_protocol::workspaces::{
    ADMINISTER_WORKSPACES, CREATE_WEBHOOK, DELETE_WEBHOOK, FETCH_GRANT, HOST_ACTIONS_PROVIDER,
    HOST_WORKSPACES_RESOURCE, LIST_WEBHOOKS as WEBHOOKS,
};
use tilo_user_identity::DeviceCert;
use tokio::sync::Mutex as AsyncMutex;

#[cfg(not(target_arch = "wasm32"))]
mod agent_record;
#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-support")))]
pub(super) use agent_record::is_agents_operation;
pub(super) mod agents;
mod server_params;
use server_params::{add_server_params, copies_params, move_server_params, remove_server_params};
pub(super) mod host;
mod invocation_states;
mod server;
use invocation_states::{answered_within, ended, still_running};

use super::authority::WorkspaceOp;
use super::providers::Answered;
use super::resources::place;
use super::weak::WeakWorkspaces;
#[cfg(not(target_arch = "wasm32"))]
use super::{
    CancelInvitationRequest, CollaborationError, HostedWorkspace, IssueInvitationRequest,
    Workspaces,
};
use crate::app_serving::AppActionServer;
use tilo_lock::MutexRecover as _;
use tilo_resource::Id;
use tilo_server_protocol::WorkspaceId;

/// Listing the invitations still redeemable: part of inviting.
pub const INVITATIONS: &str = "invitation.list";
/// Cancelling an unredeemed invitation: part of inviting.
pub const CANCEL_INVITATION: &str = "invitation.cancel";
/// Revoke a key the caller holds or one it minted: the workspace records the
/// revocation, and every holder and API server refuses the key from then on.
pub(crate) const REVOKE_KEY: &str = "key.revoke";
/// The calling person's own name in the workspace: what its member list and
/// the invitations they make show for them, from any of their devices or
/// keys. Every member names themself, so it is no governance action and no
/// grant checks it.
pub(crate) const NAME: &str = "member.name";
pub use tilo_peer_state::signed_actions::ADD_MEMBER;
pub(crate) use tilo_peer_state::signed_actions::{
    ADD_ATTESTOR, ADD_REGISTRY, DISABLE_REGISTRY, ENABLE_REGISTRY, PIN_APP, PUBLISH,
    REGISTRY_CHANNEL, REMOVE_ATTESTOR, REMOVE_REGISTRY, REVOKE_PUBLISHER, SET_CONTENT_POLICY,
    UNINSTALL_APP, UNPIN_APP, UNPUBLISH,
};
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
/// The devices keeping the workspace, as every member of it reads them: each
/// one's device key, who it is of, and whether it holds the workspace's keys
/// or a store of its ciphertext. The read beside [`SET_COPIES`]: choosing how
/// many copies it wants is its owners', reading who keeps it is every
/// member's.
pub(crate) const READ_COPIES: &str = "copies.read";
/// How much of the content the workspace's records name this server keeps
/// none of yet: what is still only on its members' devices, so a device that
/// held the last copy is switched off only once it says none.
pub(crate) const CONTENT_HELD: &str = "content.held";
/// What checks choosing the workspace's servers: its owners', which no grant
/// of its governance gives ([`Workspaces::chooses_servers`]).
const CHOOSES_SERVERS: &str = "choose-servers";
/// The limits the workspace's owners set on what members post there: the
/// largest file a host keeps, and what one member's files may take, now and in
/// any hour. Every member reads them, as what their posts may take.
pub(crate) const CONTENT_POLICY: &str = "content.policy";
/// The content this host stopped keeping because a limit said so: each
/// refusal names the limit and who can raise it, so the member who posted it
/// reads why.
pub(crate) const CONTENT_REFUSED: &str = "content.refused";
/// Whether the workspace is public, and with which listing.
pub(crate) const PUBLICATION: &str = "publication";
/// `install-app`'s flag that installs the release it names only where the workspace runs no
/// version of the app yet, answering the version it runs otherwise: how opening an app the
/// device holds installs it without moving the workspace off a version it chose.
pub const IF_ABSENT: &str = "if_absent";
/// The versions of an app the workspace ran before the one it runs, the most recent first.
pub(crate) const EARLIER_APP_VERSIONS: &str = "app.earlier";
/// Taking back an install, update, pin or uninstall by the invocation it answered.
pub(crate) const UNDO_APPS: &str = "apps.undo";
/// Hand the workspace's holder a piece of an app version's package file.
pub(crate) const UPLOAD_APP_FILE: &str = "upload-app-file";
/// The attestors whose signatures the workspace accepts for packages.
pub(crate) const TRUSTED_ATTESTORS: &str = "trust.list";
/// The publisher keys the workspace refuses.
pub(crate) const REVOKED_PUBLISHERS: &str = "trust.revoked";
/// The registries the workspace fetches apps from.
pub(crate) const REGISTRIES: &str = "registry.list";
/// The apps the configured registries offer, with the version, package and publisher an install
/// names; language packs among them, which Settings offers a person.
pub(crate) const APP_OFFERS: &str = "app.offers";

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

/// One action a hosted workspace publishes.
pub struct Verb {
    pub id: &'static str,
    pub label: &'static str,
    /// The workspace governance action [`Workspaces::authorize`] checks it on, and the
    /// catalogue offers it by.
    pub check: &'static str,
    pub kind: ActionKind,
    pub params: fn() -> Vec<ActionParam>,
}

/// What the workspaces serve, in catalogue order.
const VERBS: [Verb; 54] = [
    Verb {
        id: WORKSPACE_INVITE,
        label: "Invite",
        check: governance::ADD_MEMBER,
        kind: roles::GOVERNANCE_KIND,
        params: invite_params,
    },
    Verb {
        id: INVITATIONS,
        label: "List invitations",
        check: governance::ADD_MEMBER,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: CANCEL_INVITATION,
        label: "Cancel invitation",
        check: governance::ADD_MEMBER,
        kind: roles::GOVERNANCE_KIND,
        params: invitation_params,
    },
    Verb {
        id: ADD_MEMBER,
        label: "Admit a person",
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
        check: governance::ADD_MEMBER,
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: UNLIST,
        label: "Take an address off the wait list",
        check: governance::ADD_MEMBER,
        kind: roles::GOVERNANCE_KIND,
        params: wait_list_entry_params,
    },
    Verb {
        id: MAIL_INVITATION,
        label: "Mail an invitation to an address on the wait list",
        check: governance::ADD_MEMBER,
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
        id: CONTENT_POLICY,
        label: "What members may post here",
        // Every member posts there, so every member reads what a post may take.
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: SET_CONTENT_POLICY,
        label: "Set what members may post here",
        check: governance::SETTINGS,
        kind: roles::GOVERNANCE_KIND,
        params: content_policy_params,
    },
    Verb {
        id: CONTENT_REFUSED,
        label: "The content this server stopped keeping, and why",
        // The refusal is the poster's to read: reaching the workspace is the
        // check, as reading the copies it keeps is.
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: READ_COPIES,
        label: "List the devices keeping it",
        // Who keeps a workspace is every member's to read, as the workspace they are in:
        // reaching it at all is the check, as `machine.admit`'s is.
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: governance::RENAME,
        label: "Rename the workspace",
        check: governance::RENAME,
        kind: roles::GOVERNANCE_KIND,
        params: rename_params,
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
        id: APP_OFFERS,
        label: "The apps the registries offer",
        // A read: every member may see what could be installed.
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: TRUSTED_ATTESTORS,
        label: "List the attestors this workspace trusts",
        // A reading: every member's Store needs the list, and neither the
        // attestors nor the addresses are secrets.
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: REVOKED_PUBLISHERS,
        label: "List the publisher keys this workspace refuses",
        // A reading: every member's Store needs the list, and a refused key
        // is no secret.
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: ADD_ATTESTOR,
        label: "Trust an attestor",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: attestor_params,
    },
    Verb {
        id: REMOVE_ATTESTOR,
        label: "Stop trusting an attestor",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: attestor_id_params,
    },
    Verb {
        id: REVOKE_PUBLISHER,
        label: "Refuse a publisher",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: revoke_params,
    },
    Verb {
        id: REGISTRIES,
        label: "List the registries this workspace fetches apps from",
        // A reading: every member's Store needs the list, and neither the
        // attestors nor the addresses are secrets.
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: ADD_REGISTRY,
        label: "Add a registry",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: add_registry_params,
    },
    Verb {
        id: REMOVE_REGISTRY,
        label: "Remove a registry",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: registry_params,
    },
    Verb {
        id: ENABLE_REGISTRY,
        label: "Read a registry",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: registry_params,
    },
    Verb {
        id: DISABLE_REGISTRY,
        label: "Stop reading a registry",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: registry_params,
    },
    Verb {
        id: REGISTRY_CHANNEL,
        label: "Choose a registry's release channel",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: registry_channel_params,
    },
    Verb {
        id: UNINSTALL_APP,
        label: "Let everyone run their own version of an app",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: app_params,
    },
    Verb {
        id: PIN_APP,
        label: "Keep this version",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: app_params,
    },
    Verb {
        id: UNPIN_APP,
        label: "Let the app be updated",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: app_params,
    },
    Verb {
        id: EARLIER_APP_VERSIONS,
        label: "List the versions this workspace ran before",
        // A reading: going back a version is an update to one of these, which
        // the install grant decides.
        check: "",
        kind: ActionKind::Read,
        params: app_params,
    },
    Verb {
        id: UNDO_APPS,
        label: "Take back a change of the apps",
        check: governance::INSTALL_APP,
        kind: roles::GOVERNANCE_KIND,
        params: || {
            vec![ActionParam::required(
                "invocation",
                ParamType::Text,
                "The change to take back, as its answer names it.",
            )]
        },
    },
    Verb {
        id: REVOKE_KEY,
        label: "Revoke a key",
        // Its own person's grant, and only the key itself or one it minted,
        // which the handler checks. Each grant is read as a record of its own
        // (`tilo_actions::remote::KEY_GRANTS`), never listed by a verb.
        check: "",
        kind: roles::GOVERNANCE_KIND,
        params: key_params,
    },
    Verb {
        id: WEBHOOKS,
        label: "List the webhooks this key registered",
        // A key's webhooks are its own: the handler lists the records it owns.
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: CREATE_WEBHOOK,
        label: "Register a webhook",
        // A key reaching the workspace registers one there; the handler owns it to that key.
        check: "",
        kind: roles::GOVERNANCE_KIND,
        params: webhook_params,
    },
    Verb {
        id: DELETE_WEBHOOK,
        label: "Stop a webhook this key registered",
        check: "",
        kind: roles::GOVERNANCE_KIND,
        params: webhook_delete_params,
    },
    Verb {
        id: FETCH_GRANT,
        label: "Grant your page the host's fetch proxy token",
        // Reaching the workspace at all is the check: its roster is what decides
        // which devices ask, and the token names the one asking. A host that
        // serves no proxy grants none, and answers so.
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: WORKSPACE_GRANT_REVOKE,
        label: "Revoke a grant",
        // The person who gave the grant takes it back, or whoever the workspace's policy lets
        // revoke grants: the handler checks which.
        check: "",
        kind: roles::GOVERNANCE_KIND,
        params: grant_params,
    },
    Verb {
        id: WORKSPACE_GRANT_SET,
        label: "Share a part of the workspace",
        // A member shares what they hold as their own say; the handler decides it as every
        // receiver does.
        check: "",
        kind: roles::GOVERNANCE_KIND,
        params: share_params,
    },
    Verb {
        id: WORKSPACE_READ_GRANT,
        label: "Let read",
        check: governance::GRANT,
        kind: roles::GOVERNANCE_KIND,
        params: reading_params,
    },
    Verb {
        id: WORKSPACE_READ_REVOKE,
        label: "Stop reading",
        check: governance::REVOKE,
        kind: roles::GOVERNANCE_KIND,
        params: reading_params,
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

fn rename_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "name",
        ParamType::Text,
        "The workspace's one name, which everyone in it and every list of it reads: 1 to 80 \
         characters.",
    )]
}

fn person_params() -> Vec<ActionParam> {
    vec![person_param()]
}

/// A workspace's content policy as its reading and its settling answer it.
fn content_policy_json(policy: &tilo_action_policy::ContentPolicy) -> Value {
    // A limit nobody set is no limit, which a reader shows as 0: the same as
    // what its setter writes to lift one.
    json!({
        "file_bytes": policy.file_bytes.unwrap_or(0),
        "member_bytes": policy.member_bytes.unwrap_or(0),
        "hourly_bytes": policy.hourly_bytes.unwrap_or(0),
    })
}

/// The limits a workspace's owners set on what members post there, each one
/// left out staying as it is.
fn content_policy_params() -> Vec<ActionParam> {
    vec![
        ActionParam::optional(
            "file_bytes",
            ParamType::Integer,
            "The largest file a host keeps; a larger one stays with the member who posted it.",
        ),
        ActionParam::optional(
            "member_bytes",
            ParamType::Integer,
            "What one member's files may take in the workspace together.",
        ),
        ActionParam::optional(
            "hourly_bytes",
            ParamType::Integer,
            "What one member's files may take in the workspace in any hour.",
        ),
    ]
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
        ActionParam::optional(
            "surface",
            ParamType::Text,
            "The BLAKE3 hash of the package's compiled surface, 64 hex characters; none for a package that publishes no surface.",
        ),
        ActionParam::optional(
            "package",
            ParamType::Text,
            "The package's tapp: hash, 64 hex characters; absent for a Flatpak.",
        ),
        ActionParam::optional(
            "publisher",
            ParamType::Text,
            "The publisher key that signed the package, 64 hex characters; absent for a Flatpak.",
        ),
        ActionParam::optional(
            "flatpak",
            ParamType::Text,
            "A Flatpak reference (`app/org.example.App/x86_64/stable`): run that Flatpak instead of a Tilo package. The app id is its application id, lowercased.",
        ),
        ActionParam::optional(
            "dev",
            ParamType::Boolean,
            "A developer's own build, never reviewed by a registry.",
        ),
        ActionParam::optional(
            IF_ABSENT,
            ParamType::Boolean,
            "Install it only where the workspace runs no version of this app yet; else leave the version it runs.",
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

fn attestor_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "id",
            ParamType::Text,
            "The attestor's id, as a package's `[[trust.attested_by]]` names it.",
        ),
        ActionParam::required(
            "pubkey",
            ParamType::Text,
            "The attestor's Ed25519 public key, 64 hex characters.",
        ),
        ActionParam::optional(
            "scope",
            ParamType::Text,
            "What its signature covers: `all`, `mime:<list>` or `prefix:<list>`; `all` when omitted.",
        ),
    ]
}

fn attestor_id_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "id",
        ParamType::Text,
        "The attestor's id, as listing the trust list reports it.",
    )]
}

fn revoke_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "pubkey",
            ParamType::Text,
            "The publisher's Ed25519 public key, 64 hex characters.",
        ),
        ActionParam::optional(
            "reason",
            ParamType::Text,
            "Why it is refused; shown wherever the list is read.",
        ),
    ]
}

fn registry_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "name",
        ParamType::Text,
        "The registry, as listing the registries reports it.",
    )]
}

fn add_registry_params() -> Vec<ActionParam> {
    let mut params = registry_params();
    params.extend([
        ActionParam::required(
            "url",
            ParamType::Text,
            "Its address: credential-free HTTPS, or loopback HTTP for development.",
        ),
        ActionParam::required(
            "owner_key",
            ParamType::Text,
            "The owner key its signed catalogue is pinned to, 64 hex characters.",
        ),
        ActionParam::optional(
            "channel",
            ParamType::Text,
            "`stable` or `development`; `stable` when omitted.",
        ),
    ]);
    params
}

fn registry_channel_params() -> Vec<ActionParam> {
    let mut params = registry_params();
    params.push(ActionParam::required(
        "channel",
        ParamType::Text,
        "`stable` or `development`.",
    ));
    params
}

fn grant_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "grant",
        ParamType::Text,
        "The grant, by its id: what `workspace/permissions/<grant>` names.",
    )]
}

/// The part of `workspace` `address` names: a node in its text form, or `<workspace>/<app>/**`
/// for one whole app, Files by its name.
fn scope_of(workspace: &str, address: &str) -> Result<TreePath, ActionCallError> {
    let node = tilo_failure::ignored!(
        "an address that is no node's text form is read as `<workspace>/<app>/**`",
        address.parse::<TreePath>().ok()
    );
    if let Some(node) = node
        && node.workspace_id().is_some()
    {
        return Ok(node);
    }
    let app = (address.strip_prefix(workspace))
        .and_then(|rest| rest.strip_prefix('/'))
        .map(|rest| rest.strip_suffix("/**").unwrap_or(rest))
        .filter(|app| !app.is_empty() && !app.contains('/'))
        .ok_or_else(|| {
            ActionCallError::invalid_request(format!("`{address}` names no part of this workspace"))
        })?;
    let files = tilo_actions::tree::names_files(app);
    let app = if files {
        tilo_actions::tree::FILES_APP
    } else {
        app
    };
    Ok(TreePath::workspace(workspace).app(app).rest())
}

/// Who is given which part of the workspace, at which role.
fn share_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "person",
            ParamType::Text,
            "The person, by their user root key (64 hex characters), or `everyone`.",
        ),
        ActionParam::required(
            "address",
            ParamType::Text,
            "The part of the workspace, such as `<workspace>/files/**`.",
        ),
        ActionParam::optional(
            "role",
            ParamType::Text,
            "The role over it; none takes the share back.",
        )
        .with_choices(["viewer", "commenter", "editor", "moderator"]),
    ]
}

/// Who reads which resource: the resource, and the member or the role.
fn reading_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "resource",
            ParamType::Resource,
            "The resource, as the workspace publishes it (`tl:<app>:<id>`).",
        ),
        ActionParam::optional(
            "member",
            ParamType::Object,
            "The member: a person, an agent or a bot, as a program names them.",
        ),
        ActionParam::optional(
            "role",
            ParamType::Text,
            "Everyone holding a role, when no member is named.",
        )
        .with_choices(["viewer", "commenter", "editor", "moderator", "owner"]),
    ]
}

fn key_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "grant",
        ParamType::Text,
        "The key's grant, as the workspace lists it: the key itself or one it minted.",
    )]
}

fn webhook_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("url", ParamType::Text, "The URL events are posted to."),
        ActionParam::optional(
            "events",
            ParamType::Array,
            "The kinds posted: `change`, `invocation` or `presence`; every one when left out.",
        ),
        ActionParam::optional(
            "path",
            ParamType::Text,
            "The record whose changes are posted; every record's when left out.",
        ),
    ]
}

fn webhook_delete_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "webhook",
        ParamType::Text,
        "The webhook, as `webhook.list` names it.",
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
        ActionParam::optional(
            "admission",
            ParamType::Text,
            "Who the entry lets in: `open` admits anyone who asks, at the default role; `invite` \
             admits only people the workspace invited.",
        )
        .with_choices(["open", "invite"]),
        ActionParam::optional(
            "wait_list_max",
            ParamType::Integer,
            "How many addresses the entry keeps for whoever may invite. An `open` entry offers \
             no wait list; it keeps the number for when it takes people by invitation again.",
        ),
        ActionParam::optional(
            "contact",
            ParamType::Text,
            "Where someone asks to be taken off the wait list, shown beside a refusal.",
        ),
        ActionParam::optional(
            "mail_from",
            ParamType::Text,
            "The sender the wait list mails an address from (`hello@example.com`). Empty: it \
             lists an address at once.",
        ),
        ActionParam::optional(
            "mail_templates",
            ParamType::Text,
            "The directory the wait list's mails' words are read from. Empty: the host's own.",
        ),
    ]
}

/// Who asks one of this device's own providers: the workspace the request
/// came through, its caller, and the device the transport certified for it.
pub type Asked<'a> = (Option<WorkspaceId>, &'a Caller, Option<&'a DeviceCert>);

/// Where a provider publishes its verbs: the provider and the kind of
/// resource it publishes them on.
#[derive(Clone, Copy)]
pub struct Publisher {
    pub provider: &'static str,
    pub kind: &'static str,
}

/// A workspace's own governance.
pub const PERMISSIONS: Publisher = Publisher {
    provider: WORKSPACE_GOVERNANCE_PROVIDER,
    kind: WORKSPACE_GOVERNANCE_KIND,
};

/// A workspace's own actions, as the file its test writes beside this crate: what its root
/// catalogue row carries, so a caller reads the surface its programs compile against from the
/// workspace itself.
const WORKSPACE_SURFACE: &[u8] = include_bytes!("../../workspace-surface.json");

impl Publisher {
    /// The catalogue entry naming this provider, offered on `workspace` (empty
    /// for the holder's own).
    #[must_use]
    pub fn entry(self, workspace: &str) -> CatalogueProvider {
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
pub fn governs(provider: &str) -> bool {
    provider == WORKSPACE_GOVERNANCE_PROVIDER || provider == HOST_ACTIONS_PROVIDER
}

impl Verb {
    /// This verb as the grants deciding an agent's call of it name it.
    fn operation(&self, publisher: Publisher) -> agents::Operation<'static> {
        agents::Operation {
            provider: publisher.provider,
            kind: publisher.kind,
            action: self.id,
            effect: self.kind,
            label: self.label,
            confirms: confirms(self.id, self.kind),
        }
    }

    #[must_use]
    pub fn row(&self, publisher: Publisher, resource: &str, label: &str) -> CatalogueRow {
        action_row(
            publisher,
            (resource, label),
            (
                self.id,
                &crate::i18n::action_words(publisher.provider, self.id, self.label),
                self.kind,
            ),
            (self.params)(),
        )
    }
}

/// A provider's [`tilo_resource::Effect`] declaration as the catalogue row a [`Verb`] is, so a
/// provider the workspace answers lists its actions as it declares them rather than from a table.
#[cfg(not(target_arch = "wasm32"))]
pub trait DeclaredRow {
    /// The row of this action, published by `publisher` at `resource`, a resource labelled
    /// `label`.
    fn row(&self, publisher: Publisher, resource: &str, label: &str) -> CatalogueRow;
}

#[cfg(not(target_arch = "wasm32"))]
impl DeclaredRow for tilo_resource::EffectDecl {
    fn row(&self, publisher: Publisher, resource: &str, label: &str) -> CatalogueRow {
        action_row(
            publisher,
            (resource, label),
            (self.id, &self.label.read(), self.action_kind),
            self.params.clone(),
        )
    }
}

/// The catalogue row of the action `id`, labelled `action_label`, of kind `kind`, taking
/// `params`, published by `publisher` at `resource`, a resource labelled `label`.
pub fn action_row(
    publisher: Publisher,
    (resource, label): (&str, &str),
    (id, action_label, kind): (&str, &str, ActionKind),
    params: Vec<ActionParam>,
) -> CatalogueRow {
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
            address: ActionAddress::new(resource, id),
            resource_kind: publisher.kind.into(),
            resource_label: label.into(),
            action_label: action_label.into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: ExecutionLocus::ServerPreferred,
            kind: OperationKind::Effect,
            contract: contract_of(id, kind),
            input: InputSpec::None,
            params,
            view: None,
        }),
        has_children: false,
        visibility: None,
        surface: None,
    }
}

/// The contract of the action `id`, of kind `kind`, as the host declares it: a read risks
/// nothing, a change crosses the person's authority, and one Undo cannot take back
/// ([`tilo_actions::IRREVERSIBLE_TAG`]) is destructive, so it is confirmed before it runs.
fn contract_of(id: &str, kind: ActionKind) -> ActionContract {
    let tags = tags_of(id);
    let risk = if kind == ActionKind::Read {
        RiskLevel::None
    } else if tags.iter().any(|tag| tag == tilo_actions::IRREVERSIBLE_TAG) {
        RiskLevel::Destructive
    } else {
        RiskLevel::Privileged
    };
    let mut contract = ActionContract {
        kind,
        risk,
        tags,
        ..ActionContract::default()
    };
    contract.settle_confirmation();
    contract
}

/// Whether a call of the action `id`, of kind `kind`, waits for a confirmation before it runs,
/// as its contract declares ([`contract_of`]): an agent's waits for its person's yes, whatever
/// its grants answer.
pub(crate) fn confirms(id: &str, kind: ActionKind) -> bool {
    contract_of(id, kind).confirmation != tilo_actions::ConfirmationPolicy::None
}

/// The contract tags one of the host's verbs carries as its declaration states them: a secret
/// answer is kept in no record ([`tilo_actions::SECRET_ANSWER_TAG`]), and a change to which
/// servers keep a workspace runs only where the owner's root is kept
/// ([`tilo_actions::ROOT_ONLY_TAG`]).
pub fn tags_of(action: &str) -> Vec<String> {
    let mut tags = Vec::new();
    if tilo_actions::remote::answers_a_secret(action) {
        tags.push(tilo_actions::SECRET_ANSWER_TAG.to_owned());
    }
    // Which servers keep a workspace changes only under the key of a device of the owner's
    // that keeps their identity.
    if matches!(action, ADD_SERVER | REMOVE_SERVER | MOVE_SERVER) {
        tags.push(tilo_actions::ROOT_ONLY_TAG.to_owned());
    }
    // A deleted workspace is gone with everything kept of it: nothing brings it back.
    if action == tilo_server_protocol::workspaces::DELETE_WORKSPACE {
        tags.push(tilo_actions::IRREVERSIBLE_TAG.to_owned());
    }
    tags
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
            Some(WORKSPACE_TRUST) => "Trust",
            Some(WORKSPACE_REGISTRIES) => "App registries",
            Some(WORKSPACE_MEMBERS) => "People",
            Some(WORKSPACE_INVITATIONS) => "Invitations",
            Some(WORKSPACE_WAITING) => "Waiting to be invited",
            Some(tilo_actions::remote::WORKSPACE_AGENTS) => "Agents",
            Some(other) => other,
            None => name,
        };
        let mut row = self.row(PERMISSIONS, &address.resource, label);
        if child.is_some() {
            row.parent_resource.clone_from(&root);
        }
        // The workspace's people each hold what changes their place; its apps publish every
        // change of an app on the apps themselves, so they hold nothing under them.
        row.has_children = matches!(child, Some(WORKSPACE_MEMBERS));
        if let Some(action) = row.action.as_mut() {
            // Reading the tree (`tilo_read workspace/invitations`) lists it.
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
    let mut surface = Some(String::from_utf8_lossy(WORKSPACE_SURFACE).into_owned());
    for row in &mut rows {
        if row.parent_resource.is_empty() {
            row.has_children = holds;
            // The workspace's own actions are what its root publishes: a caller reads their
            // surface from the first row of the workspace's it finds. It travels once, on that
            // row, and not again on every row beside it: a copy on each made a page of them
            // larger than the frame it is sent in.
            row.surface = surface.take();
        }
    }
    rows
}

/// `request` as the workspace's governance runs it: its `operation` on `workspace`'s root, asked
/// of the person its address names when it names one.
fn governance_request(request: &InvokeRequest, workspace: &str, operation: &str) -> InvokeRequest {
    let mut asked = request.clone();
    asked.target.address = ActionAddress::new(workspace_governance_resource(workspace), operation);
    if let Some((name, value)) = workspace_named_argument(&request.target.address) {
        let mut arguments = match asked.argument {
            InvokeArgument::Json(arguments) => arguments,
            _ => Map::new(),
        };
        arguments.insert(name.into(), value.into());
        asked.argument = InvokeArgument::Json(arguments);
    }
    asked
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
pub(super) struct Args(Map<String, Value>);

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

    /// The named arguments, as every reader of a governance action's arguments reads them.
    fn read(&self) -> Arguments<'_> {
        Arguments(&self.0)
    }

    pub(super) fn text(&self, name: &str) -> Result<Option<&str>, ActionCallError> {
        self.read()
            .optional_text(name)
            .map_err(ActionCallError::invalid_request)
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

    /// The governance changes the caller's device signed for this call
    /// ([`tilo_actions::remote::SIGNED_ARGUMENT`]), each hex-encoded: `None` when it sent none.
    fn signed(&self) -> Result<Option<Vec<Vec<u8>>>, ActionCallError> {
        signed_in(&self.0)
    }

    pub(super) fn required_text(&self, name: &str) -> Result<&str, ActionCallError> {
        self.read()
            .text(name)
            .map_err(ActionCallError::invalid_request)
    }

    /// The key `name` holds: 64 hex characters, or a person as a program gives one
    /// (`{"person": "<hex>"}`), which is how a tilolang app passes an `Actor`.
    fn key(&self, name: &str) -> Result<Option<[u8; 32]>, ActionCallError> {
        self.read()
            .optional_key(name)
            .map_err(ActionCallError::invalid_request)
    }

    fn required_key(&self, name: &str) -> Result<[u8; 32], ActionCallError> {
        self.read()
            .key(name)
            .map_err(ActionCallError::invalid_request)
    }

    fn integer(&self, name: &str) -> Result<Option<u64>, ActionCallError> {
        self.read()
            .integer(name)
            .map_err(ActionCallError::invalid_request)
    }

    fn boolean(&self, name: &str) -> Result<Option<bool>, ActionCallError> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(ActionCallError::invalid_request(format!(
                "`{name}` must be true or false"
            ))),
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

    /// The flag `name`, false when it was not given.
    pub(super) fn flag(&self, name: &str) -> Result<bool, ActionCallError> {
        self.read()
            .flag_or(name, false)
            .map_err(ActionCallError::invalid_request)
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

/// An open invitation as the workspace lists it; `owner` when its grant hands the workspace over
/// to whoever redeems it ([`super::PendingInvitation::owner`]).
fn pending_json(invitation: &super::PendingInvitation) -> Value {
    json!({
        "invite": hex::encode(invitation.invite_id),
        "role": invitation.role.key(),
        "administrator": invitation.administrator,
        "owner": invitation.owner,
        "place": place(invitation.owner, invitation.administrator, Some(invitation.role.key())),
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

/// A hosted workspace, locked.
type Listed = Arc<AsyncMutex<HostedWorkspace>>;

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
    /// Each action is offered with what it declares it writes besides its own resource
    /// ([`tilo_workspace_records::effects::declared_effects`]), so whoever lists it reads that.
    fn add(&mut self, publisher: Publisher, workspace: &str, rows: Vec<CatalogueRow>) {
        self.add_entry(publisher.entry(workspace), rows);
    }

    /// Offer `rows`, which the provider `entry` names publishes; a provider offering nothing is
    /// not named.
    fn add_entry(&mut self, entry: CatalogueProvider, mut rows: Vec<CatalogueRow>) {
        if !rows.is_empty() {
            for row in &mut rows {
                if let Some(action) = row.action.as_mut()
                    && action.contract.effects.is_empty()
                {
                    action.contract.effects = tilo_workspace_records::effects::declared_effects(
                        &tilo_actions::remote::RemoteActionTarget {
                            provider: row.provider.clone(),
                            address: action.address.clone(),
                        },
                    );
                }
            }
            self.providers.push(entry);
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
    /// own. `None` for a workspace that carries no hosted workspace, a device
    /// the workspace never certified, and a caller acting for nobody: a bot.
    async fn device_in_list(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
    ) -> Option<(Listed, DeviceCert)> {
        let (user, device) = (caller.user_public_key()?, caller.device_public_key()?);
        let hosted = self.held(workspace_id).await?;
        let workspace = hosted.lock().await;
        let host = &self.inner.host.cert;
        let cert = workspace
            .peer()
            .certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == *device)
            .or_else(|| (host.device_public_key == *device).then(|| host.clone()))?;
        drop(workspace);
        (agents::acts_for_its_person(caller) && cert.user_public_key == *user)
            .then_some((hosted, cert))
    }

    /// The certificate the workspace holds for `caller`'s device, when it holds one: the one a
    /// call the transport named no certificate for is read by, as the run the workspace
    /// admitted. `None` for a caller with no device of its own here.
    pub(super) async fn callers_certificate(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
    ) -> Option<DeviceCert> {
        let workspace_id = workspace_id?;
        self.device_in_list(workspace_id, caller)
            .await
            .map(|(_, certificate)| certificate)
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
    ) -> Option<(Listed, DeviceCert)> {
        let (user, device) = (caller.user_public_key()?, caller.device_public_key()?);
        let hosted = self.held(workspace_id).await?;
        let workspace = hosted.lock().await;
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
        (cert.user_public_key == *user).then_some((hosted, cert))
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

    /// What `caller`, on the device `certificate` when the transport named
    /// it, may do here about workspace `workspace_id` (`None` when asked of
    /// the serving device itself): the governance of the workspace it carries, their own
    /// business in that workspace, and the serving device's operations when they are its
    /// person's.
    pub async fn catalogue(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        query: &CatalogueQuery,
    ) -> CataloguePage {
        // The providers here build their rows while they answer, after the query returns: every
        // word they read reads in the language the caller declares, or the person's own.
        tilo_resource::label::reading_while(
            query.language.clone(),
            self.offered_catalogue(workspace_id, caller, certificate, query),
        )
        .await
    }

    /// [`Self::catalogue`], its words read in whatever language the thread reading it holds.
    async fn offered_catalogue(
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
            self.offer_permissions(workspace_id, caller, certificate, query, &mut offer)
                .await;
        }
        if wants(HOST_ACTIONS_PROVIDER)
            && let Some(certificate) = certificate
            && agents::acts_for_its_person(caller)
        {
            let rows = self.account_rows(workspace_id, caller, certificate).await;
            offer.add(host::ACCOUNT, "", rows);
        }
        // What the providers answering in a workspace offer the caller here.
        let call = super::providers::WorkspaceCall {
            workspace: workspace_id,
            caller,
            certificate,
        };
        for (_, answers) in self.providers().all_in_workspace() {
            for (entry, rows) in answers.offer(call, query).await {
                offer.add_entry(entry, rows);
            }
        }
        offer.page(query)
    }

    /// Offer the governance of `workspace_id` to `caller`: the governance of the workspace it
    /// carries, or, for an agent of the person's asking what it asks of its servers, the serving
    /// device's own operations there.
    async fn offer_permissions(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        query: &CatalogueQuery,
        offer: &mut Offer,
    ) {
        let operating = self
            .server_operator(workspace_id, caller, certificate)
            .await;
        let Some((hosted, device)) = self.device_in_list(workspace_id, caller).await else {
            // An agent of the person's asks what they ask of their servers: the workspace's
            // governance is not theirs.
            if let Some((hosted, _device)) = operating {
                let workspace = hosted.lock().await;
                offer.add(PERMISSIONS, workspace.workspace(), server::rows(&workspace));
            }
            return;
        };
        // What an agent's grants here refuse it is not offered to it.
        let person = agents::is_the_person(caller, &device, Some(workspace_id));
        let grants = if person {
            None
        } else {
            Some(self.agent_grants(Some(workspace_id), caller).await)
        };
        let workspace = hosted.lock().await;
        offer.generation = workspace.peer().governance().generation();
        let now = tilo_clock::unix_now_ns();
        let offered =
            |verb: &Verb| self.offers(verb, (caller, &device), &workspace, grants.as_ref(), now);
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
        // The workspace's agents, as far as the record's grants let the caller change them.
        #[cfg(not(target_arch = "wasm32"))]
        rows.extend(self.agents_rows(&id, &name, caller.acting()).await);
        // The children of its tree another provider answers (its mail accounts, its vault, the
        // model providers its agents run on), as its policy on each lets the caller ask them.
        let call = super::providers::WorkspaceCall {
            workspace: Some(workspace_id),
            caller,
            certificate,
        };
        for (_, answers) in self.providers().all_in_workspace() {
            rows.extend(answers.tree_rows(call, &id).await);
        }
        offer.add(PERMISSIONS, &id, governance_tree(rows, &name));
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

    /// Answer one of the host's own operations `request` asks for, as the device `certificate`
    /// certifies: a page's whole administration as one action
    /// ([`Self::administer_invoke`]), else one of the operations the catalogue publishes
    /// ([`Self::account_invoke`]): once, or, where another device does its work, as that
    /// device answers it.
    async fn host_operation_invoke(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Answered, ActionCallError> {
        let certificate = Self::host_actions_certificate(caller, certificate)?;
        // A page's whole administration as one action of the host's own: its requests name no
        // workspace, and each is decided as the workspace it names decides it.
        if request.target.address.resource == HOST_WORKSPACES_RESOURCE
            && request.target.address.action == ADMINISTER_WORKSPACES
        {
            return Box::pin(self.administer_invoke(caller, certificate, request))
                .await
                .map(Answered::Value);
        }
        Box::pin(self.account_invoke(workspace_id, caller, certificate, request)).await
    }

    /// Grant the device that asked a token for this host's fetch proxy
    /// ([`FETCH_GRANT`]): a page sends it with each request it makes through
    /// the proxy, which admits the person and device it names. The workspace's
    /// roster is what let the device ask at all ([`Self::governance_invoke`]);
    /// the token names it. A host that serves no proxy grants none.
    fn fetch_grant(&self, call: GovernanceCall<'_>) -> Value {
        let device = call.device;
        let token = if self.inner.host.fetch_grants.served() {
            self.inner
                .host
                .fetch_grants
                .grant(device.user_public_key, device.device_public_key)
        } else {
            None
        };
        tilo_server_protocol::workspaces::fetch_grant_answer(token)
    }

    /// Run `request` from `caller` and answer its output: a call whose result moves has none.
    pub(crate) async fn invoke(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        match Box::pin(self.answer_call(workspace_id, caller, certificate, request)).await? {
            Answered::Value(value) => Ok(value),
            Answered::Live(_) => Err(ActionCallError::invalid_request(
                "this action's result is followed as it moves, and has no one answer",
            )),
        }
    }

    /// Answer `request` from `caller` as [`Self::invoke`] does, or, where a provider answering in
    /// the workspace answers it ([`super::providers::Providers::answer_in_workspaces`]), as that
    /// provider answers it: once, or as a result that moves.
    async fn answer_call(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Answered, ActionCallError> {
        if let Some(answers) = self.providers().in_workspace(&request.target) {
            // A provider answering in a workspace answers for the one the caller's connection
            // was admitted to, where the address names none of its own: the mail accounts, the
            // vault, the model providers are the workspace's, and a caller is confined to it
            // ([`Caller::in_workspace`]).
            let workspace_id =
                workspace_id.or_else(|| caller.workspace().and_then(WorkspaceId::parse));
            let call = super::providers::WorkspaceCall {
                workspace: workspace_id,
                caller,
                certificate,
            };
            return answers.answer(call, request).await;
        }
        if request.target.provider == HOST_ACTIONS_PROVIDER {
            return Box::pin(self.host_operation_invoke(
                workspace_id,
                caller,
                certificate,
                request,
            ))
            .await;
        }
        Box::pin(self.answer_here(workspace_id, caller, certificate, request))
            .await
            .map(Answered::Value)
    }

    /// Answer `request` from `caller` by the provider it names, of those the workspaces answer
    /// themselves.
    #[allow(
        clippy::too_many_lines,
        reason = "one request answered by the provider it names: each provider has its own \
                  dispatch"
    )]
    async fn answer_here(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        match (request.target.provider.as_str(), workspace_id) {
            (WORKSPACE_GOVERNANCE_PROVIDER, Some(workspace_id)) => {
                // The tree publishes each verb where a person looks for it

                // (`workspace/servers#add`); the verb is what it is granted and
                // checked as. An address the tree does not publish is unknown.
                let (workspace, operation) =
                    workspace_governance_operation(&request.target.address)
                        .ok_or(ActionCallError::UnknownAction)?;
                #[cfg(not(target_arch = "wasm32"))]
                if agent_record::is_agents_operation(operation) {
                    return self.agents_invoke(workspace_id, caller, request).await;
                }
                // A device that holds an invitation is admitted by it, before anyone in the
                // workspace knows its key: its issuer's grant decides, not the caller's.
                if operation == ADD_MEMBER && super::admitting::redeems(request) {
                    return self.admit_invoke(workspace_id, caller, request).await;
                }
                // Who keeps a workspace is every member's to read, from whichever workspace
                // their session is in: a device of theirs asks it of another one they are in.
                if operation == READ_COPIES
                    && let Some(named) = WorkspaceId::parse(workspace)
                    && named != workspace_id
                {
                    return self.read_copies_as_member(named, caller, certificate).await;
                }
                let asked = governance_request(request, workspace, operation);
                if operation == tilo_server_protocol::workspaces::RUN_UNATTENDED {
                    self.unattended_invoke(workspace_id, caller, certificate, &asked)
                        .await
                } else {
                    // The action the tree publishes the operation at, which says more than
                    // the operation does where one publishes several (`role` and `admit` are
                    // both `member.add`).
                    let published = request.target.address.action.clone();
                    self.governance_invoke(workspace_id, caller, certificate, &asked, &published)
                        .await
                }
            }
            (mcp_server::PROVIDER, _) => self.mcp_stop(caller, certificate, request),
            // The formats and zone the person reads dates in: theirs alone,
            // kept in their own workspace on their own devices.
            #[cfg(not(target_arch = "wasm32"))]
            (formats::PROVIDER, _) => {
                self.formats_invoke(workspace_id, caller, certificate, request)
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
            _ => Err(ActionCallError::UnknownAction),
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
                // A device of the person's own: no key they made for their programs (which acts
                // as them and is still no device of theirs), no agent's grant, and no device
                // confined to one workspace (its VM, or one placed for its records).
                caller.acting().is_person()
                    && caller.key().is_none()
                    && certificate.capabilities.agent.is_none()
                    && certificate.capabilities.workspace_name().is_none()
                    && caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
                    && certificate.user_public_key == host.user_public_key
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
        // What a person asks of their servers is asked on the workspace listing them alone; the same
        // action elsewhere is that workspace's own (`rename` names the workspace).
        if let Some(verb) = server::verb(&address.action)
            && server::where_servers_are_asked(workspace_id, caller)
        {
            return self
                .server_operation_invoke(workspace_id, caller, certificate, verb, request)
                .await;
        }
        let args = Args::of(&request.argument)?;
        let action = address.action.as_str();
        let check = server::check_of(action).ok_or(ActionCallError::UnknownAction)?;
        // A key's own grant is revoked by the key itself, whatever its grant may
        // cover, or by the key that minted it. Its webhooks are its own records too.
        let key_revoke = matches!(
            action,
            REVOKE_KEY | WEBHOOKS | CREATE_WEBHOOK | DELETE_WEBHOOK
        );
        let (hosted, device) = match self.device_in_list(workspace_id, caller).await {
            Some(found) => found,
            None if key_revoke => self
                .grant_device(workspace_id, caller)
                .await
                .ok_or(ActionCallError::UnknownAction)?,
            None => return Err(ActionCallError::UnknownAction),
        };
        let workspace = {
            let workspace = hosted.lock().await;
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
            // A listing is a read, and a change to who is in the workspace is decided where it
            // is made ([`Self::check_governance`]): the pre-check is a probe then, and a key
            // may read what the workspace lists without the governance it may not change.
            let reading = VERBS
                .iter()
                .find(|verb| verb.id == action)
                .is_some_and(|verb| verb.kind == ActionKind::Read)
                || Self::permissions_are_decided_later(action);
            let permitted = match check {
                "" => true,
                _ if leaving => true,
                CHOOSES_SERVERS => Self::chooses_servers(&workspace, &device),
                check => self.permits(
                    &device,
                    Some(&workspace),
                    if reading {
                        WorkspaceOp::Read(check)
                    } else {
                        WorkspaceOp::Govern(check)
                    },
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
        if !key_revoke && let Some(verb) = VERBS.iter().find(|verb| verb.id == action) {
            self.check_governance(&device, &workspace, verb.id, &args, published)
                .await?;
            self.admit_operation(
                Some(workspace_id),
                caller,
                &device,
                &verb.operation(PERMISSIONS),
                (&arguments_of(request), request.approval().is_some()),
            )
            .await?;
        }
        let call = GovernanceCall {
            workspace: &workspace,
            device: &device,
            request_id: request_id(&device.user_public_key, request.invocation),
            args: &args,
        };
        self.governance_run(action, call).await
    }

    /// What `call` asks under, with what its device signed for it, once it has.
    fn signed_ask<'a>(
        call: &GovernanceCall<'a>,
    ) -> Result<super::collaboration::SignedAsk<'a>, ActionCallError> {
        Ok(super::collaboration::SignedAsk {
            device: call.device,
            request_id: call.request_id,
            signed: call.args.signed()?,
        })
    }

    /// The change `changed` made, or what the caller's device signs it on
    /// ([`ActionCallError::SignatureRequired`]): nothing is written until it has.
    pub(super) fn signed_change<T>(
        changed: Result<super::Signing<T>, CollaborationError>,
    ) -> Result<T, ActionCallError> {
        match changed.map_err(|error| ActionCallError::refused(error.to_string()))? {
            super::Signing::Done(done) => Ok(done),
            super::Signing::Sign(facts) => Err(ActionCallError::SignatureRequired(Box::new(facts))),
        }
    }

    /// Whether the change `action` makes is decided where it is made, so the check a governance
    /// call makes before it is only a probe: [`Self::check_governance`] decides a change to who
    /// is in the workspace, and this host decides naming it where it renames it.
    fn permissions_are_decided_later(action: &str) -> bool {
        matches!(
            action,
            ADD_MEMBER
                | WORKSPACE_INVITE
                | governance::REMOVE_MEMBER
                | governance::BAN
                | governance::UNBAN
                | governance::RENAME
        )
    }

    /// The person's own checks of the governance action `action` with `args`
    /// by `device`, beyond the action itself, as carrying it out makes them: the place
    /// a person is given or invited to, and whom a removal or a ban reaches.
    async fn check_governance(
        &self,
        device: &DeviceCert,
        workspace: &str,
        action: &str,
        args: &Args,
        published: &str,
    ) -> Result<(), ActionCallError> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        let command;
        // An invitation is letting someone in put off: issuing one is decided as letting them in
        // at its place is.
        let op = if action == WORKSPACE_INVITE {
            match args.place_at("role")? {
                super::MemberPlace::Role(role) => WorkspaceOp::Admit(role),
                super::MemberPlace::Administrator => WorkspaceOp::AdmitAdministrator,
                super::MemberPlace::Owner => WorkspaceOp::MakeOwner,
            }
        } else {
            command =
                Command::of_action(action, &args.0).map_err(ActionCallError::invalid_request)?;
            match &command {
                // A role is changed on a member; `admit` is what gives a person a place.
                Some(Command::Place { person, place, .. })
                    if published == WORKSPACE_MEMBER_PLACED =>
                {
                    WorkspaceOp::Role(*person, *place)
                }
                Some(command) => match super::collaboration::asked_check(command) {
                    Some(op) => op,
                    None => return Ok(()),
                },
                None => return Ok(()),
            }
        };
        self.check_now(device, Some(workspace), op)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per governance action: what each answers once its change is made"
    )]
    async fn governance_run(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionCallError> {
        // Every change of the apps the workspace runs is made, written and answered by the one
        // function that carries their rules ([`Self::change_app`]).
        if matches!(
            action,
            governance::INSTALL_APP | UNINSTALL_APP | PIN_APP | UNPIN_APP | UNDO_APPS
        ) {
            return self.change_app(action, call).await;
        }
        // A change the action asks for is the caller's device's to sign; what the action answers
        // is then read as the workspace stands.
        let done = match self.asked_change(action, &call).await? {
            Some(command) => Some(Self::signed_change(
                self.govern(call.workspace, Self::signed_ask(&call)?, &command)
                    .await,
            )?),
            None => None,
        };
        let generation = || json!({ "generation": done.as_ref().map(|done| done.generation) });
        match action {
            WORKSPACE_INVITE | INVITATIONS | CANCEL_INVITATION | NAME => {
                self.governance_invitations(action, call).await
            }
            ADD_MEMBER | governance::REMOVE_MEMBER => Ok(self
                .members(call.workspace)
                .await
                .map_err(ActionCallError::refused)?
                .into()),
            governance::BAN | governance::UNBAN => Ok(generation()),
            governance::RENAME => {
                let renamed = self
                    .workspace(call.workspace)
                    .await
                    .map_err(|error| ActionCallError::refused(error.to_string()))?
                    .lock()
                    .await
                    .name();
                Ok(json!({ "workspace": call.workspace, "name": renamed }))
            }
            CONTENT_HELD => Ok(self.content_held_here(call.workspace)),
            CONTENT_POLICY | SET_CONTENT_POLICY => Ok(content_policy_json(
                &self
                    .content_policy(call.workspace)
                    .await
                    .ok_or(ActionCallError::UnknownAction)?,
            )),
            CONTENT_REFUSED => {
                // The refusal is the poster's to read, and the workspace's
                // owners' and the host's person's: a member reads none of
                // another member's.
                let refused: Vec<_> = self
                    .refusals_for(call.workspace, call.device.user_public_key)
                    .await
                    .into_iter()
                    .map(|refusal| refusal.read())
                    .collect();
                Ok(json!({
                    "workspace": call.workspace,
                    "refused": refused,
                }))
            }
            READ_COPIES => self.read_copies(call.workspace).await,
            SERVERS | ADD_SERVER | REMOVE_SERVER | MOVE_SERVER | SET_COPIES => {
                self.governance_hosts(action, call).await
            }
            server::MACHINE_ADMIT => Box::pin(self.machine_admit(call)).await,
            WAIT_LIST | UNLIST | MAIL_INVITATION => self.governance_wait_list(action, call).await,
            PUBLISH | UNPUBLISH | PUBLICATION => Ok(self.governance_listing(call.workspace).await),
            TRUSTED_ATTESTORS | REVOKED_PUBLISHERS | ADD_ATTESTOR | REMOVE_ATTESTOR
            | REVOKE_PUBLISHER => {
                let (attestors, revoked) = self
                    .package_trust(call.workspace)
                    .await
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                Ok(json!({ "attestors": attestors, "revoked": revoked }))
            }
            REGISTRIES | ADD_REGISTRY | REMOVE_REGISTRY | ENABLE_REGISTRY | DISABLE_REGISTRY
            | REGISTRY_CHANNEL => {
                let registries = self
                    .registries(call.workspace)
                    .await
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                Ok(json!({ "registries": registries }))
            }
            governance::CALL => {
                let channel = call.args.required_text("channel")?;
                collab_types::calls::validate_channel(channel)
                    .map_err(ActionCallError::invalid_request)?;
                let rule = self
                    .call_rule(call.workspace, channel)
                    .await
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                Ok(json!({ "channel": channel, "call": rule }))
            }
            FETCH_GRANT => Ok(self.fetch_grant(call)),
            UPLOAD_APP_FILE => self.upload_app_piece(call.workspace, call.args),
            APP_OFFERS => self.apps_offers(call.workspace).await,
            EARLIER_APP_VERSIONS => self.earlier_app_versions(call).await,
            REVOKE_KEY => self.governance_revoke_key(call).await,
            WEBHOOKS | CREATE_WEBHOOK | DELETE_WEBHOOK => self.webhook_actions(action, call).await,
            WORKSPACE_GRANT_REVOKE => Ok(json!({ "revoked": call.args.required_text("grant")? })),
            WORKSPACE_GRANT_SET => self.set_grant(call).await,
            WORKSPACE_READ_GRANT | WORKSPACE_READ_REVOKE => Ok(json!({
                "resource": call.args.required_text("resource")?,
                "reads": action == WORKSPACE_READ_GRANT,
                "generation": done.as_ref().map(|done| done.generation),
            })),
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// The change the governance action `action` asks for with `call`'s arguments, once this
    /// host's own checks of it pass: `None` for an action that reads, or whose change is made
    /// elsewhere (naming oneself, who reads a resource; an app's change, which
    /// [`Self::change_app`] makes in one place). A grant is taken back by whoever gave it or whom
    /// the workspace lets revoke grants.
    async fn asked_change(
        &self,
        action: &str,
        call: &GovernanceCall<'_>,
    ) -> Result<
        Option<tilo_peer_state::workspace_membership::WorkspaceMembershipCommand>,
        ActionCallError,
    > {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
        let command = match action {
            NAME => return Ok(None),
            _ => match WorkspaceMembershipCommand::of_action(action, &call.args.0)
                .map_err(ActionCallError::invalid_request)?
            {
                Some(command) => command,
                None => return Ok(None),
            },
        };
        match &command {
            // A person no device of theirs has joined is made the owner by an owner invitation,
            // which hands them the workspace as they redeem it.
            WorkspaceMembershipCommand::Place {
                person,
                place: super::MemberPlace::Owner,
                name,
            } => {
                let ask = (call.request_id, name.clone());
                let invited = self.owner_to_come(call.workspace, call.device, *person, ask);
                if invited
                    .await
                    .map_err(|error| ActionCallError::refused(error.to_string()))?
                    .is_some()
                {
                    return Ok(None);
                }
            }
            WorkspaceMembershipCommand::Revoke { grant } => self.may_revoke(call, grant).await?,
            _ => {}
        }
        command
            .validate()
            .map_err(ActionCallError::invalid_request)?;
        Ok(Some(command))
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
        // The webhooks it registered stop with it.
        let described = self.described().await;
        if let Some(workspace) = described
            .iter()
            .find(|workspace| workspace.id().to_string() == call.workspace)
        {
            for webhook in super::webhooks::saved(workspace, call.workspace)
                .iter()
                .filter(|webhook| webhook.owner == id)
            {
                if let Err(error) = self
                    .change_own_shortcuts(
                        call.workspace,
                        tilo_automation::ShortcutChange::Delete(webhook.id.clone()),
                    )
                    .await
                {
                    tracing::warn!(%error, webhook = %webhook.id, "a revoked key's webhook was not removed");
                }
            }
        }
        Ok(json!({"revoked": id}))
    }

    /// The webhooks the calling key registered in this workspace: a record of the workspace,
    /// saved as a shortcut whose one trigger posts its events, and changed only through these
    /// actions. Registering signs the record as the device that keeps the workspace, so only it
    /// posts; listing and deleting reach the key's own alone.
    #[allow(
        clippy::too_many_lines,
        reason = "one webhook's actions in order: listing the key's, registering one, removing one"
    )]
    async fn webhook_actions(
        &self,
        action: &str,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionCallError> {
        use super::webhooks;
        // The key that registered a webhook, by its grant: revoking it takes its webhooks with
        // it, as it did when a server kept them.
        let owner =
            collab_types::workspace_grant::WorkspaceGrant::id_of(&call.device.device_public_key);
        let saved = || async {
            let described = self.described().await;
            let workspace = described
                .iter()
                .find(|workspace| workspace.id().to_string() == call.workspace)
                .ok_or(ActionCallError::UnknownAction)?;
            Ok::<_, ActionCallError>(webhooks::saved(workspace, call.workspace))
        };
        match action {
            WEBHOOKS => {
                let rows: Vec<Value> = saved()
                    .await?
                    .iter()
                    .filter(|webhook| webhook.owner == owner)
                    .map(|webhook| webhook.json(webhooks::refusal(&webhook.url)))
                    .collect();
                Ok(json!({"webhooks": rows}))
            }
            CREATE_WEBHOOK => {
                let url = call.args.required_text("url")?.to_owned();
                if let Some(why) = webhooks::refusal(&url) {
                    return Err(ActionCallError::Refused {
                        reason: why.to_owned(),
                    });
                }
                let events: Vec<String> = call.args.texts("events")?.into_iter().collect();
                if let Some(unknown) = events
                    .iter()
                    .find(|kind| !tilo_automation::WEBHOOK_EVENTS.contains(&kind.as_str()))
                {
                    return Err(ActionCallError::invalid_request(format!(
                        "`{unknown}` is no kind of event; `change`, `invocation` and `presence` are"
                    )));
                }
                let path = call.args.text("path")?.map(str::to_owned);
                let simple = uuid::Uuid::new_v4().simple().to_string();
                let id = format!("wh_{}", simple.get(..12).unwrap_or(&simple));
                let shortcut = webhooks::shortcut(
                    &id,
                    &url,
                    &events,
                    path.as_deref(),
                    &owner,
                    tilo_clock::unix_now_ms_i64(),
                );
                self.change_own_shortcuts(
                    call.workspace,
                    tilo_automation::ShortcutChange::Save(Box::new(shortcut)),
                )
                .await
                .map_err(ActionCallError::refused)?;
                let mut made = json!({
                    "id": id,
                    "workspace": call.workspace,
                    "url": url,
                    "events": events,
                    "key": hex::encode(self.device_cert().device_public_key),
                });
                if let Some(path) = path
                    && let Some(object) = made.as_object_mut()
                {
                    object.insert("path".into(), json!(path));
                }
                Ok(made)
            }
            DELETE_WEBHOOK => {
                let id = call.args.required_text("webhook")?;
                if !saved()
                    .await?
                    .iter()
                    .any(|webhook| webhook.id == id && webhook.owner == owner)
                {
                    return Err(ActionCallError::Refused {
                        reason: "no such webhook of this key".into(),
                    });
                }
                self.change_own_shortcuts(
                    call.workspace,
                    tilo_automation::ShortcutChange::Delete(id.to_owned()),
                )
                .await
                .map_err(ActionCallError::refused)?;
                Ok(json!({"deleted": id}))
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Whether `call`'s device may take back `grant`, one of the workspace's policy
    /// (`workspace/permissions/<grant>#revoke`): the person who gave it may, from a device of
    /// their own, and so may whoever the workspace's policy lets revoke grants.
    async fn may_revoke(
        &self,
        call: &GovernanceCall<'_>,
        grant: &tilo_action_policy::GrantId,
    ) -> Result<(), ActionCallError> {
        let hosted = self
            .workspace(call.workspace)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        let workspace = hosted.lock().await;
        let granted_by = workspace
            .peer()
            .governance()
            .member_policy()
            .map_err(|_| ActionCallError::UnknownAction)?
            .grants
            .iter()
            .find(|held| held.id == *grant)
            .map(|held| held.granted_by.clone())
            .ok_or_else(|| ActionCallError::Refused {
                reason: "This workspace has no grant by that id".into(),
            })?;
        let in_person =
            call.device.capabilities.agent.is_none() || call.device.capabilities.server_key;
        let gave_it = in_person
            && granted_by
                == tilo_action_policy::Grantee::person(hex::encode(call.device.user_public_key));
        let permitted = self.permits(
            call.device,
            Some(&workspace),
            WorkspaceOp::Govern(governance::REVOKE),
            tilo_clock::unix_now_ns(),
        );
        drop(workspace);
        if gave_it || permitted {
            return Ok(());
        }
        Err(ActionCallError::Refused {
            reason: "Only the person who gave this grant, or someone this workspace lets \
                     revoke grants, can take it back"
                .into(),
        })
    }

    /// Give a person, or everyone, a role over one part of the workspace, or take the caller's
    /// share of it back (`workspace/grants#set`). Answers the part and whether anything changed.
    async fn set_grant(&self, call: GovernanceCall<'_>) -> Result<Value, ActionCallError> {
        let grantee = match call.args.text("person")? {
            Some("everyone") => tilo_action_policy::Grantee::role(tilo_action_policy::Role::Viewer),
            _ => {
                tilo_action_policy::Grantee::person(hex::encode(call.args.required_key("person")?))
            }
        };
        let scope = scope_of(call.workspace, call.args.required_text("address")?)?;
        let role = call
            .args
            .text("role")?
            .map(|role| {
                tilo_action_policy::Role::parse(role)
                    .ok_or_else(|| ActionCallError::invalid_request(format!("`{role}` is no role")))
            })
            .transpose()?;
        let changed = self
            .set_scoped_role(call.workspace, call.device, (grantee, &scope), role)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        Ok(json!({ "scope": scope.to_string(), "changed": changed }))
    }

    /// Making the workspace a public workspace, taking it back, and what its entry
    /// is: `{"public": bool, "description"?, "default_role"?, "admission",
    /// "wait_list_max"}`.
    async fn governance_listing(&self, workspace: &str) -> Value {
        let entry_answer = |entry: &EntryPolicy| {
            let mut answer = match &entry.listing {
                Some(listing) => json!({
                    "public": true,
                    "description": listing.description,
                    "default_role": listing.default_role.key(),
                }),
                None => json!({ "public": false }),
            };
            if let Some(fields) = answer.as_object_mut() {
                fields.insert(
                    "admission".into(),
                    json!(if entry.invitation_only {
                        "invite"
                    } else {
                        "open"
                    }),
                );
                fields.insert("wait_list_max".into(), json!(entry.wait_list_max));
                fields.insert("contact".into(), json!(entry.contact));
                fields.insert(
                    "mail_from".into(),
                    json!(entry.mail.as_ref().map(|mail| &mail.from)),
                );
                fields.insert(
                    "mail_templates".into(),
                    json!(
                        entry
                            .mail
                            .as_ref()
                            .and_then(|mail| mail.templates.as_deref())
                    ),
                );
            }
            answer
        };
        entry_answer(&self.entry_policy(workspace).await.unwrap_or_default())
    }

    /// The apps `workspace`'s registries offer: each one's name, the version to install, the
    /// signed package root and publisher an install names, and the registry offering it.
    /// The versions of the app `call` names the workspace ran before the one it runs, the most
    /// recent first: `{"releases": [...]}`, each as `#update` takes it.
    async fn earlier_app_versions(
        &self,
        call: GovernanceCall<'_>,
    ) -> Result<Value, ActionCallError> {
        let app = call.args.required_text("app")?.to_owned();
        let releases = self
            .earlier_app_versions_of(call.workspace, &app)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        Ok(json!({ "releases": releases }))
    }

    async fn apps_offers(&self, workspace: &str) -> Result<Value, ActionCallError> {
        // The workspace's own registries, read here: never the list a desktop answers.
        let registries = self
            .registries(workspace)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        let registries =
            app_cli::config::registries_from_rows(&json!({ "registries": registries }))
                .unwrap_or_default();
        let listing = tokio::task::spawn_blocking(move || {
            app_cli::config::with_registries(registries, app_cli::all_offers)
        })
        .await
        .map_err(|error| ActionCallError::refused(format!("reading the registries: {error}")))?;
        let offers: Vec<Value> = listing
            .apps
            .iter()
            .map(|app| {
                json!({
                    "app": app.package_id,
                    "name": app.display_name,
                    "version": app.version,
                    "package": app.tapp_root_hash_hex,
                    "publisher": app.publisher_pubkey_hex,
                    "surface": app.tapp_surface_hex,
                    "registry": app.registry,
                })
            })
            .collect();
        Ok(json!({ "offers": offers }))
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
        workspace: &super::HostedWorkspace,
        grants: Option<&agents::AgentGrants>,
        now: u64,
    ) -> bool {
        // A verb with no check is the caller's own business. Naming
        // themselves is every person's, and an agent's grant has it
        // not. Its own grant is a key's: the key lists and revokes
        // it by asking for the verb by name, and no person is
        // offered what only a key or the key that minted it can do.
        if verb.check.is_empty() {
            // Naming themselves is every person's; so is reading what their
            // workspace trusts and where it fetches from, which their Store
            // shows them.
            return matches!(
                verb.id,
                NAME | TRUSTED_ATTESTORS | REVOKED_PUBLISHERS | REGISTRIES
            ) && caller.acting().is_person();
        }
        if grants
            .is_some_and(|grants| grants.answer(&verb.operation(PERMISSIONS), now) == Answer::Deny)
        {
            return false;
        }
        if verb.check == CHOOSES_SERVERS {
            return Self::chooses_servers(workspace, device);
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
        workspace: &super::HostedWorkspace,
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
                let mut row = verb.row(PERMISSIONS, &resource, label);
                row.parent_resource.clone_from(&members);
                if let Some(action) = row.action.as_mut() {
                    let (name, source) = if verb.id == ADD_MEMBER {
                        (WORKSPACE_MEMBER_PLACED, "Change role")
                    } else {
                        (WORKSPACE_MEMBER_REMOVED, verb.label)
                    };
                    action.address = ActionAddress::new(resource.as_str(), name);
                    action.action_label =
                        crate::i18n::action_words(PERMISSIONS.provider, name, source);
                    action.params.retain(|param| param.name != "person");
                }
                rows.push(row);
            }
        }
        rows
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
        // The invitation's grant names the place it gives: an owner's, which only the
        // workspace's owner issues, and which hands it over as its holder joins.
        let issued = self
            .issue_for(
                workspace,
                device,
                request,
                makes_administrator,
                wait_list_entry,
            )
            .await
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
            // An owner is an administrator as well: the answer says what it makes.
            "administrator": makes_administrator || place == super::MemberPlace::Owner,
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
        if action == WORKSPACE_INVITE {
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
                Ok(pending.iter().map(pending_json).collect())
            }
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
        // Which servers keep a workspace is changed under the key of a device
        // of the owner's that keeps their identity, never a server's: the
        // change is signed where it runs, so it runs only there.
        if matches!(action, ADD_SERVER | REMOVE_SERVER | MOVE_SERVER) && !self.serves_at_root() {
            return Err(ActionCallError::refused(
                "only a device of the owner's that keeps their identity changes which servers \
                 keep a workspace; run it there",
            ));
        }
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

    /// How many contents `workspace`'s members pushed this server keeps
    /// (`held`), and how many are on their way here and not kept whole yet
    /// (`going`): what is still missing from the server.
    fn content_held_here(&self, workspace: &str) -> Value {
        let held = self.content_held(workspace);
        json!({ "workspace": workspace, "held": held.held, "going": held.going })
    }

    /// [`Self::read_copies`] of `workspace`, asked from a session in another workspace by
    /// `caller`, a device of one of its members speaking for themselves. Unknown to anyone else,
    /// as the read is to a stranger in the workspace itself.
    async fn read_copies_as_member(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
    ) -> Result<Value, ActionCallError> {
        let certificate = certificate
            .filter(|certificate| {
                caller.acting().is_person()
                    && certificate.capabilities.agent.is_none()
                    && caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
            })
            .ok_or(ActionCallError::UnknownAction)?;
        let hosted = self
            .held(workspace)
            .await
            .ok_or(ActionCallError::UnknownAction)?;
        let name = {
            let hosted = hosted.lock().await;
            if !hosted.peer().is_member(&certificate.user_public_key) {
                return Err(ActionCallError::UnknownAction);
            }
            hosted.workspace().to_owned()
        };
        self.read_copies(&name).await
    }

    /// The devices keeping `workspace` ([`READ_COPIES`]): each one's device key, who it is of,
    /// the name it goes by and whether it holds the workspace's keys or a store of its
    /// ciphertext, with the copy status the workspace keeps of itself.
    async fn read_copies(&self, workspace: &str) -> Result<Value, ActionCallError> {
        let keeps = self
            .keeping_devices(workspace)
            .await
            .map_err(ActionCallError::refused)?;
        let copies = self
            .copies(workspace)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        Ok(json!({ "workspace": workspace, "keeps": keeps, "copies": copies }))
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
}

/// One governance invocation, as its workspace method takes it.
#[derive(Clone, Copy)]
pub(super) struct GovernanceCall<'a> {
    pub(super) workspace: &'a str,
    pub(super) device: &'a DeviceCert,
    pub(super) request_id: [u8; 32],
    pub(super) args: &'a Args,
}

/// The workspaces' governance as an action host, attached once the workspaces open.
#[derive(Clone)]
pub struct ServiceWorkspaceActions {
    pub(crate) shared: Arc<Shared>,
}

pub(crate) struct Shared {
    /// The workspaces this governance answers for: held without keeping them alive, since a
    /// provider this governance answers with lives in them.
    pub(crate) workspaces: OnceLock<WeakWorkspaces>,
    runtime: tokio::runtime::Handle,
    /// What each admitted invocation came to.
    log: Mutex<InvocationLog<InvocationState>>,
    /// The admitted invocations that follow the MCP server: live reads,
    /// moved on whenever the MCP server does.
    mcp_follows: Mutex<std::collections::HashSet<InvocationId>>,
    /// The admitted invocations whose result moves ([`Answered::Live`]): what it stands at, and
    /// the task keeping each move as the invocation's newest item.
    pub(super) live: Mutex<
        std::collections::HashMap<
            InvocationId,
            (
                tokio::sync::watch::Receiver<super::providers::Live>,
                tokio::task::AbortHandle,
            ),
        >,
    >,
    /// The calls that wait in their asked records (`asked_calls`).
    pub(super) asked_calls: AskedCalls,
    /// The calls kept for a device that keeps their person's root, which this host follows to
    /// how that device wrote they ended ([`ServiceWorkspaceActions::follow_root`]).
    at_root_calls: Mutex<std::collections::HashSet<InvocationId>>,
    /// The calls asked again under another id that follow the record a call before them kept for
    /// the root, by that call: they end as it does ([`ServiceWorkspaceActions::follow_kept`]).
    at_root_again: Mutex<std::collections::HashMap<InvocationId, Vec<InvocationId>>>,
    /// Set once this host follows the records of those calls.
    following_root: OnceLock<()>,
    /// Told each time a call that was not answered at once moved: its person answered it, or its
    /// work ended. What a follow of the host's own operations waits on ([`ServiceWorkspaceActions::host_change`]).
    moved: tokio::sync::watch::Sender<()>,
    /// The calls their person said yes to that wait for their device to sign their change.
    awaiting_signature: invocation_states::AwaitingSignature,
    /// How long such a call waits, in milliseconds.
    sign_wait_ms: std::sync::atomic::AtomicU64,
}

/// The calls that wait in their asked records, each with the workspace and the device certificate
/// it was asked under: what a person's approve admits it again with.
pub(super) type AskedCalls =
    Mutex<std::collections::HashMap<InvocationId, (Option<WorkspaceId>, Option<DeviceCert>)>>;

impl Default for ServiceWorkspaceActions {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceWorkspaceActions {
    /// Governance for workspaces [`attach`](Self::attach)ed later. Call it inside
    /// the runtime the server runs on.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                workspaces: OnceLock::new(),
                runtime: tokio::runtime::Handle::current(),
                log: Mutex::new(InvocationLog::default()),
                mcp_follows: Mutex::default(),
                live: Mutex::default(),
                asked_calls: Mutex::default(),
                at_root_calls: Mutex::default(),
                at_root_again: Mutex::default(),
                following_root: OnceLock::new(),
                moved: tokio::sync::watch::channel(()).0,
                awaiting_signature: Mutex::default(),
                sign_wait_ms: std::sync::atomic::AtomicU64::new(
                    u64::try_from(tilo_peer_state::asked_signing::SIGN_WAIT.as_millis())
                        .unwrap_or(u64::MAX),
                ),
            }),
        }
    }

    /// Serve `workspaces`; only the first workspaces attached are served. Their members' devices
    /// ask this governance on the device action service too.
    pub fn attach(&self, workspaces: &Workspaces) {
        workspaces.govern_device_actions_with(Arc::downgrade(&self.shared));
        workspaces.answer_asked_with(Arc::new(HostAsked(Arc::downgrade(&self.shared))));
        self.serve(workspaces);
    }

    /// Answer the governance of `workspaces` on this device's one dispatch, as every other
    /// provider is answered: a call made here reaches it there, admission among them. Every
    /// service that holds workspaces does, whether it founded them or holds them for a device
    /// that joined them ([`Self::attach`] for one that founds).
    pub(crate) fn serve(&self, workspaces: &Workspaces) {
        workspaces.providers().provide(
            WORKSPACE_GOVERNANCE_PROVIDER,
            WORKSPACE_GOVERNANCE_PROVIDER,
            super::providers::Reach::ThisDevice,
            Arc::new(Governing(self.clone())),
        );
        let _ = self.shared.workspaces.set(workspaces.downgrade());
    }

    /// This host for requests about workspace `workspace_id`, from a caller
    /// whose device certificate the transport did not name.
    #[must_use]
    pub fn in_workspace(
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
    pub fn in_workspace_as(
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

    /// Whether a provider registered to answer in the workspaces this host serves answers
    /// `target` ([`super::providers::Providers::answer_in_workspaces`]).
    fn answers_in_workspace(&self, target: &tilo_actions::remote::RemoteActionTarget) -> bool {
        self.shared
            .workspaces
            .get()
            .and_then(WeakWorkspaces::upgrade)
            .is_some_and(|workspaces| workspaces.providers().in_workspace(target).is_some())
    }

    /// Whether a provider registered under `provider` answers in the workspaces this host serves.
    fn answers_in(&self, provider: &str) -> bool {
        self.shared
            .workspaces
            .get()
            .and_then(WeakWorkspaces::upgrade)
            .is_some_and(|workspaces| {
                workspaces
                    .providers()
                    .get(provider)
                    .is_some_and(|registered| registered.in_workspace.is_some())
            })
    }

    /// Whether `target` is the account read of a host that keeps workspaces
    /// of people's own, which it answers itself.
    fn answers_own_account(&self, target: &tilo_actions::remote::RemoteActionTarget) -> bool {
        *target == server_account::target(server_account::ACCOUNT)
            && self
                .shared
                .workspaces
                .get()
                .and_then(WeakWorkspaces::upgrade)
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
        if log.reserve(caller, request)? == tilo_action_calls::Reservation::Admitted {
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

    /// How long a call its person said yes to waits for its device's signature.
    fn sign_wait(&self) -> std::time::Duration {
        std::time::Duration::from_millis(
            self.shared
                .sign_wait_ms
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// From now on, a call its person said yes to waits `wait` for its device's signature.
    #[cfg(test)]
    pub(crate) fn sign_within(&self, wait: std::time::Duration) {
        self.shared.sign_wait_ms.store(
            u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Whether `caller` started invocation `id` here.
    fn admitted(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.log().get(caller, id).is_some()
    }

    /// Follow `invocation`, kept for a device that keeps its person's root, to how that device
    /// writes it ended into its record in `workspaces` ([`Self::root_settled`]).
    pub(super) fn follow_root(&self, workspaces: &Workspaces, invocation: InvocationId) {
        self.shared.at_root_calls.lock_recover().insert(invocation);
        if self.shared.following_root.set(()).is_ok() {
            let shared = Arc::downgrade(&self.shared);
            workspaces.follow_asked_at_root(move |asks| {
                if let Some(shared) = shared.upgrade() {
                    ServiceWorkspaceActions { shared }.root_settled(&asks);
                }
            });
        }
    }

    /// Whether `invocation` waits still for a device that keeps its person's root.
    pub(super) fn follows_root(&self, invocation: InvocationId) -> bool {
        self.shared
            .at_root_calls
            .lock_recover()
            .contains(&invocation)
    }

    /// Have `again`, a call asked again under its own id, end as `kept` does, the call whose
    /// record it follows.
    pub(super) fn follow_kept(&self, kept: InvocationId, again: InvocationId) {
        self.shared
            .at_root_again
            .lock_recover()
            .entry(kept)
            .or_default()
            .push(again);
    }

    /// Follow `invocation` no more: it was never kept for the root, or it ended here.
    pub(super) fn unfollow_root(&self, invocation: InvocationId) -> bool {
        self.shared.at_root_calls.lock_recover().remove(&invocation)
    }

    /// `asks`, the calls kept for a device that keeps the person's root, as they stand: each
    /// this host kept that the device wrote how it ended ends so here, for whoever follows it,
    /// once a workspace it made is kept here as the person's own ([`Workspaces::took_in`]), and
    /// its record is forgotten.
    fn root_settled(&self, asks: &[tilo_actions::remote::asked::KeptAsk]) {
        let Some(workspaces) = self
            .shared
            .workspaces
            .get()
            .and_then(WeakWorkspaces::upgrade)
        else {
            return;
        };
        for ask in asks {
            let invocation = ask.request.invocation;
            let Some(outcome) = ask.outcome.clone().filter(InvocationUpdate::is_terminal) else {
                continue;
            };
            let again = self
                .shared
                .at_root_again
                .lock_recover()
                .remove(&invocation)
                .unwrap_or_default();
            let followers: Vec<InvocationId> = std::iter::once(invocation)
                .chain(again)
                .filter(|follower| self.unfollow_root(*follower))
                .collect();
            if followers.is_empty() {
                continue;
            }
            let (actions, workspaces, ask) = (self.clone(), workspaces.clone(), ask.clone());
            self.shared.runtime.spawn(async move {
                let person = hex::decode(&ask.person)
                    .map_err(|error| error.to_string())
                    .and_then(|person| {
                        <[u8; 32]>::try_from(person).map_err(|_| "not 32 bytes".to_owned())
                    });
                let outcome = match person {
                    Ok(person) => workspaces.took_in(person, outcome).await,
                    Err(why) => {
                        tracing::warn!(
                            ?invocation,
                            person = %ask.person,
                            "an asked call names no person this host can read ({why}); \
                             it ends as its root wrote it, and nothing it made is taken in"
                        );
                        outcome
                    }
                };
                let mut log = actions.log();
                for follower in followers {
                    let ended = outcome.clone().with_invocation(follower);
                    log.complete(&follower, InvocationState::new(follower, ended));
                }
                drop(log);
                if let Some(store) = workspaces.asked_store() {
                    store.settled(&ask.at);
                }
                actions.moved();
            });
        }
    }

    /// A call that was not answered at once moved: whoever follows one wakes and reads it again.
    pub(super) fn moved(&self) {
        self.shared.moved.send_replace(());
    }

    /// Resolves the next time the host's own operation `id`, asked by `caller` on the device
    /// `certificate` names, may have moved: while it waits for its person's yes, once they
    /// answer, and while its work goes on, once it ends. `None` for one that ended, or that is
    /// not `caller`'s.
    pub(crate) fn host_change(
        &self,
        certificate: &DeviceCert,
        caller: &Caller,
        id: &InvocationId,
    ) -> Option<Pending<()>> {
        // Taken before the state is read, so a move in between still wakes the follow.
        let mut moved = self.shared.moved.subscribe();
        if let Some(change) = self.as_host(certificate).next_change(caller, id) {
            return Some(change);
        }
        if self.log().get(caller, id)?.is_terminal() {
            return None;
        }
        Some(self.spawn(async move {
            // Bounded, as a waiter's deadline is only checked when woken.
            let _ = tokio::time::timeout(FOLLOW_WAKE, moved.changed()).await;
        }))
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
    request.wait_ms.map(|wait_ms| {
        std::time::Duration::from_millis(u64::from(wait_ms))
            .min(tilo_actions::remote::MAX_REMOTE_WAIT)
    })
}

impl ActionProvider for InWorkspace {
    /// A record is read by the workspaces this host keeps, for a caller it holds a certificate
    /// of there, so a person's device reads their own workspace's list from the server holding
    /// it as it reads it from any holder.
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        let workspaces = self.server.shared.workspaces.get()?.upgrade()?;
        Some(workspaces.resource_reads())
    }

    /// The surface the workspace's package `app` publishes, from the
    /// workspace's own content: the version its governance keeps names the
    /// hash, and a member that has the bytes answers for them. No app has to
    /// run for its types to be readable.
    fn installed_surface(&self, app: &str) -> tilo_actions::remote::Pending<Option<Vec<u8>>> {
        let (Some(workspaces), Some(workspace_id)) = (
            self.server
                .shared
                .workspaces
                .get()
                .and_then(WeakWorkspaces::upgrade),
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
            if hash.is_empty() {
                // A release that names no surface leaves it unknown, never empty: the package
                // installed here says what it publishes.
                return collab_types::app_surface::bytes_of(&app).map(<[u8]>::to_vec);
            }
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
        let Some(workspaces) = self
            .server
            .shared
            .workspaces
            .get()
            .and_then(WeakWorkspaces::upgrade)
        else {
            return Pending::ready(Ok(CataloguePage::from_rows("0".into(), Vec::new(), &query)));
        };
        let (caller, workspace_id, certificate) =
            (caller.clone(), self.workspace_id, self.certificate.clone());
        // Its rows are built after this returns: in the language the query asked in all the same.
        let language = query.language.clone();
        self.server
            .spawn(tilo_resource::label::reading_while(language, async move {
                let certificate = match certificate {
                    Some(certificate) => Some(certificate),
                    None => workspaces.callers_certificate(workspace_id, &caller).await,
                };
                Ok(workspaces
                    .catalogue(workspace_id, &caller, certificate.as_ref(), &query)
                    .await)
            }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // A transport that named no certificate carries the run itself: the certificate the
        // workspace holds for the caller's device is the one its stores are read by.
        if self.certificate.is_none()
            && let Some(workspaces) = self
                .server
                .shared
                .workspaces
                .get()
                .and_then(WeakWorkspaces::upgrade)
        {
            let this = self.clone();
            let caller = caller.clone();
            return self.server.spawn(async move {
                let certificate = workspaces
                    .callers_certificate(this.workspace_id, &caller)
                    .await;
                let asked = Self {
                    certificate,
                    ..this
                };
                asked.run_asked(&caller, request).await.unwrap_or(Err(
                    ActionCallError::Unavailable {
                        reason: "the workspace closed".into(),
                    },
                ))
            });
        }
        self.run_asked(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        // A follow reads the MCP server as it is now.
        if self.server.follows_mcp(caller, id)
            && let Some(workspaces) = self
                .server
                .shared
                .workspaces
                .get()
                .and_then(WeakWorkspaces::upgrade)
        {
            self.server.mcp_moved(&workspaces, caller, id);
        }
        // A follow of the workspace's accounts is read again as they change
        // ([`Self::next_change`]): reading the record waits on it.
        self.server.log().get(caller, id).cloned()
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        // A result that moves is read again as it moves.
        let live = self
            .server
            .shared
            .live
            .lock_recover()
            .get(id)
            .map(|(live, _)| live.clone());
        if let Some(mut live) = live
            && self.server.admitted(caller, id)
        {
            // What it stood at until now is the caller's to observe already.
            live.borrow_and_update();
            return Some(self.server.spawn(async move {
                // Bounded, as a waiter's deadline is only checked when woken.
                let _ = tokio::time::timeout(FOLLOW_WAKE, live.changed()).await;
            }));
        }
        if !self.server.follows_mcp(caller, id) {
            return None;
        }
        let workspaces = self.server.shared.workspaces.get()?.upgrade()?;
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
        let following = self
            .server
            .admitted(caller, id)
            .then(|| self.server.shared.live.lock_recover().remove(id))
            .flatten();
        if let Some((_, task)) = following {
            task.abort();
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
        // A call that waits for its person's yes, cancelled by whoever asked it: its record is
        // withdrawn, and every card of it says the asker took it back.
        let waits = self
            .server
            .log()
            .get(caller, id)
            .is_some_and(invocation_states::waits_asked);
        if waits
            && let Some(store) = self
                .server
                .shared
                .workspaces
                .get()
                .and_then(WeakWorkspaces::upgrade)
                .and_then(|workspaces| workspaces.asked_store())
            && let Some(at) = store.record(&id.0.to_string())
        {
            store.withdraw(&at, TAKEN_BACK);
            // One kept for a device that keeps the person's root ends here: nothing here runs
            // it to be told so.
            if self.server.unfollow_root(*id) {
                self.server.log().complete(
                    id,
                    InvocationState::new(
                        *id,
                        InvocationUpdate::Cancelled {
                            invocation: *id,
                            message: Some(TAKEN_BACK.to_owned()),
                        },
                    ),
                );
                self.server.moved();
            }
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

impl InWorkspace {
    fn run_asked(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        let Some(workspaces) = self
            .server
            .shared
            .workspaces
            .get()
            .and_then(WeakWorkspaces::upgrade)
        else {
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
        // A person's approve is set only where their answer admits the call again
        // ([`HostAsked::readmit`]): a frame carrying one no resolve handed back is refused whole.
        if let Err(error) = tilo_actions::remote::asked::admit_frame(
            workspaces
                .asked_store()
                .as_deref()
                .map(|store| store as &dyn tilo_actions::remote::asked::ApprovalsHanded),
            &request,
        ) {
            return Pending::ready(Err(error));
        }
        // A person's answer to an asked record, given on a device that holds their identity: a
        // key they made for their programs acts as them, and still answers nothing for them, nor
        // does a stand-in or a device confined to one workspace (its VM, or one placed for its
        // records).
        if tilo_actions::remote::asked::is_resolve(&request.target) {
            return workspaces.resolve_kept_here(caller, self.certificate.as_ref(), &request);
        }
        // The signature a call its person said yes to waits for: it runs as their yes admitted
        // it, and answers its caller as the one asked.
        if let Some(signed) = invocation_states::signed_after_yes(
            &self.server,
            caller,
            self.certificate.as_ref(),
            &request,
        ) {
            let asked = request.invocation;
            return self
                .run(
                    workspaces,
                    caller,
                    signed,
                    Ok(tilo_action_calls::Reservation::Reserved),
                )
                .map(move |ran| {
                    ran.map(|receipt| InvocationReceipt {
                        invocation: asked,
                        ..receipt
                    })
                });
        }
        let reservation = self.server.log().reserve(caller, &request);
        self.run(workspaces, caller, request, reservation)
    }
    /// Run `request` for `caller` as `reservation` admitted it, answering its receipt.
    fn run(
        &self,
        workspaces: Workspaces,
        caller: &Caller,
        request: InvokeRequest,
        reservation: Result<tilo_action_calls::Reservation, ActionCallError>,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        match reservation {
            Err(error) => return Pending::ready(Err(error)),
            Ok(tilo_action_calls::Reservation::Admitted) => {
                return Pending::ready(
                    self.server
                        .log()
                        .get(caller, &invocation)
                        .map(InvocationState::receipt)
                        .ok_or(ActionCallError::UnknownInvocation),
                );
            }
            Ok(tilo_action_calls::Reservation::Reserved) => {}
        }
        let call = invocation_states::Call {
            workspaces,
            workspace_id: self.workspace_id,
            certificate: self.certificate.clone(),
            caller: caller.clone(),
            request,
        };
        let (host, certificate) = (self.server.clone(), self.certificate.clone());
        let wait = caller_wait(&call.request);
        let runtime = self.server.shared.runtime.clone();
        self.server.spawn(async move {
            let (workspaces, workspace_id, caller, request) = (
                call.workspaces.clone(),
                call.workspace_id,
                call.caller.clone(),
                call.request.clone(),
            );
            let mut work = runtime.spawn(async move {
                Box::pin(workspaces.answer_call(
                    workspace_id,
                    &caller,
                    certificate.as_ref(),
                    &request,
                ))
                .await
            });
            let answered = match answered_within(&mut work, wait).await {
                Some(ran) => ended(&host, &call, ran).await,
                None => Ok(still_running(&host, call, work)),
            };
            host.moved();
            answered
        })
    }
}

/// The workspaces' governance on this device's one dispatch
/// ([`super::providers::Providers`]): a call names its workspace in its address, and is answered
/// as that workspace's governance answers it ([`InWorkspace`]), the actions of its agents
/// (`workspace/agents#create`, `#update`, `#delete`) and its admission (`workspace/members#admit`)
/// among them.
#[derive(Clone)]
struct Governing(ServiceWorkspaceActions);

/// The workspace a call on a workspace's own tree names: the first part of its resource
/// (`workspace.permissions:<id>/vault`), for every child of the tree alike, its vault, mail
/// accounts, models and apps as much as its people.
fn addressed_workspace(address: &tilo_actions::ActionAddress) -> Option<WorkspaceId> {
    let named = address
        .resource
        .strip_prefix(workspace_governance_resource("").as_str())?;
    let workspace = named
        .split_once('/')
        .map_or(named, |(workspace, _)| workspace);
    WorkspaceId::parse(workspace)
}

impl Governing {
    /// What the workspace `workspace_id` names decides of its own tree, for `caller`: certified
    /// by this device's own certificate where the caller is this device, as its person on it
    /// asks (the certificate [`Workspaces::device_in_list`] reads for this device too), and
    /// otherwise by the certificate the workspace reads itself.
    fn of(&self, workspace_id: Option<WorkspaceId>, caller: &Caller) -> InWorkspace {
        let certificate = self
            .0
            .shared
            .workspaces
            .get()
            .and_then(WeakWorkspaces::upgrade)
            .map(|workspaces| workspaces.inner.host.cert.clone())
            .filter(|own| {
                caller.device_public_key() == Some(&own.device_public_key)
                    && caller.user_public_key() == Some(&own.user_public_key)
            });
        InWorkspace {
            server: self.0.clone(),
            workspace_id,
            certificate,
        }
    }
}

impl ActionProvider for Governing {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let workspace_id = caller.workspace().and_then(WorkspaceId::parse);
        self.of(workspace_id, caller).query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // A call of the person's own providers (`tilo.telling`, `tilo.account`) names its
        // resource, never a workspace: it is made in the workspace the caller is in, and
        // answered there, as a call naming a workspace's action is answered in that one.
        let workspace_id = addressed_workspace(&request.target.address)
            .or_else(|| caller.workspace().and_then(WorkspaceId::parse));
        self.of(workspace_id, caller).invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.of(None, caller).observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.of(None, caller).next_change(caller, id)
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: tilo_actions::remote::Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let workspace_id = addressed_workspace(&request.target.address);
        ActionProvider::run(&self.of(workspace_id, caller), caller, request, admitted)
    }

    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &tilo_actions::remote::Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        let workspace_id = addressed_workspace(&request.target.address);
        self.of(workspace_id, caller)
            .asks_first(caller, request, admitted)
    }

    fn watch(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::LiveView, String> {
        let workspace_id = addressed_workspace(&request.target.address);
        self.of(workspace_id, caller).watch(caller, request)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        self.of(None, caller).cancel(caller, id)
    }
}

/// The longest a live follow here waits before it reads again.
const FOLLOW_WAKE: std::time::Duration = std::time::Duration::from_secs(1);

/// The MCP server as its read answers it.
fn mcp_value(workspaces: &Workspaces) -> Value {
    serde_json::to_value(workspaces.inner.mcp_server.server()).unwrap_or(Value::Null)
}

/// The provider of what this server's agents learned about the person who asks.
pub const LEARNED: &str = "tilo.learned";

/// The providers a server that hosts workspaces answers itself, beside those registered to answer
/// in its workspaces ([`super::providers::Providers::answer_in_workspaces`]).
const PROVIDERS: [&str; 4] = [
    WORKSPACE_GOVERNANCE_PROVIDER,
    HOST_ACTIONS_PROVIDER,
    mcp_server::PROVIDER,
    // The formats and the zone the person reads dates in.
    formats::PROVIDER,
];

/// What a server that hosts workspaces answers remote actions addressed to it
/// with: the workspaces' governance (provider [`WORKSPACE_GOVERNANCE_PROVIDER`]), the
/// host's own operations ([`HOST_ACTIONS_PROVIDER`]), and the app action server its
/// embedder supplied for everything else.
pub(crate) struct HostActionServer {
    pub(crate) governance: ServiceWorkspaceActions,
    /// Answers everything else, when the embedder serves apps.
    pub(crate) apps: Option<Arc<dyn AppActionServer>>,
}

impl HostActionServer {
    /// Whether `message` from `caller` is the workspaces' to answer.
    fn governs(&self, caller: &Caller, message: &RemoteActionMessage) -> bool {
        match message {
            RemoteActionMessage::CatalogueRequest { query, .. } => {
                match query.provider.as_deref() {
                    Some(provider) => {
                        PROVIDERS.contains(&provider) || self.governance.answers_in(provider)
                    }
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
                    || self.governance.answers_in_workspace(&request.target)
                    // The calls that wait for a person's yes: read in the workspace's invocations,
                    // and answered as `<record>#resolve` on that provider.
                    || request.target.provider
                        == tilo_action_services::workspace_invocations::PROVIDER
                    || tilo_actions::remote::asked::is_resolve(&request.target)
                    || self.governance.answers_own_account(&request.target)
            }
            RemoteActionMessage::Observe { invocation, .. }
            | RemoteActionMessage::Cancel { invocation } => {
                self.governance.admitted(caller, invocation)
            }
            // A record of a workspace this host keeps: read through the one reader of the
            // workspaces it holds, decided by that workspace's governance for who asks.
            RemoteActionMessage::ResourceRead { .. } => true,
            _ => false,
        }
    }
}

impl HostActionServer {
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
                && let Some(Ok(hosted)) = publishers.await
            {
                for publisher in hosted.providers {
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
                || request.target.provider == tilo_action_services::workspace_invocations::PROVIDER
                || tilo_actions::remote::asked::is_resolve(&request.target)
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

impl AppActionServer for HostActionServer {
    fn shutdown(&self) {
        if let Some(apps) = &self.apps {
            apps.shutdown();
        }
    }

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
            return tilo_action_calls::serve(
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
            return tilo_action_calls::serve(
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
        tilo_action_calls::serve(&self.governance.as_host(certificate), caller, message)
    }

    fn host_change(
        &self,
        caller: &Caller,
        certificate: &DeviceCert,
        id: &InvocationId,
    ) -> Option<Pending<()>> {
        self.governance.host_change(certificate, caller, id)
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

#[cfg(test)]
mod share_tests;
#[cfg(test)]
pub(crate) mod tests;

/// The changes `request` carries signed by the calling device ([`signed_in`]).
///
/// # Errors
/// They are not hex changes.
pub fn signed_changes(request: &InvokeRequest) -> Result<Option<Vec<Vec<u8>>>, ActionCallError> {
    match &request.argument {
        InvokeArgument::Json(arguments) => signed_in(arguments),
        _ => Ok(None),
    }
}

/// The changes `arguments` carry signed by the calling device, once its host asked it to sign
/// them ([`tilo_actions::remote::SIGNED_ARGUMENT`]): `None` before.
///
/// # Errors
/// They are not hex changes.
pub(super) fn signed_in(
    arguments: &serde_json::Map<String, Value>,
) -> Result<Option<Vec<Vec<u8>>>, ActionCallError> {
    let name = tilo_actions::remote::SIGNED_ARGUMENT;
    let invalid = || ActionCallError::invalid_request(format!("`{name}` must be hex changes"));
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(changes)) => changes
            .iter()
            .map(|change| {
                change
                    .as_str()
                    .and_then(|change| hex::decode(change).ok())
                    .ok_or_else(invalid)
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(invalid()),
    }
}

/// This host's dispatch, as an asked record's store hands it back the call it keeps.
struct HostAsked(std::sync::Weak<Shared>);

impl HostAsked {
    fn server(&self) -> Result<ServiceWorkspaceActions, ActionCallError> {
        self.0
            .upgrade()
            .map(|shared| ServiceWorkspaceActions { shared })
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "the host's actions are not running".into(),
            })
    }
}

/// What a call that no longer waits in its record is refused another answer with.
fn not_waiting() -> ActionCallError {
    ActionCallError::Refused {
        reason: "The call no longer waits for this answer".into(),
    }
}

/// What the record of a call its caller cancelled while it waited for its person says.
const TAKEN_BACK: &str = "Whoever asked took the question back";

/// The name the store knows this host's dispatch by, across its restarts.
pub(super) const HOST_DISPATCH: &str = "host";

/// The parameters the action `target` names is published with, where it is one of the
/// workspace's governance verbs or one of the host's own operations. `None` for any other.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn published_params(
    target: &tilo_actions::remote::RemoteActionTarget,
) -> Option<Vec<ActionParam>> {
    let action = target.address.action.as_str();
    if target.provider == WORKSPACE_GOVERNANCE_PROVIDER {
        return VERBS
            .iter()
            .find(|verb| verb.id == action)
            .map(|verb| (verb.params)());
    }
    // A make kept for the person's root takes what making one here takes: none of it secret,
    // so the device that keeps the root reads its name.
    let action = if super::at_root::found_target(target) {
        tilo_server_protocol::workspaces::CREATE_WORKSPACE
    } else {
        action
    };
    ((target.provider == tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER
        && target.address.resource == HOST_WORKSPACES_RESOURCE)
        || super::at_root::found_target(target))
    .then(|| {
        host::ACCOUNT_VERBS
            .iter()
            .find(|(verb, _)| verb.id == action)
            .map(|(verb, _)| (verb.params)())
    })
    .flatten()
}

/// Whether the action `target` names answers a secret, as the workspace's governance declares
/// it ([`tilo_actions::SECRET_ANSWER_TAG`]): `None` where it is none of its verbs.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub(super) fn published_answers_secret(
    target: &tilo_actions::remote::RemoteActionTarget,
) -> Option<bool> {
    (target.provider == WORKSPACE_GOVERNANCE_PROVIDER)
        .then(|| tilo_actions::remote::answers_a_secret(&target.address.action))
}

/// [`published_answers_secret`] where the host keeps none of those.
#[cfg(target_arch = "wasm32")]
pub(super) fn published_answers_secret(
    _target: &tilo_actions::remote::RemoteActionTarget,
) -> Option<bool> {
    None
}

/// [`published_params`] where the host keeps none of those.
#[cfg(target_arch = "wasm32")]
pub(super) fn published_params(
    _target: &tilo_actions::remote::RemoteActionTarget,
) -> Option<Vec<ActionParam>> {
    None
}

/// What this host keeps beside a call that waits for its person's yes, to admit it again
/// after a restart: the workspace it acts in and the device certificate it was asked under.
#[derive(serde::Serialize, serde::Deserialize)]
struct AskedContext {
    workspace: Option<String>,
    certificate: Option<DeviceCert>,
}

impl tilo_actions::remote::asked::AskedDispatch for HostAsked {
    fn context(&self, invocation: InvocationId) -> Value {
        let Ok(server) = self.server() else {
            return Value::Null;
        };
        let asked = server
            .shared
            .asked_calls
            .lock_recover()
            .get(&invocation)
            .cloned();
        asked
            .and_then(|(workspace_id, certificate)| {
                serde_json::to_value(AskedContext {
                    workspace: workspace_id.map(|id| id.to_string()),
                    certificate,
                })
                .ok()
            })
            .unwrap_or(Value::Null)
    }

    fn readmit(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        context: &Value,
    ) -> Result<(), ActionCallError> {
        let server = self.server()?;
        let workspaces = server
            .shared
            .workspaces
            .get()
            .and_then(WeakWorkspaces::upgrade)
            .ok_or_else(not_waiting)?;
        let mut asking = server.shared.asked_calls.lock_recover();
        let remembered = asking.remove(&request.invocation);
        let Some((workspace_id, certificate)) = remembered else {
            drop(asking);
            // Kept from before this host started again: admitted as a call of its own under
            // its id, which the record's approve admits once.
            let kept = serde_json::from_value::<AskedContext>(context.clone())
                .map_err(|_| not_waiting())?;
            let workspace_id = match kept.workspace {
                Some(workspace) => Some(WorkspaceId::parse(&workspace).ok_or_else(not_waiting)?),
                None => None,
            };
            let here = InWorkspace {
                server,
                workspace_id,
                certificate: kept.certificate,
            };
            here.invoke(caller, request).then(|_| {});
            return Ok(());
        };
        let mut log = server.log();
        if !log.readmit(caller, &request, invocation_states::waits_an_answer) {
            return Err(not_waiting());
        }
        // Admitted again under its own id: it is under way from this answer, so a caller that
        // follows it from here finds it, never a call with nothing to say.
        log.complete(
            &request.invocation,
            InvocationState::new(
                request.invocation,
                tilo_actions::InvocationUpdate::Accepted {
                    invocation: request.invocation,
                },
            ),
        );
        drop(log);
        drop(asking);
        server.moved();
        let here = InWorkspace {
            server,
            workspace_id,
            certificate,
        };
        // It runs for its caller, who follows it under its id: the receipt is theirs to read.
        here.run(
            workspaces,
            caller,
            request,
            Ok(tilo_action_calls::Reservation::Reserved),
        )
        .then(|_| {});
        Ok(())
    }

    fn withdrawn(&self, caller: &Caller, invocation: InvocationId, message: &str) {
        let Ok(server) = self.server() else {
            return;
        };
        let mut asking = server.shared.asked_calls.lock_recover();
        asking.remove(&invocation);
        drop(asking);
        let mut log = server.log();
        if log
            .admitted(caller, &invocation)
            .is_some_and(invocation_states::waits_an_answer)
        {
            log.complete(
                &invocation,
                InvocationState::new(
                    invocation,
                    tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some(message.to_owned()),
                    },
                ),
            );
        }
        drop(log);
        server.moved();
    }

    fn declined(&self, caller: &Caller, invocation: InvocationId) -> Result<(), ActionCallError> {
        let server = self.server()?;
        let mut asking = server.shared.asked_calls.lock_recover();
        if asking.remove(&invocation).is_none() {
            // Kept from before this host started again: it ended with it.
            return Ok(());
        }
        drop(asking);
        let mut log = server.log();
        if !log
            .admitted(caller, &invocation)
            .is_some_and(invocation_states::waits_an_answer)
        {
            return Err(not_waiting());
        }
        log.complete(
            &invocation,
            InvocationState::new(
                invocation,
                tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some(tilo_actions::remote::asked::DECLINED.to_owned()),
                },
            ),
        );
        drop(log);
        server.moved();
        Ok(())
    }
}

/// The workspace's own actions as the file beside this crate holds them, written from what the
/// tree publishes and what each action takes.
#[cfg(test)]
mod surface_tests {
    use serde_json::{Value, json};
    use tilo_action_policy::roles;
    use tilo_actions::ParamType;
    use tilo_actions::remote::{
        WORKSPACE_AGENTS, WORKSPACE_INVITATIONS, WORKSPACE_MEMBER, WORKSPACE_MEMBER_LIST_PUBLISHED,
        WORKSPACE_MEMBERS, WORKSPACE_WAITING,
    };
    use tilo_actions::remote::{
        WORKSPACE_APPS, WORKSPACE_CONTENT, WORKSPACE_GRANTS, WORKSPACE_PERMISSION,
        WORKSPACE_REGISTRIES, WORKSPACE_SERVERS, WORKSPACE_TRUST,
    };

    use super::{VERBS, Verb};

    // The header the file carries: written by this test, so a change to it reads in review as
    // an API change.

    /// The verbs a member change names, declared on the members root it changes beside the
    /// actions that make it: a change names the governance operation it is (`remove-member`,
    /// `set-role`), where the action API publishes the same change under the name its resource
    /// offers (`remove`, `role`). A store judges a change by the verb it names, so the
    /// operations are declared by those words too: joining by an invitation and handing the
    /// workspace over among them.
    const CHANGE_VERBS: [&str; 5] = [
        roles::governance::ADD_MEMBER,
        roles::governance::JOIN,
        roles::governance::SET_ROLE,
        roles::governance::REMOVE_MEMBER,
        roles::governance::TRANSFER_OWNERSHIP,
    ];

    /// The resources the workspace's actions are published on, in file order: each child of its
    /// governance tree, named after what it holds. Each is a resource type of its own, so a verb
    /// several offer (`list` on the members, the invitations and the servers) is one action of
    /// each, named by its type.
    const RESOURCES: [(&str, &str); 13] = [
        ("members", "Members"),
        ("member", "Member"),
        ("invitations", "Invitations"),
        ("waiting", "Wait list"),
        ("servers", "Servers"),
        ("content", "Content"),
        ("access", "Access"),
        ("trust", "Trust"),
        ("registries", "Registries"),
        ("apps", "Apps"),
        ("permission", "Permission"),
        ("grants", "Grants"),
        // A folder of the workspace's tree, read as what Files' `list` answers for it: what a
        // view of Files follows as `files/folder~<path>` (the language's `workspace.Folder`).
        ("folder", "Folder"),
    ];

    /// The resource the action published at `child` stands on: the child of the governance tree
    /// itself. A child this surface names no resource of fails the test, so an action is never
    /// left out of the file.
    fn resource_of(child: Option<&str>) -> &'static str {
        match child {
            Some(WORKSPACE_MEMBERS) => "members",
            Some(WORKSPACE_MEMBER) => "member",
            Some(WORKSPACE_INVITATIONS) => "invitations",
            Some(WORKSPACE_WAITING) => "waiting",
            Some(WORKSPACE_SERVERS) => "servers",
            Some(WORKSPACE_CONTENT) => "content",
            Some(WORKSPACE_TRUST) => "trust",
            Some(WORKSPACE_REGISTRIES) => "registries",
            Some(WORKSPACE_APPS) => "apps",
            Some(WORKSPACE_PERMISSION) => "permission",
            Some(WORKSPACE_GRANTS) => "grants",
            other => panic!("the surface names no resource for the actions on {other:?}"),
        }
    }

    /// The resource of `kind`, labelled `label`, publishing `verbs`: the fields and types a
    /// workspace's members, its content and its access reading answer with.
    fn described_resource(kind: &str, label: &str, verbs: &[Value]) -> Value {
        let mut resource = json!({
            "kind": format!("workspace.{kind}"),
            "label": label,
            "version": 1,
            "verbs": verbs,
        });
        // The members read as everyone in the workspace now: what every host is handed
        // from the member list it decides by (the language runtime's `Host::follow_members`).
        if kind == "members" {
            resource["fields"] = json!([
                { "path": "people", "type": { "list": "actor" }, "readonly": true },
            ]);
        }
        // The content reads as its owners' limits and the content a limit refused: what
        // every host is handed as `workspace.content` (the language runtime's
        // `Host::give_content`), each refusal only to whoever reads it.
        // The workspace's grants read as what its apps, people and agents were given
        // beyond their own documents: what the session publishes as `workspace/access`,
        // in the words the workspace's Permissions tab lists them.
        if kind == "access" {
            access_reading(&mut resource);
        }
        if kind == "content" {
            content_reading(&mut resource);
        }
        if kind == "folder" {
            folder_reading(&mut resource);
        }
        resource
    }

    /// A folder of the workspace's tree, as a view of Files reads it: the folder's path, what it
    /// holds a page at a time, and where the next page starts. It is the answer Files' `list`
    /// gives for the path, published as the reading `files/folder~<path>` so a view follows it
    /// as the workspace's tree changes and holds no copy of its own.
    fn folder_reading(resource: &mut Value) {
        resource["fields"] = json!([
            { "path": "folder", "type": "text", "readonly": true },
            {
                "path": "entries",
                "type": { "list": { "named": "FolderEntry" } },
                "readonly": true,
            },
            { "path": "from", "type": "int", "readonly": true },
            { "path": "next", "type": { "option": "int" }, "readonly": true },
            { "path": "total", "type": "int", "readonly": true },
            { "path": "error", "type": "text", "readonly": true },
        ]);
        resource["types"] = json!([
            {
                "name": "Folder",
                "doc": "A folder of the workspace's tree, as Files lists it.",
                "fields": [
                    { "name": "folder", "type": "text", "title": "The folder's path in the workspace; empty for the workspace's own folder." },
                    { "name": "entries", "type": { "list": { "named": "FolderEntry" } }, "title": "What the folder holds, this page of it." },
                    { "name": "from", "type": "int", "title": "Where this page starts among all the folder holds." },
                    { "name": "next", "type": { "option": "int" }, "title": "Where the next page starts, while there is more." },
                    { "name": "total", "type": "int", "title": "How many entries the folder holds in all." },
                    { "name": "error", "type": "text", "title": "Why the folder does not read, in words a person reads; empty when it does." },
                ],
            },
            {
                "name": "FolderEntry",
                "doc": "One file or folder of a folder of the workspace's tree.",
                "fields": [
                    { "name": "name", "type": "text", "title": "What it is called, as its folder shows it." },
                    { "name": "path", "type": "text", "title": "Its path in the workspace." },
                    { "name": "kind", "type": "text", "title": "Whether it is a `file` or a `folder`." },
                    { "name": "size", "type": { "option": "int" }, "title": "How many bytes its content is; none for a folder." },
                    { "name": "changed", "type": "time", "title": "When the workspace last changed it." },
                ],
            },
        ]);
    }

    /// The access reading's actions, fields and types, in the words the workspace's
    /// Permissions tab lists them.
    fn access_reading(resource: &mut Value) {
        // The two actions the access view offers: take a grant or a device-record choice
        // back, and turn one of an app's outside capabilities off or on. The session that
        // publishes this reading answers both where the person's own device keeps the
        // choice and the workspace keeps its grants.
        resource["verbs"] = json!([
            {
                "id": "revoke",
                "kind": "outside",
                "params": [
                    { "name": "id", "required": true, "schema": { "type": "string" } },
                ],
            },
            {
                "id": "set-outside",
                "kind": "outside",
                "params": [
                    { "name": "workspace", "required": true, "schema": { "type": "string" } },
                    { "name": "app", "required": true, "schema": { "type": "string" } },
                    { "name": "capability", "required": true, "schema": { "type": "string" } },
                    { "name": "allowed", "required": true, "schema": { "type": "boolean" } },
                ],
            },
        ]);
        resource["fields"] = json!([
            { "path": "known", "type": "bool", "readonly": true },
            {
                "path": "workspaces",
                "type": { "list": { "named": "WorkspaceAccess" } },
                "readonly": true,
            },
            {
                "path": "outside",
                "type": { "list": { "named": "OutsideAccess" } },
                "readonly": true,
            },
            {
                "path": "shares",
                "type": { "list": { "named": "SharedOut" } },
                "readonly": true,
            },
        ]);
        resource["types"] = access_types();
    }

    /// The types the access reading's grants and outside capabilities are listed in.
    fn access_types() -> Value {
        json!([
            {
                "name": "WorkspaceAccess",
                "doc": "The grants one workspace gives.",
                "fields": [
                    { "name": "id", "type": "text", "title": "The workspace's id." },
                    { "name": "name", "type": "text", "title": "What the workspace is called." },
                    {
                        "name": "grants",
                        "type": { "list": { "named": "AccessEntry" } },
                        "title": "What it gives, in the order its Permissions tab lists them.",
                    },
                ],
            },
            {
                "name": "SharedOut",
                "doc": "One part of a workspace shared with someone besides its owner.",
                "fields": [
                    { "name": "workspace", "type": "text", "title": "The workspace it is shared out of." },
                    { "name": "id", "type": "text", "title": "What revoking it names." },
                    { "name": "node", "type": "text", "title": "The resource's address, empty for a folder outside the tree." },
                    { "name": "app", "type": "text", "title": "The app the shared part belongs to, empty for a folder." },
                    { "name": "path", "type": "text", "title": "The host path a folder capability reaches, empty for a resource." },
                    { "name": "what", "type": "text", "title": "What is shared, as a person reads it." },
                    { "name": "holder", "type": "text", "title": "Whom it is shared with, by the name the workspace knows them by." },
                    { "name": "holder_id", "type": "text", "title": "The holder's id, empty for anyone with the link." },
                    { "name": "holder_kind", "type": "text", "title": "What the holder is: a member, a person outside, an agent, a bot or a link." },
                    { "name": "access", "type": "text", "title": "What it lets them do: read or write." },
                    { "name": "granted_by", "type": "text", "title": "Who gave it, by name." },
                    { "name": "granted_by_you", "type": "bool", "title": "Whether the viewer gave it." },
                    { "name": "granted_at", "type": "int", "title": "When it was given, in Unix milliseconds; 0 where the log does not know." },
                    { "name": "can_revoke", "type": "bool", "title": "Whether the viewer may take it back." },
                ],
            },
            {
                "name": "AccessEntry",
                "doc": "One grant a workspace gives.",
                "fields": [
                    { "name": "id", "type": "text", "title": "What revoking it names." },
                    { "name": "section", "type": "text", "title": "Who holds it: people, agents or apps." },
                    { "name": "holder", "type": "text", "title": "Whom it is given to." },
                    { "name": "allows", "type": "text", "title": "What it allows." },
                    { "name": "expiry", "type": "text", "title": "How long it lasts." },
                    { "name": "granted_by", "type": "text", "title": "Who gave it, where the workspace knows." },
                    { "name": "app", "type": "text", "title": "The app it is for, empty for a person." },
                    { "name": "path", "type": "text", "title": "The file or folder it reaches, where it is on this device." },
                    { "name": "workspace", "type": "text", "title": "The workspace it is in." },
                    { "name": "holder_kind", "type": "text", "title": "What holds it: a person, an agent or an app." },
                    { "name": "granted_by_name", "type": "text", "title": "Who gave it, by name." },
                    { "name": "granted_by_kind", "type": "text", "title": "What the giver is: you, a person, an agent or an app." },
                    { "name": "granted_by_you", "type": "bool", "title": "Whether the viewer gave it." },
                    { "name": "device", "type": "bool", "title": "Whether this device's own record keeps it." },
                    { "name": "can_revoke", "type": "bool", "title": "Whether the viewer may take it back." },
                ],
            },
            {
                "name": "OutsideAccess",
                "doc": "The outside capabilities the apps of one workspace declare.",
                "fields": [
                    { "name": "workspace", "type": "text", "title": "The workspace's id." },
                    { "name": "workspace_name", "type": "text", "title": "What the workspace is called." },
                    {
                        "name": "apps",
                        "type": { "list": { "named": "OutsideApp" } },
                        "title": "The apps that declare outside capabilities, by id.",
                    },
                ],
            },
            {
                "name": "OutsideApp",
                "doc": "One app that declares outside capabilities, and what it may reach.",
                "fields": [
                    { "name": "app", "type": "text", "title": "The app, by its package id." },
                    { "name": "name", "type": "text", "title": "What the app is called." },
                    {
                        "name": "capabilities",
                        "type": { "list": { "named": "OutsideCapability" } },
                        "title": "Each capability its code can queue, in name order.",
                    },
                ],
            },
            {
                "name": "OutsideCapability",
                "doc": "One capability an app's code can queue, and whether it goes.",
                "fields": [
                    { "name": "capability", "type": "text", "title": "The capability, as the app's package names it." },
                    { "name": "allowed", "type": "bool", "title": "Whether the app may carry it out now." },
                    { "name": "can_change", "type": "bool", "title": "Whether the viewer may turn it." },
                ],
            },
        ])
    }

    /// The content reading's fields and the refusal it lists.
    fn content_reading(resource: &mut Value) {
        resource["fields"] = json!([
            { "path": "file_bytes", "type": "int", "readonly": true },
            { "path": "member_bytes", "type": "int", "readonly": true },
            { "path": "hourly_bytes", "type": "int", "readonly": true },
            {
                "path": "refusals",
                "type": { "list": { "named": "Refusal" } },
                "readonly": true,
            },
        ]);
        resource["types"] = json!([
            {
                "name": "Refusal",
                "doc": "Content a host stopped keeping because a limit said so.",
                "fields": [
                    { "name": "record", "type": "text", "title": "The record whose content it is." },
                    { "name": "hash", "type": "text", "title": "The content's address, BLAKE3 hex." },
                    { "name": "bytes", "type": "int", "title": "How many bytes it is." },
                    { "name": "limit", "type": "text", "title": "The limit it went over: file, member, hourly, workspace or host." },
                    { "name": "allowed", "type": "int", "title": "What that limit allowed, in bytes." },
                    { "name": "member", "type": "text", "title": "Who posted it, by person id." },
                    { "name": "at", "type": "int", "title": "When it was refused, Unix milliseconds." },
                    { "name": "raise", "type": "text", "title": "Who can raise the limit: owners or host." },
                ],
            },
        ]);
    }

    /// The surface file's text: every action a workspace publishes, on the resource it holds,
    /// with the parameters and kind its action declares, and the record of its agents, which
    /// the apps of the workspace read from the same package.
    fn surface() -> String {
        let mut resources: Vec<Value> = RESOURCES
            .iter()
            .map(|(kind, label)| {
                let mut verbs: Vec<Value> = WORKSPACE_MEMBER_LIST_PUBLISHED
                    .iter()
                    // The workspace's agents are the record's own actions, published on
                    // `workspace/agents` by the record's grants: no verb of this surface.
                    .filter(|(_, child, _)| *child != Some(WORKSPACE_AGENTS))
                    .filter(|(_, child, _)| resource_of(*child) == *kind)
                    .map(|(operation, _, action)| {
                        let verb = VERBS
                            .iter()
                            .find(|verb| verb.id == *operation)
                            .unwrap_or_else(|| panic!("`{operation}` is no action of a workspace"));
                        json!({
                            "id": action,
                            "kind": verb.kind,
                            "params": params_of(verb),
                        })
                    })
                    .collect();
                // The verbs a member change names, declared on the members root it changes: the
                // store judges every change by the verb its author stamped it with.
                if *kind == "members" {
                    verbs.extend(CHANGE_VERBS.iter().map(|id| {
                        json!({
                            "id": id,
                            "kind": roles::GOVERNANCE_KIND,
                            "params": [],
                        })
                    }));
                }
                described_resource(kind, label, &verbs)
            })
            .collect();
        resources.push(
            serde_json::to_value(tilo_resource::surface::surface_of::<
                tilo_workspace_agent::WorkspaceAgents,
            >())
            .expect("a surface writes as JSON"),
        );
        let resources: Vec<Value> = resources
            .iter()
            .map(tilo_surface_compile::file::filed)
            .collect();
        tilo_surface_compile::canonical(&json!({
            "format": tilo_surface_compile::file::FORMAT,
            "resources": resources,
        }))
    }

    /// The parameters the action `verb` takes, each with the schema its type gives and the
    /// values it accepts.
    fn params_of(verb: &Verb) -> Vec<Value> {
        (verb.params)()
            .into_iter()
            .map(|param| {
                let mut schema = param
                    .schema
                    .as_deref()
                    .cloned()
                    .unwrap_or_else(|| schema_of(param.ty));
                if !param.choices.is_empty()
                    && let Some(object) = schema.as_object_mut()
                {
                    object.insert("enum".to_owned(), json!(param.choices));
                }
                let mut written = json!({
                    "name": param.name,
                    "required": param.required,
                    "schema": schema,
                });
                // A resource of any kind: what a program names it by, which reaches the
                // workspace as the id it publishes it under. A person or a member is an actor to
                // a program: a tilolang app passes the `Actor` it holds, which the action reads
                // as their key.
                if param.ty == ParamType::Resource {
                    written["type"] = json!({ "resource": "" });
                } else if param.ty == ParamType::Secret {
                    // Handed over once and kept nowhere: a record of a call keeps its name alone.
                    written["type"] = json!("secret");
                } else if param.name == "person" || param.name == "member" {
                    written["type"] = json!("actor");
                }
                written
            })
            .collect()
    }

    fn schema_of(ty: ParamType) -> Value {
        match ty {
            ParamType::Text | ParamType::Secret | ParamType::DateTime | ParamType::Resource => {
                json!({"type": "string"})
            }
            ParamType::Integer => json!({"type": "integer"}),
            ParamType::Number => json!({"type": "number"}),
            ParamType::Boolean => json!({"type": "boolean"}),
            ParamType::Array => json!({"type": "array"}),
            ParamType::Object => json!({"type": "object"}),
        }
    }

    #[test]
    fn the_surface_file_is_what_this_crate_publishes() {
        let text = surface();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("workspace-surface.json");
        let committed = std::fs::read_to_string(&path).ok();
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
            std::fs::write(&path, &text).expect("the surface file is written");
            return;
        }
        assert_eq!(
            committed.as_deref(),
            Some(text.as_str()),
            "crates/core/tilo-workspace-service/workspace-surface.json is not what a \
             workspace publishes; regenerate it with UPDATE_SNAPSHOTS=1"
        );
    }
}
