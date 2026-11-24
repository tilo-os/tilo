//! The person's shell on the daemon's dispatch.
//!
//! A desktop session never enters its person's workspaces itself: the device's daemon holds
//! them, and the session is its client. The daemon admits a shell by its launch, as it admits an
//! app: it makes a link for one shell, a connection it opened itself with a one-use secret, and
//! hands it to the shell it starts ([`SHELL_FD_ENV`]). A process that holds no link, whatever
//! account it runs as, is not the shell. A shell connection asks four things of the daemon:
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
//! - the device's tilolang processes, to which the shell hands each of its windows' instances
//!   ([`ShellCall::Processes`]);
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

/// What a connection the shell opens to hand the device's tilolang processes an instance begins
/// with, followed by the one-use secret the daemon answered the shell's [`ShellCall::Processes`]
/// with: both written in one message that carries the shell's end of a packet socket pair
/// (`runtime_portal::packet::pair`) as `SCM_RIGHTS`. The daemon's processes then take the
/// hand-over on that packet socket (`runtime_portal::device_processes`). As long as
/// [`SHELL_MAGIC`].
pub const PROCESSES_MAGIC: &[u8; 4] = b"QSP1";

/// A connection the shell hands the daemon ([`ShellCall::Vouch`]), made on the shell's app
/// socket: the apps the person runs reach the shell there, and only the shell can tell them apart
/// (they run as the person, beside it; the daemon runs as the device's own account and sees
/// neither their processes nor the sockets they could reach). The connection itself travels on
/// the shell link as a descriptor, so it speaks to the daemon directly from then on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vouch {
    /// The number, on the shell link, of the descriptor of the connection.
    pub connection: u32,
    /// The app the shell launched and attested the connection as, by its own launch record: the
    /// daemon admits the connection as that app, workspace and window. `None` for a client that
    /// named no launch, which the daemon admits by what it presents, as it admits any connection
    /// to its socket.
    pub app: Option<VouchedApp>,
    /// The service of the daemon's the connection was made for.
    #[serde(default)]
    pub to: HandedTo,
}

/// The service of the daemon's a [`Vouch`]'s connection was made for: each the shell serves at a
/// socket of its own, where the person's apps reach it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandedTo {
    /// The collaboration broker: the app's documents, actions and services.
    #[default]
    Collaboration,
    /// The identity service: what the app signs with in its workspace. An app presents itself
    /// there by what it says, so the connection carries no [`VouchedApp`].
    Identity,
}

/// The app a [`Vouch`] admits its connection as.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VouchedApp {
    pub app: String,
    pub workspace: String,
    /// The window the app runs in, as the shell's launch named it.
    pub window: Option<u64>,
    /// The app runs with no window: it takes its actions on its records, never an open.
    pub windowless: bool,
    /// The ticket the shell's launch answered, which the app's open is tied back to.
    pub ticket: Option<u64>,
    /// The number of the descriptor pinning the app's process (a pidfd), and of its launcher's
    /// when the launcher handed the app's process over.
    pub process: u32,
    pub launcher: Option<u32>,
    /// The workspace agent the shell launched the app to act as; `None` for its person.
    pub member: Option<String>,
}

/// Where on the shell's screen the person pressed an action: the window's tab key and the
/// resource the action is on, as the shell names them. Only the window key and the resource
/// travel: the daemon fills the device with itself and the app with the action's own, so a shell
/// cannot claim another app's window or another device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellPress {
    /// The window's tab key, as the session publishes it (`device.Window.key`).
    pub instance: String,
    /// The resource the action acts on.
    pub resource: String,
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
    /// (`RemoteActionTarget::workspace_governance`) or of the host's own operations. With
    /// `caller`, the request is that caller's, an app or an agent acting for the person, and the
    /// daemon decides it as that caller.
    Action {
        workspace: Option<String>,
        frame: Vec<u8>,
        /// Boxed: who a call is for is large, and every call carrying one would be as large.
        caller: Option<Box<tilo_actions::remote::KeptCaller>>,
        /// Where on this shell's screen the person pressed the call, when they did: what the
        /// daemon keeps as the question's `pressed`. Only the shell connection may set it; any
        /// other connection that sends one is refused whole.
        #[serde(default)]
        pressed: Option<ShellPress>,
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
    /// Admit the connection the vouch hands over as the app it names, which the shell launched:
    /// answered once the daemon serves it.
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
    /// Admit one connection to the device's tilolang processes: answered with the one-use secret
    /// that connection opens with after [`PROCESSES_MAGIC`]. The link itself is a byte stream that
    /// many calls share, so the descriptors of a hand-over travel on that connection instead.
    Processes,
}

/// What the name of every provider a shell serves begins with, besides the session's own actions
/// ([`tilo_actions::remote::session_actions::PROVIDER`]).
pub const SHELL_PROVIDERS: &str = "session.";

/// The providers a shell serves under the ids apps have always called them by, each with the
/// application the dispatch admits its calls under (the one whose window offers them). Each needs
/// the person's seat, so the desktop session answers it: the activity the shell shows, the person's
/// settings and model keys on this computer, the account this computer is signed in to, the
/// machines it plans and the tickets it issues to add another, and the workspace's events and
/// its assistant's look-round, which read the shortcuts this session saves.
pub const SEAT_PROVIDERS: &[(&str, &str)] = {
    use tilo_actions::remote::{
        device_tickets, machines, model_providers, server_account, settings_resources,
        workspace_activity, workspace_events, workspace_initiative,
    };
    const SETTINGS: &str = "so.tilo.settings";
    &[
        (workspace_activity::PROVIDER, workspace_activity::APP_ID),
        (device_tickets::PROVIDER, SETTINGS),
        (machines::PROVIDER, "so.tilo.imager"),
        (model_providers::PROVIDER, SETTINGS),
        (server_account::PROVIDER, SETTINGS),
        (settings_resources::PROVIDER, SETTINGS),
        (workspace_events::PROVIDER, workspace_events::PROVIDER),
        (
            workspace_initiative::PROVIDER,
            workspace_initiative::PROVIDER,
        ),
    ]
};

/// The application the dispatch admits the calls of `provider` under, when it is one of the
/// [`SEAT_PROVIDERS`].
#[must_use]
pub fn seat_app(provider: &str) -> Option<&'static str> {
    SEAT_PROVIDERS
        .iter()
        .find_map(|(served, app)| (*served == provider).then_some(*app))
}

/// Whether `provider` is one a shell serves: its own, or one of the [`SEAT_PROVIDERS`], never one
/// the device or an app answers.
#[must_use]
pub fn serves(provider: &str) -> bool {
    provider == tilo_actions::remote::session_actions::PROVIDER
        || seat_app(provider).is_some()
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
    /// Write how this device's sync of the account `account` went (`health`, a word of
    /// `accounts::SyncHealth`, whether it is signed in, and what it said) into the workspace's
    /// accounts as this device's report, as `person` asks from `device`. Answered with the
    /// account.
    ReportSync {
        workspace: String,
        person: [u8; 32],
        device: [u8; 32],
        account: String,
        health: String,
        signed_in: bool,
        message: String,
    },
    /// Browser's ask `command` for one of the website sessions the person `person` selected,
    /// decided where the vault is. Answered with Browser's reply, as JSON.
    WebsiteSession {
        workspace: String,
        person: [u8; 32],
        command: crate::BrowserServiceSessionCommand,
    },
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
            Self::ReportSync {
                workspace,
                account,
                health,
                ..
            } => write!(
                formatter,
                "ReportSync {{ workspace: {workspace}, account: {account}, health: {health} }}"
            ),
            Self::WebsiteSession { workspace, .. } => {
                write!(formatter, "WebsiteSession {{ workspace: {workspace} }}")
            }
        }
    }
}

/// What a [`VaultCall`] is answered with: an entry's reference, a released secret, the account
/// reported, or Browser's reply about a website session. Never shown in a log.
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
    /// A [`ShellCall::Vouch`]: the daemon serves the connection as the app.
    Vouched,
    /// A [`ShellCall::Receipt`]: kept in the workspace, or why it was not.
    Receipted(Result<(), String>),
    /// A [`ShellCall::Authorizes`]: allowed, or why not.
    Authorized(Result<(), String>),
    /// A [`ShellCall::Provide`]: the handle the provider's calls arrive on.
    Providing(super::Handle),
    /// A [`ShellCall::Processes`]: the daemon's socket the connection to its processes opens on,
    /// and the secret it opens with.
    Processes {
        socket: std::path::PathBuf,
        secret: [u8; SHELL_SECRET_BYTES],
    },
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
