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
//! - any action, as the person, as a `tilo_actions::remote::RemoteActionFrame` the daemon's one
//!   dispatch answers ([`ShellCall::Action`]), the frame an app's connection carries
//!   (`Service::RemoteAction`): a workspace's key access and membership and the host's own
//!   operations among them, answered from a workspace the daemon keeps and forwarded to the
//!   workspace's keeping device over tilo-actions/1 otherwise;
//! - how each workspace stands (its people, its policy, its hosts and what this device may do
//!   there): resources the daemon publishes and the shell watches on the same connection
//!   ([`super::Operation::Services`], [`super::Operation::Watch`]).
//!
//! The other way, the shell serves providers on the daemon's dispatch ([`ShellCall::Provide`]):
//! a call of one reaches the shell as [`super::Ask::Action`] on the handle its registration was
//! answered with, carrying the same frames, and the registration goes when the shell closes that
//! handle or its link.

use serde::{Deserialize, Serialize};

/// What a shell link opens with, before its first call: as long as an app's launch marker, which
/// it is told apart from. The one-use secret the daemon made for the link follows it.
pub const SHELL_MAGIC: &[u8; 4] = b"QSH1";

/// How many bytes the one-use secret of a shell link takes.
pub const SHELL_SECRET_BYTES: usize = 32;

/// Names the descriptor of the shell link the daemon handed the shell it started.
pub const SHELL_FD_ENV: &str = "TILO_SHELL_FD";

/// What a connection the shell opens for one of the apps it launched begins with, followed by the
/// one-use secret the daemon answered the shell's [`ShellCall::Vouch`] with: the daemon admits the
/// connection as the app that vouch named. As long as [`SHELL_MAGIC`].
pub const VOUCH_MAGIC: &[u8; 4] = b"QSV1";

/// The app a shell vouches for ([`ShellCall::Vouch`]): one the shell launched and admitted on its
/// own connection, in the workspace that launch named. The connection the vouch admits then
/// carries that app's calls on its documents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vouch {
    pub app: String,
    pub workspace: String,
    /// The window the app runs in, as the shell's launch named it.
    pub window: Option<u64>,
    /// The app runs with no window: it takes its actions on its records, never an open.
    pub windowless: bool,
}

/// What a shell asks of the device's documents and actions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellCall {
    /// Whether `app`'s `resource` of `workspace` reads now: held here, nothing it waits for still
    /// on its way.
    Readable {
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
    /// A `tilo_actions::remote::RemoteActionFrame` request the shell makes as its person, answered
    /// by the daemon's dispatch as [`ShellReply::Action`]: a call in `workspace`, or, in none, one
    /// of the person's workspace list, of a workspace's key access and membership
    /// (`RemoteActionTarget::workspace_governance`) or of the host's own operations. With `app`,
    /// the request is that app's, acting for the person: a read in it is decided as the app.
    Action {
        workspace: Option<String>,
        frame: Vec<u8>,
        app: Option<String>,
    },
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
    /// Admit one connection as the app `Vouch` names, which the shell launched: answered with the
    /// one-use secret that connection opens with after [`VOUCH_MAGIC`].
    Vouch(Vouch),
    /// Record the receipt of an effect an action the shell ran had, in `workspace`, as this
    /// device's: `redacted` is the redacted `tilo_desktop_model::WorkspaceEffectReceipt` JSON.
    Receipt {
        workspace: String,
        redacted: Vec<u8>,
    },
    /// Whether the device records the receipts of effects in `workspace`: it does in a workspace
    /// it joined, each device in its own slice of it.
    KeepsReceipts { workspace: String },
    /// Whether the member whose device `certificate` names (`tilo_user_identity::DeviceCert` JSON)
    /// may read `app`'s `resource` of `workspace`: decided at the resource, for an agent the shell
    /// admitted itself, before the shell reads it for that agent.
    Authorizes {
        workspace: String,
        app: String,
        resource: String,
        certificate: String,
    },
    /// Serve `provider` on the device's dispatch from now on, offering what `catalogue` lists
    /// (a `tilo_actions::remote::CataloguePage` as JSON): answered with the handle each call of it
    /// arrives on ([`ShellReply::Providing`], [`super::Ask::Action`]). Only a provider of the
    /// shell's own ([`serves`]) that nothing else serves. Asked again for a provider it serves
    /// already, the shell's catalogue of it is what `catalogue` lists from then on. It goes when
    /// the shell closes the handle ([`super::Operation::Close`]) or its link, and a call it had not
    /// answered fails.
    Provide { provider: String, catalogue: String },
}

/// What the name of every provider a shell serves begins with, besides the session's own actions
/// ([`tilo_actions::remote::session_actions::PROVIDER`]).
pub const SHELL_PROVIDERS: &str = "session.";

/// Whether `provider` is one a shell serves: its own, never one the device or an app answers.
#[must_use]
pub fn serves(provider: &str) -> bool {
    provider == tilo_actions::remote::session_actions::PROVIDER
        || provider
            .strip_prefix(SHELL_PROVIDERS)
            .is_some_and(|name| !name.is_empty())
}

/// What a person typed into the fields of an ask an approved call answers (a password, a
/// passphrase), in the order the ask took them. It travels on the shell link alone, beside the
/// call's frame and never in it, and is never shown in a log.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Typed(pub Vec<zeroize::Zeroizing<String>>);

impl std::fmt::Debug for Typed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Typed({} answers)", self.0.len())
    }
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
    /// A [`ShellCall::Readable`] or [`ShellCall::KeepsReceipts`].
    Is(bool),
    /// A [`ShellCall::Workspaces`].
    Workspaces(Vec<String>),
    /// A [`ShellCall::Write`]: the write as the workspace took it, or why it took none, as
    /// `tilo_peer_state::owner::WriteRefusal` JSON.
    Written(Result<ShellWrite, String>),
    /// A [`ShellCall::Served`]: `Ok` once the serving device holds each write, `Err` with why it
    /// refused one. `None` when the workspace has no serving device to ask: one the device keeps
    /// itself.
    Served(Option<Result<(), String>>),
    /// The `tilo_actions::remote::RemoteActionFrame` answering a [`ShellCall::Action`].
    Action(Vec<u8>),
    /// A [`ShellCall::Writes`]: each document written, as its workspace, app and record (`""` for
    /// the app's primary). `None` when the device took more than it kept told, so the shell reads
    /// everything it follows again.
    Writes(Option<Vec<DocumentWritten>>),
    /// A [`ShellCall::Resources`]: the store's `tilo_resource_store::wire::Reply` as JSON, or why
    /// the command was not run.
    Resources(Result<String, String>),
    /// A [`ShellCall::Vault`]: what the call is answered with, or why it was refused.
    Vault(Result<VaultAnswer, String>),
    /// A [`ShellCall::Vouch`]: the daemon's socket the vouched connection opens on, and the secret
    /// it opens with.
    Vouched {
        socket: std::path::PathBuf,
        secret: [u8; SHELL_SECRET_BYTES],
    },
    /// A [`ShellCall::Receipt`]: kept in the workspace, or why it was not.
    Receipted(Result<(), String>),
    /// A [`ShellCall::Authorizes`]: allowed, or why not.
    Authorized(Result<(), String>),
    /// A [`ShellCall::Provide`]: the handle the provider's calls arrive on.
    Providing(super::Handle),
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
