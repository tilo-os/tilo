//! The person's shell on the daemon's dispatch.
//!
//! A desktop session never enters its person's workspaces itself: the device's daemon holds
//! them, and the session is its client. The daemon admits a shell by its seat: it gives the shell
//! it starts, or answers a sign-in with, a one-use secret ([`SHELL_TOKEN_ENV`]), and the shell
//! redeems it for its link with the one request that establishes one ([`SHELL_LINK`]) on the
//! daemon's own socket. A shell whose link ended asks for another for the same seat, proved by
//! the account and process the kernel names and the link the daemon admitted before, so a daemon
//! restart leaves no dead desktop. A process the daemon admitted no link for, whatever account it
//! runs as, is not the shell. A shell connection asks six things of the daemon:
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
//! - the links of the workspaces' live sessions, which the daemon carries into the relay it runs
//!   or on to another device's: each a connection the shell hands over ([`HandedTo::Live`]);
//! - the person's own programs' connections to Tilo's API, which the shell takes at the socket
//!   they have always reached in the login's private directory and the daemon serves as the
//!   person ([`HandedTo::Api`]);
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

/// Names the one-use secret the daemon gave the shell it started or answered a sign-in with: the
/// shell redeems it for its link with the one request that establishes a shell link, `shells#link`,
/// on the daemon's own socket.
pub const SHELL_TOKEN_ENV: &str = "TILO_SHELL_TOKEN";

/// The request a shell makes for its link on the daemon's native socket: `{"native":"shells#link"}`,
/// with the one-use secret a daemon gave it in `token` the first time, and none again for the same
/// seat.
pub const SHELL_LINK: &str = "shells#link";

/// The request a shell hands the device's tilolang processes an instance with, on the daemon's
/// native socket: `{"native":"shells#processes","secret":"<hex>"}`, the one-use secret the daemon
/// answered the shell's [`ShellCall::Processes`] with, sent with the daemon's end of a packet
/// socket pair (`runtime_portal::packet::pair`) as the line's one descriptor. It is answered
/// `{"handed":true}`, or `{"error":…}`. The daemon admits it by the account the kernel names for
/// the connection's peer, which must be the account of the seat the shell's link was made for,
/// and by the secret, which admits one hand-over within a few seconds of being answered. The
/// processes then take the hand-over on that packet socket (`runtime_portal::device_processes`).
pub const SHELL_PROCESSES: &str = "shells#processes";

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
    /// The program that made the connection, as the kernel named it to the shell: what an API
    /// connection ([`HandedTo::Api`]) is the person's on, once the person said yes to that program.
    #[serde(default)]
    pub program: Option<VouchedProgram>,
}

/// The program at the other end of a connection the shell hands over: its executable, as the
/// kernel named it for the process pinned by the connection's pidfd. The person said yes to this
/// program before the shell handed its connection over; where the connection came from never did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VouchedProgram {
    pub exe: std::path::PathBuf,
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
    /// The device's live session: the connection is one link of the shell's (`tilo_live::link`),
    /// which the daemon carries into the relay it runs or on to another device's. It is the
    /// shell's own, so it carries no [`VouchedApp`].
    Live,
    /// Tilo's API for the person's own programs on this computer (the `tilo` command, the SDKs,
    /// scripts): a connection one of them made at the API socket in the login's private
    /// directory, which only this account's own programs reach and the shell took after the
    /// kernel said its peer is one of them, and the person said yes to the program that made it
    /// ([`Vouch::program`]). The daemon serves it as the person, so it carries
    /// no [`VouchedApp`].
    Api,
    /// The device VPN: the connection is one link of the shell's (`device_vpn::link`), which the
    /// daemon carries on this device's endpoint, where only it dials as the device. It is the
    /// shell's own, so it carries no [`VouchedApp`].
    DeviceVpn,
    /// The person's speakers and the audio routes between their devices: the connection is one
    /// link of the shell's (`tilo_session_client::speakers`), which the daemon carries on this
    /// device's endpoint, handing each envelope to the device it is for. It is the shell's own,
    /// so it carries no [`VouchedApp`].
    Speakers,
}

/// The launch a [`Vouch`] admits its connection as: the daemon decided and recorded the launch
/// before any process of it existed, so the connection is admitted from that record (its app,
/// workspace, window and the member it acts as), never from what the shell says of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VouchedApp {
    /// The ticket of the daemon's launch the connecting process belongs to: the launch's own
    /// process, or one the shell judged descends from it.
    pub ticket: u64,
    /// The number of the descriptor pinning the connecting process (a pidfd).
    pub process: u32,
}

/// The machine's own kinds of application, which run unconfined as the person: what a host launch
/// is, and the daemon decides whether one may start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostKind {
    /// A Flatpak, in its own sandbox.
    Flatpak,
    /// An `AppImage` the person downloaded.
    AppImage,
    /// A program the person added.
    Program,
    /// One of the operating system's applications.
    System,
}

/// The machine's own application a host launch starts, as the shell names it: the executor
/// resolves it to a command from the entry it names, never from a command the asker wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostEntry {
    /// A desktop entry, by its id, with the files and links it opens and whether it is asked for
    /// another window of an app already showing one.
    Desktop {
        id: String,
        files: Vec<String>,
        urls: Vec<String>,
        new_window: bool,
    },
    /// A program file with no desktop entry (a downloaded `AppImage`, an executable opened in
    /// Files).
    Program { path: std::path::PathBuf },
}

impl HostEntry {
    /// The id the launch is recorded and shown under: the desktop entry's, or the program's file
    /// name.
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Desktop { id, .. } => id.clone(),
            Self::Program { path } => path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        }
    }
}

/// What a launch starts. A Tilo package's command is only ever built by the daemon, from the
/// package's signed manifest; a host launch names the entry its executor resolves. Neither turns
/// into the other: a package asked for as a host app is refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaunchKind {
    Package,
    Host { kind: HostKind, entry: HostEntry },
}

/// A launch the shell asks the daemon for ([`ShellCall::Launch`]): the daemon checks the app is
/// installed, the workspace is served here and the kind may launch, records the launch and only
/// then orders the shell to start its process ([`LaunchOrder`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchAsk {
    /// The app: a package id, or the host entry's id.
    pub app: String,
    /// The workspace it runs in; empty for a host app launched into none.
    pub workspace: String,
    pub kind: LaunchKind,
    /// The record or link the app's window opens (`tilo://…`), for a package.
    pub open: Option<String>,
    /// The window opens the record for whoever works it, with none of the person's sign-ins.
    pub visitor: bool,
    /// The tab the shell holds for the launch's window.
    pub window: Option<u64>,
    /// The app runs with no window, for an action that shows nothing.
    pub windowless: bool,
    /// The workspace agent the app acts as; `None` for the person.
    pub member: Option<String>,
}

impl LaunchAsk {
    /// The person's launch of the package `app` in `workspace`, showing a window.
    #[must_use]
    pub fn package(app: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            workspace: workspace.into(),
            kind: LaunchKind::Package,
            open: None,
            visitor: false,
            window: None,
            windowless: false,
            member: None,
        }
    }

    /// The person's launch of the machine's own application `entry`, of `kind`, in `workspace`.
    #[must_use]
    pub fn host(kind: HostKind, entry: HostEntry, workspace: impl Into<String>) -> Self {
        Self {
            app: entry.id(),
            workspace: workspace.into(),
            kind: LaunchKind::Host { kind, entry },
            open: None,
            visitor: false,
            window: None,
            windowless: false,
            member: None,
        }
    }
}

/// A Tilo package's launch as the daemon built it from the package's signed manifest: what
/// `tilo-launcher run` is started with. The shell adds what is the person's own: the display, its
/// services, its environment, and the grants the person gave the app (the workspace's, the
/// devices they allowed it, the files they chose for it and, for Files, their home), which it
/// writes where the launcher reads them, exactly as for a launch it asked for itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageCommand {
    pub manifest: std::path::PathBuf,
    pub payload_root: std::path::PathBuf,
    /// The hash the package was admitted with, which the launcher checks before it runs.
    pub package_hash: String,
    /// The record or link the window opens.
    pub open: Option<String>,
    pub visitor: bool,
}

/// What a [`LaunchOrder`] starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderCommand {
    Package(PackageCommand),
    Host { kind: HostKind, entry: HostEntry },
}

/// A launch the daemon decided and recorded, which this shell starts as its person: one the shell
/// asked for itself ([`ShellReply::Launching`]), or one the daemon ordered of it for another
/// caller ([`ShellReply::Orders`]) because it runs as the device's own account and cannot start a
/// process as the person or reach their display. The shell starts it held, reports it
/// ([`ShellCall::Started`]), and lets it go on once the daemon bound the process to the ticket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchOrder {
    pub ticket: u64,
    pub app: String,
    /// The name the app is shown by.
    pub title: String,
    pub workspace: String,
    pub window: Option<u64>,
    pub windowless: bool,
    pub command: OrderCommand,
}

/// What became of a launch the daemon decided, as the shell that started its process saw it
/// ([`ShellCall::LaunchEvent`]): the daemon keeps it on the launch's record, which is what the
/// device answers about the launch from then on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaunchEvent {
    /// A phase of the launch's start was reached (`planned`, `spawned`, `connected`,
    /// `window`), `after_ms` after it was asked for.
    Phase { phase: String, after_ms: u64 },
    /// A window of the launch was shown, its process `pid` as the shell's display names it: the
    /// process a connection naming the window comes from.
    Window { pid: u32 },
    /// The memory tier the launch runs in now.
    Tier(MemoryTier),
    /// The launch's process ended.
    Ended(LaunchEnd),
}

/// The memory tier a launch's cgroup is in: what it is served and what it gives back first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryTier {
    /// Still opening: served before everything else.
    Startup,
    /// A window of it is shown.
    Foreground,
    /// Nothing of it is shown: it gives memory back first.
    Background,
}

/// How a launch's process ended ([`LaunchEvent::Ended`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchEnd {
    /// The exit code, when it exited.
    pub code: Option<i32>,
    /// The signal that ended it, when one did.
    pub signal: Option<i32>,
    /// Why memory ended it, when memory did.
    pub memory: Option<MemoryKill>,
    /// The last lines it wrote to its standard error.
    pub stderr: Vec<String>,
    /// The person asked it to end.
    pub asked_to_end: bool,
}

/// Why memory ended a launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryKill {
    /// The kernel's out-of-memory killer.
    Kernel,
    /// The session's memory guard, which ends the one app growing out of control.
    Runaway,
}

/// A process of the person's that runs a launch the shell did not ask the daemon for in this link
/// ([`ShellCall::Adopt`]): a kept app it takes back after the desktop restarted in place, or a
/// window the shell draws itself for an app with no process of its own. The daemon decides it as
/// any launch, binds `process` to it and answers its ticket; a process it already holds a launch
/// of for the same app, workspace and window keeps that launch's ticket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adopt {
    pub app: String,
    pub workspace: String,
    pub kind: LaunchKind,
    pub window: Option<u64>,
    pub windowless: bool,
    /// The process's id as the shell sees it.
    pub pid: u32,
    /// The number of the descriptor pinning it (a pidfd).
    pub process: u32,
}

/// The window a press on the system's own surfaces (the shell's palette, menus and shortcuts)
/// stands for in [`ShellCall::Press`]: no app's window, so no app's call can name it, and only the
/// person's own calls on the shell's link prove themselves with it.
pub const SHELL_SURFACES: u64 = u64::MAX;

/// The window a press on the shell's own surfaces stands for in a question it asks: the shell's
/// screen, no app's window. The whole shell draws it, so the question stands beside the control
/// the person pressed wherever the shell is (`so.tilo.shell`'s `asked_over_shell` reads it).
pub const SHELL_SCREEN: &str = "screen";

/// Where on the shell's screen the person pressed an action: the window's tab key and the
/// resource the action is on, as the shell names them. Only the window key and the resource
/// travel: the daemon fills the device with itself and the app with the action's own, so a shell
/// cannot claim another app's window or another device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellPress {
    /// The window's tab key, as the session publishes it (`device.Window.key`), or
    /// [`SHELL_SCREEN`] for the shell's own surfaces.
    pub instance: String,
    /// The resource the action acts on.
    pub resource: String,
    /// Where in that window the person pressed, in the window's logical pixels, as the shell
    /// drew the control: what the question stands at. `None` where the shell knows no point.
    /// Always written: the link's encoding is positional.
    pub at: Option<tilo_actions::remote::asked::PressPoint>,
    /// The control the press answered, where the shell knew it: the question stands beside it,
    /// never over it. Always written: the link's encoding is positional.
    pub control: Option<tilo_actions::remote::asked::PressRect>,
}

/// What a shell asks of the device's documents and actions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Write `changes` to `app`'s record `record` of `workspace`, signed and sealed as the device:
    /// founding the record from them when the workspace holds none of it. `kind` names the
    /// record's kind, where the writer knows it.
    Write {
        workspace: String,
        app: String,
        kind: Option<String>,
        record: String,
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
    /// The person pressed in `window` on this shell's screen, delivered with the input serial
    /// `serial` the compositor issued to that window while it had focus and did not send
    /// elsewhere: what lets a call an app's window makes that names `serial` run as the person,
    /// once. Only the shell connection, which the compositor told, may send one, and a shell
    /// names only the device's own person's press.
    Press {
        /// The window the press reached, by its tab id, as `device.Window.key` names it, or
        /// [`SHELL_SURFACES`] for the system's own.
        window: u64,
        /// The input serial the compositor issued to that window with the press.
        serial: u32,
        /// Where in the window the press was, for a pointer's or a finger's: where a question
        /// the call it proves raises stands. Always written: the wire reads by position.
        at: Option<tilo_actions::remote::asked::PressPoint>,
    },
    /// The documents the device took a write to since this connection last asked, waited for
    /// until there is one: the device's own writes, its apps' and every other device's alike.
    Writes,
    /// Run a resource store command (`collab_types::store::wire::Command` JSON) on `app`'s
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
    /// device's: `redacted` is the redacted `tilo_actions::receipt::WorkspaceEffectReceipt` JSON.
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
    /// Admit one hand-over to the device's tilolang processes: answered with the daemon's native
    /// socket and the one-use secret the hand-over is made with there ([`SHELL_PROCESSES`]).
    Processes,
    /// What the person says of themselves in `workspace` now, as their shell knows it
    /// ([`ShellWord`]), or nothing any more (`None`), and the runs this device executes there,
    /// each said as a member of its own: the device's daemon says it as the device's one word in
    /// the workspace's live session, beside what its apps say.
    Say {
        workspace: String,
        word: Option<Box<ShellWord>>,
        agents: Vec<SpokenFor>,
    },
    /// The person joins the call `call` names in `workspace`: answered with its channel and the
    /// generation of its rule once the daemon holds its media keys
    /// ([`ShellReply::Joined`]), or with why the call does not take this device.
    Join {
        workspace: String,
        call: ShellInCall,
    },
    /// The person leaves the call they are in in `workspace`.
    Leave { workspace: String },
    /// Every change of `workspace`'s members its governance names, oldest first: what a feed of
    /// the workspace's changes lists as changes of its members.
    MemberChanges { workspace: String },
    /// The key of an agent the person admitted at the shell: made, certified and kept by the
    /// device, which signs as it, so no key of an agent's is ever the shell's.
    AgentKey(AgentKeyCall),
    /// Launch an app as the person: answered with the order the shell starts it by once the
    /// daemon decided and recorded it ([`ShellReply::Launching`]), or why it refused, before any
    /// process exists.
    Launch(LaunchAsk),
    /// The launches the daemon ordered this shell to start since it last asked, waited for until
    /// there is one ([`ShellReply::Orders`]).
    Orders,
    /// The process of the ordered launch `ticket` started, held at its launch hold: `process` is
    /// the number of the descriptor pinning it (a pidfd), and `pid` its id as the shell sees it.
    /// Answered once the daemon bound it to the launch ([`ShellReply::Started`]); only then does
    /// the shell let the process go on.
    Started { ticket: u64, pid: u32, process: u32 },
    /// What became of the launch `ticket` this shell started ([`ShellReply::Noted`]).
    LaunchEvent { ticket: u64, event: LaunchEvent },
    /// Bind a process this shell holds to a launch the daemon decides now
    /// ([`ShellReply::Adopted`]).
    Adopt(Adopt),
}

/// What the shell asks of the key of an agent its person admitted ([`ShellCall::AgentKey`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentKeyCall {
    /// The key `agent`, which the person's devices list as `name`, signs with in `workspace`,
    /// certified by the root of `person`, the shell's own person: answered
    /// [`AgentKeyAnswer::Granted`].
    Grant {
        workspace: String,
        agent: String,
        name: String,
        person: [u8; 32],
    },
    /// `request` (a `tilo_actions::remote::InvokeRequest` as JSON) signed in `workspace` with
    /// `key`, a key a [`Self::Grant`] gave: answered [`AgentKeyAnswer::Signed`].
    Sign {
        workspace: String,
        key: [u8; 32],
        request: Vec<u8>,
    },
    /// The revocation the root of `person` signed for `key`, as their devices say now: answered
    /// [`AgentKeyAnswer::Revocation`].
    Revocation { person: [u8; 32], key: [u8; 32] },
}

/// The answer to an [`AgentKeyCall`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentKeyAnswer {
    /// The agent's public key.
    Granted([u8; 32]),
    /// The request, signed, as JSON.
    Signed(Vec<u8>),
    /// The revocation (a `tilo_user_identity::RevocationEntry` as JSON), or none while the key
    /// acts.
    Revocation(Option<Vec<u8>>),
}

/// What the person says of themselves in one workspace, as their shell knows it: what the
/// device's daemon says on the workspace's live session as the device's word
/// (`tilo_live::presence::Own`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ShellWord {
    /// Their name, as everyone sees it.
    pub label: String,
    /// What this device calls itself.
    pub device: String,
    /// The person this device belongs to, their root key as hex.
    pub user: Option<String>,
    /// When they last changed their profile, in Unix milliseconds; `0` for none known.
    pub profile_ms: u64,
    /// What they are doing, and whether the device read it off their steps.
    pub status: Option<(String, bool)>,
    /// How available they say they are.
    pub availability: collab_types::presence::Availability,
    /// The call they are in here.
    pub call: Option<ShellInCall>,
    /// Whose view they follow, by that member's publisher.
    pub following: Option<String>,
    /// The last wave they sent here.
    pub wave: Option<collab_types::presence::ShownWave>,
    /// Their pointer over a tile: its tab and the point in the tile's pixels.
    pub pointer: Option<(u64, f32, f32)>,
    /// Their screen, as a shell draws it.
    pub shell: Option<Box<collab_types::presence::ShellPresence>>,
    /// What each app the shell runs itself last said of them, under each of its keys.
    pub apps: Vec<collab_types::presence::Presence>,
}

/// A call the person is in, as their presence says it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "camera, screen, muted and deafened are each their own flag a person shows in a call"
)]
pub struct ShellInCall {
    pub call: collab_types::presence::CallPresence,
    pub camera: bool,
    pub screen: bool,
    pub muted: bool,
    pub deafened: bool,
}

/// One run this device executes, said as a member of its own in a workspace: its session, the
/// member it is, and its face.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpokenFor {
    pub session: String,
    pub member: String,
    pub face: collab_types::presence::ShellPresence,
}

/// What the name of every provider a shell serves begins with, besides the session's own actions
/// ([`tilo_actions::remote::session_actions::PROVIDER`]).
pub const SHELL_PROVIDERS: &str = "session.";

/// The providers a shell serves under the ids apps have always called them by, each with the
/// application the dispatch admits its calls under (the one whose window offers them). Each needs
/// the person's seat, so the desktop session answers it: the activity the shell shows, the person's
/// settings and model keys on this computer, the account this computer is signed in to, the
/// machines it plans, and the workspace's events, which read the shortcuts this session saves.
pub const SEAT_PROVIDERS: &[(&str, &str)] = {
    use tilo_actions::remote::{
        MACHINES_PROVIDER, MODELS_PROVIDER, SERVER_ACCOUNT_PROVIDER, WORKSPACE_EVENTS_PROVIDER,
        settings_resources, workspace_activity,
    };
    const SETTINGS: &str = settings_resources::APP;
    &[
        (workspace_activity::PROVIDER, workspace_activity::APP_ID),
        (MACHINES_PROVIDER, "so.tilo.imager"),
        (MODELS_PROVIDER, SETTINGS),
        (SERVER_ACCOUNT_PROVIDER, SETTINGS),
        (settings_resources::PROVIDER, SETTINGS),
        (WORKSPACE_EVENTS_PROVIDER, WORKSPACE_EVENTS_PROVIDER),
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
    /// A [`ShellCall::Write`]: the workspace holds it, or why it took nothing of it, as
    /// `tilo_peer_state::owner::WriteRefusal` JSON.
    Written(Result<(), String>),
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
    /// A [`ShellCall::Press`]: the press is recorded, and proves one call.
    Pressed,
    /// A [`ShellCall::Say`]: the daemon says it from now on.
    Said,
    /// A [`ShellCall::Join`]: the call's channel and the generation of its rule, or why the call
    /// does not take this device. A [`ShellCall::Leave`] is answered `Ok(None)`.
    Joined(Result<Option<(String, u64)>, String>),
    /// A [`ShellCall::Resources`]: the store's `collab_types::store::wire::Reply` as JSON, or why
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
    /// A [`ShellCall::Processes`]: the daemon's native socket the hand-over is made on
    /// ([`SHELL_PROCESSES`]), and the secret it is made with.
    Processes {
        socket: std::path::PathBuf,
        secret: [u8; SHELL_SECRET_BYTES],
    },
    /// A [`ShellCall::MemberChanges`], or why the device holds none.
    MemberChanges(Result<Vec<ShellMemberChange>, String>),
    /// A [`ShellCall::AgentKey`], or why the device refused it.
    AgentKey(Result<AgentKeyAnswer, String>),
    /// A [`ShellCall::Launch`]: the order the shell starts the launch by, or why the daemon
    /// refused it.
    Launching(Result<LaunchOrder, String>),
    /// A [`ShellCall::Orders`].
    Orders(Vec<LaunchOrder>),
    /// A [`ShellCall::Started`]: the process is the launch's, or why the daemon refused it (the
    /// shell then ends it).
    Started(Result<(), String>),
    /// A [`ShellCall::LaunchEvent`]: kept, or why the daemon knows no such launch.
    Noted(Result<(), String>),
    /// A [`ShellCall::Adopt`]: the launch's ticket, or why the daemon refused it.
    Adopted(Result<u64, String>),
}

/// One change of a workspace's members as its governance names it: the change's hash, the
/// message naming its action and who decided it, the time its commit carries (Unix seconds), and
/// the people it is about, as the signed operations it wrote name them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellMemberChange {
    pub change: [u8; 32],
    pub message: String,
    pub time: i64,
    pub subjects: Vec<String>,
}

/// A document the device took a write to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentWritten {
    pub workspace: String,
    pub app: String,
    /// The record's id on the wire, `""` for the app's primary.
    pub record: String,
}
