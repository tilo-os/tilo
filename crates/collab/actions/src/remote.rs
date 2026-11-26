//! Using another peer's actions: a transport-neutral vocabulary.
//!
//! A peer that serves live apps (a desktop session, a headless app server)
//! can let an authenticated remote caller list the actions it
//! publishes, invoke one, follow its progress, and cancel it. This module is
//! the whole contract between the two sides:
//!
//! - [`RemoteActionMessage`] — requests and their answers, wrapped in a
//!   versioned [`RemoteActionFrame`] that any transport carries as opaque
//!   bytes.
//! - [`ActionProvider`] — what a peer implements to answer them, and [`serve`],
//!   which maps one request frame onto a provider. Answers that need an
//!   application's help arrive later as a [`Pending`], so no caller blocks.
//! - [`CatalogueQuery`] — paged, filtered browsing of what a provider offers.
//! - [`InvocationLog`] — the idempotency rule every provider shares: an
//!   invocation is keyed by its caller-chosen [`InvocationId`], so a retry
//!   after a lost reply observes the original invocation instead of running
//!   the action twice.
//!
//! Who is calling is never read from a payload. The transport that
//! authenticated the connection constructs the [`Caller`] and hands it to the
//! provider beside the message; nothing in a frame can name or change it.
//!
//! Descriptors, addresses, arguments, outputs and lifecycle updates are the
//! same types the local action surface already uses ([`ActionDescriptor`](crate::api::ActionDescriptor),
//! [`ActionAddress`], [`InvocationUpdate`], [`ActionOutput`],
//! [`ActionFailure`]), so a remote caller sees exactly what a local one does.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

pub use window_actions::{
    APPLICATION_ACTIVATE_ACTION, APPLICATION_ACTIVATE_TYPE, APPLICATION_OPEN_LINK_ACTION,
    APPLICATION_OPEN_LINK_TYPE, APPLICATION_OPEN_TYPE, OPEN_LINK_PARAM, activate_spec,
    activation_requested, link_scheme, open_link_arguments,
};

pub mod agent_tools;
mod application;
pub mod browser;
mod caller;
pub mod environment_device;
pub mod linux_apps;
pub mod mcp_server;
pub use caller::{Caller, device_agent_id};
pub mod signed_invocation;
pub use signed_invocation::{SignedCall, SignedInvocation};
mod catalogue;
mod changes;
mod confirmation;
mod content_get;
mod content_link;
mod content_put;
pub mod crash_reports;
pub mod downloaded_apps;
pub mod environment_commands;
pub mod environment_machines;
pub mod environment_terminals;
pub mod feedback;
pub mod formats;
mod nodes;
pub mod own_device;
mod pending;
pub mod person_accounts;
pub mod person_asking;
pub mod person_connections;
pub mod person_models;
pub mod person_telling;
pub mod person_vault;
mod providers;
mod readings;
mod resource_read;
pub mod search;
pub mod server_account;
pub mod session_actions;
mod session_state;
mod surfaces;
pub use session_state::{SessionState, SyncState, WorkspaceState};
mod shared_provider;
pub mod task_programs;
#[cfg(any(test, feature = "test-support"))]
mod test_provider;
mod wait;
mod watch;
pub use watch::{CHANGED_UNAVAILABLE, Here, HereWindow, WatchScope};
mod window_actions;
mod within;
pub mod workspace_activity;
pub mod workspace_events;
pub mod workspace_initiative;
pub mod workspace_invocations;

pub use session_actions::PROVIDER as SESSION_ACTIONS_PROVIDER;

pub use application::{
    APPLICATION_PROVIDER, ApplicationLaunch, CONTENT_HASH_ARGUMENT, NAME_ARGUMENT,
    application_action_target, application_open_target, application_provider,
    application_provider_resource, application_resource_target, valid_file_name,
};
pub use confirmation::{
    ASK, ASK_PROVIDER, ASK_RESOURCE, ASK_WITH_CHOICES, Confirmation, ConfirmationQuestion,
    ConfirmationReply, ConfirmedInvocation, MAX_ASK_CHARS, MAX_ASK_CHOICES, MAX_CHOICE_CHARS,
    PersonAnswer, PersonAsk, argument_digest, ask_target, is_ask,
};
pub use providers::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_KIND, APPLICATION_RESOURCE_PREFIX,
    APPLICATION_START_ACTION, OWN_PROVIDER, RESOURCE_PROVIDER, SHARE_LINK_ACTION,
    WORKSPACE_PROVIDER, is_resource_provider, resource_parts,
};

pub use catalogue::{
    CanonicalActionRoute, CatalogueLimits, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, MAX_BY_RESOURCE_PAGE, MAX_CATALOGUE_PAGE, Offered,
    ResourceViewLocator, RowVisibility, ViewResourceQuery, compact, expand, rows_weight,
    truncate_resources, truncate_rows,
};
pub use changes::{CHANGE_FEED_UNAVAILABLE, ChangeFeed, ChangePage, ResourceChanged};
pub use content_get::{CONTENT_GETS_UNAVAILABLE, ContentGetAnswer, ContentGets, ContentPiece};
pub use content_link::{
    CONTENT_LINK_TTL_SECS, CONTENT_LINKS_UNAVAILABLE, ContentLink, ContentLinkAsk, ContentLinkFile,
    MAX_CONTENT_LINK_FILES, viewable,
};
pub use content_put::{
    CONTENT_PUTS_UNAVAILABLE, ContentPut, ContentPutAnswer, ContentPutStep, ContentPuts,
    HostedContentPuts, MAX_CONTENT_PUT_AHEAD, MAX_CONTENT_PUT_CHUNK, MAX_HAVE_HASHES,
    MAX_HOSTED_CONTENT_PUTS, MAX_OPEN_CONTENT_PUTS, MAX_PUT_MANY_BYTES, MAX_PUT_MANY_FILES,
};
pub use nodes::{
    NODE_PERMITS_ACTION, NODE_RESOURCE_PREFIX, SHARE_NODE_ACTION, UNSHARE_NODE_ACTION, node_target,
};
mod execution_lease;
mod line;
pub use execution_lease::ExecutionLease;
pub use line::{answer_line, answer_message, frame_line, line_refusal};
pub use pending::{Abandoned, Completer, Pending, PendingPoll, pending};
pub use readings::Readings;
pub use resource_read::{
    HeldCopies, RESOURCE_READ_REFUSED, RESOURCE_READS_FOR_AGENTS, RESOURCE_READS_UNAVAILABLE,
    ResourceAddress, ResourceAsk, ResourceChange, ResourceChanges, ResourceCopy, ResourceReadings,
    ResourceReads, ResourceWake, answer_read, read_resources, readings_of,
};
pub use search::{FOUND_FILE, Found, NOT_NARROWED, ResourceSearch, Within, stem_word};
#[cfg(any(test, feature = "test-support"))]
pub use test_provider::TestProvider;
pub use wait::{MAX_REMOTE_WAIT, UpdateMark, settle_with};

use crate::api::ActionAddress;
use crate::context::{ActionContext, ActionFacet};
use crate::outcome::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use crate::params::ARGUMENTS_JSON_PREFIX;

/// Version of the [`RemoteActionFrame`] schema this build speaks.
///
/// Additive changes keep the version; a change an older peer would misread
/// bumps it, and [`RemoteActionFrame::decode`] refuses a version it does not know.
pub const REMOTE_ACTIONS_VERSION: u16 = 3;

/// Upper bound on one encoded frame: a provider pages its catalogue rather than raise this.
pub const MAX_REMOTE_ACTION_FRAME_BYTES: usize = 1024 * 1024;

/// The most raw bytes one piece of content carries, into a workspace's store
/// ([`ContentPutStep::Append`]) or out of it ([`ContentGetAnswer`]): hex doubles
/// them, and a caller splits anything larger.
pub const MAX_CONTENT_FRAME_BYTES: usize = 384 * 1024;

/// Default number of invocations an [`InvocationLog`] remembers.
pub const DEFAULT_LOG_CAPACITY: usize = 1024;

/// Which published action an invocation names on the provider.
///
/// `provider` is an opaque token the provider issued in a [`CataloguePage`]
/// (which live app published the action); `address` is the app's own stable
/// resource/action pair. A caller copies both from a catalogue entry and
/// never constructs a provider itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteActionTarget {
    pub provider: String,
    pub address: ActionAddress,
}

/// The provider a workspace's own governance actions (membership, roles, grants,
/// bans) are answered under. It is the provider's, not an app's, so it is the
/// one provider a caller names without reading it from a catalogue page.
pub const WORKSPACE_GOVERNANCE_PROVIDER: &str =
    tilo_action_policy::agent_answers::WORKSPACE_GOVERNANCE_APP;
/// The native service that lists names of the person's other workspaces for their assistant.
pub const OTHER_WORKSPACES_PROVIDER: &str = "tilo.other-workspaces";
/// The resource kind of a workspace's governance resource.
pub const WORKSPACE_GOVERNANCE_KIND: &str = "workspace.permissions";

impl RemoteActionTarget {
    /// The governance action granted as `verb` (one of
    /// `tilo_action_policy::roles::governance`, `server.add` and the like) on
    /// the workspace `workspace`, at the address its tree publishes it
    /// ([`workspace_governance_address`]).
    #[must_use]
    pub fn workspace_governance(workspace: &str, verb: &str) -> Self {
        Self {
            provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
            address: workspace_governance_address(workspace, verb),
        }
    }
}

/// The root of a workspace's governance tree: the workspace itself, the
/// resource an agent's path `workspace` names.
#[must_use]
pub fn workspace_governance_resource(workspace: &str) -> String {
    format!("{WORKSPACE_GOVERNANCE_KIND}:{workspace}")
}

/// The servers keeping a workspace, under its governance root
/// (`workspace/servers`).
pub const WORKSPACE_SERVERS: &str = "servers";
/// The apps and bots a workspace uses, under its governance root
/// (`workspace/apps`).
pub const WORKSPACE_APPS: &str = "apps";
/// The mail accounts a workspace keeps on its server, under its governance
/// root (`workspace/mail-accounts`): `tilo.accounts`'s, reached by path.
pub const WORKSPACE_MAIL_ACCOUNTS: &str = "mail-accounts";
/// The people in a workspace, under its governance root (`workspace/members`).
pub const WORKSPACE_MEMBERS: &str = "members";
/// One person among a workspace's people (`workspace/members/<person>`, their user public key
/// in hex): what changes their role and removes them is published on them.
pub const WORKSPACE_MEMBER: &str = "members/*";
/// The question a device of a person's asks the workspace's host before it signs a change to
/// who is in the workspace: `{change, ...the change's arguments}`, where `change` is the action
/// the tree publishes the change at ([`WORKSPACE_MEMBER_ADDED`],
/// [`WORKSPACE_MEMBER_PLACED`] or [`WORKSPACE_MEMBER_REMOVED`]). Answered `{}` when the change
/// is the device's to make, and refused when it is not.
pub const WORKSPACE_MEMBER_ADMIT: &str = "member.admit";
/// The action `workspace/members#add` publishes: give a person a place in the workspace,
/// whether or not they held one, as [`WORKSPACE_MEMBER_PLACED`] would a member's.
pub const WORKSPACE_MEMBER_ADDED: &str = "add";
/// The action `workspace/members/<person>#role` publishes: change the place of a person the
/// workspace holds. A person it does not hold is added by [`WORKSPACE_MEMBER_ADDED`], never
/// given a place here.
pub const WORKSPACE_MEMBER_PLACED: &str = "role";
/// The action `workspace/members/<person>#remove` publishes: remove the person, as the
/// workspace's grants on removing let whoever asks, or themselves.
pub const WORKSPACE_MEMBER_REMOVED: &str = "remove";
/// The invitations a workspace has open, under its governance root
/// (`workspace/invitations`).
pub const WORKSPACE_INVITATIONS: &str = "invitations";
/// The addresses left at a workspace's entry while it takes people by
/// invitation, under its governance root (`workspace/waiting`).
pub const WORKSPACE_WAITING: &str = "waiting";

/// Where a workspace's governance tree publishes the actions it grants by
/// another name: the operation a grant and a policy name it by, the resource
/// under the root it is on (none for the root itself) and its id there, as a
/// person names it. An operation not listed is published on the root under its
/// own name; one listed on [`WORKSPACE_MEMBER`] is published on each person, who
/// is its `person`.
pub const WORKSPACE_GOVERNANCE_PUBLISHED: [(&str, Option<&str>, &str); 22] = [
    ("member.list", Some(WORKSPACE_MEMBERS), "list"),
    (
        "member.add",
        Some(WORKSPACE_MEMBERS),
        WORKSPACE_MEMBER_ADDED,
    ),
    (
        "member.add",
        Some(WORKSPACE_MEMBER),
        WORKSPACE_MEMBER_PLACED,
    ),
    (
        "remove-member",
        Some(WORKSPACE_MEMBER),
        WORKSPACE_MEMBER_REMOVED,
    ),
    ("member.name", Some(WORKSPACE_MEMBERS), "name"),
    ("invite", Some(WORKSPACE_INVITATIONS), "invite"),
    ("invitation.list", Some(WORKSPACE_INVITATIONS), "list"),
    ("invitation.cancel", Some(WORKSPACE_INVITATIONS), "cancel"),
    ("wait-list.list", Some(WORKSPACE_WAITING), "list"),
    ("wait-list.remove", Some(WORKSPACE_WAITING), "unlist"),
    ("wait-list.mail", Some(WORKSPACE_WAITING), "mail"),
    ("server.list", Some(WORKSPACE_SERVERS), "list"),
    ("server.add", Some(WORKSPACE_SERVERS), "add"),
    ("server.remove", Some(WORKSPACE_SERVERS), "remove"),
    ("server.move", Some(WORKSPACE_SERVERS), "move"),
    ("copies.set", Some(WORKSPACE_SERVERS), "copies"),
    ("content.held", Some(WORKSPACE_SERVERS), "content"),
    ("install-app", Some(WORKSPACE_APPS), "install-app"),
    ("uninstall-app", Some(WORKSPACE_APPS), "uninstall-app"),
    ("upload-app-file", Some(WORKSPACE_APPS), "upload-app-file"),
    ("request-bot", Some(WORKSPACE_APPS), "request-bot"),
    (
        "withdraw-bot-request",
        Some(WORKSPACE_APPS),
        "withdraw-bot-request",
    ),
];

/// The resource `child` of `workspace`'s governance tree: the root for `None`.
#[must_use]
pub fn workspace_governance_child(workspace: &str, child: Option<&str>) -> String {
    let root = workspace_governance_resource(workspace);
    match child {
        Some(child) => format!("{root}/{child}"),
        None => root,
    }
}

/// The resource of the person `person` (their user public key in hex) among `workspace`'s people.
#[must_use]
pub fn workspace_member(workspace: &str, person: &str) -> String {
    format!(
        "{}/{person}",
        workspace_governance_child(workspace, Some(WORKSPACE_MEMBERS))
    )
}

/// The address `workspace`'s governance tree publishes the operation
/// `operation` at, where it is not one person's.
#[must_use]
pub fn workspace_governance_address(workspace: &str, operation: &str) -> ActionAddress {
    match WORKSPACE_GOVERNANCE_PUBLISHED
        .iter()
        .find(|(granted, child, _)| *granted == operation && *child != Some(WORKSPACE_MEMBER))
    {
        Some((_, child, action)) => {
            ActionAddress::new(workspace_governance_child(workspace, *child), *action)
        }
        None => ActionAddress::new(workspace_governance_resource(workspace), operation),
    }
}

/// The workspace and the operation `address` asks of that workspace's
/// governance, when its tree publishes one there: what
/// [`workspace_governance_address`] maps back. An operation published
/// elsewhere, asked by its grant's name, is none.
#[must_use]
pub fn workspace_governance_operation(address: &ActionAddress) -> Option<(&str, &str)> {
    let named = address
        .resource
        .strip_prefix(workspace_governance_resource("").as_str())?;
    let (workspace, child) = match named.split_once('/') {
        Some((workspace, child)) => (workspace, Some(child)),
        None => (named, None),
    };
    if workspace.is_empty() {
        return None;
    }
    let action = address.action.as_str();
    let child = if workspace_person(address).is_some() {
        Some(WORKSPACE_MEMBER)
    } else {
        child
    };
    let published = WORKSPACE_GOVERNANCE_PUBLISHED
        .iter()
        .find(|(_, at, published)| *at == child && *published == action);
    match published {
        Some((granted, _, _)) => Some((workspace, granted)),
        None if child.is_none()
            && !WORKSPACE_GOVERNANCE_PUBLISHED
                .iter()
                .any(|(granted, _, _)| *granted == action) =>
        {
            Some((workspace, action))
        }
        None => None,
    }
}

/// The person (their user public key in hex) `address` names among its workspace's people
/// (`workspace/members/<person>`), when it names one.
#[must_use]
pub fn workspace_person(address: &ActionAddress) -> Option<&str> {
    let named = address
        .resource
        .strip_prefix(workspace_governance_resource("").as_str())?;
    let (members, person) = named.split_once('/')?.1.split_once('/')?;
    (members == WORKSPACE_MEMBERS
        && person.len() == 64
        && person.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then_some(person)
}

/// Adding a computer to the person's devices with a one-use ticket: the computer's own daemon
/// issues it, prints the one string, and the new computer logs in with it.
pub mod device_tickets {
    use super::{ActionAddress, RemoteActionTarget};

    /// The provider the device's daemon answers tickets under.
    pub const PROVIDER: &str = "tilo.device";
    /// The resource the actions act on: the person's devices.
    pub const RESOURCE: &str = "devices";
    /// Serve a ticket for a new computer.
    pub const ISSUE: &str = "issue";

    #[must_use]
    pub fn target(action: &str) -> RemoteActionTarget {
        RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: ActionAddress::new(RESOURCE, action),
        }
    }
}

/// Machines a person prepared to install: the plan and its waiting entry in
/// their Servers workspace, read and written through the session that holds
/// it. The command and the installer's window name one set of steps.
pub mod machines;

/// The person's settings, kept by the provider whether or not the app that shows
/// them is open: each section a record published with its field edits and
/// verbs (`settings:<kind>`), changed through them as that app's window
/// changes them. It is answered under the settings provider.
pub mod settings_resources {
    /// The provider that answers the person's settings.
    pub const PROVIDER: &str = "tilo.settings";
}

/// Administering the person's identities and the devices trusted under
/// them. The provider keeps the identity store — records, root secrets, the
/// active selector, device fleets — and runs every step, so no application
/// reads a secret or signs with the root key. It is answered under the
/// identity provider, which a caller names without reading a catalogue, for the
/// person at this device in the workspace their connection was admitted to.
pub mod identity_admin {
    use serde::{Deserialize, Serialize};

    use super::{ActionAddress, RemoteActionTarget};

    /// The provider that answers identity administration.
    pub const PROVIDER: &str = "tilo.identity";
    /// The resource the steps are actions of.
    pub const RESOURCE: &str = "identity";
    /// The person's identities on this device, without secrets.
    pub const LIST: &str = "list";
    /// Sign new apps in as identity `id`.
    pub const ACTIVATE: &str = "activate";
    /// Remove identity `id` from this device (`confirm`).
    pub const REMOVE: &str = "remove";
    /// Move identity `id`'s key from a plain file into protected storage.
    pub const PROTECT: &str = "protect";
    /// The devices trusted under the workspace's identity, as [`FleetDevice`]s.
    pub const DEVICES: &str = "devices";
    /// Root-sign a device's permanent revocation (`device`, `reason`,
    /// `confirm`), answering the signed entry (`revocation`).
    pub const REVOKE: &str = "revoke";
    /// Name one of the person's devices (`device`, `name`): what their device
    /// lists read it by from now on. A page's API forwards it to the provider
    /// that keeps their record; it is no step of the device's own store.
    pub const RENAME: &str = "rename-device";

    /// Every step, in the order a catalogue lists them.
    pub const STEPS: [&str; 6] = [LIST, ACTIVATE, REMOVE, PROTECT, DEVICES, REVOKE];

    #[must_use]
    pub fn target(step: &str) -> RemoteActionTarget {
        RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: ActionAddress::new(RESOURCE, step),
        }
    }

    /// One device trusted under an identity, as [`DEVICES`] answers it.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct FleetDevice {
        /// The name the device was enrolled with.
        pub label: String,
        /// The device's public key, 64 lowercase hex characters.
        pub key_hex: String,
        /// The key's short form people compare between screens.
        pub fingerprint: String,
        pub current: bool,
        /// Whether its trust was revoked.
        pub revoked: bool,
        /// What the key is: a device of the person's, or a key acting for
        /// them.
        #[serde(default)]
        pub kind: KeyKind,
        /// The browser profile it is a tab of, by the profile's id: a
        /// browser keeps one key per open tab, and the person sees and takes
        /// back all of one browser's keys as one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub browser: Option<String>,
        /// The agent this key runs, as the sessions it opened on a host named
        /// it ("Claude Code"): a computer set up with `tilo login` is a
        /// device of the person's that an agent works on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String>,
        /// When its certificate was issued, ms since the epoch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub issued_ms: Option<i64>,
        /// When it expires, ms since the epoch; `None` for one that does not.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub expires_ms: Option<i64>,
        /// When a session of this key with one of the person's hosts last
        /// opened or ended, ms since the epoch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub last_active_ms: Option<i64>,
        /// A session of this key is open with one of their hosts now.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        pub online: bool,
        /// The one workspace an agent's grant reaches, by its name where the
        /// person's record names it, else its id. `None` every workspace of
        /// theirs: a device of theirs, and an agent with the whole account.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace: Option<String>,
    }
    /// What a key the person's root certified is.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub enum KeyKind {
        /// One of their devices.
        #[default]
        Device,
        /// A key acting for them: an agent's grant, a script's key.
        Agent,
    }

    impl KeyKind {
        /// What the key `capabilities` certify is.
        #[must_use]
        pub fn of(agent: bool, server_key: bool) -> Self {
            if agent || server_key {
                Self::Agent
            } else {
                Self::Device
            }
        }
    }
}

/// Why a provider refuses a directory action that asks for confirmation when
/// the request names no revision: the confirmation covers the action as the
/// person was shown it, which only that revision identifies. A caller that
/// confirms on its own policy rather than a person's reads the current
/// revision and retries.
pub const CONFIRMED_ACTION_NEEDS_REVISION: &str =
    "a confirmed action needs the revision it was shown at";

/// What an invocation carries for the action's declared input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvokeArgument {
    /// The action takes nothing.
    #[default]
    None,
    /// The single text input a prompted action collects.
    Text(String),
    /// The typed argument object checked against the action's parameters.
    Json(serde_json::Map<String, serde_json::Value>),
}

impl InvokeArgument {
    /// The single-slot form the local invocation path accepts: text as is,
    /// a typed object behind [`ARGUMENTS_JSON_PREFIX`].
    #[must_use]
    pub fn to_slot(&self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Text(text) => Some(text.clone()),
            Self::Json(object) => Some(format!(
                "{ARGUMENTS_JSON_PREFIX}{}",
                serde_json::Value::Object(object.clone())
            )),
        }
    }

    /// What [`Self::to_slot`] carries, as the wire field it came from. A
    /// slot no typed object is written behind is text, which is what an
    /// action taking a prompt collects.
    #[must_use]
    pub fn from_slot(slot: Option<&str>) -> Self {
        match slot {
            None => Self::None,
            Some(text) => match text
                .strip_prefix(ARGUMENTS_JSON_PREFIX)
                .and_then(|json| serde_json::from_str(json).ok())
            {
                Some(object) => Self::Json(object),
                None => Self::Text(text.to_owned()),
            },
        }
    }
}

/// A request to run one published action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InvokeRequest {
    /// Chosen by the caller and reused verbatim on retry; the provider runs the
    /// action at most once per id.
    pub invocation: InvocationId,
    pub target: RemoteActionTarget,
    #[serde(default)]
    pub argument: InvokeArgument,
    /// Human provenance carried into history. It never grants authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ActionContext>,
    /// The caller's explicit confirmation, for an action whose contract
    /// requires one. The provider checks it against the contract; it is
    /// evidence, not authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<Confirmation>,
    /// The catalogue revision the caller chose the target from, for a
    /// provider whose actions are resolved live at that revision. A provider
    /// refuses a target that no longer exists at it rather than guessing.
    ///
    /// Omitted, a directory target resolves against the directory as it is
    /// now, and the admission is bound to the revision the provider
    /// resolved it at: an executing provider still refuses the action once the
    /// directory moved on before it ran. An action whose contract asks for
    /// confirmation needs the revision it was shown at; without one the
    /// provider refuses with [`CONFIRMED_ACTION_NEEDS_REVISION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The live invocation this request continues: an edit an effect
    /// submits while running for someone else. The provider admits it as that
    /// invocation's original caller, with their grants, never as the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<InvocationId>,
    /// Hold the receipt until the invocation settles or this many
    /// milliseconds pass (at most [`MAX_REMOTE_WAIT`]), so a quick action
    /// answers in one round trip. How the answer travels, not what runs:
    /// [`serve`] takes it off the request before the provider sees it, and
    /// [`InvocationLog`] ignores it. A provider that predates it answers at
    /// admission, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u32>,
    /// The name and avatar a bot asks to show for this action. Only an API
    /// that can check the caller is a bot allowed it admits one
    /// (the workspace-action API a bot drives); every other API refuses the request
    /// ([`crate::appearance::refuse_here`]) rather than dropping the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<crate::Appearance>,
    /// The caller's own signature over who asks for which action on which
    /// resource with which arguments ([`SignedInvocation`]). A host that runs
    /// an invocation for a caller on another device requires it and acts as
    /// its verified signer; a request that never leaves its device (an app
    /// asking its own session) carries none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<Box<SignedInvocation>>,
}

impl InvokeRequest {
    /// What the agent's harness calls the session this request was sent in ("Release prep"), as
    /// the client's agent-session group says it: display only, the name people see the agent by
    /// ([`Caller::in_named_session`]). `None` when the client named no session.
    #[must_use]
    pub fn said_session_name(&self) -> Option<String> {
        let group = self
            .context
            .as_ref()?
            .group
            .as_ref()
            .filter(|group| group.kind == crate::context::AGENT_SESSION_GROUP)?;
        crate::context::one_line(group.session.as_deref()?, crate::context::MAX_LABEL_CHARS)
    }

    /// Whether `other` asks for the same invocation: every field but the
    /// transport-only [`wait_ms`](Self::wait_ms), so a retry that waits
    /// differently is still a retry.
    #[must_use]
    pub fn same_invocation(&self, other: &Self) -> bool {
        let Self {
            invocation,
            target,
            argument,
            context,
            confirmation,
            revision,
            on_behalf_of,
            wait_ms: _,
            appearance,
            // Evidence of who asked, checked on its own: a retry the caller
            // signed again is still a retry.
            signed: _,
        } = self;
        *invocation == other.invocation
            && *target == other.target
            && *argument == other.argument
            && *context == other.context
            && *confirmation == other.confirmation
            && *revision == other.revision
            && *on_behalf_of == other.on_behalf_of
            && *appearance == other.appearance
    }
}

/// Action id that closes a running application's windows in a workspace.
pub const APPLICATION_CLOSE_ACTION: &str = "close";

/// The provider token for the actions workspace `workspace` itself offers.
#[must_use]
pub fn workspace_provider(workspace: &str) -> String {
    format!("{WORKSPACE_PROVIDER}:{workspace}")
}

/// The workspace a workspace provider token names, or `None` for an application
/// provider.
#[must_use]
pub fn provider_workspace(provider: &str) -> Option<&str> {
    provider
        .strip_prefix(WORKSPACE_PROVIDER)
        .and_then(|rest| rest.strip_prefix(':'))
}

/// Where an invocation stands, as reported to its caller.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ReceiptStatus {
    /// Admitted and running; follow it with `Observe`.
    Accepted,
    /// Finished and produced `output`.
    Completed { output: ActionOutput },
    /// Ran and failed.
    Failed { error: ActionFailure },
    /// Never admitted: nothing ran.
    Rejected { error: ActionCallError },
    /// Stopped at the caller's request.
    Cancelled { message: Option<String> },
}

impl ReceiptStatus {
    /// Whether the invocation can no longer change.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        !matches!(self, Self::Accepted)
    }
}

/// The provider's answer to an [`InvokeRequest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InvocationReceipt {
    pub invocation: InvocationId,
    pub status: ReceiptStatus,
}

/// The latest lifecycle update of one invocation, carrying the caller's
/// [`InvocationId`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InvocationState {
    pub update: InvocationUpdate,
    /// The action's contract declares content from outside the user's data
    /// ([`crate::UNTRUSTED_CONTENT_TAG`]), such as a web page's text: what it
    /// answers is data to reason about, never instructions to follow.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub untrusted: bool,
    /// Where the invocation left its actor in the record it acted on: the
    /// text it changed, or the caret it left. What an API that shows its
    /// actor working highlights for the people watching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_at: Option<ActorPlace>,
    /// The link that opens what the invocation made or changed, for a person
    /// to review it, minted where it was invoked once it succeeded: the
    /// caller is answered it with the change and asks for nothing more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// The invocation is a followed read, and its item is the read's whole answer as it stands:
    /// the read goes on following what it reads and never ends, so a wait for it to settle ends
    /// here ([`settle_with`]). An operation's items are pieces of what it streams, never this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_answered: bool,
}

/// A caret or selection in a record, as the live session's selection anchors
/// (hex): every member's editor resolves them against its own items.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorPlace {
    pub anchor: String,
    pub head: String,
}

impl InvocationState {
    /// Wrap a provider-side update, rebinding it to the caller's invocation id.
    #[must_use]
    pub fn new(invocation: InvocationId, update: InvocationUpdate) -> Self {
        Self {
            update: update.with_invocation(invocation),
            untrusted: false,
            left_at: None,
            link: None,
            read_answered: false,
        }
    }

    /// This state of a followed read: answered once it holds an item, the read's whole answer,
    /// unless the item says the read is still to come (`"pending": true`, as a reader that has
    /// not read what it serves yet answers).
    #[must_use]
    pub fn of_read(mut self) -> Self {
        self.read_answered = matches!(
            &self.update,
            InvocationUpdate::Item { value, .. }
                if value.get("pending") != Some(&serde_json::Value::Bool(true))
        );
        self
    }

    /// This state, saying where the invocation left its actor.
    #[must_use]
    pub fn left_at(mut self, place: Option<ActorPlace>) -> Self {
        self.left_at = place;
        self
    }

    /// This state, naming the link that opens what it made or changed.
    #[must_use]
    pub fn linked(mut self, link: Option<String>) -> Self {
        self.link = link;
        self
    }

    /// This state, saying whether what it answers is untrusted content.
    #[must_use]
    pub fn with_untrusted(mut self, untrusted: bool) -> Self {
        self.untrusted = untrusted;
        self
    }

    #[must_use]
    pub fn invocation(&self) -> InvocationId {
        self.update.invocation()
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.update.is_terminal()
    }

    /// The receipt this state amounts to: accepted while running, the
    /// outcome once terminal.
    #[must_use]
    pub fn receipt(&self) -> InvocationReceipt {
        let status = match &self.update {
            InvocationUpdate::Accepted { .. }
            | InvocationUpdate::Progress { .. }
            | InvocationUpdate::Item { .. } => ReceiptStatus::Accepted,
            InvocationUpdate::Succeeded { output, .. } => ReceiptStatus::Completed {
                output: output.clone(),
            },
            // An action that reached something only the person may allow
            // stopped before it ran: their answer is what it waits on, which
            // is a refusal to the caller, not a failure of the action.
            InvocationUpdate::Failed { error, .. } if error.code == CONFIRMATION_REQUIRED_CODE => {
                ReceiptStatus::Rejected {
                    error: ActionCallError::ConfirmationRequired {
                        departure: browser::departure_stated(error.details.as_ref()),
                    },
                }
            }
            InvocationUpdate::Failed { error, .. } => ReceiptStatus::Failed {
                error: error.clone(),
            },
            InvocationUpdate::Cancelled { message, .. } => ReceiptStatus::Cancelled {
                message: message.clone(),
            },
        };
        InvocationReceipt {
            invocation: self.invocation(),
            status,
        }
    }
}

/// The failure code an action answers while it runs when what it reached
/// waits for the person's word ([`ApiError::ConfirmationRequired`]): the
/// caller's invocation is refused for want of their answer, and their
/// answer runs it again.
///
/// [`ApiError::ConfirmationRequired`]: crate::api::ApiError::ConfirmationRequired
pub const CONFIRMATION_REQUIRED_CODE: &str = "action_confirmation_required";

/// The phase of an invocation's [`InvocationUpdate::Progress`] while it waits for its person to
/// approve it on a device that holds their identity: it goes on once they allow it.
pub const AWAITING_APPROVAL: &str = "awaiting_approval";

/// The code a server refuses a person it does not know yet with: its operator has not declared
/// them an owner, and no invitation of a workspace there brought them in.
pub const NOT_ON_THIS_SERVER: &str = "not_on_this_server";

/// The code a workspace refuses a key its person revoked with: nothing it asks is answered
/// until they approve another.
pub const KEY_REVOKED: &str = "key_revoked";

/// What a key that no longer acts (revoked, expired, or no longer granted in its workspace) is
/// told to do: the one command that approves a key again.
pub const APPROVE_AGAIN: &str = "approve a new key: the person types `tilo keys create \"<name>\"` \
     and approves it in their Tilo tab (on their own computer, `tilo login` again)";

/// The refusal of a server that does not know `person` (their root key) yet, saying what to do:
/// its operator declares them an owner, or they join with an invitation link.
#[must_use]
pub fn not_on_this_server(person: &[u8; 32]) -> ActionCallError {
    let key = hex::encode(person);
    ActionCallError::Refused {
        reason: format!(
            "{NOT_ON_THIS_SERVER}: This server doesn't know you yet: its operator hasn't made you \
             an owner, and no invitation brought you into a workspace here. Ask the operator to \
             declare your key {key} an owner (on a local stack: `stack.sh owner {key}`), or join \
             with an invitation link someone here sends you."
        ),
    }
}

/// The reason a provider refuses its caller for now with, when what it asked was put to its
/// person and they have not answered: the invocation goes on, and is done once they allow it.
pub const AWAITING_APPROVAL_REASON: &str = "awaiting_approval: it waits for its person to approve it on a device that holds their identity";

/// Why a provider would not do what was asked. Every variant but [`Self::Unanswered`] means
/// nothing ran.
///
/// On the wire it keeps the shape peers decoded before
/// [`Self::StaleRevision`] existed: that variant travels as the
/// [`Self::InvalidRequest`] those peers sent and understand, and such a
/// refusal from them reads back as it. A variant an older peer cannot
/// decode would fail its whole frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(from = "WireActionCallError", into = "WireActionCallError")]
pub enum ActionCallError {
    /// The provider serves no actions right now (no live apps, service off).
    #[error("remote actions are unavailable: {reason}")]
    Unavailable { reason: String },
    /// The target is not offered to this caller — unknown and forbidden are
    /// deliberately the same answer.
    #[error("no such action is offered")]
    UnknownAction,
    /// The caller's authority does not cover the action.
    #[error("the caller may not invoke this action")]
    NotPermitted,
    /// The action requires a confirmation step this request did not carry.
    ///
    /// `departure` states the page departure the action's own run reached and
    /// holds for the person, when it reached one ([`browser::DEPARTURE_FACET`]s):
    /// what is about to leave, where, and the picture of the filled page, so
    /// the question they are asked names it rather than a label for it. It is
    /// empty for an action that asks before it runs anything.
    #[error("the action requires confirmation")]
    ConfirmationRequired { departure: Vec<ActionFacet> },
    /// The request is malformed for this action (arguments, input).
    #[error("invalid request: {reason}")]
    InvalidRequest { reason: String },
    /// The target changed since the revision the request named, or the one
    /// its provider resolved it against: list it again and invoke what is
    /// offered now.
    #[error("invalid request: {}", STALE_REVISION_REASON)]
    StaleRevision,
    /// The invocation id is unknown to this caller.
    #[error("no such invocation")]
    UnknownInvocation,
    /// The invocation id was already used for a different request.
    #[error("the invocation id was already used for a different request")]
    InvocationConflict,
    /// The invocation cannot be cancelled (finished, or never cancellable).
    #[error("the invocation cannot be cancelled: {reason}")]
    NotCancellable { reason: String },
    /// The provider refused for a reason of its own.
    #[error("refused: {reason}")]
    Refused { reason: String },
    /// The workspace is not serving this caller here yet: the same call
    /// works once it is (its `serving` state).
    #[error("the workspace is not ready for this call yet")]
    NotReady,
    /// The device or provider that runs the call did not answer in time. Unlike
    /// every other variant this one does not say that nothing ran: the same
    /// call under the same invocation id is answered by what it did, and runs
    /// it once if it never arrived.
    #[error("the action's provider did not answer in time")]
    Unanswered,
}

impl ActionCallError {
    /// A refusal of the action API ([`crate::api`]) as the action wire says it: the API's own
    /// code, so a caller tells "no such action" apart from "not now" and "not shaped like that"
    /// without reading prose. An API that refused for a reason of its own says it in words, its
    /// code leading them.
    #[must_use]
    pub fn from_api(error: &crate::ApiError) -> Self {
        use crate::ApiError;
        match error {
            ApiError::Unknown(_) | ApiError::UnregisteredType(_) => Self::UnknownAction,
            ApiError::MissingArgument(_)
            | ApiError::UnexpectedArgument(_)
            | ApiError::InvalidArguments(_)
            | ApiError::ArgumentOutOfRange { .. } => Self::InvalidRequest {
                reason: error.to_string(),
            },
            ApiError::ConfirmationRequired => Self::confirmation_required(),
            ApiError::Unavailable { .. } | ApiError::Refused { .. } | ApiError::Failed(_) => {
                Self::Refused {
                    reason: error.to_string(),
                }
            }
        }
    }

    /// The refusal for want of the person's answer, for an action that
    /// reached no page departure to name.
    #[must_use]
    pub const fn confirmation_required() -> Self {
        Self::ConfirmationRequired {
            departure: Vec::new(),
        }
    }

    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unavailable { .. } => "remote_actions_unavailable",
            Self::UnknownAction => "unknown_action",
            Self::NotPermitted => "not_permitted",
            Self::ConfirmationRequired { .. } => "action_confirmation_required",
            Self::InvalidRequest { .. } => "invalid_arguments",
            Self::StaleRevision => "stale_revision",
            Self::UnknownInvocation => "unknown_invocation",
            Self::InvocationConflict => "invocation_conflict",
            Self::NotCancellable { .. } => "not_cancellable",
            Self::Refused { .. } => "refused",
            Self::NotReady => "not_ready",
            Self::Unanswered => "unanswered",
        }
    }

    /// This refusal as a caller is told it: the code the API documents
    /// (`docs/tilo-sh/errors.md`), then what happened, so a caller reading the
    /// typed answer gets the code and one reading the text still reads a
    /// sentence that says what to do.
    #[must_use]
    pub fn said(&self) -> String {
        match self {
            // A caller that retries the same stale revision fails the same way:
            // what to do instead is said.
            Self::StaleRevision => "stale_revision: the resource changed since you read it. \
                Call the action again with tilo_do, which runs it against the resource as it \
                is now, or tilo_read the resource first to see what changed"
                .to_owned(),
            // Refused by the workspace's permissions: said as that, with where it is
            // changed, never as an action the caller cannot name.
            Self::NotPermitted => {
                format!("not_permitted: {}", crate::Availability::NOT_ALLOWED_HERE)
            }
            // What the host could not run now is `unavailable`, the code the API
            // documents; the host's own words follow rather than a restatement.
            Self::Unavailable { reason } => format!("unavailable: {reason}"),
            // No answer is not an answer that nothing is there: a workspace that just
            // started may still be opening its documents, and answers when asked again.
            Self::Unanswered => "unanswered: the workspace did not answer in time; it may \
                still be opening its documents after it started, so ask again"
                .to_owned(),
            Self::InvalidRequest { reason } | Self::Refused { reason } => {
                format!("{}: {reason}", self.code())
            }
            other => format!("{}: {other}", other.code()),
        }
    }

    /// Whether the same call, under the same invocation id, is worth making
    /// again: once the workspace serves the caller ([`Self::NotReady`]), or at
    /// once ([`Self::Unanswered`]).
    #[must_use]
    pub const fn ask_again(&self) -> bool {
        matches!(self, Self::NotReady | Self::Unanswered)
    }

    /// Whether `code` is the [`code`](Self::code) of a refusal the same call is asked again
    /// for ([`Self::ask_again`]), as a failed invocation carries it.
    #[must_use]
    pub fn ask_again_code(code: &str) -> bool {
        code == Self::NotReady.code() || code == Self::Unanswered.code()
    }

    /// Whether the provider refused a directory action that asks for
    /// confirmation because the request named no revision
    /// ([`CONFIRMED_ACTION_NEEDS_REVISION`]).
    #[must_use]
    pub fn needs_revision(&self) -> bool {
        matches!(self, Self::InvalidRequest { reason } if reason == CONFIRMED_ACTION_NEEDS_REVISION)
    }
}

/// What [`ActionCallError::StaleRevision`] says, and the reason it carries
/// on the wire as an [`ActionCallError::InvalidRequest`].
pub const STALE_REVISION_REASON: &str = "the target changed since that revision; list it again";

/// [`ActionCallError`] as it travels: only variants every peer decodes.
#[derive(Serialize, Deserialize)]
#[serde(rename = "ActionCallError")]
enum WireActionCallError {
    Unavailable {
        reason: String,
    },
    UnknownAction,
    NotPermitted,
    ConfirmationRequired {
        /// Written even when there is no departure: a refusal is encoded by
        /// position too, and a part left out is one its reader still expects.
        #[serde(default)]
        departure: Vec<ActionFacet>,
    },
    InvalidRequest {
        reason: String,
    },
    UnknownInvocation,
    InvocationConflict,
    NotCancellable {
        reason: String,
    },
    Refused {
        reason: String,
    },
    NotReady,
    Unanswered,
}

impl From<ActionCallError> for WireActionCallError {
    fn from(error: ActionCallError) -> Self {
        match error {
            ActionCallError::Unavailable { reason } => Self::Unavailable { reason },
            ActionCallError::UnknownAction => Self::UnknownAction,
            ActionCallError::NotPermitted => Self::NotPermitted,
            ActionCallError::ConfirmationRequired { departure } => {
                Self::ConfirmationRequired { departure }
            }
            ActionCallError::InvalidRequest { reason } => Self::InvalidRequest { reason },
            ActionCallError::StaleRevision => Self::InvalidRequest {
                reason: STALE_REVISION_REASON.to_owned(),
            },
            ActionCallError::UnknownInvocation => Self::UnknownInvocation,
            ActionCallError::InvocationConflict => Self::InvocationConflict,
            ActionCallError::NotCancellable { reason } => Self::NotCancellable { reason },
            ActionCallError::Refused { reason } => Self::Refused { reason },
            ActionCallError::NotReady => Self::NotReady,
            ActionCallError::Unanswered => Self::Unanswered,
        }
    }
}

impl From<WireActionCallError> for ActionCallError {
    fn from(error: WireActionCallError) -> Self {
        match error {
            WireActionCallError::InvalidRequest { reason } if reason == STALE_REVISION_REASON => {
                Self::StaleRevision
            }
            WireActionCallError::Unavailable { reason } => Self::Unavailable { reason },
            WireActionCallError::UnknownAction => Self::UnknownAction,
            WireActionCallError::NotPermitted => Self::NotPermitted,
            WireActionCallError::ConfirmationRequired { departure } => {
                Self::ConfirmationRequired { departure }
            }
            WireActionCallError::InvalidRequest { reason } => Self::InvalidRequest { reason },
            WireActionCallError::UnknownInvocation => Self::UnknownInvocation,
            WireActionCallError::InvocationConflict => Self::InvocationConflict,
            WireActionCallError::NotCancellable { reason } => Self::NotCancellable { reason },
            WireActionCallError::Refused { reason } => Self::Refused { reason },
            WireActionCallError::NotReady => Self::NotReady,
            WireActionCallError::Unanswered => Self::Unanswered,
        }
    }
}

/// What `provider` says of `invocation`, read off the runtime's worker when one drives the
/// caller. A provider that asks a device for an answer waits for the runtime to carry it (a
/// session's actions do), and a wait on a worker stops the task that carries it: with enough
/// callers inside that wait at once, nothing is left to deliver any of their answers, and each
/// waits out the provider's own deadline.
#[cfg_attr(
    target_arch = "wasm32",
    expect(
        clippy::unused_async,
        reason = "a page has no worker to leave, so nothing waits"
    )
)]
pub async fn observed_off_the_worker<P>(
    provider: P,
    caller: Caller,
    invocation: InvocationId,
) -> Option<InvocationState>
where
    P: ActionProvider + Send + 'static,
{
    #[cfg(not(target_arch = "wasm32"))]
    if tokio::runtime::Handle::try_current().is_ok() {
        return tokio::task::spawn_blocking(move || provider.observe(&caller, &invocation))
            .await
            .ok()
            .flatten();
    }
    provider.observe(&caller, &invocation)
}

/// A peer that runs actions on behalf of authenticated remote callers.
///
/// Implementations resolve authority from `caller` alone and must be
/// idempotent per [`InvocationId`]: invoking an id again with the same
/// request reports the original invocation, with a different request fails
/// with [`ActionCallError::InvocationConflict`]. [`InvocationLog`]
/// implements that rule.
pub trait ActionProvider {
    /// End this admitted transport run without cancelling its app jobs.
    fn retire_execution(&self, caller: &Caller) {
        caller.retire_execution();
    }

    /// Whether this provider answers the actions published under `provider`, the name a request
    /// targets. A device's dispatcher answers its workspace's own, and a provider a device's
    /// action service serves beside it (`ActionService::add_provider`) answers the one it names,
    /// so one request reaches the provider it was meant for.
    fn serves(&self, provider: &str) -> bool {
        let _ = provider;
        false
    }

    /// One page of what `caller` is offered. An action the caller may not
    /// invoke is absent, not flagged.
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>>;

    /// Admit and start one invocation, or report the one already admitted
    /// under the same id. Never blocks: the answer is the admission (an
    /// accepted receipt, an immediate outcome, or a refusal) once the provider
    /// has it; completion is followed with [`Self::observe`].
    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>>;

    /// The latest state of an invocation `caller` started, or `None` for an
    /// id this caller does not own.
    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState>;

    /// The state [`Self::observe`] answered for `id` was read for `caller`'s call, on the
    /// caller's own thread: what a provider that counts the looks a tool makes of it keeps here.
    fn observed(&self, caller: &Caller, id: &InvocationId, state: Option<&InvocationState>) {
        let _ = (caller, id, state);
    }

    /// Resolves the next time invocation `id` (owned by `caller`) may have
    /// changed state, so a waiter observes it again then instead of on a
    /// timer. `None` when this provider cannot tell; the caller then observes on
    /// its own schedule.
    ///
    /// Wakeups may be spurious: the waiter must [`observe`](Self::observe)
    /// again and tolerate finding nothing new. A provider that answers `Some`
    /// must resolve it within a bounded interval even when nothing changes
    /// (the broker's is a second), so a change it failed to signal costs
    /// latency, never a stalled waiter: a waiting answer ([`settle_with`])
    /// checks its deadline only when woken.
    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let _ = (caller, id);
        None
    }

    /// Where `caller`'s workspace and this device's writes stand on the session that serves
    /// it; `None` for a provider that serves from no session.
    fn session_state(&self, caller: &Caller) -> Option<SessionState> {
        let _ = caller;
        None
    }

    /// Resolves the next time [`Self::session_state`] changed, so a waiter reads it again then
    /// instead of on a timer; `None` when this provider cannot tell. It may never resolve when
    /// nothing changes: a waiter bounds its own wait.
    fn session_state_changed(&self, caller: &Caller) -> Option<Pending<()>> {
        let _ = caller;
        None
    }

    /// Resolves the next time what a watch of the workspace follows under `scope` may have
    /// changed, so a waiter reads it again then instead of on a timer; `None` when this provider
    /// cannot tell, and the waiter then waits until its own deadline.
    ///
    /// Wakeups may be spurious: the waiter reads again and tolerates finding nothing new. A provider
    /// that answers `Some` must resolve it within a bounded interval even when nothing changes,
    /// so a change it failed to signal costs latency rather than a stalled waiter. A wait the
    /// provider finds it cannot follow after all (the device it asks refuses it, or cannot be
    /// asked) resolves at once with why: the waiter is told, never left to its deadline.
    fn changed(
        &self,
        caller: &Caller,
        scope: WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        let _ = (caller, scope);
        None
    }

    /// Ask a running invocation to stop, answered once the provider has kept
    /// the request as durably as it keeps the invocation.
    ///
    /// # Errors
    ///
    /// Returns [`ActionCallError::UnknownInvocation`] for an id this caller
    /// does not own, or [`ActionCallError::NotCancellable`].
    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>>;

    /// Ask the person `caller` acts for to confirm `request` on their own
    /// device, as the provider shows it to them. The answer carries a
    /// [`Confirmation::Approved`] that `caller` invokes `request` again
    /// with, and the arguments as the person changed them, if they did
    /// ([`ConfirmedInvocation::apply`]); a caller never confirms by its own
    /// word. A provider with nobody to ask answers
    /// [`ActionCallError::ConfirmationRequired`].
    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionCallError>> {
        let _ = (caller, request);
        Pending::ready(Err(ActionCallError::confirmation_required()))
    }

    /// Where this provider's callers put content into their workspace's content
    /// store ([`ContentPuts`]); `None` for a provider without one.
    fn content_puts(&self) -> Option<std::sync::Arc<dyn ContentPuts>> {
        None
    }

    /// Where this provider's callers read content back out of their workspace's
    /// content store ([`ContentGets`]); `None` for a provider without one.
    fn content_gets(&self) -> Option<std::sync::Arc<dyn ContentGets>> {
        None
    }

    /// Who is in the workspace now, as this provider's collaboration knows it:
    /// its people and the agents its roster names. `None` for a provider that
    /// keeps no roster, which reports no presence rather than an empty one.
    fn here(&self, caller: &Caller) -> Option<Vec<Here>> {
        let _ = caller;
        None
    }

    /// Where an agent task reads its workspace's records outside their apps ([`ResourceReads`]);
    /// `None` for a provider without them.
    fn resource_reads(&self) -> Option<std::sync::Arc<dyn ResourceReads>> {
        None
    }

    /// Where a task's programs read the resources this provider publishes itself, rather than from
    /// an app's record (a desktop's device, its windows, its styles, Settings' sections): one
    /// reading for the shell's parts and a program alike ([`Readings`]). `None` for a provider that
    /// publishes no such resource.
    fn readings(&self) -> Option<std::sync::Arc<dyn Readings>> {
        None
    }

    /// Where an agent task's changes go to their records' origin ([`ResourceChanges`]).
    fn resource_changes(&self) -> Option<std::sync::Arc<dyn ResourceChanges>> {
        None
    }

    /// This provider's search of what its workspace holds ([`ResourceSearch`]),
    /// which an agent's `find` asks: a desktop's one search, or the records
    /// a provider with no desktop holds, ranked by what they say. `None` for a
    /// provider that holds no records to search.
    fn resource_search(&self) -> Option<std::sync::Arc<dyn ResourceSearch>> {
        None
    }

    /// Every change to the records of a caller's workspace, as this provider
    /// keeps them ([`ChangeFeed`]): what a watch of the whole workspace
    /// reads. `None` for a provider that keeps no records.
    fn change_feed(&self) -> Option<std::sync::Arc<dyn ChangeFeed>> {
        None
    }

    /// The surface the signed package `app_id` the workspace installed
    /// publishes, running or not: what an agent reads its types from, and a
    /// change of its records is decided against, before the app opens.
    fn installed_surface(&self, app_id: &str) -> Pending<Option<Vec<u8>>> {
        let _ = app_id;
        Pending::ready(None)
    }
}

/// Whether `value` is the zero an asker that names no ask uses: left out of
/// the bytes, as it always was.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde hands a skipped field to it by reference"
)]
const fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// An answer to an [`RemoteActionMessage::Observe`], naming the ask it answers.
fn updated(
    correlation: u64,
    invocation: InvocationId,
    state: Option<InvocationState>,
) -> RemoteActionMessage {
    RemoteActionMessage::Update {
        invocation,
        state,
        correlation: Some(correlation),
    }
}

/// Everything that crosses the wire for remote action use.
#[allow(
    clippy::large_enum_variant,
    reason = "a message is built, encoded once and dropped; boxing would only add noise to every constructor and match"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RemoteActionMessage {
    /// Inspect existing feedback admission or read an existing canonical ack.
    /// This never invokes a document mutation or grants workspace access.
    FeedbackRequest {
        correlation: u64,
        query: feedback::FeedbackQuery,
    },
    FeedbackResponse {
        correlation: u64,
        result: Result<feedback::FeedbackAnswer, ActionCallError>,
    },
    /// Ask for one page of the actions offered to the caller.
    CatalogueRequest {
        correlation: u64,
        #[serde(default)]
        query: CatalogueQuery,
    },
    /// Answer to a [`Self::CatalogueRequest`] with the same correlation.
    CatalogueResponse {
        correlation: u64,
        result: Result<CataloguePage, ActionCallError>,
    },
    /// Invocation one action.
    Invoke(InvokeRequest),
    /// Answer to [`Self::Invoke`]; a refusal is [`ReceiptStatus::Rejected`].
    Receipt(InvocationReceipt),
    /// Ask for an invocation's latest state.
    Observe {
        /// Names this ask, so its [`Self::Update`] answers it and no other: a
        /// caller that gave up on one ask and sent another must not mistake
        /// the first one's late answer — which may carry the state it already
        /// saw — for its own. An asker that names none keeps the bytes it
        /// always sent.
        #[serde(default, skip_serializing_if = "is_zero")]
        correlation: u64,
        invocation: InvocationId,
        /// Hold the answer until the invocation moves past `seen` (or, with
        /// no `seen`, settles) or this many milliseconds pass, at most
        /// [`MAX_REMOTE_WAIT`]; then answer with the state as it is. A provider
        /// that predates it answers at once, as before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_ms: Option<u32>,
        /// How far the caller has already seen the invocation get.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seen: Option<UpdateMark>,
    },
    /// Answer to [`Self::Observe`]; `None` for an id the caller does not own.
    /// A provider may also push this unprompted as an invocation progresses.
    Update {
        invocation: InvocationId,
        state: Option<InvocationState>,
        /// The ask this answers, as it named itself; absent for a push, and
        /// for a provider that predates named asks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        correlation: Option<u64>,
    },
    /// Ask a running invocation to stop.
    Cancel { invocation: InvocationId },
    /// Answer to [`Self::Cancel`].
    CancelResponse {
        invocation: InvocationId,
        result: Result<(), ActionCallError>,
    },
    /// Ask the person, on one of their own devices ([`ConfirmationQuestion`]).
    ConfirmationRequest {
        correlation: u64,
        question: ConfirmationQuestion,
    },
    /// Answer to [`Self::ConfirmationRequest`], same correlation.
    ConfirmationResponse {
        correlation: u64,
        result: Result<ConfirmationReply, ActionCallError>,
    },
    /// What the surface offers changed: the page it lists now, sent without
    /// being asked, so the provider that placed it reads what the surface can do
    /// as it is rather than as it was when it first joined.
    CatalogueChanged { page: CataloguePage },
    /// One step of putting bytes into the content store of the device's
    /// workspace ([`ContentPutStep`]): how a client of another device's
    /// actions (an agent's MCP session through its own device) hands it a
    /// file an action then names by `tilo.content:<hash>`.
    ContentPutRequest {
        correlation: u64,
        step: ContentPutStep,
    },
    /// Answer to [`Self::ContentPutRequest`] with the same correlation.
    ContentPutResponse {
        correlation: u64,
        result: Result<ContentPutAnswer, ActionCallError>,
    },
    /// Ask the device for a piece of content in its workspace's store: what a
    /// client of another device's actions (an agent's MCP session through its
    /// own device) downloads a file by, having the file's
    /// `tilo.content:<hash>` reference.
    ContentGetRequest {
        correlation: u64,
        /// The content's reference, `tilo.content:<hash>`.
        content: String,
        /// Where in the content the piece starts.
        offset: u64,
        /// How many bytes the piece may carry; zero asks for one piece's
        /// worth ([`MAX_CONTENT_FRAME_BYTES`]).
        length: u64,
    },
    /// Answer to [`Self::ContentGetRequest`] with the same correlation.
    ContentGetResponse {
        correlation: u64,
        result: Result<ContentGetAnswer, ActionCallError>,
    },
    /// Ask the provider to put `request` to the person the caller acts for and
    /// answer with their confirmation of that one invocation
    /// ([`ActionProvider::confirm`]): where they are asked is the provider's, and a
    /// provider with nobody to ask answers
    /// [`ActionCallError::ConfirmationRequired`].
    Confirm {
        correlation: u64,
        request: Box<InvokeRequest>,
    },
    /// Answer to [`Self::Confirm`], same correlation.
    ConfirmResponse {
        correlation: u64,
        result: Result<Box<ConfirmedInvocation>, ActionCallError>,
    },
    /// Read a record of the caller's workspace through the one record API
    /// ([`ResourceReads`]), decided as every reader of it is; continuing
    /// `on_behalf_of`, an invocation the provider delivered to the caller, it is
    /// that invocation's caller's read.
    ResourceRead {
        correlation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_behalf_of: Option<InvocationId>,
        read: ResourceReadRequest,
    },
    /// Answer to [`Self::ResourceRead`], same correlation: the readings as
    /// JSON text ([`ResourceReadings::readable`]).
    ResourceReading {
        correlation: u64,
        result: Result<String, ActionCallError>,
    },
    /// Ask for the changes to the caller's workspace's records after
    /// `after`, a cursor an earlier answer gave ([`ActionProvider::change_feed`]).
    ChangesRequest {
        correlation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
    },
    /// Answer to [`Self::ChangesRequest`], same correlation.
    ChangesResponse {
        correlation: u64,
        result: Result<ChangePage, ActionCallError>,
    },
    /// Ask for the surface `app`'s signed package publishes, as its text.
    SurfaceRequest { correlation: u64, app: String },
    /// Answer to [`Self::SurfaceRequest`]: the text, when the workspace has it.
    SurfaceResponse {
        correlation: u64,
        result: Result<Option<String>, ActionCallError>,
    },
    /// Ask the device for a link its web origin serves the content of
    /// `link.files` at, for a little while ([`ContentLink`]): what a page
    /// drags a file out by, and saves a large one through.
    ContentLinkRequest {
        correlation: u64,
        link: ContentLinkAsk,
    },
    /// Answer to [`Self::ContentLinkRequest`] with the same correlation.
    ContentLinkResponse {
        correlation: u64,
        result: Result<ContentLink, ActionCallError>,
    },
    /// Ask the device to answer when what a watch follows under `scope` may have
    /// changed ([`ActionProvider::changed`]): a write landed in the workspace's records,
    /// or who is in it moved.
    ChangedRequest { correlation: u64, scope: WatchScope },
    /// Answer to [`Self::ChangedRequest`], same correlation: it may have changed, or
    /// why this provider cannot tell when it does.
    ChangedResponse {
        correlation: u64,
        result: Result<(), ActionCallError>,
    },
}

/// One record a reader asks for over the action API: alone, or `joined`
/// as the chain of parts whose base it is, once it differs from the revision
/// `after` or `wait_ms` passed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceReadRequest {
    /// The app whose record it is.
    pub app: String,
    /// Its id there; empty for the app's record of the workspace.
    pub record: String,
    #[serde(default)]
    pub joined: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// How long to wait for a write past `after`, at most [`MAX_REMOTE_WAIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u32>,
}

impl RemoteActionMessage {
    /// Ask for `invocation`'s state as it is now.
    #[must_use]
    pub const fn observe(invocation: InvocationId) -> Self {
        Self::Observe {
            correlation: 0,
            invocation,
            wait_ms: None,
            seen: None,
        }
    }

    /// Whether this request asks its provider to hold the answer until the
    /// invocation moves, or until a person answers, so whatever carries it must not wait behind
    /// it.
    #[must_use]
    pub const fn waits(&self) -> bool {
        matches!(
            self,
            Self::Observe {
                wait_ms: Some(_),
                ..
            } | Self::Invoke(InvokeRequest {
                wait_ms: Some(_),
                ..
            }) | Self::Confirm { .. }
                | Self::ChangedRequest { .. }
        )
    }

    /// Whether a provider answers this message (as opposed to a caller
    /// receiving it).
    #[must_use]
    pub const fn is_request(&self) -> bool {
        matches!(
            self,
            Self::CatalogueRequest { .. }
                | Self::Invoke(_)
                | Self::Observe { .. }
                | Self::Cancel { .. }
                | Self::ResourceRead { .. }
                | Self::FeedbackRequest { .. }
                | Self::ConfirmationRequest { .. }
                | Self::ContentPutRequest { .. }
                | Self::ContentGetRequest { .. }
                | Self::ContentLinkRequest { .. }
                | Self::Confirm { .. }
                | Self::ChangesRequest { .. }
                | Self::ChangedRequest { .. }
                | Self::SurfaceRequest { .. }
        )
    }
}

/// `request` put to the person `caller` acts for, answered for
/// `correlation` ([`ActionProvider::confirm`]).
fn confirmed<H: ActionProvider>(
    provider: &H,
    caller: &Caller,
    correlation: u64,
    request: &InvokeRequest,
) -> Pending<RemoteActionMessage> {
    provider
        .confirm(caller, request)
        .map(move |result| RemoteActionMessage::ConfirmResponse {
            correlation,
            result: result.map(Box::new),
        })
}

/// The refusal of a request [`serve`] keeps nothing for: canonical feedback,
/// which only its own service answers; uploads over a session, kept by a
/// provider that keeps them open between steps ([`HostedContentPuts`]); and
/// content, read from a store a device keeps. `None` for any other message.
fn not_kept(message: &RemoteActionMessage) -> Option<RemoteActionMessage> {
    let reason = match message {
        RemoteActionMessage::FeedbackRequest { .. } => "canonical feedback service unavailable",
        RemoteActionMessage::ContentPutRequest { .. } => CONTENT_PUTS_UNAVAILABLE,
        RemoteActionMessage::ContentGetRequest { .. } => CONTENT_GETS_UNAVAILABLE,
        RemoteActionMessage::ContentLinkRequest { .. } => CONTENT_LINKS_UNAVAILABLE,
        _ => return None,
    };
    refuse(
        message,
        ActionCallError::Unavailable {
            reason: reason.into(),
        },
    )
}

/// The changes to `caller`'s workspace's records after `after`, from
/// `provider`'s feed, answered for `correlation` ([`ActionProvider::change_feed`]).
fn changed<H: ActionProvider>(
    provider: &H,
    caller: &Caller,
    correlation: u64,
    after: Option<&str>,
) -> Pending<RemoteActionMessage> {
    let result = match provider.change_feed() {
        Some(feed) => feed.since(caller, after),
        None => Pending::ready(Err(ActionCallError::Unavailable {
            reason: CHANGE_FEED_UNAVAILABLE.into(),
        })),
    };
    result.map(move |result| RemoteActionMessage::ChangesResponse {
        correlation,
        result,
    })
}

/// A versioned [`RemoteActionMessage`], the unit a transport carries.
///
/// The encoding is self-describing JSON: action arguments and outputs are
/// open JSON values, and a server forwarding the bytes need not understand
/// them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteActionFrame {
    pub version: u16,
    pub message: RemoteActionMessage,
}

/// Why bytes are not a usable [`RemoteActionFrame`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RemoteFrameError {
    #[error("remote action frame of {len} bytes exceeds the limit")]
    TooLarge { len: usize },
    #[error("remote action frame version {version} is not supported")]
    UnsupportedVersion { version: u16 },
    #[error("malformed remote action frame: {0}")]
    Malformed(String),
}

impl RemoteActionFrame {
    /// Wrap `message` at this build's [`REMOTE_ACTIONS_VERSION`].
    #[must_use]
    pub const fn new(message: RemoteActionMessage) -> Self {
        Self {
            version: REMOTE_ACTIONS_VERSION,
            message,
        }
    }

    /// Encode for a transport.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteFrameError::TooLarge`] past
    /// [`MAX_REMOTE_ACTION_FRAME_BYTES`], or `Malformed` if a value cannot
    /// be encoded.
    pub fn encode(&self) -> Result<Vec<u8>, RemoteFrameError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| RemoteFrameError::Malformed(error.to_string()))?;
        if bytes.len() > MAX_REMOTE_ACTION_FRAME_BYTES {
            return Err(RemoteFrameError::TooLarge { len: bytes.len() });
        }
        Ok(bytes)
    }

    /// Decode bytes from a transport. The version is checked before the
    /// message, so a frame from a newer peer is refused by version rather
    /// than misread.
    pub fn decode(bytes: &[u8]) -> Result<Self, RemoteFrameError> {
        #[derive(Deserialize)]
        struct Version {
            version: u16,
        }
        if bytes.len() > MAX_REMOTE_ACTION_FRAME_BYTES {
            return Err(RemoteFrameError::TooLarge { len: bytes.len() });
        }
        let Version { version } = serde_json::from_slice(bytes)
            .map_err(|error| RemoteFrameError::Malformed(error.to_string()))?;
        if version != REMOTE_ACTIONS_VERSION {
            return Err(RemoteFrameError::UnsupportedVersion { version });
        }
        serde_json::from_slice(bytes)
            .map_err(|error| RemoteFrameError::Malformed(error.to_string()))
    }
}

/// Answer one request from `caller` against `provider`.
///
/// Returns `None` for a message that is not a request (a response arriving
/// at a provider is dropped, never answered). A refused invocation becomes a
/// [`ReceiptStatus::Rejected`] receipt so the caller always learns the fate
/// of the id it chose; an answer the provider abandons becomes a refusal too.
///
/// A request that asks to wait (`wait_ms` on [`RemoteActionMessage::Observe`]
/// or [`InvokeRequest`]) is answered once the invocation moves, through
/// [`ActionProvider::next_change`] and without holding a thread; see
/// [`settle_with`]. A provider that cannot signal changes answers at once.
pub fn serve<H: ActionProvider + Clone + Send + 'static>(
    provider: &H,
    caller: &Caller,
    message: RemoteActionMessage,
) -> Option<Pending<RemoteActionMessage>> {
    let abandoned = refuse(
        &message,
        ActionCallError::Unavailable {
            reason: "the provider dropped the request".into(),
        },
    )?;
    let answer = match message {
        // What a stateless provider keeps none of, it refuses.
        RemoteActionMessage::FeedbackRequest { .. }
        | RemoteActionMessage::ContentPutRequest { .. }
        | RemoteActionMessage::ContentGetRequest { .. }
        | RemoteActionMessage::ContentLinkRequest { .. } => Pending::ready(not_kept(&message)?),
        RemoteActionMessage::CatalogueRequest { correlation, query } => provider
            .query(caller, query)
            .map(move |result| RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            }),
        // The workspace's search, asked as an action of its own, is answered by the provider's
        // search at once: it runs nothing and is remembered as no invocation.
        RemoteActionMessage::Invoke(request) if search::is_find(&request.target) => {
            search::searched(provider, caller, &request).map(RemoteActionMessage::Receipt)
        }
        RemoteActionMessage::Invoke(mut request) => {
            // How the answer travels is not part of what runs.
            let until = request.wait_ms.take().map(wait::deadline);
            let invocation = request.invocation;
            let receipt = provider.invoke(caller, request).map(move |result| {
                result.unwrap_or_else(|error| InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Rejected { error },
                })
            });
            match until {
                Some(until) => wait::settle_receipt(provider, caller, receipt, until),
                None => receipt,
            }
            .map(RemoteActionMessage::Receipt)
        }
        RemoteActionMessage::Observe {
            correlation,
            invocation,
            wait_ms: Some(wait_ms),
            seen,
        } => settle_with(provider, caller, invocation, seen, wait::deadline(wait_ms))
            .map(move |state| updated(correlation, invocation, state)),
        RemoteActionMessage::Observe {
            correlation,
            invocation,
            ..
        } => Pending::ready(updated(
            correlation,
            invocation,
            provider.observe(caller, &invocation),
        )),
        RemoteActionMessage::Cancel { invocation } => provider
            .cancel(caller, &invocation)
            .map(move |result| RemoteActionMessage::CancelResponse { invocation, result }),
        // Read by a provider's own reader, which answers through its trait object: what a reading is
        // made of loads with the code that reads records, not with every provider's API.
        RemoteActionMessage::ResourceRead {
            correlation, read, ..
        } => provider.resource_reads().map_or_else(
            || resource_read::unanswered(correlation),
            |reads| reads.answer_read(caller, correlation, read),
        ),
        // A question is the person's to answer, on the device they are at.
        RemoteActionMessage::ConfirmationRequest { correlation, .. } => Pending::ready(
            confirmation::unanswered(correlation, ActionCallError::confirmation_required()),
        ),
        RemoteActionMessage::Confirm {
            correlation,
            request,
        } => confirmed(provider, caller, correlation, &request),
        RemoteActionMessage::ChangesRequest { correlation, after } => {
            changed(provider, caller, correlation, after.as_deref())
        }
        RemoteActionMessage::ChangedRequest { correlation, scope } => {
            watch::noticed(provider, caller, correlation, scope)
        }
        RemoteActionMessage::SurfaceRequest { correlation, app } => {
            surfaces::surfaced(provider, correlation, &app)
        }
        RemoteActionMessage::CatalogueResponse { .. }
        | RemoteActionMessage::Receipt(_)
        | RemoteActionMessage::Update { .. }
        | RemoteActionMessage::CancelResponse { .. }
        | RemoteActionMessage::ResourceReading { .. }
        | RemoteActionMessage::FeedbackResponse { .. }
        | RemoteActionMessage::ConfirmationResponse { .. }
        | RemoteActionMessage::ContentPutResponse { .. }
        | RemoteActionMessage::ContentGetResponse { .. }
        | RemoteActionMessage::ContentLinkResponse { .. }
        | RemoteActionMessage::ConfirmResponse { .. }
        | RemoteActionMessage::ChangesResponse { .. }
        | RemoteActionMessage::ChangedResponse { .. }
        | RemoteActionMessage::SurfaceResponse { .. }
        | RemoteActionMessage::CatalogueChanged { .. } => return None,
    };
    // An abandoned answer still answers: the caller hears a refusal.
    let (completer, answered) = pending();
    answer.then(move |answer| completer.complete(answer.unwrap_or(abandoned)));
    Some(answered)
}

/// The answer that refuses request `message` with `error` — for a provider that
/// cannot be reached or is not running. Returns `None` for a message that
/// is not a request.
#[must_use]
pub fn refuse(
    message: &RemoteActionMessage,
    error: ActionCallError,
) -> Option<RemoteActionMessage> {
    Some(match message {
        RemoteActionMessage::FeedbackRequest { correlation, .. } => {
            RemoteActionMessage::FeedbackResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::CatalogueRequest { correlation, .. } => {
            RemoteActionMessage::CatalogueResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::Invoke(request) => RemoteActionMessage::Receipt(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Rejected { error },
        }),
        RemoteActionMessage::Observe {
            correlation,
            invocation,
            ..
        } => updated(*correlation, *invocation, None),
        RemoteActionMessage::Cancel { invocation } => RemoteActionMessage::CancelResponse {
            invocation: *invocation,
            result: Err(error),
        },
        RemoteActionMessage::ResourceRead { correlation, .. } => {
            RemoteActionMessage::ResourceReading {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::ConfirmationRequest { correlation, .. } => {
            confirmation::unanswered(*correlation, error)
        }
        RemoteActionMessage::ContentPutRequest { correlation, .. } => {
            RemoteActionMessage::ContentPutResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::ContentGetRequest { correlation, .. } => {
            RemoteActionMessage::ContentGetResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::ContentLinkRequest { correlation, .. } => {
            RemoteActionMessage::ContentLinkResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::Confirm { correlation, .. } => RemoteActionMessage::ConfirmResponse {
            correlation: *correlation,
            result: Err(error),
        },
        RemoteActionMessage::ChangesRequest { correlation, .. } => {
            RemoteActionMessage::ChangesResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::ChangedRequest { correlation, .. } => {
            RemoteActionMessage::ChangedResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        RemoteActionMessage::SurfaceRequest { correlation, .. } => {
            RemoteActionMessage::SurfaceResponse {
                correlation: *correlation,
                result: Err(error),
            }
        }
        // Every other message answers a request: there is nothing to refuse.
        _ => return None,
    })
}

struct LogEntry<T> {
    caller: Caller,
    request: InvokeRequest,
    /// `None` while a start begun with [`InvocationLog::reserve`] is in
    /// flight.
    handle: Option<T>,
}

/// The idempotency log behind [`ActionProvider::invoke`].
///
/// Maps each admitted [`InvocationId`] to the caller and request that
/// admitted it and to the provider's own handle for the running job. A retry
/// with the same caller and request returns the existing handle without
/// starting anything; any other reuse of the id is a conflict. Only admitted
/// invocations are remembered — a refused one ran nothing, so retrying it
/// may be admitted later. At `capacity`, the oldest entry whose admission
/// completed is forgotten. In-flight reservations are never evicted.
pub struct InvocationLog<T> {
    entries: HashMap<InvocationId, LogEntry<T>>,
    order: VecDeque<InvocationId>,
    capacity: usize,
}

impl<T> Default for InvocationLog<T> {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_LOG_CAPACITY)
    }
}

/// What [`InvocationLog::reserve`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reservation {
    /// The id is newly claimed; the caller must now start the job.
    Reserved,
    /// The same caller and request were already admitted.
    Admitted,
}

/// Whether [`InvocationLog::admit`] started a job or found one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Started,
    Retried,
}

impl<T> InvocationLog<T> {
    /// A log remembering at most `capacity` invocations (at least one).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Admit `request` for `caller`, calling `start` only if the id is new.
    ///
    /// # Errors
    ///
    /// Returns [`ActionCallError::InvocationConflict`] when the id was
    /// admitted for another caller or request, [`ActionCallError::Unavailable`]
    /// when all slots are reserved, or whatever `start` returns.
    pub fn admit(
        &mut self,
        caller: &Caller,
        request: &InvokeRequest,
        start: impl FnOnce() -> Result<T, ActionCallError>,
    ) -> Result<(&T, Admission), ActionCallError> {
        let id = request.invocation;
        let admission = match self.reserve(caller, request)? {
            Reservation::Admitted => Admission::Retried,
            Reservation::Reserved => match start() {
                Ok(handle) => {
                    self.complete(&id, handle);
                    Admission::Started
                }
                Err(error) => {
                    self.abandon(&id);
                    return Err(error);
                }
            },
        };
        self.entries
            .get(&id)
            .and_then(|entry| entry.handle.as_ref())
            .map(|handle| (handle, admission))
            .ok_or(ActionCallError::UnknownInvocation)
    }

    /// First half of an admission whose start completes later (for example
    /// across an asynchronous resolution): claim the id for `caller` and
    /// `request`, then [`complete`](Self::complete) or
    /// [`abandon`](Self::abandon) it.
    ///
    /// # Errors
    ///
    /// Returns [`ActionCallError::InvocationConflict`] when the id belongs to
    /// another caller or request, [`ActionCallError::Unanswered`] while an
    /// earlier start of the same invocation is still in flight, and
    /// [`ActionCallError::Unavailable`] while every slot is held by an
    /// in-flight reservation.
    pub fn reserve(
        &mut self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Reservation, ActionCallError> {
        let id = request.invocation;
        if let Some(entry) = self.entries.get_mut(&id) {
            if !caller.continues(&entry.caller) || !entry.request.same_invocation(request) {
                return Err(ActionCallError::InvocationConflict);
            }
            // Asked again on the connection that replaced its own: it is this one's from now on.
            entry.caller.clone_from(caller);
            // Its first start has not ended: a workspace action that runs whole as it starts
            // (a server taking in a workspace's gigabytes) is still going. Nothing is known of
            // what it did yet, so the caller asks the same invocation again, as for one whose
            // answer was lost.
            return if entry.handle.is_some() {
                Ok(Reservation::Admitted)
            } else {
                Err(ActionCallError::Unanswered)
            };
        }
        while self.order.len() >= self.capacity {
            let oldest_completed = self.order.iter().position(|candidate| {
                self.entries
                    .get(candidate)
                    .is_some_and(|entry| entry.handle.is_some())
            });
            let Some(index) = oldest_completed else {
                return Err(ActionCallError::Unavailable {
                    reason: "all invocation slots are still being admitted; retry".into(),
                });
            };
            if let Some(oldest) = self.order.remove(index) {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back(id);
        self.entries.insert(
            id,
            LogEntry {
                caller: caller.clone(),
                request: InvokeRequest {
                    wait_ms: None,
                    appearance: None,
                    ..request.clone()
                },
                handle: None,
            },
        );
        Ok(Reservation::Reserved)
    }

    /// Record the provider's handle for a reserved invocation.
    pub fn complete(&mut self, id: &InvocationId, handle: T) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.handle = Some(handle);
        }
    }

    /// Release a reservation whose start failed: nothing ran, so the id may
    /// be admitted later.
    pub fn abandon(&mut self, id: &InvocationId) {
        if self
            .entries
            .get(id)
            .is_some_and(|entry| entry.handle.is_none())
        {
            self.entries.remove(id);
            self.order.retain(|candidate| candidate != id);
        }
    }

    /// The handle for an invocation `caller` admitted, or the caller it continues
    /// ([`Caller::continues`]).
    #[must_use]
    pub fn get(&self, caller: &Caller, id: &InvocationId) -> Option<&T> {
        self.entries
            .get(id)
            .filter(|entry| caller.continues(&entry.caller))
            .and_then(|entry| entry.handle.as_ref())
    }

    /// Mutable access to the handle for an invocation `caller` admitted.
    pub fn get_mut(&mut self, caller: &Caller, id: &InvocationId) -> Option<&mut T> {
        self.entries
            .get_mut(id)
            .filter(|entry| caller.continues(&entry.caller))
            .and_then(|entry| entry.handle.as_mut())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::{ActionContract, ActionDescriptor, ExecutionLocus, InputSpec, OperationKind};

    tilo_test_env::scenario! {
        fn a_caller_keeps_its_identity_and_authority;

        phase a_bot_is_its_own_member_acting_for_its_installer;
        {
            let installer = [7u8; 32];
            let caller = Caller::bot("6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6", installer, [9u8; 32]);
            assert_eq!(
                caller.acting().member_id(),
                Some(&tilo_action_policy::MemberId::bot(
                    "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6"
                ))
            );
            assert_eq!(
                caller.acting().acted_for_person(),
                Some(hex::encode(installer).as_str())
            );
            assert!(caller.acting().is_bot());
            assert!(caller.acting().delegated());
        }

        phase an_app_caller_acts_as_the_person_whose_session_runs_it;
        {
            let caller = Caller::local_app("so.tilo.example").acting_for("ada");
            assert_eq!(caller.acting().person_id(), Some("ada"));
            assert_eq!(
                caller.acting().app().map(ToString::to_string).as_deref(),
                Some("so.tilo.example")
            );
            assert_eq!(caller.acting_for_person(), Some("ada"));
        }

        phase execution_lifetime_is_shared_without_changing_caller_identity;
        {
            let first = Caller::local_person("owner").in_new_execution();
            let retained = first.clone();
            let second = first.clone().in_new_execution();
            let hash = |caller: &Caller| {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(caller, &mut hasher);
                std::hash::Hasher::finish(&hasher)
            };
            let before = hash(&first);
            first.retire_execution();
            assert!(!retained.execution_is_live());
            assert!(second.execution_is_live());
            assert_eq!(first, retained);
            assert_eq!(hash(&first), before);
            let reconnected = retained.in_new_execution();
            assert!(reconnected.execution_is_live());
            assert_ne!(reconnected.execution_id(), first.execution_id());
        }

        phase projected_execution_retains_transport_retirement_without_changing_authority;
        {
            let original = Caller::local_person("projected-person").in_new_execution();
            let projected = Caller::authenticated([1; 32], [2; 32])
                .in_execution(original.execution_lease().unwrap());
            assert_eq!(projected.execution_id(), original.execution_id());
            assert_eq!(projected.user_public_key(), Some(&[1; 32]));
            assert_eq!(original.user_public_key(), None);
            original.retire_execution();
            assert!(!projected.execution_is_live());
        }
    }

    fn alice() -> Caller {
        Caller::authenticated([1; 32], [2; 32])
    }

    fn mallory() -> Caller {
        Caller::authenticated([9; 32], [8; 32])
    }

    tilo_test_env::scenario! {
        fn an_application_action_names_its_destination;

        phase opening_an_app_needs_no_workspace_consent;
        // Camera's "Open Photos" opens a window of the person's own, as the
        // launcher does: a workspace's grants have nothing to consent to, so a guest
        // is not asked first.
        {
            for spec in [
                ApplicationLaunch::app("so.tilo.photos").spec("open-photos", "Open Photos", ""),
                ApplicationLaunch::content_spec("so.tilo.photos", "open-capture", "Open", ""),
            ] {
                assert!(spec.contract.is_own_device(), "{}", spec.id);
                assert_eq!(
                    spec.contract.need().kind,
                    crate::ActionKind::Read,
                    "{}",
                    spec.id
                );
            }
        }

        phase application_launch_action_preserves_destination_and_never_falls_back_to_open;
        {
            let launch = ApplicationLaunch::with_action(
                "so.tilo.file-explorer",
                "files.remote-account:616461",
                "open-remote-account",
            );
            let spec = launch.spec("open-files", "Browse files", "Open this account in Files");
            let encoded = spec.build_args([]).unwrap();
            assert_eq!(encoded.type_id, ApplicationLaunch::TYPE);
            let decoded: ApplicationLaunch = serde_json::from_slice(&encoded.payload).unwrap();
            assert!(decoded.valid());
            for workspace in ["workspace-a", "workspace-b"] {
                assert_eq!(
                    decoded.target(workspace),
                    application_action_target(
                        workspace,
                        "so.tilo.file-explorer",
                        "files.remote-account:616461",
                        "open-remote-account",
                    )
                );
            }
            assert!(matches!(decoded.argument(), InvokeArgument::None));
            let legacy: ApplicationLaunch = serde_json::from_value(serde_json::json!({
                "app_id": "so.tilo.photos"
            }))
            .unwrap();
            assert!(legacy.valid());
            assert_eq!(
                legacy.target("workspace-a"),
                application_open_target("workspace-a", "so.tilo.photos")
            );
            let mut invalid = decoded;
            invalid.content_hash = Some("a".repeat(64));
            assert!(
                !invalid.valid(),
                "a handoff must not mix content staging and an app action"
            );
            invalid.content_hash = None;
            invalid.action.as_mut().unwrap().resource.clear();
            assert!(
                !invalid.valid(),
                "an invalid destination must fail, not open an empty app"
            );
        }

        phase an_application_action_target_names_its_workspace_and_app;
        {
            let target =
                application_action_target("workspace-1", "so.tilo.chat", "chat.workspace", "call");
            assert_eq!(
                application_provider(&target.provider),
                Some(("workspace-1", "so.tilo.chat"))
            );
            assert_eq!(target.address, ActionAddress::new("chat.workspace", "call"));
            for other in [
                "7",
                "workspace:workspace-1",
                "application:workspace-1",
                "application::so.tilo.chat",
                "application:workspace-1:",
                "application:workspace-1:a:",
            ] {
                assert_eq!(application_provider(other), None, "{other}");
            }
            assert_eq!(application_provider_resource(&target.provider), None);

            let target = application_resource_target(
                "workspace-1",
                "so.tilo.notes",
                "uuid:7",
                "notes.page:1",
                "open",
            );
            assert_eq!(
                application_provider(&target.provider),
                Some(("workspace-1", "so.tilo.notes"))
            );
            assert_eq!(
                application_provider_resource(&target.provider),
                Some("uuid:7")
            );
            assert_eq!(target.address, ActionAddress::new("notes.page:1", "open"));
        }
    }

    fn descriptor(resource: &str, action: &str) -> ActionDescriptor {
        ActionDescriptor {
            did: None,
            state: crate::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: "notes.note".into(),
            resource_label: "Groceries".into(),
            action_label: "Archive".into(),
            keywords: Vec::new(),
            icon: None,
            description: Some("Move the note to the archive".into()),
            locus: ExecutionLocus::ClientLocal,
            kind: OperationKind::Document,
            contract: ActionContract::default(),
            input: InputSpec::None,
            params: Vec::new(),
            view: None,
        }
    }

    /// A provider with one action whose runs are counted, so a test can prove
    /// how many times the action actually started.
    struct CountingHost {
        runs: AtomicU32,
        log: Mutex<InvocationLog<Mutex<InvocationUpdate>>>,
    }

    impl CountingHost {
        fn new() -> Self {
            Self {
                runs: AtomicU32::new(0),
                log: Mutex::new(InvocationLog::default()),
            }
        }

        fn runs(&self) -> u32 {
            self.runs.load(Ordering::SeqCst)
        }

        fn finish(&self, caller: &Caller, id: &InvocationId) {
            if let Some(update) = self.log.lock().unwrap().get(caller, id) {
                *update.lock().unwrap() = InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: ActionOutput::default(),
                };
            }
        }
    }

    impl CountingHost {
        fn cancel_now(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionCallError> {
            let log = self.log.lock().unwrap();
            let mut update = log
                .get(caller, id)
                .ok_or(ActionCallError::UnknownInvocation)?
                .lock()
                .unwrap();
            if update.is_terminal() {
                return Err(ActionCallError::NotCancellable {
                    reason: "finished".into(),
                });
            }
            *update = InvocationUpdate::Cancelled {
                invocation: *id,
                message: None,
            };
            drop(update);
            drop(log);
            Ok(())
        }
    }

    impl ActionProvider for CountingHost {
        fn query(
            &self,
            _caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Ok(CataloguePage::from_rows(
                "1".into(),
                vec![catalogue_row("note:1", "archive")],
                &query,
            )))
        }

        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            Pending::ready((|| {
                if request.target.address != ActionAddress::new("note:1", "archive") {
                    return Err(ActionCallError::UnknownAction);
                }
                let update = {
                    let mut log = self.log.lock().unwrap();
                    let (update, _) = log.admit(caller, &request, || {
                        self.runs.fetch_add(1, Ordering::SeqCst);
                        Ok(Mutex::new(InvocationUpdate::Accepted {
                            invocation: request.invocation,
                        }))
                    })?;
                    let update = update.lock().unwrap().clone();
                    drop(log);
                    update
                };
                Ok(InvocationState::new(request.invocation, update).receipt())
            })())
        }

        fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
            self.log
                .lock()
                .unwrap()
                .get(caller, id)
                .map(|update| InvocationState::new(*id, update.lock().unwrap().clone()))
        }

        fn cancel(
            &self,
            caller: &Caller,
            id: &InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(self.cancel_now(caller, id))
        }
    }

    fn catalogue_row(resource: &str, action: &str) -> CatalogueRow {
        CatalogueRow {
            offered: None,
            provider: "7".into(),
            app_id: "notes".into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: resource.into(),
                kind: "notes.note".into(),
                label: "Groceries".into(),
                presentation: None,
            },
            action: Some(descriptor(resource, action)),
            has_children: false,
            visibility: None,
            surface: None,
        }
    }

    /// The answer of a provider that answers immediately.
    fn now<T>(mut answer: Pending<T>) -> T {
        match answer.poll_ready() {
            PendingPoll::Ready(value) => value,
            PendingPoll::Waiting | PendingPoll::Abandoned => panic!("the provider answers at once"),
        }
    }

    fn archive(invocation: InvocationId) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: "7".into(),
                address: ActionAddress::new("note:1", "archive"),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    fn round_trip(message: RemoteActionMessage) -> RemoteActionMessage {
        let bytes = RemoteActionFrame::new(message).encode().unwrap();
        RemoteActionFrame::decode(&bytes).unwrap().message
    }

    /// One of each catalogue and invoke message shape.
    fn request_messages(invocation: InvocationId) -> Vec<RemoteActionMessage> {
        let mut arguments = serde_json::Map::new();
        arguments.insert("title".into(), serde_json::json!("Weekly"));
        vec![
            RemoteActionMessage::CatalogueRequest {
                correlation: 3,
                query: CatalogueQuery::default(),
            },
            RemoteActionMessage::CatalogueRequest {
                correlation: 5,
                query: CatalogueQuery {
                    provider: Some("7".into()),
                    parent_resource: Some("folder:a".into()),
                    needle: "gro".into(),
                    cursor: vec![1, 2],
                    revision: Some("r1".into()),
                    max_results: 8,
                    limits: Some(CatalogueLimits {
                        max_bytes: Some(4096),
                        max_work: None,
                        max_depth: Some(1),
                    }),
                    ..CatalogueQuery::default()
                },
            },
            RemoteActionMessage::CatalogueResponse {
                correlation: 3,
                result: now(CountingHost::new().query(&alice(), CatalogueQuery::default())),
            },
            RemoteActionMessage::CatalogueResponse {
                correlation: 4,
                result: Err(ActionCallError::Unavailable {
                    reason: "no apps".into(),
                }),
            },
            RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                argument: InvokeArgument::Json(arguments),
                context: Some(ActionContext::for_resource(
                    "note:1",
                    "notes.note",
                    "Groceries",
                )),
                ..archive(invocation)
            }),
            RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                argument: InvokeArgument::Text("hello".into()),
                ..archive(invocation)
            }),
            RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                confirmation: Some(Confirmation::Phrase("delete it".into())),
                ..archive(invocation)
            }),
            RemoteActionMessage::Invoke(InvokeRequest {
                wait_ms: Some(2_000),
                appearance: None,
                ..archive(invocation)
            }),
        ]
    }

    /// One of each receipt, update and cancel message shape.
    fn outcome_messages(invocation: InvocationId) -> Vec<RemoteActionMessage> {
        vec![
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(serde_json::json!({"archived": true})),
                        resources: vec!["notes.note:1".into()],
                    },
                },
            }),
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Failed {
                    error: ActionFailure {
                        code: "disk_full".into(),
                        message: "no space".into(),
                        details: None,
                        retryable: true,
                    },
                },
            }),
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionCallError::InvocationConflict,
                },
            }),
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Cancelled {
                    message: Some("stopped".into()),
                },
            }),
            RemoteActionMessage::observe(invocation),
            RemoteActionMessage::Observe {
                correlation: 7,
                invocation,
                wait_ms: Some(10_000),
                seen: Some(UpdateMark::Progress { sequence: 2 }),
            },
            RemoteActionMessage::Update {
                invocation,
                state: Some(InvocationState::new(
                    invocation,
                    InvocationUpdate::Progress {
                        invocation,
                        sequence: 2,
                        phase: "copying".into(),
                        completed: Some(1),
                        total: Some(4),
                        message: None,
                    },
                )),
                correlation: Some(7),
            },
            RemoteActionMessage::Update {
                invocation,
                state: None,
                correlation: Some(7),
            },
            RemoteActionMessage::Cancel { invocation },
            RemoteActionMessage::CancelResponse {
                invocation,
                result: Err(ActionCallError::NotCancellable {
                    reason: "finished".into(),
                }),
            },
        ]
    }

    tilo_test_env::scenario! {
        fn remote_messages_and_failures_read_back_typed;

        phase a_stale_revision_is_typed_and_travels_as_older_peers_read_it;
        // A stale revision is typed in this build and travels in the shape
        // every peer decodes: an older peer reads the invalid request it always
        // sent, and the reasons older providers gave it read back as the variant.
        {
            let encoded = serde_json::to_value(ActionCallError::StaleRevision).unwrap();
            assert_eq!(
                encoded,
                serde_json::json!({"InvalidRequest": {"reason": STALE_REVISION_REASON}})
            );
            assert_eq!(
                serde_json::from_value::<ActionCallError>(encoded).unwrap(),
                ActionCallError::StaleRevision
            );
            assert_eq!(ActionCallError::StaleRevision.code(), "stale_revision");
            // Only that exact reason: a refusal that merely mentions a revision
            // is the refusal it is.
            let other = ActionCallError::Refused {
                reason: "the call changed since that revision, honestly".into(),
            };
            let round_trip: ActionCallError =
                serde_json::from_value(serde_json::to_value(&other).unwrap()).unwrap();
            assert_eq!(round_trip, other);
            let needs = ActionCallError::InvalidRequest {
                reason: CONFIRMED_ACTION_NEEDS_REVISION.into(),
            };
            assert!(needs.needs_revision());
            assert!(!ActionCallError::StaleRevision.needs_revision());
        }

        phase a_failure_with_details_round_trips;
        // A failure's structured details travel beside its message.
        {
            let update = InvocationUpdate::Failed {
                invocation: InvocationId::new(),
                error: ActionFailure {
                    code: "invalid_arguments".into(),
                    message: "argument `pageSize` must be at most 64".into(),
                    details: Some(serde_json::json!({
                        "param": "pageSize",
                        "violation": {"kind": "maximum", "bound": 64}
                    })),
                    retryable: false,
                },
            };
            let encoded = serde_json::to_string(&update).unwrap();
            assert_eq!(
                serde_json::from_str::<InvocationUpdate>(&encoded).unwrap(),
                update
            );
        }

        phase a_refusal_for_want_of_the_person_carries_the_departure_the_run_stated;
        // A run that reached something only the person may allow is refused for
        // want of their answer, and that refusal carries what the run stated
        // about it: the page departure it holds, as it travels to the provider that
        // asks them.
        {
            let departure = vec![
                browser::departure_facet("page", "https://visa.example/apply"),
                browser::departure_facet("kind", "submit"),
                browser::departure_facet("method", "POST"),
                browser::departure_facet("destination", "https://visa.example"),
                browser::departure_facet("picture", "tilo.content:5f2b"),
            ];
            let invocation = InvocationId::new();
            let state = InvocationState::new(
                invocation,
                InvocationUpdate::Failed {
                    invocation,
                    error: ActionFailure {
                        code: CONFIRMATION_REQUIRED_CODE.into(),
                        message: "the person has to allow this".into(),
                        details: serde_json::to_value(&departure).ok(),
                        retryable: false,
                    },
                },
            );
            let receipt = state.receipt();
            let ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired { departure: stated },
            } = &receipt.status
            else {
                panic!("a run waiting on the person is a refusal: {receipt:?}");
            };
            assert_eq!(*stated, departure);
            // The refusal travels with it, so a provider in another process asks the
            // person about what leaves and not about a name for it.
            let error = ActionCallError::ConfirmationRequired {
                departure: stated.clone(),
            };
            let wire = serde_json::to_value(&error).unwrap();
            assert_eq!(
                serde_json::from_value::<ActionCallError>(wire).unwrap(),
                error
            );
        }

        phase the_remote_wire_is_versioned_and_explicit;
        // Every message round-trips through a frame, frames from another version are refused not
        // misread, and the wire names versions and messages explicitly.
        // Invocations each phase below in order; every phase builds its own fixture.
        {
            every_message_round_trips_through_a_frame();
            frames_from_another_version_are_refused_not_misread();
            the_wire_names_versions_and_messages_explicitly();
        }
    }

    fn every_message_round_trips_through_a_frame() {
        let invocation = InvocationId(uuid::Uuid::from_u128(7));
        let messages = request_messages(invocation)
            .into_iter()
            .chain(outcome_messages(invocation));
        for message in messages {
            assert_eq!(round_trip(message.clone()), message);
        }
    }

    fn frames_from_another_version_are_refused_not_misread() {
        let mut frame = serde_json::to_value(RemoteActionFrame::new(
            RemoteActionMessage::CatalogueRequest {
                correlation: 1,
                query: CatalogueQuery::default(),
            },
        ))
        .unwrap();
        frame["version"] = serde_json::json!(REMOTE_ACTIONS_VERSION + 1);
        // A newer peer may also use a message this build has never seen.
        frame["message"] = serde_json::json!({"Subscribe": {"topic": 1}});
        let bytes = serde_json::to_vec(&frame).unwrap();
        assert_eq!(
            RemoteActionFrame::decode(&bytes),
            Err(RemoteFrameError::UnsupportedVersion {
                version: REMOTE_ACTIONS_VERSION + 1
            })
        );
        assert!(matches!(
            RemoteActionFrame::decode(
                format!(r#"{{"version":{REMOTE_ACTIONS_VERSION},"message":{{"Subscribe":{{}}}}}}"#)
                    .as_bytes()
            ),
            Err(RemoteFrameError::Malformed(_))
        ));
        assert!(matches!(
            RemoteActionFrame::decode(&vec![b' '; MAX_REMOTE_ACTION_FRAME_BYTES + 1]),
            Err(RemoteFrameError::TooLarge { .. })
        ));
    }

    fn the_wire_names_versions_and_messages_explicitly() {
        let bytes = RemoteActionFrame::new(RemoteActionMessage::Cancel {
            invocation: InvocationId(uuid::Uuid::nil()),
        })
        .encode()
        .unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            format!(
                r#"{{"version":{REMOTE_ACTIONS_VERSION},"message":{{"Cancel":{{"invocation":"00000000-0000-0000-0000-000000000000"}}}}}}"#
            )
        );
    }

    fn a_retried_invocation_reports_the_original_and_runs_once() {
        let provider = CountingHost::new();
        let id = InvocationId::new();
        let first = now(provider.invoke(&alice(), archive(id))).unwrap();
        assert_eq!(first.status, ReceiptStatus::Accepted);
        let retry = now(provider.invoke(&alice(), archive(id))).unwrap();
        assert_eq!(retry, first);
        assert_eq!(provider.runs(), 1, "a retry never starts a second run");

        provider.finish(&alice(), &id);
        let after = now(provider.invoke(&alice(), archive(id))).unwrap();
        assert!(matches!(after.status, ReceiptStatus::Completed { .. }));
        assert_eq!(provider.runs(), 1, "a retry after completion reports it");
    }

    fn an_invocation_id_cannot_be_reused_or_borrowed() {
        let provider = CountingHost::new();
        let id = InvocationId::new();
        now(provider.invoke(&alice(), archive(id))).unwrap();

        let mut different = archive(id);
        different.argument = InvokeArgument::Text("other".into());
        assert_eq!(
            now(provider.invoke(&alice(), different)),
            Err(ActionCallError::InvocationConflict)
        );
        assert_eq!(
            now(provider.invoke(&mallory(), archive(id))),
            Err(ActionCallError::InvocationConflict),
            "another actor cannot attach to someone else's invocation"
        );
        assert_eq!(provider.observe(&mallory(), &id), None);
        assert_eq!(
            now(provider.cancel(&mallory(), &id)),
            Err(ActionCallError::UnknownInvocation)
        );
        assert_eq!(provider.runs(), 1);
    }

    fn a_refused_invocation_is_not_remembered() {
        let mut log = InvocationLog::<u32>::default();
        let request = archive(InvocationId::new());
        assert_eq!(
            log.admit(&alice(), &request, || Err(ActionCallError::NotPermitted)),
            Err(ActionCallError::NotPermitted)
        );
        let (handle, admission) = log.admit(&alice(), &request, || Ok(5)).unwrap();
        assert_eq!((*handle, admission), (5, Admission::Started));
    }

    fn the_log_forgets_oldest_invocations_past_capacity() {
        let mut log = InvocationLog::<u32>::with_capacity(2);
        let requests: Vec<_> = (0..3).map(|_| archive(InvocationId::new())).collect();
        for (n, request) in (0u32..).zip(&requests) {
            log.admit(&alice(), request, || Ok(n)).unwrap();
        }
        assert_eq!(log.get(&alice(), &requests[0].invocation), None);
        assert_eq!(log.get(&alice(), &requests[2].invocation), Some(&2));
    }

    fn serve_answers_requests_and_ignores_responses() {
        let provider = std::sync::Arc::new(CountingHost::new());
        let id = InvocationId::new();
        let Some(RemoteActionMessage::Receipt(receipt)) = serve(
            &provider,
            &alice(),
            RemoteActionMessage::Invoke(archive(id)),
        )
        .map(now) else {
            panic!("an invoke is answered with a receipt");
        };
        assert_eq!(receipt.status, ReceiptStatus::Accepted);

        let mut unknown = archive(InvocationId::new());
        unknown.target.address = ActionAddress::new("note:2", "delete");
        assert_eq!(
            serve(
                &provider,
                &alice(),
                RemoteActionMessage::Invoke(unknown.clone())
            )
            .map(now),
            Some(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: unknown.invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionCallError::UnknownAction
                },
            }))
        );
        assert_eq!(
            serve(
                &provider,
                &alice(),
                RemoteActionMessage::Cancel { invocation: id }
            )
            .map(now),
            Some(RemoteActionMessage::CancelResponse {
                invocation: id,
                result: Ok(()),
            })
        );
        let Some(RemoteActionMessage::Update {
            state: Some(state), ..
        }) = serve(&provider, &alice(), RemoteActionMessage::observe(id)).map(now)
        else {
            panic!("an observe is answered with the invocation state");
        };
        assert!(matches!(
            state.receipt().status,
            ReceiptStatus::Cancelled { .. }
        ));
        assert!(serve(&provider, &alice(), RemoteActionMessage::Receipt(receipt)).is_none());
    }

    fn typed_arguments_use_the_local_invocation_slot() {
        let mut object = serde_json::Map::new();
        object.insert("n".into(), serde_json::json!(2));
        assert_eq!(InvokeArgument::None.to_slot(), None);
        assert_eq!(
            InvokeArgument::Text("hi".into()).to_slot().as_deref(),
            Some("hi")
        );
        assert_eq!(
            InvokeArgument::Json(object.clone()).to_slot(),
            Some(format!("{ARGUMENTS_JSON_PREFIX}{{\"n\":2}}"))
        );
        // A slot read back names the argument it was written from, so a
        // device that sends an invocation on runs what it admitted.
        for argument in [
            InvokeArgument::None,
            InvokeArgument::Text("hi".into()),
            InvokeArgument::Json(object),
        ] {
            assert_eq!(
                InvokeArgument::from_slot(argument.to_slot().as_deref()),
                argument
            );
        }
    }

    fn a_remote_device_is_the_person_its_root_names() {
        let caller = Caller::authenticated([0xAB; 32], [1; 32]);
        assert_eq!(
            caller.acting().member_id(),
            Some(&tilo_action_policy::MemberId::person("ab".repeat(32))),
            "every device of one person is the same member"
        );
        assert_eq!(caller.workspace(), None);
        let agent = Caller::local_agent("helper").in_workspace("workspace-1");
        assert_eq!(
            agent.acting().member_id(),
            Some(&tilo_action_policy::MemberId::agent("helper"))
        );
        assert_eq!(agent.workspace(), Some("workspace-1"));
        assert_eq!(agent.device_public_key(), None);
    }

    fn a_refusal_answers_every_request_and_nothing_else() {
        let error = ActionCallError::Unavailable {
            reason: "down".into(),
        };
        let id = InvocationId::new();
        assert_eq!(
            refuse(&invoke_message(id), error.clone()),
            Some(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: id,
                status: ReceiptStatus::Rejected {
                    error: error.clone()
                },
            }))
        );
        assert_eq!(
            refuse(&RemoteActionMessage::observe(id), error.clone()),
            Some(RemoteActionMessage::Update {
                invocation: id,
                state: None,
                correlation: Some(0),
            })
        );
        assert_eq!(
            refuse(
                &RemoteActionMessage::Update {
                    invocation: id,
                    state: None,
                    correlation: None,
                },
                error
            ),
            None
        );
    }

    fn invoke_message(id: InvocationId) -> RemoteActionMessage {
        RemoteActionMessage::Invoke(archive(id))
    }

    /// A search and a change feed, asked across the API.
    mod search_wire;

    tilo_test_env::scenario! {
        fn a_host_serves_each_invocation_once_as_its_caller;

        phase a_retry_with_another_appearance_is_another_invocation;
        {
            let request = archive(InvocationId(uuid::Uuid::from_u128(9)));
            let appearing = InvokeRequest {
                appearance: Some(crate::Appearance::new("alice", None, None).unwrap()),
                ..request.clone()
            };
            assert!(!request.same_invocation(&appearing));
            assert!(appearing.same_invocation(&InvokeRequest {
                wait_ms: Some(10),
                ..appearing.clone()
            }));
            let json = serde_json::to_string(&request).unwrap();
            assert!(!json.contains("appearance"), "{json}");
        }

        phase remote_invocations_run_at_most_once_per_owned_id;
        // A retried invocation reports the original and runs once; ids cannot be reused or borrowed;
        // refusals are not remembered; the log forgets the oldest past capacity; a reservation holds
        // the id until the start completes.
        // Invocations each phase below in order; every phase builds its own fixture.
        {
            a_retried_invocation_reports_the_original_and_runs_once();
            an_invocation_id_cannot_be_reused_or_borrowed();
            a_refused_invocation_is_not_remembered();
            the_log_forgets_oldest_invocations_past_capacity();
            a_reservation_holds_the_id_until_the_start_completes();
        }

        phase admitted_executions_share_authority_but_cannot_borrow_invocations;
        {
            let first = alice().in_new_execution();
            let second = alice().in_new_execution();
            assert_eq!(first.acting(), second.acting());
            assert_eq!(first.workspace(), second.workspace());
            assert_ne!(first.execution_id(), second.execution_id());
            let provider = CountingHost::new();
            let id = InvocationId::new();
            let admitted = now(provider.invoke(&first, archive(id))).unwrap();
            assert_eq!(now(provider.invoke(&first, archive(id))).unwrap(), admitted);
            assert_eq!(
                now(provider.invoke(&second, archive(id))),
                Err(ActionCallError::InvocationConflict)
            );
            assert_eq!(provider.observe(&second, &id), None);
            assert_eq!(
                now(provider.cancel(&second, &id)),
                Err(ActionCallError::UnknownInvocation)
            );
            let retained = first.clone();
            let hash = |caller: &Caller| {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(caller, &mut hasher);
                std::hash::Hasher::finish(&hasher)
            };
            let before = hash(&first);
            provider.retire_execution(&first);
            assert!(!retained.execution_is_live());
            assert!(second.execution_is_live());
            assert_eq!(first, retained);
            assert_eq!(hash(&first), before);
            let reconnected = retained.in_new_execution();
            assert!(reconnected.execution_is_live());
            assert_ne!(reconnected.execution_id(), first.execution_id());
            provider.finish(&first, &id);
            assert!(matches!(
                now(provider.invoke(&first, archive(id))).unwrap().status,
                ReceiptStatus::Completed { .. }
            ));
            assert!(provider.observe(&first, &id).is_some());
            assert!(provider.observe(&second, &id).is_none());
            // The connection that replaced the ended one is the same caller going on: it follows
            // and asks again what it started there, and the invocation is carried out once. The
            // one open beside it borrows nothing, even now.
            assert!(reconnected.continues(&first));
            assert!(!second.continues(&first));
            assert!(provider.observe(&reconnected, &id).is_some());
            assert!(matches!(
                now(provider.invoke(&reconnected, archive(id))).unwrap().status,
                ReceiptStatus::Completed { .. }
            ));
            assert_eq!(provider.runs(), 1);
            let someone_else = Caller::authenticated([3; 32], [4; 32]).in_new_execution();
            assert!(!someone_else.continues(&first), "another device continues nothing");
        }

        phase serving_answers_each_request_as_the_person_the_device_belongs_to;
        // Serve answers requests and ignores responses, typed arguments use the local invocation slot,
        // a remote device is the person its root names, and a refusal answers every request and nothing
        // else.
        // Invocations each phase below in order; every phase builds its own fixture.
        {
            serve_answers_requests_and_ignores_responses();
            typed_arguments_use_the_local_invocation_slot();
            a_remote_device_is_the_person_its_root_names();
            a_refusal_answers_every_request_and_nothing_else();
        }

        phase capacity_churn_preserves_in_flight_deduplication;
        {
            let caller = alice();
            let mut log = InvocationLog::<u32>::with_capacity(3);
            let pending = archive(InvocationId::new());
            assert_eq!(log.reserve(&caller, &pending), Ok(Reservation::Reserved));
            let mut completed = VecDeque::new();
            for n in 0..8 {
                let request = archive(InvocationId::new());
                assert_eq!(
                    log.admit(&caller, &request, || Ok(n)),
                    Ok((&n, Admission::Started))
                );
                completed.push_back(request.invocation);
                if completed.len() > 2 {
                    let evicted = completed.pop_front().unwrap();
                    assert_eq!(log.get(&caller, &evicted), None);
                }
                for id in &completed {
                    assert!(log.get(&caller, id).is_some());
                }
                assert!(matches!(
                    log.admit(&caller, &pending, || panic!("duplicate dispatch")),
                    Err(ActionCallError::Unanswered)
                ));
                assert_eq!(
                    log.reserve(&mallory(), &pending),
                    Err(ActionCallError::InvocationConflict)
                );
            }
            log.complete(&pending.invocation, 99);
            assert_eq!(
                log.admit(&caller, &pending, || panic!("duplicate dispatch")),
                Ok((&99, Admission::Retried))
            );
            let next = archive(InvocationId::new());
            assert_eq!(log.reserve(&caller, &next), Ok(Reservation::Reserved));
            assert_eq!(log.get(&caller, &pending.invocation), None);
        }

        phase capacity_churn_refuses_new_ids_when_every_slot_is_reserved;
        {
            let caller = alice();
            // Zero capacity is clamped to one, which must still protect its reservation.
            for capacity in 0..=2 {
                let mut log = InvocationLog::<u32>::with_capacity(capacity);
                let pending: Vec<_> = (0..capacity.max(1))
                    .map(|_| archive(InvocationId::new()))
                    .collect();
                for request in &pending {
                    assert_eq!(log.reserve(&caller, request), Ok(Reservation::Reserved));
                }
                let refused = archive(InvocationId::new());
                for _ in 0..4 {
                    assert!(matches!(
                        log.admit(&caller, &refused, || panic!("dispatch at capacity")),
                        Err(ActionCallError::Unavailable { .. })
                    ));
                    for request in &pending {
                        assert!(matches!(
                            log.admit(&caller, request, || panic!("duplicate dispatch")),
                            Err(ActionCallError::Unanswered)
                        ));
                    }
                }
                for (n, request) in pending.iter().enumerate() {
                    let n = u32::try_from(n).unwrap();
                    log.complete(&request.invocation, n);
                    assert_eq!(
                        log.admit(&caller, request, || panic!("duplicate dispatch")),
                        Ok((&n, Admission::Retried))
                    );
                }
                // Capacity refusals are not remembered and can be retried later.
                assert_eq!(log.reserve(&caller, &refused), Ok(Reservation::Reserved));
                log.abandon(&refused.invocation);
                assert_eq!(log.reserve(&caller, &refused), Ok(Reservation::Reserved));
            }
        }
    }

    fn a_reservation_holds_the_id_until_the_start_completes() {
        let mut log = InvocationLog::<u32>::default();
        let request = archive(InvocationId::new());
        assert_eq!(log.reserve(&alice(), &request), Ok(Reservation::Reserved));
        assert!(
            matches!(
                log.reserve(&alice(), &request),
                Err(ActionCallError::Unanswered)
            ),
            "a retry during the start neither runs twice nor pretends to be admitted: it is \
             asked again, as an answer lost on its way is"
        );
        assert_eq!(log.get(&alice(), &request.invocation), None);
        log.complete(&request.invocation, 9);
        assert_eq!(log.reserve(&alice(), &request), Ok(Reservation::Admitted));
        assert_eq!(log.get(&alice(), &request.invocation), Some(&9));

        let failed = archive(InvocationId::new());
        assert_eq!(log.reserve(&alice(), &failed), Ok(Reservation::Reserved));
        log.abandon(&failed.invocation);
        assert_eq!(
            log.reserve(&alice(), &failed),
            Ok(Reservation::Reserved),
            "an abandoned start ran nothing and may be admitted again"
        );
    }
}
