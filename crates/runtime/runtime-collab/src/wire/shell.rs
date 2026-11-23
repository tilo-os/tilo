//! The person's shell on the daemon's dispatch.
//!
//! A desktop session never enters its person's workspaces itself: the device's daemon holds
//! them, and the session is its client. The daemon admits a shell by its launch, as it admits an
//! app: it makes a link for one shell, a connection it opened itself with a one-use secret, and
//! hands it to the shell it starts ([`SHELL_FD_ENV`]). A process that holds no link, whatever
//! account it runs as, is not the shell. A shell connection asks three things of the daemon:
//!
//! - the documents of the workspaces the device holds, read for the device itself and written as
//!   it: [`ShellCall`] carries what `tilo_peer_state::workspace_documents::DeviceDocuments` and
//!   `DeviceWrites` ask, and the daemon signs and seals each write with the device's key, so the
//!   shell holds no key of any workspace;
//! - a key access or membership action of a workspace, or one of the host itself, as a typed
//!   `tilo_actions::remote::InvokeRequest` ([`ShellCall::Invoke`]): the daemon answers it from a
//!   workspace it keeps, and forwards it to the workspace's keeping device over tilo-actions/1
//!   otherwise;
//! - how each workspace stands (its people, its policy, its hosts and what this device may do
//!   there): resources the daemon publishes and the shell watches on the same connection
//!   ([`super::Operation::Services`], [`super::Operation::Watch`]).

use serde::{Deserialize, Serialize};

/// What a shell link opens with, before its first call: as long as an app's launch marker, which
/// it is told apart from. The one-use secret the daemon made for the link follows it.
pub const SHELL_MAGIC: &[u8; 4] = b"QSH1";

/// How many bytes the one-use secret of a shell link takes.
pub const SHELL_SECRET_BYTES: usize = 32;

/// Names the descriptor of the shell link the daemon handed the shell it started.
pub const SHELL_FD_ENV: &str = "TILO_SHELL_FD";

/// What a shell asks of the device's documents and actions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellCall {
    /// `app`'s `resource` of `workspace`, whole, or only what changed after the heads `since`.
    Read {
        workspace: String,
        app: String,
        resource: String,
        since: Option<Vec<[u8; 32]>>,
    },
    /// Whether `app`'s `resource` of `workspace` reads now: held here, nothing it waits for still
    /// on its way.
    Readable {
        workspace: String,
        app: String,
        resource: String,
    },
    /// Every document `workspace` holds, by its app and route.
    Listing { workspace: String },
    /// Whether the device holds anything of `app`'s `resource` in `workspace`.
    Holds {
        workspace: String,
        app: String,
        resource: String,
    },
    /// The workspaces the device holds, by their ids.
    Workspaces,
    /// Write `changes` to `app`'s `document` of `workspace`, signed and sealed as the device.
    Write {
        workspace: String,
        app: String,
        document: String,
        /// The members who read it, where only some do.
        readers: Option<String>,
        /// The document's founding write: the whole document, which the workspace must not hold
        /// yet.
        founding: bool,
        /// The whole document: a founding, or a checkpoint of one held already.
        snapshot: bool,
        changes: Vec<u8>,
    },
    /// Wait until the device serving `workspace` answered every write the device made to
    /// `documents` (each an app and a document of it) before this call.
    Served {
        workspace: String,
        documents: Vec<(String, String)>,
    },
    /// Invoke an action as the shell's person on this device: a workspace's key access and
    /// membership (`RemoteActionTarget::workspace_governance`) or the host's own operations. The request is
    /// `tilo_actions::remote::InvokeRequest` as JSON.
    Invoke { request: String },
    /// The documents the device took a write to since this connection last asked, waited for
    /// until there is one: the device's own writes, its apps' and every other device's alike.
    Writes,
    /// Run a resource store command (`tilo_resource_store::wire::Command` JSON) on `app`'s
    /// resources in `workspace`, as the device's person: what an app the shell runs in its own
    /// process reads and writes its resources with.
    Resources {
        workspace: String,
        app: String,
        command: String,
    },
    /// Keep or release an account's sign-in in a workspace's vault, or forget the account.
    Vault(VaultCall),
}

/// A shell's ask of a workspace's vault and accounts. The vault opens on this device alone, so the
/// shell asks, and is answered only what the person who asks may have.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VaultCall {
    /// Keep `password` as the sign-in of the account titled `title`, replacing the entry `item`
    /// names when it names one, as the person `person` asks from their device `device`. Answered
    /// with the entry's reference (`vault.item:<id>`).
    Keep {
        workspace: String,
        person: [u8; 32],
        device: [u8; 32],
        item: Option<String>,
        title: String,
        username: String,
        /// The servers the sign-in is for, and nowhere else.
        places: Vec<String>,
        password: String,
    },
    /// Release the sign-in of the entry `item` to `app`, as `person` asks from `device`. Answered
    /// with the secret.
    Release {
        workspace: String,
        person: [u8; 32],
        device: [u8; 32],
        app: String,
        item: String,
    },
    /// Forget the account `account` from the workspace's accounts. Answered `true` when the
    /// workspace had it.
    Forget { workspace: String, account: String },
}

impl std::fmt::Debug for VaultCall {
    /// Names the call and what it is about, never the password it carries.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep {
                workspace, title, ..
            } => write!(
                formatter,
                "Keep {{ workspace: {workspace}, title: {title} }}"
            ),
            Self::Release {
                workspace,
                app,
                item,
                ..
            } => write!(
                formatter,
                "Release {{ workspace: {workspace}, app: {app}, item: {item} }}"
            ),
            Self::Forget { workspace, account } => write!(
                formatter,
                "Forget {{ workspace: {workspace}, account: {account} }}"
            ),
        }
    }
}

/// What a [`VaultCall`] is answered with: an entry's reference, a released secret, or whether an
/// account was forgotten. Never shown in a log.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultAnswer(pub String);

impl std::fmt::Debug for VaultAnswer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VaultAnswer(..)")
    }
}

/// The daemon's answer to a [`ShellCall`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellReply {
    /// A [`ShellCall::Read`].
    Read(ShellRead),
    /// A [`ShellCall::Readable`] or [`ShellCall::Holds`].
    Is(bool),
    /// A [`ShellCall::Listing`]: `(app, route)` of each document.
    Listing(Vec<(String, String)>),
    /// A [`ShellCall::Workspaces`].
    Workspaces(Vec<String>),
    /// A [`ShellCall::Write`]: the write as the workspace took it, or why it took none, as
    /// `tilo_peer_state::owner::WriteRefusal` JSON.
    Written(Result<ShellWrite, String>),
    /// A [`ShellCall::Served`]: `Ok` once the serving device holds each write, `Err` with why it
    /// refused one. `None` when the workspace has no serving device to ask: one the device keeps
    /// itself.
    Served(Option<Result<(), String>>),
    /// A [`ShellCall::Invoke`] once it settled: the value the action answered, as JSON, or why it
    /// failed.
    Invoked(Result<String, String>),
    /// A [`ShellCall::Writes`]: each document written, as its workspace, app and record (`""` for
    /// the app's primary). `None` when the device took more than it kept told, so the shell reads
    /// everything it follows again.
    Writes(Option<Vec<DocumentWritten>>),
    /// A [`ShellCall::Resources`]: the store's `tilo_resource_store::wire::Reply` as JSON, or why
    /// the command was not run.
    Resources(Result<String, String>),
    /// A [`ShellCall::Vault`]: what the call is answered with, or why it was refused.
    Vault(Result<VaultAnswer, String>),
}

/// What the device holds of one document, as `tilo_peer_state::workspace_documents::DocumentRead`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellRead {
    Whole {
        revision: [u8; 32],
        heads: Vec<[u8; 32]>,
        bytes: Vec<u8>,
    },
    Changes {
        revision: [u8; 32],
        heads: Vec<[u8; 32]>,
        bytes: Vec<u8>,
    },
    Unchanged([u8; 32]),
    Absent,
    Pending,
}

/// A document the device took a write to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentWritten {
    pub workspace: String,
    pub app: String,
    /// The record's id on the wire, `""` for the app's primary.
    pub record: String,
}

/// A write the workspace took, as `tilo_peer_state::owner::AppWrite`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellWrite {
    pub peer: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub document_id: String,
    /// The author's access, as `tilo_action_policy::MemberAccess` JSON.
    pub author: String,
    pub replayed: bool,
    /// The signed op, as the postcard bytes of its `collab_types::SignedOp`.
    pub envelope: Vec<u8>,
}
