//! The certified session of one device, over the link to its host.
//!
//! One task owns the links to the user's host and everything that must be
//! serialized with them: drafting and signing this device's writes, the
//! workspaces it takes part in, reconnects. Callers talk to it through the
//! client's cheap [`Link`].
//!
//! - **One link per workspace.** A workspace's records, its governance and its
//!   channels travel on a link of their own
//!   ([`tilo_peer_state::direct::DirectSync`] owning the device's replica of
//!   the workspace), opened over [`Purpose::Records`] and naming the workspace
//!   first. A page takes a workspace's records only as it opens them
//!   ([`DirectSync::when_opened`] and [`DirectSync::open_record`]).
//! - **Optimistic local edits.** A write a page publishes is drafted, sealed
//!   and signed by this device's ownership, kept there, and sent on its
//!   workspace's link. Its host's answer comes back on that link
//!   ([`Answered`]): durable, or refused. A barrier completes once everything
//!   said before it is answered.
//! - **Reconnect replays.** After any disconnect each workspace's link is
//!   dialled again, the records the page opened are opened again, and this
//!   device's ownership is sent again in order. Re-applying a write is
//!   idempotent.
//! - **Fail closed.** An expired certificate, or a host that keeps refusing the
//!   link (a revoked device), ends in [`TransportPhase::Failed`] instead of
//!   retrying forever; only [`Cmd::Reconnect`] retries.
//!
//! What this session no longer carries itself: the host's word on presence and
//! calls (the live session, [`crate::session::live_session`]), and workspace
//! administration and entries, which are the host's own actions.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use crate::network::Kept;
use crate::{
    CallPicture, CallShown, Closed, Cmd, Event, Failure, FailureKind, Link, Outbox, PageShown,
    PeerStatus, Reports, TransportPhase,
};
use tilo_session_actions::Heard;
use tilo_user_identity::{DeviceCert, RevocationList};

use tokio::sync::mpsc;

use crate::link::Held;
use crate::membership_check::PeerStore;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::direct::{self, Answered, DirectSync, Kept as Answer};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_peer_state::owner::{NativeCommittedView, NativeWorkspaceOwner};
use tilo_peer_state::transport::Transport;
use tilo_server_client::LinkError;
use tilo_server_client::link::Purpose;

mod agent_session;
mod connect;
mod host_voice;
mod live_session;

use agent_session::AgentSessionCompletion;
use connect::Dial;
use tilo_server_protocol::WorkspaceId;

/// Longest wait between attempts at a link a host refused or lost: a host that
/// comes back is reached again within this (and its jitter) of being back.
const MAX_BACKOFF: Duration = Duration::from_secs(8);
/// How often overdue channel access requests are sent again, and a link that is
/// down is tried again.
const CHANNEL_TICK: Duration = Duration::from_secs(1);
/// The most commands the session takes in one pass before it settles its links.
const MAX_TAKEN_AT_ONCE: usize = 256;
/// Consecutive refusals with no reason after which the host is taken to no
/// longer admit this device rather than to be unreachable. A refusal that says
/// why decides at once.
const REFUSALS_BEFORE_FAILING: u32 = 3;
/// The host does not admit this device's person (`not-admitted`): it never
/// knew them, forgot them (its state was reset, or it pruned them) or a
/// workspace took them out. The page joins again where the workspace's entry
/// lets it.
const NOT_ADMITTED: (&str, &str) = (
    "not-admitted",
    "This workspace’s host no longer has you as a member.",
);

/// The host deleted a workspace this session held (`workspace-deleted`): its
/// owner deleted it, so it never comes back and the device forgets it.
pub(crate) const WORKSPACE_DELETED: (&str, &str) = (
    "workspace-deleted",
    "This workspace was deleted by its owner.",
);

/// How long a connected session waits for the host to admit its workspaces
/// before it reports `admission-timeout`. Checked on the channel tick.
const ADMISSION_TIMEOUT_MS: u64 = 30_000;
/// How long after this device's replica refused a compaction it is behind its
/// workspace is asked for the document again, the first time; each time after,
/// it waits twice as long, at most [`BEHIND_ASKED_AT_MOST_EVERY_MS`]. Checked
/// on the channel tick.
const BEHIND_ASKED_AFTER_MS: u64 = 15_000;
/// The longest a document this device is behind in waits before it is asked for
/// again.
const BEHIND_ASKED_AT_MOST_EVERY_MS: u64 = 300_000;

/// Where to reach the user's host: the iroh endpoint it serves its peers'
/// sessions on ([`tilo_server_client::ServerAddress`]).
pub use tilo_server_client::ServerAddress;

/// Where to connect and as whom.
#[derive(Clone)]
pub struct PeerConfig {
    /// The user's host (`tilo-server`).
    pub host: ServerAddress,
    /// This device's certificate.
    pub cert: DeviceCert,
    /// The host certificate pinned at certification.
    pub server_cert: DeviceCert,
    /// Root-signed revocations this device knows about.
    pub revocations: RevocationList,
    /// The workspace this device enters through its entry on its first
    /// session, when the host has not admitted it yet
    /// ([`tilo_server_protocol::entering`]); which the host's `enter` action
    /// redeems, so a session carries it no further.
    pub entering: Option<tilo_server_protocol::entering::Entering>,
}

impl PeerConfig {
    /// A member device's session with `host`, whose certificate is
    /// `server_cert`: no revocations of its own to announce, no entry to ask for.
    #[must_use]
    pub fn member(host: ServerAddress, cert: DeviceCert, server_cert: DeviceCert) -> Self {
        Self {
            host,
            cert,
            server_cert,
            revocations: RevocationList::new(),
            entering: None,
        }
    }
}

/// The workspace id a session asks the host's own operations under: none of its
/// workspaces, the host itself (creating, listing and entering the person's
/// workspaces). Its answers come back from the host's device under the same id.
pub const HOST_ITSELF: WorkspaceId = WorkspaceId::nil();

/// What a session's task is told: the client's own commands, and what its links
/// heard.
enum Command {
    /// One of the client's commands.
    Client(Cmd),
    /// What asking the host for `workspace_id`'s live session, and joining it,
    /// came to: the live session itself, except on wasm, where the page's carrier
    /// cannot travel in a command and waits in [`live_session::arrived`].
    #[cfg(not(target_arch = "wasm32"))]
    LiveSessionJoined(
        WorkspaceId,
        Result<Arc<tilo_live::live_session::LiveSession>, live_session::Failed>,
    ),
    #[cfg(target_arch = "wasm32")]
    LiveSessionJoined(WorkspaceId),
    /// The live session of `workspace_id` heard something: drain it.
    LiveSessionHeard(WorkspaceId),
    /// The live session of `workspace_id` ended: the relay closed it, or the live session
    /// no longer admits what it reads.
    LiveSessionEnded(WorkspaceId),
    /// The first consumer that reads the members' presence came, or the last one went.
    PresenceReaders,
    /// `workspace_id`'s link brought a frame this device's replica took in.
    Heard(WorkspaceId, Box<direct::Heard>),
    /// `workspace_id`'s link carried the host's answer to one of this device's writes.
    Answered(WorkspaceId, Answered),
    /// `workspace_id`'s link ended, with why the host gave.
    LinkEnded(WorkspaceId, Option<tilo_server_protocol::refusal::Refusal>),
    /// A record this page asked its workspace for was answered: all the host
    /// keeps of it is here, or it had none.
    RecordOpened(WorkspaceId, [u8; 32], bool),
}

/// What sealing or opening a live frame came to.
enum Opening {
    Opened(Vec<u8>),
    /// This device holds no keys for the record yet: the digest it carries.
    NoKeys([u8; 32]),
    /// This device holds no keys for the record a frame is to be sealed for.
    NoKeysOf(tilo_server_protocol::live::LiveResource),
    /// Malformed, or does not open.
    Dropped,
}
/// Where a barrier's outcome goes ([`Cmd::Barrier`]).
pub(crate) type Completion = crate::Answer<Result<(), String>>;
/// Where a live frame sealed with its record's channel keys goes, or `None` when this device holds
/// none ([`Cmd::SealLive`]).
pub(crate) type LiveCompletion = crate::Answer<Option<Vec<u8>>>;

#[derive(Debug, PartialEq, Eq)]
enum Admission {
    Admitted,
    Waiting,
    Overdue,
}

/// Where the joined workspaces' admission stands `now_ms`, for a link dialled at
/// `since_ms`.
fn admission_state(status: &PeerStatus, since_ms: Option<u64>, now_ms: u64) -> Admission {
    if status.workspaces.values().all(|ready| *ready) {
        return Admission::Admitted;
    }
    match since_ms {
        Some(since) if now_ms.saturating_sub(since) >= ADMISSION_TIMEOUT_MS => Admission::Overdue,
        _ => Admission::Waiting,
    }
}

/// Say that a session without its server `since` then reached it again, after `attempts`.
fn reached_again(since: Option<tilo_clock::time::Instant>, attempts: u32) {
    if let Some(since) = since {
        tracing::info!(
            attempts,
            away_ms = tilo_clock::now()
                .saturating_duration_since(since)
                .as_millis(),
            "the session reached its server again"
        );
    }
}

/// Start a certified server session with `config`'s host. It connects in the background; observe it
/// with [`Link::status`] and [`Link::on_change`]. It ends with its last link.
pub fn start_session(
    config: PeerConfig,
    signer: Arc<dyn tilo_server_client::DeviceSigner>,
    store: Arc<dyn PeerStore>,
) -> Link {
    let (task, link, receiver) = session_task(config, signer, store, None);
    n0_future::task::spawn(task.run(receiver));
    link
}

/// What a device's own session lends a session it opens to another host, so the device stays one
/// device there.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct Lent {
    /// The device's own key service: the session reads and asks with it, and settles no keys of
    /// its own. A device holds one place in a workspace's key group, and a second service under
    /// the same device key would take that place from the first each time it asked to be admitted.
    /// Lent none, the session holds no keys and asks for none: the device's member key in each
    /// group is its own session's.
    pub keys: Option<tilo_workspace_keys::SharedKeys>,
    /// The device service's member copies, by workspace: the links this session reads and writes
    /// through, which the device's own service dials and settles its groups' keys on. The session
    /// dials none of its own.
    pub links: Arc<dyn MemberLinks>,
    /// Where the endpoint the device runs under its key lands once bound: the session dials its
    /// live sessions from it. A server keeps one connection per endpoint id, so a second endpoint
    /// under the same key cut the first off; a session opened before the endpoint is bound waits
    /// for it and binds none in its place.
    pub endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
    /// The session is the device's own, not one beside it: it joins the workspaces' live
    /// sessions on the lent endpoint. A device that runs a session of its own beside this one
    /// lends none, as a second live session under its key would shadow that one's.
    pub own: bool,
}

/// What a session lent by its device's own session reads and writes through, on every target: the
/// device's key service and member copies, and the endpoint its live sessions dial from.
#[derive(Clone)]
pub(crate) struct LentParts {
    keys: Option<tilo_workspace_keys::SharedKeys>,
    links: Arc<dyn MemberLinks>,
    #[cfg(not(target_arch = "wasm32"))]
    endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
    #[cfg(not(target_arch = "wasm32"))]
    own: bool,
}

/// Where a session lent the device's member copies reads and writes a workspace: the link the
/// device's own service keeps for it, once it is up.
pub trait MemberLinks: collab_types::signer::MaybeSendSync {
    /// The member copy's link of `workspace`, while it is up.
    fn link(&self, workspace: WorkspaceId) -> Option<DirectSync>;
}

/// [`start_session`] for a session of a device that lends it what its own session runs
/// ([`Lent`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn start_session_lent(
    config: PeerConfig,
    signer: Arc<dyn tilo_server_client::DeviceSigner>,
    store: Arc<dyn PeerStore>,
    lent: Lent,
) -> Link {
    let parts = LentParts {
        keys: lent.keys,
        links: lent.links,
        endpoint: lent.endpoint,
        own: lent.own,
    };
    let (mut task, link, receiver) = session_task(config, signer, store, Some(parts.clone()));
    if let Some(endpoint) = parts.endpoint.clone() {
        task.dial.lend(endpoint, parts.own);
    }
    n0_future::task::spawn(task.run(receiver));
    link
}

/// The session [`start_session`] runs, the link to it and its command queue,
/// before it runs: over `keys` when the device's own key service settles them.
fn session_task(
    config: PeerConfig,
    signer: Arc<dyn tilo_server_client::DeviceSigner>,
    store: Arc<dyn PeerStore>,
    lent: Option<LentParts>,
) -> (Task, Link, mpsc::UnboundedReceiver<Command>) {
    let (commands, receiver) = mpsc::unbounded_channel();
    let reports = Reports::connecting(config.cert.device_public_key);
    let readers = commands.downgrade();
    reports.on_presence_readers(Arc::new(move || {
        if let Some(commands) = readers.upgrade() {
            let _ = commands.send(Command::PresenceReaders);
        }
    }));
    let record = store.load().unwrap_or_default();
    let vault = store.channel_vault().unwrap_or_else(rand::random);
    let owner = crate::membership_check::open_owner(&config.cert, Arc::clone(&store));
    let workspaces = crate::membership_check::held_workspaces(&owner);
    let key_state = crate::membership_check::StoredKeys::load(store.as_ref());
    // The transport this session hands the groups' governance it hears over: a session that
    // settles its own keys judges them itself, and one lent the device's key service hands
    // them to that service, which judges them as it judges the frames its links bring. A
    // session lent no keys holds none and asks for none.
    let lent_keys = lent.as_ref().and_then(|lent| lent.keys.clone());
    let holds_no_keys = lent.is_some() && lent_keys.is_none();
    let governance = (!holds_no_keys).then(ChannelLinks::default);
    let workspace_keys = lent_keys.unwrap_or_else(|| {
        let checks = Arc::new(tilo_workspace_keys::SealChecks::default());
        tilo_workspace_keys::SharedKeys::new(if lent.is_some() {
            keyless_keys(&owner, &checks)
        } else {
            open_keys(&owner, &key_state, vault, &signer, &checks)
        })
    });
    let engine = session_engine(&config, &signer, &owner);
    engine.keys().set(Some(workspace_keys.clone()));
    let held = Held::default();
    {
        let mut waiting = held.lock();
        for (route, mutation) in key_state.held() {
            waiting.entry(route).or_default().push(mutation);
        }
    }
    #[cfg(feature = "toolkit")]
    crate::surfaces::decide_with_kept_surfaces();
    let task = Task {
        engine: Arc::clone(&engine),
        held: held.clone(),
        owner,
        commands: commands.downgrade(),
        config,
        signer,
        store,
        dial: Dial::for_page(&vault),
        reports: Arc::clone(&reports),
        workspaces,
        emptied: false,
        live_sessions_due: BTreeSet::new(),
        kept: BTreeMap::new(),
        live_sessions: live_session::LiveSessions::default(),
        record,
        workspace_keys,
        governance,
        key_state,
        route_keys: BTreeMap::new(),
        new_keys: Vec::new(),
        opens_as_before: std::collections::HashSet::new(),
        keys_generation: 0,
        refused: (0, String::new()),
        surface_wants: neighbour_wire::content::ContentWants::default(),
        held_live: crate::held_live::HeldLive::new(),
        outbound_due: false,
        held_since: BTreeMap::new(),
        kept_why: BTreeMap::new(),
        pending_signed: BTreeMap::new(),
        owed_since: BTreeMap::new(),
        answered: Vec::new(),
        let_go: BTreeSet::new(),
        held_inbound: BTreeMap::new(),
        behind: BTreeMap::new(),
        links: BTreeMap::new(),
        dialing: BTreeSet::new(),
        sent: BTreeSet::new(),
        actions: None,
        fetch_grant_asked: false,
        lent,
        barriers: Vec::new(),
        awaiting_surface: BTreeMap::new(),
        surfaces_arrived: false,
        replays: BTreeSet::new(),
        held_barriers: Vec::new(),
        unheard_refusals: BTreeMap::new(),
        admission_since: None,
        workspaces_pending: BTreeMap::new(),
        agent_sessions_pending: BTreeMap::new(),
        agent_session: None,
        opened_records: BTreeMap::new(),
        opened_routes: BTreeSet::new(),
        begun: BTreeSet::new(),
        unadmitted: BTreeMap::new(),
        admitted_here: BTreeSet::new(),
    };
    // The link's own queue is there before the session says anything.
    let link = Link::new(
        Arc::new(Commands::holding(commands, held.clone())),
        engine,
        held,
        reports,
    );
    (task, link, receiver)
}

/// The engine a session writes its workspaces through: this device's identity, kept beside
/// the signer, with the founders a workspace's session runs.
fn session_engine(
    config: &PeerConfig,
    signer: &Arc<dyn tilo_server_client::DeviceSigner>,
    owner: &NativeWorkspaceOwner,
) -> Arc<crate::engine::Engine> {
    Arc::new(crate::engine::Engine::holding(
        owner.clone(),
        Arc::default(),
        tilo_identity::identity_store::DeviceIdentity::kept(
            config.cert.clone(),
            Arc::clone(signer),
        ),
    ))
}

/// The session's own sender: what its links say the client's commands through.
struct Commands {
    commands: mpsc::UnboundedSender<Command>,
    /// The writes it holds for their record's keys, shared with the link: the same ones the
    /// session drafts and admits.
    held: Held,
}

impl std::fmt::Debug for Commands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("server session")
    }
}

impl Commands {
    /// The transport of a session whose queue is `commands`, holding the writes `held` keeps for
    /// their record's keys.
    fn holding(commands: mpsc::UnboundedSender<Command>, held: Held) -> Self {
        Self { commands, held }
    }
}

/// The session carries the client's commands: what a member says to its workspaces goes
/// through the links it keeps with its host, and a write it holds for its record's
/// keys is drafted and admitted by the session whichever handle of it made the write.
impl Outbox for Commands {
    fn send(&self, command: Cmd) -> Result<(), Closed> {
        self.commands
            .send(Command::Client(command))
            .map_err(|_| Closed)
    }

    fn write(
        &self,
        route: DocumentRoute,
        mutation: &collab_types::DocumentMutation,
    ) -> Result<(), String> {
        self.held.hold(route, mutation, self)
    }
}

enum Exit {
    Closed,
    Reconnect,
    Lost(String),
}

/// The host's answer to a write held, waiting to retire from it.
struct AnsweredWrite {
    workspace: WorkspaceId,
    /// The hash of the write's frame, as the host named it.
    id: [u8; 32],
    /// The frame, which leaves this device's record once it is answered.
    frame: Vec<u8>,
    /// Durable at this order, or refused, and why.
    outcome: Result<u64, tilo_server_protocol::refusal::Refusal>,
    /// What a refusal withdraws from the replica.
    withdrawn: Option<Event>,
}

/// One workspace's link to its host, while it runs.
struct WorkspaceLink {
    /// The device's replica of the workspace, which the link admits into and sends from.
    sync: DirectSync,
    /// The link under it, to close it when the session ends.
    transport: Option<Arc<dyn Transport>>,
}

struct Task {
    /// This session's own command queue, for answers it waits on in the
    /// background; weak, so the session still ends with its last handle.
    commands: mpsc::WeakUnboundedSender<Command>,
    config: PeerConfig,
    signer: Arc<dyn tilo_server_client::DeviceSigner>,
    store: Arc<dyn PeerStore>,
    /// Where this session dials its host from, kept across reconnects.
    dial: Dial,
    /// What this session reports to its links.
    reports: Arc<Reports>,
    workspaces: BTreeSet<WorkspaceId>,
    /// Whether the host deleted the last workspace this session held: it has
    /// nothing left to be admitted to, so it stays done instead of dialling a
    /// host that only ends the link again.
    emptied: bool,
    /// The joined workspaces whose live session may have to be joined, followed
    /// or left ([`Self::settle_live_sessions`]).
    live_sessions_due: BTreeSet<WorkspaceId>,
    /// What this device keeps of each workspace's content ([`Cmd::KeepContent`]).
    kept: BTreeMap<WorkspaceId, Arc<dyn crate::content::Kept>>,
    /// This page's own live session per joined workspace
    /// ([`crate::session::live_session`]).
    live_sessions: live_session::LiveSessions,
    record: crate::membership_check::PeerEntry,
    /// The device's replica of every workspace it holds, which the links own.
    owner: NativeWorkspaceOwner,
    /// This device's side of the workspaces' encrypted channels: the engine every member runs,
    /// here as a reader that asks for access and follows the members' commits.
    workspace_keys: tilo_workspace_keys::SharedKeys,
    /// What the channels say to and hear from the workspaces' links: the frames a link to this
    /// device would bring, taken into [`Self::workspace_keys`] exactly as a link frame is.
    /// `None` for a session lent no keys, which holds none and asks for none.
    governance: Option<ChannelLinks>,
    /// The channels' state and the writes held for their keys, in the store.
    key_state: crate::membership_check::StoredKeys,
    /// Every route this session opened, and the keys it holds for it.
    route_keys: BTreeMap<DocumentRoute, RouteKeys>,
    /// Routes whose keys arrived since the session last acted on them.
    new_keys: Vec<DocumentRoute>,
    /// Those of [`Self::new_keys`] whose sealed history the keys held before already opened
    /// whole: their records did not land.
    opens_as_before: std::collections::HashSet<DocumentRoute>,
    /// The channels' generation the keys were last read at.
    keys_generation: u64,
    /// Documents refused, with the last reason, for the diagnostics.
    refused: (u64, String),
    /// App surfaces this page asked its workspaces for, by the route whose
    /// writes named them: they arrive as pieces of record content.
    surface_wants: neighbour_wire::content::ContentWants<()>,
    /// Live frames waiting for their record's keys.
    held_live: crate::held_live::HeldLive,
    /// This device's writes its session has not drafted and admitted yet, shared with its links:
    /// each waits here for its record's keys, or for the record it changes, and readers count it.
    held: Held,
    /// Writes were published, or the host answered writes held, since held writes were
    /// last committed: what waits may commit ([`Self::commit_held`]).
    outbound_due: bool,
    /// Since when each document holds writes of this device that are not committed to its
    /// replica: how long they have waited, for whoever asks ([`Self::keeps`]).
    held_since: BTreeMap<DocumentRoute, tilo_clock::time::Instant>,
    /// What the replica last said a document's held writes wait for, where it named something.
    kept_why: BTreeMap<DocumentRoute, crate::Waits>,
    /// Signed writes of this device whose publication waits for what they need: their record
    /// here, or a signature.
    pending_signed: BTreeMap<DocumentRoute, Vec<collab_types::SignedOp>>,
    /// Since when each document has committed writes of this device its holder has not answered.
    owed_since: BTreeMap<DocumentRoute, tilo_clock::time::Instant>,
    /// Records of writes the replica let go undecided, its queue of undecided writes full: the
    /// host replays them once what they waited for landed.
    let_go: BTreeSet<DocumentRoute>,
    /// What the host answered of the writes this session sent, retired from ownership together.
    answered: Vec<AnsweredWrite>,
    /// The records whose last write the replica did not take in, and why.
    held_inbound: BTreeMap<DocumentRoute, String>,
    /// The documents a compaction of which this device's replica refused: each is asked of its
    /// workspace again ([`Self::ask_behind_again`]) until a compaction of it is taken.
    behind: BTreeMap<DocumentRoute, Behind>,
    /// Each joined workspace's link, while it is up.
    links: BTreeMap<WorkspaceId, WorkspaceLink>,
    /// The workspaces whose link is being dialled now.
    dialing: BTreeSet<WorkspaceId>,
    /// The ownership entries sent on the current link of each workspace: a new link sends
    /// everything again.
    sent: BTreeSet<(WorkspaceId, [u8; 32])>,
    /// The link this session asks its host's action service on, once it is up.
    actions: Option<tilo_server_client::link::Link>,
    /// Whether this session asked its host for its fetch proxy token: asked once, in a workspace
    /// it is in, and the answer kept for [`Link::fetch_proxy_session`].
    fetch_grant_asked: bool,
    /// The device service's member copies, for a session lent them: it dials no link of its own.
    lent: Option<LentParts>,
    /// Each waits for the writes that were in this device's ownership when it was called, and
    /// completes with the host's refusal of any of them ([`crate::session::outbound::Scope`]).
    barriers: Vec<Owed>,
    /// Writes the owner holds for an app surface this page does not have, by that surface.
    awaiting_surface: BTreeMap<[u8; 32], (WorkspaceId, DocumentRoute)>,
    /// Whether an app surface arrived since the session last asked for what waited for one.
    surfaces_arrived: bool,
    /// Records whose writes the replica asked to hear again, replayed by the host.
    replays: BTreeSet<DocumentRoute>,
    /// This device's replicas, as the page's and agents' readers read them.
    engine: Arc<crate::engine::Engine>,
    /// Barriers called while frames were held, with the routes those frames
    /// are held on.
    held_barriers: Vec<HeldBarrier>,
    /// Why the workspace refused writes of each route that no barrier hearing that route waited
    /// for when they were let go.
    unheard_refusals: BTreeMap<DocumentRoute, String>,
    /// When the workspaces not yet admitted were first dialled (ms since the epoch).
    admission_since: Option<u64>,
    /// The workspace administration this session asked its host.
    workspaces_pending: BTreeMap<u64, WorkspacesCompletion>,
    /// The agent runs this session declared, by the id they asked with.
    agent_sessions_pending: BTreeMap<u64, AgentSessionCompletion>,
    /// The agent whose run this session said it is.
    agent_session: Option<String>,
    /// The on-demand records this page opened, by workspace and channel: opened again on every
    /// new link.
    opened_records: BTreeMap<(WorkspaceId, [u8; 32]), DocumentRoute>,
    /// The routes this session was asked to open ([`Cmd::OpenRoute`]).
    opened_routes: BTreeSet<DocumentRoute>,
    /// The workspaces whose link began replaying to this session on this connection.
    begun: BTreeSet<WorkspaceId>,
    /// The records that landed in a workspace whose governance, as this device's replica holds it,
    /// did not admit this device yet.
    unadmitted: BTreeMap<DocumentRoute, Option<[u8; 32]>>,
    /// The workspaces whose governance, as this device's replica holds it, admitted this device
    /// on this session.
    admitted_here: BTreeSet<WorkspaceId>,
}

/// A barrier waiting on what this device owes: the writes still unanswered of those it was
/// asked about, the refusal one of them met, where its outcome goes, what it answers for, and
/// whose refusals it hears.
type Owed = (BTreeSet<[u8; 32]>, Option<String>, Completion, Scope, Hears);

/// A barrier called while writes were held: the routes they are held on, where its outcome goes,
/// what it answers for, and whose refusals it hears.
type HeldBarrier = (BTreeSet<DocumentRoute>, Completion, Scope, Hears);

/// What a barrier answers for: every write of this session, the writes to some workspaces alone
/// ([`crate::Outbox::barrier_of`]), or the writes to some documents alone
/// ([`crate::Outbox::barrier_for`]).
#[derive(Debug)]
pub(super) enum Scope {
    /// Every workspace this session joined.
    Session,
    /// These workspaces alone.
    Workspaces(BTreeSet<WorkspaceId>),
    /// These documents alone, whatever else their workspaces hold back.
    Documents(BTreeSet<DocumentRoute>),
}

/// The writes whose refusal a barrier answers with ([`crate::Outbox::barrier_for`]).
pub(super) type Hears = Option<BTreeSet<DocumentRoute>>;

/// One entry of this device's ownership of a workspace: its frame's hash, its bytes, the channel
/// it was written to and how long the workspace retains it.
type HeldEntry = ([u8; 32], Vec<u8>, [u8; 32], neighbour_wire::Retention);

/// One write this device's record still keeps, as [`Task::outgoing_named`] answers it: the hash
/// of its frame, the frame, and where it was written with the changes it carries, when it names a
/// route.
type NamedOutgoing = Option<([u8; 32], Vec<u8>, Option<(DocumentRoute, Vec<[u8; 32]>)>)>;

/// A document this device is behind in: its replica refused a compaction of it.
struct Behind {
    /// Why, as the replica said it.
    why: String,
    /// How often its workspace was asked for the document again.
    asked: u32,
    /// When it is asked next (ms since the epoch).
    next_ms: u64,
    /// Whether a compaction of it was refused since it was last asked for.
    refused_since: bool,
}

/// Where the host's answer to a workspace administration request goes ([`Cmd::Administer`]).
pub(crate) type WorkspacesCompletion =
    crate::Answer<Result<tilo_server_protocol::workspaces::WorkspacesReply, String>>;
/// Where the host's answer to an entry goes ([`Cmd::Enter`]).
pub(crate) type EnterCompletion =
    crate::Answer<Result<tilo_server_protocol::entering::Entered, LinkError>>;

impl Task {
    fn fail(&self, code: &'static str, message: impl Into<String>) {
        self.fail_on(FailureKind::Session, code, message);
    }

    fn fail_on(&self, kind: FailureKind, code: &'static str, message: impl Into<String>) {
        let message = message.into();
        self.reports.change(|status| {
            status.upsert(Failure {
                code,
                message,
                at_ms: tilo_clock::unix_now_ms(),
                kind,
            });
        });
    }

    fn persist(&self) {
        if let Err(error) = self.store.save(&self.record) {
            self.fail_on(FailureKind::Storage, "storage-transaction-failed", error);
        }
    }

    fn bind_record_identity(&mut self) -> Result<(), &'static str> {
        let same_identity = |saved: &DeviceCert, configured: &DeviceCert| {
            saved.user_public_key == configured.user_public_key
                && saved.device_public_key == configured.device_public_key
        };
        if self
            .record
            .cert
            .as_ref()
            .is_some_and(|saved| !same_identity(saved, &self.config.cert))
            || self
                .record
                .server_cert
                .as_ref()
                .is_some_and(|saved| !same_identity(saved, &self.config.server_cert))
        {
            return Err("The saved peer identity or host does not match this session.");
        }
        self.record.cert = Some(self.config.cert.clone());
        self.record.server_cert = Some(self.config.server_cert.clone());
        self.persist();
        Ok(())
    }

    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        if let Err(message) = self.bind_record_identity() {
            self.fail("certificate-rejected", message);
            self.reports
                .change(|status| status.phase = TransportPhase::Failed);
            return;
        }
        let mut attempt: u32 = 0;
        // Since when the session has been without its server, for the log line that says it is
        // back: a person reading why their device was away sees how long it was.
        let mut lost_at: Option<tilo_clock::time::Instant> = None;
        loop {
            if self.config.cert.is_expired_at(tilo_clock::unix_now_ns()) {
                self.fail("certificate-expired", "This browser's certificate expired.");
                self.reports
                    .change(|status| status.phase = TransportPhase::Failed);
                if !self.wait_for_reconnect(&mut commands).await {
                    return self.closed().await;
                }
                continue;
            }
            self.reports.change(|status| {
                status.phase = if attempt == 0 {
                    TransportPhase::Connecting
                } else {
                    TransportPhase::Reconnecting
                };
                status.next_attempt_at_ms = None;
            });
            // A session closed while it dials stops dialing then: its device may enter again
            // under the same key, and a server keeps one connection per key.
            let closing = self.reports.closed();
            let connected = tokio::select! {
                connected = self.dial_all() => connected,
                () = closing => return self.closed().await,
            };
            match connected {
                Ok(()) => {
                    reached_again(lost_at.take(), attempt);
                    attempt = 0;
                    self.admission_since = Some(tilo_clock::unix_now_ms());
                    self.connected();
                    let exit = self.serve(&mut commands).await;
                    self.requests_lost();
                    match exit {
                        Exit::Closed => return self.closed().await,
                        Exit::Reconnect => {
                            self.drop_links("the session reconnects");
                            continue;
                        }
                        Exit::Lost(reason) => {
                            tracing::warn!(%reason, "the session lost its server; it dials again");
                            lost_at = Some(tilo_clock::now());
                            self.fail("transport-ended", reason);
                        }
                    }
                }
                Err(error) => {
                    let stop = match refusal(&error) {
                        Refusal::Final(code, message) => Some((code, message)),
                        Refusal::Unexplained => {
                            attempt = attempt.saturating_add(1);
                            (attempt >= REFUSALS_BEFORE_FAILING)
                                .then(|| (NOT_ADMITTED.0, NOT_ADMITTED.1.to_owned()))
                        }
                        Refusal::Reachability => {
                            attempt = 0;
                            None
                        }
                    };
                    if let Some((code, message)) = stop
                        && !self.stop_until_asked(code, message, &mut commands).await
                    {
                        return self.closed().await;
                    }
                    tracing::info!(attempt, %error, "the session did not reach its server");
                    lost_at.get_or_insert_with(tilo_clock::now);
                    self.fail("transport-unavailable", error.to_string());
                }
            }
            if self.emptied && !self.asked_again(&mut commands).await {
                return self.closed().await;
            }
            attempt = attempt.saturating_add(1);
            if !self.wait_offline(attempt, &mut commands).await {
                return self.closed().await;
            }
        }
    }

    /// Dial every joined workspace's link: the host answered, so the session serves.
    ///
    /// # Errors
    ///
    /// The first link the host refused or did not answer.
    async fn dial_all(&mut self) -> Result<(), LinkError> {
        // A live session's grant is its session's token: the last session's live sessions ended
        // with it, and every workspace's is joined again on this connection.
        self.live_sessions.end_session();
        // The entry first, on the actions link: a device the host has not admitted yet reaches no
        // workspace's link until the entry lets it in.
        self.dial_actions().await;
        self.enter_if_entering().await;
        for workspace_id in self.workspaces.clone() {
            self.dial_link(workspace_id).await?;
        }
        // The person's own workspace, whose record holds their list of their workspaces: read
        // from its record as it changes. A host that holds none answers nothing, which is no
        // failure of this session.
        let own = self.own_workspace();
        if !self.workspaces.contains(&own) {
            let _ = self.dial_link(own).await;
        }
        Ok(())
    }

    /// Dial the host's action service, which every device's requests travel through: a host
    /// that does not answer it leaves them undeliverable, never the session down.
    async fn dial_actions(&mut self) {
        if self.actions.is_some() {
            return;
        }
        match self.reach(Purpose::Actions).await {
            Ok(link) => self.actions = Some(link),
            Err(error) => {
                tracing::info!(%error, "the host did not answer its action service");
            }
        }
    }

    /// Ask the host for this session's fetch proxy token, in one of the workspaces this page is
    /// in: the page sends it with each request it makes through the host's proxy
    /// ([`crate::Link::fetch_proxy_session`]). The workspace's roster is what decides the ask, so
    /// a page in no workspace here has none, and a host that serves no proxy mints none.
    fn request_fetch_grant(&mut self) {
        if self.fetch_grant_asked {
            return;
        }
        let Some(link) = self.actions.clone() else {
            return;
        };
        let Some(&workspace_id) = self.links.keys().next() else {
            return;
        };
        self.fetch_grant_asked = true;
        let signer = Arc::clone(&self.signer);
        let cert = self.config.cert.clone();
        let host = self.config.server_cert.device_public_key;
        let reports = Arc::clone(&self.reports);
        n0_future::task::spawn(async move {
            let Some(token) = ask_fetch_grant(&link, signer, &cert, host, workspace_id).await
            else {
                return;
            };
            if reports.status().phase != TransportPhase::Closed {
                reports.set_fetch_proxy(Some(hex::encode(token)));
            }
        });
    }

    /// Dial the host on `purpose`: a link of its own for each conversation.
    ///
    /// An actions link proves nothing: each request it carries is signed by the device that
    /// made it, and the host reads the link's first frame as the request it is. A records link
    /// is served only to a device the host knows, and where its carrier names no endpoint key —
    /// a page's WebTransport session — the device proves itself with the server's handshake on
    /// the link's first stream ([`tilo_server_client::link::Link::prove_member`]). Over iroh the
    /// endpoint this device runs under its key is who it is.
    async fn reach(
        &mut self,
        purpose: Purpose,
    ) -> Result<tilo_server_client::link::Link, LinkError> {
        let dialer = self.dial.dialer(&self.config, self.signer.as_ref()).await?;
        let link = dialer.link(purpose).await?;
        if purpose == Purpose::Records
            && self.config.host.carrier() == tilo_server_client::Carrier::WebTransport
        {
            link.prove_member(
                &self.config.cert,
                self.signer.as_ref(),
                &self.config.server_cert,
            )
            .await?;
        }
        Ok(link)
    }

    /// Ask the host's entry for the workspace this session was told to enter
    /// ([`PeerConfig::entering`]), on the actions link, and report what it answered: the host
    /// admits the device before it is a member, and the session then joins the workspace's link.
    async fn enter_if_entering(&self) {
        let Some(entering) = self.config.entering.clone() else {
            return;
        };
        let answer = ask_entry(
            self.actions.as_ref(),
            Arc::clone(&self.signer),
            self.config.cert.clone(),
            self.config.server_cert.device_public_key,
            &entering,
        )
        .await;
        match answer {
            Ok(entered) => {
                let state = entry_state(&entered);
                self.reports.change(|status| status.entry = Some(state));
            }
            Err(failure) => {
                self.reports.change(|status| {
                    status.entry = Some(crate::Entry::Refused {
                        code: failure.code,
                        message: failure.message,
                        retry_after_secs: None,
                        entry: None,
                    });
                });
            }
        }
    }

    /// Ask the host to admit this device through `entering`, answering `done`: what
    /// [`Cmd::Enter`] asks for a device entering a workspace it is not in yet.
    fn enter(&self, entering: tilo_server_protocol::entering::Entering, done: EnterCompletion) {
        let signer = Arc::clone(&self.signer);
        let cert = self.config.cert.clone();
        let host = self.config.server_cert.device_public_key;
        let actions = self.actions.clone();
        n0_future::task::spawn(async move {
            let answer = ask_entry(actions.as_ref(), signer, cert, host, &entering)
                .await
                .map_err(|failure| LinkError::Unavailable(failure.message));
            let _ = done.send(answer);
        });
    }

    /// Dial `workspace_id`'s link and start what reads it, unless it runs already.
    async fn dial_link(&mut self, workspace_id: WorkspaceId) -> Result<(), LinkError> {
        if self.links.contains_key(&workspace_id) || !self.dialing.insert(workspace_id) {
            return Ok(());
        }
        let dialed = self.open_link(workspace_id).await;
        self.dialing.remove(&workspace_id);
        match dialed {
            Ok(()) => {
                self.link_up(workspace_id);
                self.request_fetch_grant();
                Ok(())
            }
            Err(error) => {
                self.reports.change(|status| {
                    status.workspaces.entry(workspace_id).or_insert(false);
                });
                Err(error)
            }
        }
    }

    /// Open `workspace_id`'s link, ready to run: the device's replica of it, the link the host
    /// listens on, and the tasks that read what it brings.
    async fn open_link(&mut self, workspace_id: WorkspaceId) -> Result<(), LinkError> {
        // A lent session reads and writes through the device service's member copy, which its
        // own service dials and keeps: the session dials nothing.
        if let Some(lent) = &self.lent {
            let Some(link) = lent.links.link(workspace_id) else {
                return Err(LinkError::Unavailable(
                    "the device service holds no link for the workspace yet".into(),
                ));
            };
            let reading = link.reader();
            let answers = link.answers();
            self.watch_link(workspace_id, reading, answers);
            self.links.insert(
                workspace_id,
                WorkspaceLink {
                    sync: link,
                    transport: None,
                },
            );
            return Ok(());
        }
        let sync = self
            .owner_link(workspace_id)
            .map_err(LinkError::Unavailable)?;
        let link = self.reach(Purpose::Records).await?;
        let transport: Arc<dyn Transport> = Arc::new(link);
        let dialed = sync
            .dial_over(
                Arc::clone(&transport),
                self.config.server_cert.device_public_key,
            )
            .await
            .map_err(|refusal| LinkError::Refused(refusal.to_string()))?;
        let reading = sync.reader();
        let answers = sync.answers();
        let running = sync.clone();
        let ended = self.commands.clone();
        n0_future::task::spawn(async move {
            let why = dialed.run(&running).await.err();
            if let Some(commands) = ended.upgrade() {
                let _ = commands.send(Command::LinkEnded(workspace_id, why));
            }
        });
        self.watch_link(workspace_id, reading, answers);
        self.links.insert(
            workspace_id,
            WorkspaceLink {
                sync,
                transport: Some(transport),
            },
        );
        Ok(())
    }

    /// The device's replica of `workspace_id`, as the link that owns it syncs it, taking its
    /// records only as this page opens them.
    fn owner_link(&self, workspace_id: WorkspaceId) -> Result<DirectSync, String> {
        let replica = self.owner.replica(workspace_id)?;
        DirectSync::over(replica, Arc::clone(&self.signer))
            .owning(self.owner.clone(), workspace_id)
            .map(|sync| sync.when_opened())
    }

    /// Have this session's queue take every frame and answer `workspace_id`'s link brings.
    fn watch_link(
        &self,
        workspace_id: WorkspaceId,
        mut reading: direct::Reader,
        mut answers: tokio::sync::broadcast::Receiver<Answered>,
    ) {
        let commands = self.commands.clone();
        n0_future::task::spawn(async move {
            while let Some(heard) = reading.recv().await {
                let Some(sender) = commands.upgrade() else {
                    return;
                };
                if sender
                    .send(Command::Heard(workspace_id, Box::new(heard)))
                    .is_err()
                {
                    return;
                }
            }
        });
        let commands = self.commands.clone();
        n0_future::task::spawn(async move {
            loop {
                match answers.recv().await {
                    Ok(answered) => {
                        let Some(sender) = commands.upgrade() else {
                            return;
                        };
                        if sender
                            .send(Command::Answered(workspace_id, answered))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    /// `workspace_id`'s link listens: whatever this device writes reaches it from now on.
    fn link_up(&mut self, workspace_id: WorkspaceId) {
        self.emptied = false;
        self.reports.change(|status| {
            status.workspaces.insert(workspace_id, true);
        });
        self.sent
            .retain(|(workspace, _)| *workspace != workspace_id);
        if let Ok(peer) =
            iroh_base::EndpointId::from_bytes(&self.config.server_cert.device_public_key)
        {
            self.reports.emit(&crate::Event::HostReady {
                ws: workspace_id,
                peer,
                documents_when_opened: true,
            });
        }
        self.begun.insert(workspace_id);
        self.live_sessions_due.insert(workspace_id);
        self.reopen_records(workspace_id);
        self.outbound_due = true;
        self.follow_governance(workspace_id);
        self.check_admission();
    }

    /// Let go of every link: the next connection dials them again.
    fn drop_links(&mut self, reason: &str) {
        for (_, link) in std::mem::take(&mut self.links) {
            if let Some(transport) = link.transport {
                transport.close(0, reason.as_bytes());
            }
        }
        self.sent.clear();
        self.dialing.clear();
        self.begun.clear();
        self.reports.change(|status| {
            for ready in status.workspaces.values_mut() {
                *ready = false;
            }
        });
    }

    async fn closed(mut self) {
        self.drop_links("the session ended");
        if let Some(link) = self.actions.take() {
            link.close("the session ended");
        }
        self.dial.shutdown().await;
        self.reports.set_fetch_proxy(None);
        self.reports.change(|status| {
            status.phase = TransportPhase::Closed;
            status.authenticated = false;
        });
    }

    /// Serve commands offline until a reconnect (`true`) or close (`false`).
    async fn wait_for_reconnect(
        &mut self,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> bool {
        loop {
            match commands.recv().await {
                None | Some(Command::Client(Cmd::Stop)) => return false,
                Some(Command::Client(Cmd::Reconnect | Cmd::Refresh)) => return true,
                Some(command) => self.apply_offline(command).await,
            }
        }
    }

    /// The session is up: connected, its link failures cleared, every workspace waiting to be
    /// admitted again.
    fn connected(&self) {
        self.reports.change(|status| {
            status.phase = TransportPhase::Connected;
            status.connections = status.connections.wrapping_add(1);
            status.authenticated = true;
            status.attempt = 0;
            status.last_contact_at_ms = Some(tilo_clock::unix_now_ms());
            status.failures.retain(|failure| {
                !failure.code.starts_with("transport") && !failure.code.starts_with("certificate")
            });
            for ready in status.workspaces.values_mut() {
                *ready = false;
            }
        });
    }

    fn note_contact(&self) {
        self.reports.note_contact(tilo_clock::unix_now_ms());
    }

    /// Report `admission-timeout` once the workspaces dialled at
    /// `admission_since` are still not admitted, and clear it once they are.
    fn check_admission(&mut self) {
        let status = self.reports.status();
        match admission_state(&status, self.admission_since, tilo_clock::unix_now_ms()) {
            Admission::Admitted => {
                self.admission_since = None;
                if status
                    .failures
                    .iter()
                    .any(|failure| failure.kind == FailureKind::Admission)
                {
                    self.clear_admission_failure();
                }
            }
            Admission::Waiting => {}
            Admission::Overdue => {
                self.admission_since = None;
                self.fail_on(
                    FailureKind::Admission,
                    "admission-timeout",
                    "Shared state is still catching up. Refresh it to ask the host again.",
                );
            }
        }
    }

    fn clear_admission_failure(&self) {
        self.reports.change(|status| {
            status
                .failures
                .retain(|failure| failure.kind != FailureKind::Admission);
        });
    }

    /// Wait out this attempt's backoff, serving what the person asks offline.
    /// `false` when they closed the session instead.
    async fn wait_offline(
        &mut self,
        attempt: u32,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> bool {
        let due = tilo_clock::now()
            .checked_add(self.offline(attempt))
            .unwrap_or_else(tilo_clock::now);
        #[allow(
            clippy::useless_conversion,
            reason = "on the page the clock and the timer are one type; on the host the timer's \
                      instant wraps the clock's"
        )]
        let sleep = n0_future::time::sleep_until(due.into());
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                biased;
                command = commands.recv() => match command {
                    None | Some(Command::Client(Cmd::Stop)) => return false,
                    Some(Command::Client(Cmd::Reconnect | Cmd::Retry | Cmd::Refresh)) => {
                        return true;
                    }
                    Some(command) => self.apply_offline(command).await,
                },
                () = &mut sleep => return true,
            }
        }
    }

    /// The session is offline after its `attempt`th failed one in a row: how long it waits
    /// before the next, which its status says.
    fn offline(&self, attempt: u32) -> Duration {
        let base = MAX_BACKOFF.min(Duration::from_millis(
            250u64.saturating_mul(1u64 << attempt.min(7)),
        ));
        let delay = MAX_BACKOFF.min(base.mul_f64(0.75 + 0.5 * rand::random::<f64>()));
        let delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
        tracing::info!(
            attempt,
            wait_ms = delay_ms,
            "the session dials its server again after a wait"
        );
        self.reports.change(|status| {
            status.phase = TransportPhase::Offline;
            status.authenticated = false;
            status.attempt = attempt;
            status.next_attempt_at_ms = Some(tilo_clock::unix_now_ms().saturating_add(delay_ms));
        });
        delay
    }

    /// The session stops, failed with `code` and `message`, until the page
    /// asks it to connect again (`true`) or closes it (`false`).
    async fn stop_until_asked(
        &mut self,
        code: &'static str,
        message: String,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> bool {
        self.fail(code, message);
        self.reports.change(|status| {
            status.phase = TransportPhase::Failed;
            status.authenticated = false;
        });
        self.drop_links("the session stopped");
        self.wait_for_reconnect(commands).await
    }

    /// Whether a session the host left holding nothing goes on: it stays
    /// failed until the person asks for a connection again.
    async fn asked_again(&mut self, commands: &mut mpsc::UnboundedReceiver<Command>) -> bool {
        self.stop_until_asked(
            WORKSPACE_DELETED.0,
            WORKSPACE_DELETED.1.to_owned(),
            commands,
        )
        .await
    }

    /// Serve what the session owes, take commands and what the links bring, until it ends.
    async fn serve(&mut self, commands: &mut mpsc::UnboundedReceiver<Command>) -> Exit {
        let mut tick = n0_future::time::interval(CHANNEL_TICK);
        loop {
            if !self.settle_channels().await {
                return Exit::Lost("the workspace's channels stopped".into());
            }
            if !self.settle_owed().await {
                return Exit::Lost("this device's ownership cannot be read".into());
            }
            tokio::select! {
                biased;
                command = commands.recv() => {
                    let mut next = command;
                    let mut taken = 0_usize;
                    loop {
                        if let Some(exit) = self.take_command(next).await {
                            return exit;
                        }
                        taken = taken.saturating_add(1);
                        match (taken < MAX_TAKEN_AT_ONCE)
                            .then(|| n0_future::future::now_or_never(commands.recv()))
                            .flatten()
                        {
                            Some(command) => next = command,
                            None => break,
                        }
                    }
                }
                _ = tick.tick() => {
                    self.retry_links().await;
                    self.check_admission();
                    self.ask_behind_again(tilo_clock::unix_now_ms());
                    self.live_sessions_due.extend(self.workspaces.iter().copied());
                }
            }
        }
    }

    /// Try again each workspace's link that is not up, and take up a member copy that came up
    /// after the session asked for it.
    async fn retry_links(&mut self) {
        for workspace_id in self.workspaces.clone() {
            if self.links.contains_key(&workspace_id) || self.dialing.contains(&workspace_id) {
                continue;
            }
            if let Err(error) = self.dial_link(workspace_id).await {
                tracing::info!(%workspace_id, %error, "a workspace's link is not up yet");
            }
        }
        // A lent link the device service closed is no longer the session's to read.
        if self.lent.is_some() {
            let gone: Vec<WorkspaceId> = self
                .links
                .iter()
                .filter(|(workspace_id, link)| {
                    link.transport.is_none()
                        && self
                            .lent
                            .as_ref()
                            .is_some_and(|lent| lent.links.link(**workspace_id).is_none())
                })
                .map(|(workspace_id, _)| *workspace_id)
                .collect();
            for workspace_id in gone {
                self.links.remove(&workspace_id);
                self.reports.change(|status| {
                    status.workspaces.insert(workspace_id, false);
                });
            }
        }
    }

    /// Take one command while connected. Returns how the session ends, if it does.
    async fn take_command(&mut self, command: Option<Command>) -> Option<Exit> {
        match command {
            None | Some(Command::Client(Cmd::Stop)) => Some(Exit::Closed),
            Some(Command::Client(Cmd::Reconnect)) => Some(Exit::Reconnect),
            Some(Command::Client(Cmd::Refresh)) => {
                self.clear_admission_failure();
                for workspace_id in self.workspaces.clone() {
                    self.reopen_records(workspace_id);
                    if let Some(link) = self.links.get(&workspace_id) {
                        link.sync.ask_replay();
                    }
                }
                None
            }
            Some(Command::Client(Cmd::Retry)) => None,
            Some(command) => {
                self.apply(command).await;
                None
            }
        }
    }

    /// Apply one command while disconnected; what it publishes commits at once.
    async fn apply_offline(&mut self, command: Command) {
        self.apply(command).await;
        self.commit_held().await;
    }

    /// Apply one command. Returns `false` when the session can no longer serve.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per command, each handing off to its own method"
    )]
    async fn apply(&mut self, command: Command) -> bool {
        match command {
            Command::Client(Cmd::Join(workspace_id)) => {
                if self.join(workspace_id) {
                    let _ = self.dial_link(workspace_id).await;
                }
                true
            }
            Command::Client(Cmd::JoinPlaced(workspace, name)) => {
                let Some(workspace_id) = WorkspaceId::parse(&workspace) else {
                    return true;
                };
                // A workspace of this device's person is the host's to place; the host's
                // `workspace.enter` action does that, so the session takes it as a join.
                tracing::debug!(%name, "a placed workspace joins its link");
                if self.join(workspace_id) {
                    let _ = self.dial_link(workspace_id).await;
                }
                true
            }
            Command::Client(Cmd::OpenDocumentAt(workspace_id, route)) => {
                self.open_resource(workspace_id, &route)
            }
            #[cfg(not(target_arch = "wasm32"))]
            Command::LiveSessionJoined(workspace_id, joined) => {
                self.live_session_joined(workspace_id, joined);
                true
            }
            #[cfg(target_arch = "wasm32")]
            Command::LiveSessionJoined(workspace_id) => {
                self.live_session_joined(workspace_id, live_session::arrived(workspace_id));
                true
            }
            Command::LiveSessionHeard(workspace_id) => {
                self.live_session_heard(workspace_id);
                true
            }
            Command::LiveSessionEnded(workspace_id) => {
                self.live_session_ended(workspace_id);
                true
            }
            Command::PresenceReaders => {
                self.hear_presence();
                true
            }
            Command::Heard(workspace_id, heard) => {
                self.brought(workspace_id, *heard);
                true
            }
            Command::Answered(workspace_id, answered) => {
                self.acknowledge(workspace_id, answered);
                true
            }
            Command::LinkEnded(workspace_id, refusal) => {
                self.link_ended(workspace_id, refusal);
                true
            }
            Command::RecordOpened(workspace_id, channel, sent) => {
                self.record_opened(workspace_id, channel, sent);
                true
            }
            Command::Client(command @ (Cmd::Live(..) | Cmd::SealLive(..))) => {
                self.apply_live(command);
                true
            }
            Command::Client(Cmd::RemoteAction(workspace_id, to, frame)) => {
                self.remote_action(workspace_id, to, &frame);
                true
            }
            Command::Client(Cmd::WantContent(workspace_id, request)) => {
                self.want_content(workspace_id, &request);
                true
            }
            Command::Client(Cmd::KeepContent(workspace_id, kept)) => {
                self.kept.insert(workspace_id, kept);
                true
            }
            Command::Client(Cmd::InstallGroupKeys(route, _)) => {
                self.route_keys.insert(route.clone(), RouteKeys::Handed);
                self.new_keys.push(route);
                self.report_new_keys();
                true
            }
            Command::Client(Cmd::OpenRoute(route)) => {
                self.open_channel(&route);
                if let Ok(workspace_id) = route.workspace_id() {
                    self.open_record_once(workspace_id, &route);
                }
                self.opened_routes.insert(route);
                true
            }
            Command::Client(Cmd::Write(route)) => {
                if let Ok(workspace_id) = route.workspace_id() {
                    self.open_record_once(workspace_id, &route);
                }
                self.publish(&route);
                true
            }
            Command::Client(Cmd::PublishSigned(route, op)) => {
                if let Ok(workspace_id) = route.workspace_id() {
                    self.open_record_once(workspace_id, &route);
                }
                self.publish_signed(&route, *op);
                true
            }
            Command::Client(Cmd::Barrier(done)) => {
                self.barrier(done, Scope::Session, None);
                true
            }
            Command::Client(Cmd::BarrierOf(workspaces, done)) => {
                self.barrier(
                    done,
                    Scope::Workspaces(workspaces.into_iter().collect()),
                    None,
                );
                true
            }
            Command::Client(Cmd::BarrierFor(routes, done)) => {
                let routes: BTreeSet<DocumentRoute> = routes.into_iter().collect();
                self.barrier(done, Scope::Documents(routes.clone()), Some(routes));
                true
            }
            Command::Client(Cmd::KeptOf(workspaces, said)) => {
                let _ = said.send(self.keeps(&workspaces.into_iter().collect()));
                true
            }
            Command::Client(Cmd::Presence(workspace_id, frames)) => {
                self.publish_presence(workspace_id, frames);
                true
            }
            Command::Client(Cmd::CallAudio(workspace_id, frames)) => {
                self.publish_call_audio(workspace_id, frames);
                true
            }
            Command::Client(Cmd::AgentSession(label, done)) => {
                self.session_as_agent(label, done);
                true
            }
            Command::Client(Cmd::Enter(entering, done)) => {
                self.enter(entering, done);
                true
            }
            Command::Client(Cmd::CallPictures(workspace_id, pictures)) => {
                self.publish_call_pictures(workspace_id, pictures);
                true
            }
            Command::Client(Cmd::CallShowing(workspace_id, shown)) => {
                self.show_call_pictures(workspace_id, &shown);
                true
            }
            Command::Client(Cmd::ShowPages(workspace_id, shown)) => {
                self.show_pages(workspace_id, &shown);
                true
            }
            Command::Client(Cmd::CallPictureRestart(workspace_id, device, source)) => {
                self.restart_call_picture(workspace_id, device, source);
                true
            }
            Command::Client(Cmd::Administer(request, done)) => {
                self.administer(&request, done);
                true
            }
            Command::Client(Cmd::Revoke(entry)) => self.revoke(entry),
            Command::Client(Cmd::Share(workspace_id, said)) => {
                self.say_share(workspace_id, &said);
                true
            }
            // Handled by the caller's loop.
            Command::Client(Cmd::Reconnect | Cmd::Retry | Cmd::Refresh | Cmd::Stop) => true,
            // What only a desktop's own live session carries: nothing a server session says.
            Command::Client(_) => true,
        }
    }

    /// Say `said` on `workspace_id`'s live session's share track: what a viewer says to the device
    /// hosting a window, each object to the device it names alone. Input lands within its frame or
    /// not at all, so what is said while the live session is not open is dropped.
    fn say_share(&mut self, workspace_id: WorkspaceId, said: &[tilo_live::share::Said]) {
        let Some(joined) = self.live_sessions.joined_mut(workspace_id) else {
            return;
        };
        if let Err(error) = joined.live_session.say(said) {
            tracing::debug!(%error, "a shared window's word was not said");
        }
    }

    /// Take part in `workspace_id`'s link, now or as soon as it is dialled.
    fn join(&mut self, workspace_id: WorkspaceId) -> bool {
        if self.workspaces.contains(&workspace_id) {
            return true;
        }
        self.workspaces.insert(workspace_id);
        self.live_sessions_due.insert(workspace_id);
        self.reports.change(|status| {
            status.workspaces.entry(workspace_id).or_insert(false);
        });
        self.begun.remove(&workspace_id);
        true
    }

    /// The session stops following `workspace_id`, for `why` (its owner deleted it, or took this
    /// person out), and says so for the page or layout to act on.
    fn left(&mut self, workspace_id: WorkspaceId, (code, message): (&'static str, &str)) {
        if let Some(link) = self.links.remove(&workspace_id)
            && let Some(transport) = link.transport
        {
            transport.close(0, message.as_bytes());
        }
        self.workspaces.remove(&workspace_id);
        self.live_sessions_due.remove(&workspace_id);
        self.opened_records
            .retain(|(workspace, _), _| *workspace != workspace_id);
        self.fail(code, message);
        let nothing_left = self.workspaces.is_empty();
        self.emptied = nothing_left;
        self.reports.change(|status| {
            status.workspaces.remove(&workspace_id);
            status.serving.remove(&workspace_id);
            if nothing_left {
                status.phase = TransportPhase::Failed;
            }
        });
    }

    /// `workspace_id`'s link ended: a deleted workspace or a removed device stops the session
    /// there, anything else is dialled again on the tick.
    fn link_ended(
        &mut self,
        workspace_id: WorkspaceId,
        refusal: Option<tilo_server_protocol::refusal::Refusal>,
    ) {
        self.links.remove(&workspace_id);
        self.sent
            .retain(|(workspace, _)| *workspace != workspace_id);
        self.reports.change(|status| {
            status.workspaces.insert(workspace_id, false);
        });
        match refusal {
            Some(tilo_server_protocol::refusal::Refusal::WorkspaceDeleted) => {
                self.left(workspace_id, WORKSPACE_DELETED);
            }
            Some(tilo_server_protocol::refusal::Refusal::NotAMember(_)) => {
                self.left(workspace_id, NOT_ADMITTED);
            }
            Some(other) => {
                tracing::info!(%workspace_id, %other, "a workspace's link ended");
            }
            None => {}
        }
    }

    /// Administration requests are the host's actions now, asked over the actions link.
    fn administer(
        &self,
        request: &tilo_server_protocol::workspaces::WorkspacesRequest,
        done: WorkspacesCompletion,
    ) {
        use tilo_actions::remote::{ReceiptStatus, RemoteActionFrame, RemoteActionMessage};
        let Some(link) = self.actions.clone() else {
            let _ = done.send(Err(tilo_session_actions::NOT_CONNECTED.into()));
            return;
        };
        let signer = Arc::clone(&self.signer);
        let cert = self.config.cert.clone();
        let host = self.config.server_cert.device_public_key;
        let request = request.clone();
        n0_future::task::spawn(async move {
            let answer = async {
                let mut invoke = tilo_server_protocol::workspaces::administer_invoke(
                    &request,
                    tilo_actions::InvocationId::new(),
                )
                .map_err(|error| format!("the request does not encode: {error}"))?;
                // No workspace's roster names the device here, so the host reads who asks from
                // the invocation this device signs.
                invoke.signed = Some(Box::new(
                    tilo_server_protocol::invocation::sign_invocation(
                        "",
                        &invoke,
                        &cert,
                        signer.as_ref(),
                        tilo_clock::unix_now_ns(),
                    )
                    .await
                    .map_err(|error| format!("this device could not sign the request: {error}"))?,
                ));
                let frame = ask_action(
                    &link,
                    signer,
                    cert.device_public_key,
                    "",
                    host,
                    RemoteActionMessage::Invoke(invoke),
                )
                .await
                .map_err(|refusal| refusal.to_string())?;
                let message = RemoteActionFrame::decode(&frame)
                    .map_err(|error| format!("the host's answer does not decode: {error}"))?
                    .message;
                let RemoteActionMessage::Receipt(receipt) = message else {
                    return Err("the host answered no receipt".to_owned());
                };
                match receipt.status {
                    ReceiptStatus::Completed { output } => {
                        tilo_server_protocol::workspaces::administered_reply(
                            &output.value.unwrap_or_default(),
                        )
                    }
                    ReceiptStatus::Rejected { error } => Err(error.to_string()),
                    ReceiptStatus::Failed { error } => Err(error.message),
                    ReceiptStatus::Cancelled { message } => {
                        Err(message.unwrap_or_else(|| "the host gave the request up".into()))
                    }
                    ReceiptStatus::Accepted => Err("the host did not answer the request".into()),
                }
            }
            .await;
            let _ = done.send(answer);
        });
    }

    /// The session ended with requests the host never answered: each is answered here.
    fn requests_lost(&mut self) {
        for (_, done) in std::mem::take(&mut self.workspaces_pending) {
            let _ = done.send(Err("the session ended before the host answered".into()));
        }
        for (_, done) in std::mem::take(&mut self.agent_sessions_pending) {
            let _ = done.send(Err("the session ended before the host answered".into()));
        }
    }

    /// Announce a revocation this device's person's root signed to every workspace this session
    /// holds, by saying this device's identity again on each link.
    fn revoke(&mut self, entry: tilo_user_identity::RevocationEntry) -> bool {
        if entry.user_public_key != self.config.cert.user_public_key {
            return true;
        }
        let Ok(changed) = self.config.revocations.insert(entry) else {
            return true;
        };
        if !changed {
            return true;
        }
        for link in self.links.values() {
            let _ = link
                .sync
                .send(identity_hello(&self.config.cert, &self.config.revocations));
        }
        true
    }

    /// The session took in what `heard` brought: a record's write, a channel's governance, or
    /// what its workspace made of either.
    fn brought(&mut self, workspace_id: WorkspaceId, heard: direct::Heard) {
        self.note_contact();
        let direct::Heard {
            author,
            message,
            landed,
            fresh,
            report,
            ..
        } = heard;
        // Channel governance and access requests are the key service's to settle, as a link
        // hands them to a device's own.
        if matches!(
            message,
            neighbour_wire::NeighbourMsg::MlsGovernance { .. }
                | neighbour_wire::NeighbourMsg::MlsAccessRequest { .. }
        ) {
            if let Some(inbound) = tilo_workspace_keys::InboundGovernance::from_message(message)
                && let Some(governance) = &self.governance
            {
                governance.inbound.lock_recover().push(inbound);
                self.pump_channels(false);
            }
            return;
        }
        // Content a workspace's records reference: an ask this device answers from what it
        // keeps, an app surface this page asked for, or a piece for its readers.
        if matches!(
            message,
            neighbour_wire::NeighbourMsg::DocumentContentWant { .. }
                | neighbour_wire::NeighbourMsg::DocumentContentPiece { .. }
                | neighbour_wire::NeighbourMsg::DocumentContentLacks { .. }
                | neighbour_wire::NeighbourMsg::DocumentContentRefused { .. }
        ) {
            self.content_arrived(workspace_id, author, &message);
            return;
        }
        let changed = matches!(&report, Ok(report) if report.changed);
        // What a record's write named, for its readers and its outcome.
        if let Some(route) = DocumentRoute::of_message(&message) {
            let surface = match &message {
                neighbour_wire::NeighbourMsg::AppDocument { envelope, .. }
                    if envelope.kind == collab_types::OpKind::Change =>
                {
                    envelope.surface
                }
                _ => None,
            };
            let compaction = matches!(
                &message,
                neighbour_wire::NeighbourMsg::AppDocument { envelope, .. }
                    if envelope.kind == collab_types::OpKind::Compaction
            );
            match report {
                Ok(report) => {
                    let took = landed && tilo_peer_state::owner::took_in(&report, true);
                    if compaction && took {
                        self.caught_up(&route);
                    }
                    if took {
                        self.held_inbound.remove(&route);
                        self.landed(&route, Some(author));
                    }
                    if let tilo_peer_state::InputDisposition::RetryLater(waiting) = report.primary()
                    {
                        self.held(&route, waiting, surface);
                    }
                    if report.replay_wanted {
                        self.replays.insert(route.clone());
                        self.ask_replay(&route);
                    }
                    self.follow_report(&route.cluster_id, &report);
                }
                Err(error) => {
                    if error.is_device_failure() {
                        tracing::warn!(%error, "this computer could not decide a workspace write");
                        return;
                    }
                    let why = error.to_string();
                    if compaction {
                        self.fell_behind(&route, &why, tilo_clock::unix_now_ms());
                    }
                    self.refused = (self.refused.0.saturating_add(1), why);
                }
            }
            if fresh && !landed {
                // A frame this device already held: nothing changed for its readers.
                tracing::debug!(app = %route.app_id, record = %route.document, "a write this device held");
            }
            return;
        }
        // Anything else a link brings that names no record (a workspace document's frames, an
        // identity) is the replica's, already decided.
        if changed && let Some(workspace_id) = self.workspace_of(&message) {
            self.follow_governance(workspace_id);
        }
    }

    /// The workspace a governance frame names, where it names one.
    fn workspace_of(&self, message: &neighbour_wire::NeighbourMsg) -> Option<WorkspaceId> {
        message
            .governance_workspace()
            .and_then(WorkspaceId::parse)
            .or_else(|| {
                // A workspace document's frame names its workspace in the channel it writes.
                let (channel, _) = neighbour_wire::retention(message, false)?;
                self.opened_records
                    .iter()
                    .find(|(_, route)| {
                        neighbour_wire::app_document_group(&route.app_id, &route.document)
                            == channel
                    })
                    .map(|((workspace, _), _)| *workspace)
            })
    }

    /// The host answered one of this device's writes on `workspace_id`'s link: retire it.
    fn acknowledge(&mut self, workspace_id: WorkspaceId, answered: Answered) {
        self.note_contact();
        let Some((id, frame, written)) = self.outgoing_named(workspace_id, answered.write) else {
            return;
        };
        let outcome = match answered.kept {
            Answer::Durable => Ok(answered.seq),
            // The workspace's own words for a write it refuses: what its author
            // waits with, so a refused write is never answered saved.
            Answer::Refused { reason } => {
                Err(tilo_server_protocol::refusal::Refusal::Other(reason))
            }
        };
        let withdrawn = match (&outcome, written) {
            (Err(reason), Some((route, changes))) => Some(crate::Event::Refused {
                route,
                changes,
                refusal: reason.clone(),
            }),
            _ => None,
        };
        self.answered.push(AnsweredWrite {
            workspace: workspace_id,
            id,
            frame,
            outcome,
            withdrawn,
        });
    }

    /// This device's own frame of `workspace_id` that hashes to `write`, with what it was
    /// written to and the changes it carries.
    fn outgoing_named(&self, workspace_id: WorkspaceId, write: [u8; 32]) -> NamedOutgoing {
        self.ownership(workspace_id)
            .into_iter()
            .find(|(id, ..)| *id == write)
            .map(|(id, bytes, ..)| {
                let written = written_to(&bytes);
                (id, bytes, written)
            })
    }

    /// A record this page opened was answered by its workspace: its frames are in the replica
    /// already, so its readers hear it opened.
    fn record_opened(&mut self, workspace_id: WorkspaceId, channel: [u8; 32], sent: bool) {
        self.answered_behind(workspace_id, channel);
        let Some(route) = self.opened_records.get(&(workspace_id, channel)).cloned() else {
            return;
        };
        let holds = self
            .engine
            .owner()
            .with_current(workspace_id, |view| {
                Ok(view.peer().holds_document(&route.app_id, &route.document))
            })
            .unwrap_or(false);
        self.reports.emit(&crate::Event::DocumentOpened {
            ws: workspace_id,
            channel,
            route: Some(route),
            answer: tilo_server_protocol::DocumentAnswer::Sent {
                frames: u32::from(sent && holds),
            },
        });
    }

    /// Open the on-demand record `route` of `workspace_id`, and ask for its keys at the same
    /// time.
    fn open_resource(&mut self, workspace_id: WorkspaceId, route: &DocumentRoute) -> bool {
        let channel = neighbour_wire::app_document_group(&route.app_id, &route.document);
        if !matches!(
            neighbour_wire::document_sync(&route.document),
            neighbour_wire::DocumentSync::ChainHead { .. }
        ) {
            self.open_channel(route);
        }
        self.opened_records
            .insert((workspace_id, channel), route.clone());
        self.open_record(workspace_id, channel);
        true
    }

    /// Open `route` as a record unless this session opened it already; the shared desktop is
    /// replayed to every device and is never asked for.
    fn open_record_once(&mut self, workspace_id: WorkspaceId, route: &DocumentRoute) {
        let channel = neighbour_wire::app_document_group(&route.app_id, &route.document);
        if self.opened_records.contains_key(&(workspace_id, channel))
            || (route.app_id == tilo_desktop_model::ownership::DESKTOP_APP
                && route.document == tilo_desktop_model::ownership::DESKTOP_DOCUMENT)
        {
            return;
        }
        self.open_resource(workspace_id, route);
    }

    /// Ask `workspace_id`'s link for the record on `channel`, off the session's loop: what its
    /// host keeps of it reaches this device, and its answer says so.
    fn open_record(&self, workspace_id: WorkspaceId, channel: [u8; 32]) {
        let Some(link) = self.links.get(&workspace_id) else {
            return;
        };
        let sync = link.sync.clone();
        let commands = self.commands.clone();
        n0_future::task::spawn(async move {
            let sent = sync.open_record(channel).await.is_ok();
            if let Some(commands) = commands.upgrade() {
                let _ = commands.send(Command::RecordOpened(workspace_id, channel, sent));
            }
        });
    }

    /// Open every record this session opened again on a new link.
    fn reopen_records(&self, workspace_id: WorkspaceId) {
        let channels: Vec<[u8; 32]> = self
            .opened_records
            .keys()
            .filter(|(workspace, _)| *workspace == workspace_id)
            .map(|(_, channel)| *channel)
            .collect();
        for channel in channels {
            self.open_record(workspace_id, channel);
        }
    }

    /// Ask the link of `route`'s workspace to read its history again.
    fn ask_replay(&self, route: &DocumentRoute) {
        if let Ok(workspace_id) = route.workspace_id()
            && let Some(link) = self.links.get(&workspace_id)
        {
            link.sync.ask_replay();
        }
    }

    /// Take the channel changes' work: await what the device's ownership signs, install new
    /// keys, open what waited for them, and seal and commit what was held.
    async fn settle_channels(&mut self) -> bool {
        self.retire_answered();
        if self.workspace_keys.signing() {
            self.workspace_keys.signatures().await;
            self.pump_channels(false);
        }
        loop {
            for route in std::mem::take(&mut self.new_keys) {
                if let Ok(workspace_id) = route.workspace_id() {
                    self.live_sessions_due.insert(workspace_id);
                }
                self.release_live(&route);
                if !self.opens_as_before.remove(&route) {
                    self.landed(&route, None);
                }
                if self.behind.contains_key(&route) {
                    self.replays.insert(route.clone());
                    self.ask_replay(&route);
                }
                self.outbound_due |= self.held.lock().contains_key(&route);
            }
            self.commit_held().await;
            if self.new_keys.is_empty() {
                break;
            }
        }
        self.persist_keys(false);
        self.report_keys();
        true
    }

    /// What this session owes its host before waiting for the next event: its live sessions,
    /// the app surfaces it asks for, and its ownership.
    async fn settle_owed(&mut self) -> bool {
        self.settle_live_sessions();
        self.ask_surfaces();
        if std::mem::take(&mut self.surfaces_arrived) || !self.replays.is_empty() {
            self.replay_surfaced();
        }
        // What the host brought into a sealed record's history is this device's to fold, as its
        // own writes are before each of them.
        let now = crate::clock::system_time(tilo_clock::unix_now_ns());
        for workspace_id in self.workspaces.clone() {
            let Some(workspace) = self.held_workspace(workspace_id) else {
                continue;
            };
            if let Err(error) = self
                .engine
                .owner()
                .compact_outgoing(workspace, self.signer.as_ref(), now)
                .await
            {
                tracing::warn!(%error, "sealed histories stay uncompacted");
            }
        }
        for workspace_id in self.workspaces.clone() {
            self.send_ownership(workspace_id);
        }
        true
    }
}

/// Governs `owner`'s channels as a reader: their state sealed under `vault`, signed by its
/// ownership, and their readers its workspaces'.
fn open_keys(
    owner: &NativeWorkspaceOwner,
    state: &crate::membership_check::StoredKeys,
    vault: [u8; 32],
    signer: &Arc<dyn tilo_server_client::DeviceSigner>,
    checks: &Arc<tilo_workspace_keys::SealChecks>,
) -> tilo_workspace_keys::WorkspaceKeys {
    let directory = || {
        Box::new(crate::workspace_keys::OwnerDirectory::reader(
            owner.to_owned(),
        ))
    };
    tilo_workspace_keys::WorkspaceKeys::open(
        tilo_workspace_keys::KeysConfig {
            state: Arc::new(state.clone()),
            vault: key_group::VaultKey::new(vault),
            signer: Arc::clone(signer),
            role: tilo_workspace_keys::KeyRole::Follower,
        },
        directory(),
        Arc::clone(checks) as Arc<dyn tilo_workspace_keys::SealCheckRegistry>,
    )
    .unwrap_or_else(|_| {
        tilo_workspace_keys::WorkspaceKeys::without_store(directory(), Arc::clone(checks) as _)
    })
}

/// The key service of a session beside its device's own that was handed none of the device's
/// ([`start_session_lent`]): it holds no keys and asks for none.
fn keyless_keys(
    owner: &NativeWorkspaceOwner,
    checks: &Arc<tilo_workspace_keys::SealChecks>,
) -> tilo_workspace_keys::WorkspaceKeys {
    tilo_workspace_keys::WorkspaceKeys::without_store(
        Box::new(crate::workspace_keys::OwnerDirectory::reader(
            owner.to_owned(),
        )),
        Arc::clone(checks) as Arc<dyn tilo_workspace_keys::SealCheckRegistry>,
    )
}

/// Where the keys of a route this session opened stand: the engine holds them
/// ([`Task::keys_of`]).
enum RouteKeys {
    /// No keys yet.
    Waiting,
    /// The channels hold keys, as of the channel's change, at the epoch they seal with.
    Engine(u64, u64),
    /// A host device handed over the keys of a record whose group this device
    /// is not admitted to itself: the workspace's own keys for the record.
    Handed,
}

impl Task {
    /// The routes this session holds keys of.
    fn keyed_routes(&self) -> impl Iterator<Item = &DocumentRoute> {
        self.route_keys
            .iter()
            .filter(|(_, keys)| !matches!(keys, RouteKeys::Waiting))
            .map(|(route, _)| route)
    }

    /// The keys this device seals and opens `route`'s content with, as its engine holds them.
    fn keys_of(&self, route: &DocumentRoute) -> Option<key_group::ContentKeys> {
        here(self.engine.document_seal_keys(route))
    }

    fn keys_for_ciphertext(
        &self,
        route: &DocumentRoute,
        metadata: collab_types::EncryptionMetadata,
    ) -> Option<key_group::ContentKeys> {
        let workspace = route.workspace_id().ok()?;
        let readers = self
            .engine
            .named_document_readers(workspace, &route.document)
            .ok()?;
        let readers = readers.as_deref().unwrap_or(collab_types::EVERY_MEMBER);
        let original = collab_types::reader_group(workspace, readers)?;
        let group = metadata.key_group(original);
        let keys = self.engine.document_content_keys(route, group).ok()?;
        match keys {
            crate::engine::SealKeys::Here(keys) => Some(keys),
            _ => None,
        }
    }

    /// The workspace `workspace_id`, when this device holds it.
    fn held_workspace(&self, workspace_id: WorkspaceId) -> Option<WorkspaceId> {
        self.engine.holds(workspace_id).then_some(workspace_id)
    }

    fn with_workspace<T>(
        &self,
        workspace_id: WorkspaceId,
        read: impl FnOnce(&NativeCommittedView) -> Result<T, String>,
    ) -> Option<Result<T, String>> {
        self.engine
            .holds(workspace_id)
            .then(|| self.engine.owner().with_current(workspace_id, read))
    }
}

/// The keys `sealing` names, when they are here.
fn here(sealing: Result<crate::engine::SealKeys, String>) -> Option<key_group::ContentKeys> {
    match sealing {
        Ok(crate::engine::SealKeys::Here(keys)) => Some(keys),
        _ => None,
    }
}

/// This device's certificate for a workspace's other members, and the revocations its person's
/// root signed, as a message its links carry.
fn identity_hello(cert: &DeviceCert, revocations: &RevocationList) -> neighbour_wire::NeighbourMsg {
    let mut entries: Vec<tilo_user_identity::RevocationEntry> =
        revocations.iter().cloned().collect();
    entries.sort_by_key(|entry| entry.revoked_device_public_key);
    neighbour_wire::NeighbourMsg::IdentityHello {
        protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
        cert: Box::new(cert.clone()),
        revocations: entries,
    }
}

/// What a failed dial says about this device.
enum Refusal {
    /// The host was not reached, or something in front of it answered: retried with backoff.
    Reachability,
    /// The host turned the device away without saying why: counted, and taken as
    /// [`NOT_ADMITTED`] after [`REFUSALS_BEFORE_FAILING`] in a row.
    Unexplained,
    /// The host said why: the session stops with this failure code and copy.
    Final(&'static str, String),
}

fn refusal(error: &LinkError) -> Refusal {
    match error {
        LinkError::ServerChanged => Refusal::Final(
            "host-changed",
            "This workspace’s server was set up again.".to_owned(),
        ),
        LinkError::Unavailable(_) | LinkError::Malformed(_) | LinkError::Sign(_) => {
            Refusal::Reachability
        }
        LinkError::DeviceRevoked => Refusal::Final(
            "device-revoked",
            "This device was removed from your devices.".to_owned(),
        ),
        LinkError::CertificateRejected => Refusal::Final(
            "certificate-rejected",
            "This workspace’s server refused this browser’s certificate.".to_owned(),
        ),
        // A host that refuses the member proof a link's first stream carries says why in its
        // refusal's typed code (`HelloRefusal`): neither of the reasons it gives changes by
        // asking again. A refusal with anything else to say is the link's, and is tried again.
        LinkError::Refused(code) if code.as_str() == "device-revoked" => Refusal::Final(
            "device-revoked",
            "This device was removed from your devices.".to_owned(),
        ),
        LinkError::Refused(code) if code.as_str() == "certificate-rejected" => Refusal::Final(
            "certificate-rejected",
            "This workspace’s server refused this browser’s certificate.".to_owned(),
        ),
        LinkError::Refused(_) => Refusal::Unexplained,
        LinkError::PeerVersion { theirs }
            if tilo_server_protocol::auth::this_device_is_behind(*theirs) =>
        {
            Refusal::Final(
                "update-required",
                "This page is out of date. Reload it to keep working together.".to_owned(),
            )
        }
        LinkError::PeerVersion { .. } => Refusal::Final(
            "host-update-required",
            "This workspace’s host needs to update to keep working together.".to_owned(),
        ),
    }
}

/// The channel transport of a session that settles its own keys: what the channels say goes out
/// on its workspaces' links, and what those links bring is handed to the key service.
///
/// A page is admitted to a workspace, never to a group, so everything it says to a group's
/// members travels the workspace's link: its ask to be let into a group, and the call's media
/// group it founds and commits where no other device of the call does. The host keeps a carried
/// frame of a group only from a device the workspace admits for its keys, so a page's frame is
/// read there as any member's is.
#[derive(Default)]
pub(super) struct ChannelLinks {
    inbound: std::sync::Mutex<Vec<tilo_workspace_keys::InboundGovernance>>,
    /// The workspaces' links, by workspace, as the session keeps them.
    links: Arc<std::sync::RwLock<BTreeMap<WorkspaceId, DirectSync>>>,
}

impl ChannelLinks {
    fn set_links(&self, links: &BTreeMap<WorkspaceId, WorkspaceLink>) {
        if let Ok(mut held) = self.links.write() {
            held.clear();
            held.extend(
                links
                    .iter()
                    .map(|(workspace_id, link)| (*workspace_id, link.sync.clone())),
            );
        }
    }
}

impl tilo_workspace_keys::GovernanceTransport for ChannelLinks {
    fn publish(&self, message: neighbour_wire::NeighbourMsg) -> bool {
        use neighbour_wire::NeighbourMsg;
        // The group a message is for is not always the workspace that carries it: a document's
        // or a reader set's group asks on its workspace's transport. A page is admitted to the
        // workspace, never to the group, so the message goes on the workspace's link.
        let Some(workspace) = message.governance_workspace().and_then(WorkspaceId::parse) else {
            return false;
        };
        let says = match &message {
            // A follower of a group asks to be let into it.
            NeighbourMsg::MlsAccessRequest { .. } => true,
            // A page founds and commits a call's media group, and no other group
            // ([`tilo_workspace_keys::KeyRole::Follower`]).
            NeighbourMsg::MlsGovernance {
                workspace: scope, ..
            } => neighbour_wire::KeyScope::parse(scope)
                .is_some_and(|scope| tilo_workspace_keys::is_call_media(&scope)),
            _ => false,
        };
        if !says {
            return false;
        }
        let link = self
            .links
            .read()
            .ok()
            .and_then(|links| links.get(&workspace).cloned());
        link.is_some_and(|link| link.send(message).is_ok())
    }

    fn take_inbound(&self) -> Vec<tilo_workspace_keys::InboundGovernance> {
        std::mem::take(&mut *self.inbound.lock_recover())
    }
}

impl Scope {
    /// Whether it answers for the writes to `route`.
    fn covers(&self, route: &DocumentRoute) -> bool {
        match self {
            Self::Session => true,
            Self::Workspaces(workspaces) => within(Some(workspaces), route),
            Self::Documents(documents) => documents.contains(route),
        }
    }

    /// Whether it answers for writes to `workspace`.
    fn reaches(&self, workspace: WorkspaceId) -> bool {
        match self {
            Self::Session => true,
            Self::Workspaces(workspaces) => workspaces.contains(&workspace),
            Self::Documents(documents) => documents
                .iter()
                .any(|route| route.workspace_id().is_ok_and(|named| named == workspace)),
        }
    }
}

/// Whether a barrier that hears `hears` answers with the refusal of a write to `route`.
fn hears(hears: Option<&BTreeSet<DocumentRoute>>, route: &DocumentRoute) -> bool {
    hears.is_none_or(|routes| routes.contains(route))
}

/// Whether `route` is of a workspace `scope` answers for.
fn within(scope: Option<&BTreeSet<WorkspaceId>>, route: &DocumentRoute) -> bool {
    scope.is_none_or(|workspaces| {
        route
            .workspace_id()
            .is_ok_and(|workspace| workspaces.contains(&workspace))
    })
}

/// The document a write in this device's ownership was written to, where its bytes say.
fn written_to(bytes: &[u8]) -> Option<(DocumentRoute, Vec<[u8; 32]>)> {
    let message = neighbour_wire::NeighbourMsg::decode(bytes).ok()?;
    let route = DocumentRoute::of_message(&message)?;
    Some((
        route,
        tilo_peer_state::document_frame::claimed_changes(bytes),
    ))
}

/// What became of a write this session handed to its device's ownership.
enum Publishing {
    /// Ownership carries it, and its host answers for it.
    Owned,
    /// Its record is not here yet, or this device has not signed it: it is kept, and goes when
    /// what it waits for comes.
    Waiting,
    /// Its record's history is full: it is kept as a signed write, and goes once the record is
    /// compacted.
    History,
    /// The workspace refused it for good: it is never saved.
    Refused(tilo_peer_state::owner::WriteRefusal),
}

/// The most bytes one action answer may carry.
const MAX_ACTION_FRAME: usize = 8 * 1024 * 1024;

/// A record's history is full and this device could not compact it: its writes wait.
const RECORD_FULL: &str = "record-history-full";
/// This device's signer did not sign its writes: they wait.
const SIGNING_FAILED: &str = "signing-failed";
/// The failures of writes that wait, held: gone once nothing is held.
const HELD_CODES: &[&str] = &[RECORD_FULL, SIGNING_FAILED];

impl Task {
    // ---- what a link brought, and what its workspace says ----

    /// Tell this device's readers that `route` changed in its replica, once the replica's
    /// governance admits this device.
    fn landed(&mut self, route: &DocumentRoute, author: Option<[u8; 32]>) {
        self.outbound_due |= self.held.lock().contains_key(route);
        if !self.reads_as_member(route) {
            self.unadmitted.insert(route.clone(), author);
            return;
        }
        let held = route.workspace_id().is_ok_and(|workspace| {
            self.engine.owner().with_current(workspace, |view| {
                Ok(view.peer().holds_document(&route.app_id, &route.document))
            }) == Ok(true)
        });
        if !held {
            return;
        }
        self.engine.arrivals().landed();
        self.reports.emit(&crate::Event::Landed {
            author,
            route: route.clone(),
        });
    }

    /// The person's own workspace: the one their list of their workspaces is kept in.
    fn own_workspace(&self) -> WorkspaceId {
        WorkspaceId::from(collab_types::person_workspace(
            &self.config.cert.user_public_key,
        ))
    }

    /// Whether this device reads `route`'s workspace as a member: its replica's governance
    /// admitted it on this session.
    fn reads_as_member(&mut self, route: &DocumentRoute) -> bool {
        let Ok(workspace_id) = route.workspace_id() else {
            return true;
        };
        if self.admitted_here.contains(&workspace_id) {
            return true;
        }
        if !self.admits_this_device(workspace_id) {
            return false;
        }
        self.admitted_here.insert(workspace_id);
        self.reports.change(|status| {
            status.serving.insert(workspace_id);
        });
        true
    }

    /// Whether `workspace_id`'s governance, as this device's replica holds it, admits this
    /// device now.
    fn admits_this_device(&self, workspace_id: WorkspaceId) -> bool {
        let device = self.config.cert.device_public_key;
        self.with_workspace(workspace_id, |view| {
            view.peer()
                .permissions(device, tilo_clock::wall())
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .is_some_and(|admitted| admitted.is_ok())
    }

    /// The records of `workspace_id` that landed before its governance admitted this device,
    /// landed now that it does.
    fn land_admitted(&mut self, workspace_id: WorkspaceId) {
        if self.admitted_here.contains(&workspace_id) || !self.admits_this_device(workspace_id) {
            return;
        }
        self.admitted_here.insert(workspace_id);
        self.reports.change(|status| {
            status.serving.insert(workspace_id);
        });
        let waiting: Vec<(DocumentRoute, Option<[u8; 32]>)> = self
            .unadmitted
            .extract_if(.., |route, _| {
                route.workspace_id().ok() == Some(workspace_id)
            })
            .collect();
        for (route, author) in waiting {
            self.landed(&route, author);
        }
    }

    /// Follow what `workspace_id`'s governance says now: who its members are, and the key
    /// group's commits that waited for a member it now admits.
    fn follow_governance(&mut self, workspace_id: WorkspaceId) {
        self.live_sessions_due.insert(workspace_id);
        self.land_admitted(workspace_id);
        if let Some((workspace, members)) = self.members(workspace_id) {
            if self.reports.status().workspace_members.get(&workspace) != members.as_ref() {
                self.reports.change(|status| match members {
                    Some(members) => {
                        _ = status.workspace_members.insert(workspace.clone(), members);
                    }
                    None => _ = status.workspace_members.remove(&workspace),
                });
            }
            let devices = self.member_devices(workspace_id);
            if self.reports.status().workspace_devices.get(&workspace) != devices.as_ref() {
                self.reports.change(|status| match devices {
                    Some(devices) => {
                        _ = status.workspace_devices.insert(workspace.clone(), devices);
                    }
                    None => _ = status.workspace_devices.remove(&workspace),
                });
            }
            let calls = self.call_rules(workspace_id);
            if self.reports.status().workspace_calls.get(&workspace) != calls.as_ref() {
                self.reports.change(|status| match calls {
                    Some(calls) => _ = status.workspace_calls.insert(workspace.clone(), calls),
                    None => _ = status.workspace_calls.remove(&workspace),
                });
            }
            let names = self.member_names(workspace_id);
            if self.reports.status().workspace_names.get(&workspace) != names.as_ref() {
                self.reports.change(|status| match names {
                    Some(names) => _ = status.workspace_names.insert(workspace.clone(), names),
                    None => _ = status.workspace_names.remove(&workspace),
                });
            }
        }
        self.pump_channels(true);
        // A record opened before the governance admitted it waits with its key group unopened.
        let unopened: Vec<DocumentRoute> = self.held.lock().keys().cloned().collect();
        for route in unopened {
            self.open_channel(&route);
        }
    }

    /// Follow what else the replica did with `report`'s input to the workspace `cluster`.
    fn follow_report(&mut self, cluster: &str, report: &tilo_peer_state::AdmissionReport) {
        for still in report.still_held() {
            if let neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } = &still.message
                && let Some(route) = DocumentRoute::of_message(&still.message)
            {
                let surface = (envelope.kind == collab_types::OpKind::Change)
                    .then_some(envelope.surface)
                    .flatten();
                self.held(&route, &still.waiting_for, surface);
            }
        }
        for dropped in &report.summary().dropped {
            if dropped.waiting_for.is_some() && !dropped.app.is_empty() {
                self.let_go.insert(DocumentRoute {
                    app_id: dropped.app.clone(),
                    cluster_id: cluster.to_owned(),
                    document: dropped.record.clone(),
                });
            }
        }
        if report.replay_wanted {
            let let_go = std::mem::take(&mut self.let_go);
            self.replays.extend(let_go);
        }
        let landed: BTreeMap<DocumentRoute, [u8; 32]> = report
            .released()
            .iter()
            .filter_map(|released| match &released.message {
                neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } => {
                    DocumentRoute::of_message(&released.message)
                        .map(|route| (route, *envelope.author.as_bytes()))
                }
                _ => None,
            })
            .collect();
        for (route, author) in landed {
            self.held_inbound.remove(&route);
            self.landed(&route, Some(author));
        }
    }

    /// The replica refused a compaction of `route`, saying `why`.
    fn fell_behind(&mut self, route: &DocumentRoute, why: &str, now_ms: u64) {
        if !self.behind.contains_key(route) {
            tracing::warn!(
                app = %route.app_id,
                document = %route.document,
                %why,
                "this device holds less of a document than its workspace compacted; it asks for the document again"
            );
        }
        let behind = self.behind.entry(route.clone()).or_insert_with(|| Behind {
            why: String::new(),
            asked: 0,
            next_ms: now_ms.saturating_add(BEHIND_ASKED_AFTER_MS),
            refused_since: true,
        });
        why.clone_into(&mut behind.why);
        behind.refused_since = true;
        self.held_inbound.insert(
            route.clone(),
            format!("behind a compaction it could not take: {why}"),
        );
    }

    /// A compaction of `route` was taken: this device is behind in it no more.
    fn caught_up(&mut self, route: &DocumentRoute) {
        if self.behind.remove(route).is_some() {
            self.held_inbound.remove(route);
            tracing::info!(
                app = %route.app_id,
                document = %route.document,
                "the compaction this device was behind was taken"
            );
        }
    }

    /// Ask the workspaces again, at `now_ms`, for each document this device is behind in whose
    /// time has come.
    fn ask_behind_again(&mut self, now_ms: u64) {
        for (route, behind) in &mut self.behind {
            if now_ms < behind.next_ms {
                continue;
            }
            behind.asked = behind.asked.saturating_add(1);
            behind.refused_since = false;
            let wait = BEHIND_ASKED_AFTER_MS
                .saturating_mul(2_u64.saturating_pow(behind.asked.min(32)))
                .min(BEHIND_ASKED_AT_MOST_EVERY_MS);
            behind.next_ms = now_ms.saturating_add(wait);
            tracing::warn!(
                app = %route.app_id,
                document = %route.document,
                asked = behind.asked,
                why = %behind.why,
                "a document this device is behind in is asked of its workspace again"
            );
            self.replays.insert(route.clone());
        }
        let asking: Vec<DocumentRoute> = self.replays.iter().cloned().collect();
        for route in asking {
            self.ask_replay(&route);
        }
    }

    /// The workspace answered that it sent all it holds of the document `digest` names in
    /// `workspace_id`: one this device was behind in is behind no more.
    fn answered_behind(&mut self, workspace_id: WorkspaceId, digest: [u8; 32]) {
        let settled: Vec<DocumentRoute> = self
            .behind
            .iter()
            .filter(|(route, behind)| {
                behind.asked > 0
                    && !behind.refused_since
                    && route.workspace_id().ok() == Some(workspace_id)
                    && neighbour_wire::app_document_group(&route.app_id, &route.document) == digest
            })
            .map(|(route, _)| route.clone())
            .collect();
        for route in settled {
            self.caught_up(&route);
        }
    }

    /// Say that the replica holds a write of `route` undecided until `waiting` arrives: an app
    /// surface the write names (`surface`) is asked of its workspace.
    fn held(
        &mut self,
        route: &DocumentRoute,
        waiting: &tilo_peer_state::MissingPrerequisite,
        surface: Option<[u8; 32]>,
    ) {
        let reason = match (waiting, surface, route.workspace_id()) {
            (tilo_peer_state::MissingPrerequisite::AppSurface, Some(surface), Ok(workspace_id)) => {
                self.awaiting_surface
                    .insert(surface, (workspace_id, route.clone()));
                format!("for its app surface {}", hex::encode(surface))
            }
            (waiting, ..) => format!("for {waiting}"),
        };
        self.held_inbound.insert(route.clone(), reason);
    }

    /// Ask the workspaces for the app surfaces their writes here wait for and this page does
    /// not have.
    fn ask_surfaces(&mut self) {
        let device = self.config.cert.device_public_key;
        let now = tilo_clock::unix_now_ms();
        let wants: Vec<_> = self
            .awaiting_surface
            .iter()
            .map(|(surface, (workspace_id, route))| (*surface, *workspace_id, route.clone()))
            .collect();
        for (surface, workspace_id, route) in wants {
            if crate::surfaces::holds(&route.app_id, &surface) {
                continue;
            }
            let Some(request) = self.surface_wants.ask(
                &device,
                (),
                (&route.app_id, &route.cluster_id),
                &hex::encode(surface),
                now,
            ) else {
                continue;
            };
            self.want_content(workspace_id, &request);
        }
    }

    /// Let each record whose writes waited for an app surface that is here now, and each one the
    /// replica asked to hear again, ask its workspace for the record.
    fn replay_surfaced(&mut self) {
        self.awaiting_surface
            .retain(|surface, (_, route)| !crate::surfaces::holds(&route.app_id, surface));
        let replays = std::mem::take(&mut self.replays);
        for route in replays {
            if let Ok(workspace_id) = route.workspace_id() {
                let channel = neighbour_wire::app_document_group(&route.app_id, &route.document);
                self.open_record(workspace_id, channel);
            }
        }
    }

    /// What a workspace's content frame becomes here: an ask this device answers from what it
    /// keeps, a piece an app surface wants, or a piece for whoever reads records.
    fn content_arrived(
        &mut self,
        workspace_id: WorkspaceId,
        author: [u8; 32],
        message: &neighbour_wire::NeighbourMsg,
    ) {
        let Some(frame) = postcard::to_allocvec(message).ok() else {
            return;
        };
        if matches!(
            message,
            neighbour_wire::NeighbourMsg::DocumentContentWant { .. }
        ) {
            self.answer_content(workspace_id, author, &frame);
            return;
        }
        let device = self.config.cert.device_public_key;
        let now = tilo_clock::unix_now_ms();
        if let Some(arrived) = self.surface_wants.piece(&device, &frame, now) {
            self.surfaces_arrived |= crate::surfaces::keep(&arrived.hash, arrived.bytes);
            return;
        }
        self.reports.emit(&crate::Event::DocumentContent {
            ws: workspace_id,
            from: author,
            frame,
        });
    }

    /// Answer a workspace's ask for content from what this device keeps, on the workspace's
    /// link: the host hands each piece to the device that asked alone.
    fn answer_content(&self, workspace_id: WorkspaceId, from: [u8; 32], frame: &[u8]) {
        let device = self.config.cert.device_public_key;
        let Some(kept) = self.kept.get(&workspace_id) else {
            return;
        };
        let Some(link) = self.links.get(&workspace_id) else {
            return;
        };
        // A device keeps nothing of what it asks for itself.
        if from == device {
            return;
        }
        for piece in
            neighbour_wire::content::answer(&device, &from, frame, |app, workspace, hash| {
                kept.read(app, workspace, hash)
            })
        {
            if let Ok(message) = neighbour_wire::NeighbourMsg::decode(&piece) {
                let _ = link.sync.send(message);
            }
        }
    }

    // ---- channels ----

    /// Take this device into `route`'s channel, with the members its workspace's governance
    /// admits.
    fn open_channel(&mut self, route: &DocumentRoute) {
        if !self.route_keys.contains_key(route) {
            self.route_keys.insert(route.clone(), RouteKeys::Waiting);
        }
        if matches!(self.route_keys.get(route), Some(RouteKeys::Waiting)) && !self.read_keys(route)
        {
            self.pump_channels(false);
        } else {
            self.report_new_keys();
        }
    }

    /// Run the channels' protocol once, then take up the keys it changed.
    fn pump_channels(&mut self, all: bool) {
        let now = tilo_clock::now();
        if all {
            self.read_all_keys();
        }
        if let Some(governance) = &self.governance {
            governance.set_links(&self.links);
            self.workspace_keys.pump_at(governance, now);
        }
        if self.workspace_keys.generation() != self.keys_generation {
            self.read_all_keys();
        }
        self.report_new_keys();
    }

    fn read_all_keys(&mut self) {
        self.keys_generation = self.workspace_keys.generation();
        let routes: Vec<DocumentRoute> = self
            .route_keys
            .iter()
            .filter(|(_, keys)| !matches!(keys, RouteKeys::Handed))
            .map(|(route, _)| route.clone())
            .collect();
        let keys = self.engine.documents_seal_keys(&routes);
        for (route, keys) in routes.iter().zip(keys) {
            self.take_keys(route, here(keys));
        }
    }

    fn read_keys(&mut self, route: &DocumentRoute) -> bool {
        let keys = self.keys_of(route);
        self.take_keys(route, keys)
    }

    fn take_keys(&mut self, route: &DocumentRoute, keys: Option<key_group::ContentKeys>) -> bool {
        let Some(keys) = keys else {
            return false;
        };
        let change = self.workspace_keys.workspace_change(keys.workspace());
        let installed = match self.route_keys.get(route) {
            Some(RouteKeys::Engine(held, _)) => *held == change,
            Some(RouteKeys::Handed) => true,
            Some(RouteKeys::Waiting) | None => false,
        };
        if !installed {
            let before = match self.route_keys.get(route) {
                Some(RouteKeys::Engine(_, epoch)) => Some(*epoch),
                _ => None,
            };
            if before.is_some_and(|before| self.opened_before(route, &keys, before)) {
                self.opens_as_before.insert(route.clone());
            } else {
                self.opens_as_before.remove(route);
            }
            self.route_keys
                .insert(route.clone(), RouteKeys::Engine(change, keys.epoch()));
            self.new_keys.push(route.clone());
        }
        true
    }

    /// Whether every write the replica holds sealed of `route` opened with the keys it held at
    /// epoch `before`, which `keys` still hold.
    fn opened_before(
        &self,
        route: &DocumentRoute,
        keys: &key_group::ContentKeys,
        before: u64,
    ) -> bool {
        let Ok(workspace) = route.workspace_id() else {
            return false;
        };
        keys.holds_epoch(before)
            && self
                .engine
                .owner()
                .with_current(workspace, |view| {
                    Ok(view.peer().publication().sealed_keys_available(
                        &route.app_id,
                        &route.document,
                        |sealed| {
                            matches!(
                                sealed.scheme,
                                collab_types::EncryptionScheme::ContentKey { .. }
                            ) && sealed.epoch <= before
                                && keys.holds_epoch(sealed.epoch)
                        },
                    ))
                })
                .unwrap_or(false)
    }

    fn report_new_keys(&self) {
        if !self.new_keys.is_empty() {
            self.report_keys();
        }
    }

    /// Say the key state when it changed.
    fn report_keys(&self) {
        let key_state = self.describe_keys();
        if self.reports.read(|status| status.key_state != key_state) {
            self.reports.change(|status| status.key_state = key_state);
        }
    }

    /// Store channel state and held edits when either changed.
    fn persist_keys(&self, held_changed: bool) {
        if held_changed {
            self.key_state.hold(
                self.held
                    .lock()
                    .iter()
                    .flat_map(|(route, writes)| {
                        writes.iter().map(|write| (route.clone(), write.clone()))
                    })
                    .collect(),
            );
        }
        if let Err(error) = self.key_state.save(self.store.as_ref()) {
            self.fail_on(FailureKind::Storage, "storage-transaction-failed", error);
        }
    }

    /// The key state's diagnostics: each workspace, route, live session and held write.
    fn describe_keys(&self) -> String {
        let mut channels = self
            .workspaces
            .iter()
            .filter_map(|workspace_id| {
                self.with_workspace(*workspace_id, |view| {
                    Ok(format!(
                        "workspace {}: joined through {}",
                        view.peer().workspace_id(),
                        hex::encode(&view.workspace_host().device_public_key[..4])
                    ))
                })?
                .ok()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for (route, keys) in &self.route_keys {
            let state = match keys {
                RouteKeys::Waiting => "waiting",
                RouteKeys::Engine(..) => "ready",
                RouteKeys::Handed => "handed",
            };
            let _ = write!(
                channels,
                "\nroute {}/{}: {state};",
                route.app_id, route.document
            );
        }
        for (route, reason) in &self.held_inbound {
            let _ = write!(
                channels,
                "\nheld {}/{}: {reason}",
                route.app_id, route.document
            );
        }
        let live_sessions = self.live_sessions.describe();
        if !live_sessions.is_empty() {
            channels.push('\n');
            channels.push_str(&live_sessions);
        }
        if self.refused.0 > 0 {
            let _ = write!(channels, "\nrefused {}: {}", self.refused.0, self.refused.1);
        }
        channels
    }

    // ---- live frames ----

    /// Send, seal or open one live frame.
    fn apply_live(&mut self, command: Cmd) {
        use crate::held_live::Waiting;
        let waiting = match command {
            Cmd::Live(workspace_id, frame) => Waiting::Send {
                workspace_id,
                frame,
            },
            Cmd::SealLive(workspace_id, frame, done) => Waiting::Seal {
                workspace_id,
                frame,
                done,
            },
            _ => return,
        };
        self.live_frame(waiting, true);
    }

    /// Seal or open one live frame and deliver it; with `hold`, keep a
    /// frame whose keys are missing until they arrive.
    fn live_frame(&mut self, waiting: crate::held_live::Waiting, hold: bool) {
        use crate::held_live::Waiting;
        match waiting {
            Waiting::Send {
                workspace_id,
                frame,
            } => match self.seal_live(workspace_id, &frame) {
                Opening::Opened(sealed) => {
                    if let Some(joined) = self.live_sessions.joined_mut(workspace_id) {
                        let sequence = joined.activity_seq.wrapping_add(1);
                        joined.activity_seq = sequence;
                        let _ = joined.live_session.publish(
                            tilo_live::live::Track::Activity,
                            sequence,
                            &sealed,
                        );
                    }
                }
                Opening::NoKeysOf(record) if hold => self.held_live.hold_outbound(
                    workspace_id,
                    record,
                    Waiting::Send {
                        workspace_id,
                        frame,
                    },
                ),
                Opening::NoKeys(_) | Opening::NoKeysOf(_) | Opening::Dropped => {}
            },
            Waiting::Seal {
                workspace_id,
                frame,
                done,
            } => match self.seal_live(workspace_id, &frame) {
                Opening::Opened(sealed) => {
                    let _ = done.send(Some(sealed));
                }
                Opening::NoKeysOf(record) if hold => self.held_live.hold_outbound(
                    workspace_id,
                    record,
                    Waiting::Seal {
                        workspace_id,
                        frame,
                        done,
                    },
                ),
                Opening::NoKeys(_) | Opening::NoKeysOf(_) | Opening::Dropped => {
                    let _ = done.send(None);
                }
            },
            Waiting::Receive {
                workspace_id,
                from,
                frame,
            } => match self.open_live(workspace_id, &frame) {
                Opening::Opened(opened) if host_voice::live_session_narrates_a_run(&opened) => {}
                Opening::Opened(opened) => self.reports.emit(&Event::Live {
                    ws: workspace_id,
                    from,
                    frame: opened,
                }),
                Opening::NoKeys(record) if hold => self.held_live.hold_inbound(
                    workspace_id,
                    record,
                    Waiting::Receive {
                        workspace_id,
                        from,
                        frame,
                    },
                ),
                Opening::NoKeys(_) | Opening::NoKeysOf(_) | Opening::Dropped => {}
            },
        }
    }

    /// `route`'s keys arrived: seal, send and open the live frames that waited for them.
    fn release_live(&mut self, route: &DocumentRoute) {
        let Ok(workspace_id) = route.workspace_id() else {
            return;
        };
        let record = tilo_server_protocol::live::LiveResource {
            app: route.app_id.clone(),
            record: route.document.clone(),
        };
        let released: Vec<_> = self.held_live.release(workspace_id, record).collect();
        for waiting in released {
            self.live_frame(waiting, false);
        }
    }

    /// Seal canonical live `frame` of `workspace_id` with the content cipher of the record it
    /// names.
    fn seal_live(&self, workspace_id: WorkspaceId, frame: &[u8]) -> Opening {
        let Some(frame) = tilo_server_protocol::live::LiveFrame::decode(frame).ok() else {
            return Opening::Dropped;
        };
        let (_, _, record) = frame.seal_tags();
        let keys = self.keyed_routes().find_map(|route| {
            let names = tilo_server_protocol::live::LiveResource {
                app: route.app_id.clone(),
                record: route.document.clone(),
            };
            (names == *record && route.workspace_id().ok() == Some(workspace_id))
                .then(|| self.keys_of(route))
                .flatten()
        });
        let Some(keys) = keys else {
            return Opening::NoKeysOf(record.clone());
        };
        tilo_server_protocol::live::SealedLiveFrame::seal(
            &frame,
            &workspace_id.to_string(),
            |artifact, retention, cleartext| {
                keys.seal_content(artifact, retention, cleartext)
                    .map_err(|error| error.to_string())
            },
        )
        .map_or(Opening::Dropped, Opening::Opened)
    }

    /// Open sealed live `frame` of `workspace_id` with the content cipher of the record it names
    /// into the canonical frame.
    fn open_live(&self, workspace_id: WorkspaceId, frame: &[u8]) -> Opening {
        let Ok(sealed) = tilo_server_protocol::live::SealedLiveFrame::decode(frame) else {
            return Opening::Dropped;
        };
        let bound = self.keyed_routes().find_map(|route| {
            let resource = tilo_server_protocol::live::LiveResource {
                app: route.app_id.clone(),
                record: route.document.clone(),
            };
            (resource.digest() == sealed.record && route.workspace_id().ok() == Some(workspace_id))
                .then_some((resource, route))
        });
        let Some((resource, route)) = bound else {
            return Opening::NoKeys(sealed.record);
        };
        sealed
            .open(
                &workspace_id.to_string(),
                &resource,
                |artifact, metadata, payload| {
                    let keys = self
                        .keys_for_ciphertext(route, *metadata)
                        .ok_or_else(|| "the document's keys have not arrived".to_owned())?;
                    keys.open_content(artifact, metadata, payload)
                        .map(|plaintext| plaintext.to_vec())
                        .map_err(|error| error.to_string())
                },
            )
            .ok()
            .and_then(|frame| frame.encode().ok())
            .map_or(Opening::Dropped, Opening::Opened)
    }

    /// Say `frames`, one burst, as one group of `workspace_id`'s presence track.
    fn publish_presence(
        &mut self,
        workspace_id: WorkspaceId,
        frames: Vec<collab_types::presence::Presence>,
    ) {
        let Some(joined) = self.live_sessions.joined_mut(workspace_id) else {
            return;
        };
        let numbered: Vec<(u32, collab_types::presence::Presence)> = frames
            .into_iter()
            .map(|frame| {
                joined.presence_seq = joined.presence_seq.wrapping_add(1);
                (joined.presence_seq, frame)
            })
            .collect();
        let wanted = joined.live_session.call_keys_wanted();
        let _ = tilo_live::presence::say(&joined.live_session, &numbered);
        // A page that joined another call needs that call's keys: its live session is settled
        // again, which asks for them.
        if joined.live_session.call_keys_wanted() != wanted {
            self.live_sessions_due.insert(workspace_id);
        }
    }

    // ---- this device's writes ----

    /// `route` holds new writes of this device, held as the session was told of them.
    fn publish(&mut self, route: &DocumentRoute) {
        if route.workspace_id().is_err() {
            self.held.lock().remove(route);
            self.fail(
                "protocol-rejected",
                "A document names an invalid workspace.",
            );
            return;
        }
        self.held_since
            .entry(route.clone())
            .or_insert_with(tilo_clock::now);
        self.open_channel(route);
        if self.keys_of(route).is_some() {
            self.outbound_due = true;
        } else {
            self.persist_keys(true);
        }
    }

    /// Draft and publish the writes held for their route's keys, once they may.
    async fn commit_held(&mut self) {
        self.commit_signed_waiting();
        if !std::mem::take(&mut self.outbound_due) {
            return;
        }
        let ready: Vec<DocumentRoute> = self
            .held
            .lock()
            .keys()
            .filter(|route| self.keys_of(route).is_some())
            .cloned()
            .collect();
        let mut refused = BTreeMap::new();
        let mut drained = false;
        for route in ready {
            loop {
                let Some(mutation) = self
                    .held
                    .lock()
                    .get(&route)
                    .and_then(|writes| writes.first())
                    .cloned()
                else {
                    break;
                };
                match self.commit_held_write(&route, &mutation).await {
                    None => break,
                    Some(Ok(())) => {
                        self.drop_held(&route, 1);
                        drained = true;
                    }
                    Some(Err(reason)) => {
                        refused.insert(route.clone(), reason);
                        let held = self.held.lock().remove(&route);
                        if held.is_some_and(|held| !held.is_empty()) {
                            drained = true;
                        }
                        break;
                    }
                }
            }
        }
        if drained && self.held.lock().is_empty() {
            self.reports.change(|status| {
                status.failures.retain(|failure| {
                    failure.kind != FailureKind::Session || !HELD_CODES.contains(&failure.code)
                });
            });
        }
        {
            let held = self.held.lock();
            self.held_since.retain(|route, _| held.contains_key(route));
            self.kept_why.retain(|route, _| held.contains_key(route));
        }
        self.persist_keys(drained);
        self.release_barriers(&refused);
    }

    /// Take `count` writes of `route` out of what this session holds of it.
    fn drop_held(&self, route: &DocumentRoute, count: usize) {
        let mut held = self.held.lock();
        if let Some(writes) = held.get_mut(route) {
            writes.drain(..count.min(writes.len()));
            if writes.is_empty() {
                held.remove(route);
            }
        }
    }

    /// Publish `op`, a write of this device that its holder signed and this replica admitted
    /// already, as its outgoing ownership.
    fn publish_signed(&mut self, route: &DocumentRoute, op: collab_types::SignedOp) {
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
            self.fail(
                "protocol-rejected",
                "A document names an invalid workspace.",
            );
            return;
        };
        match self.publish_request(route, workspace, op) {
            Publishing::Refused(refusal) => {
                let refused = BTreeMap::from([(route.clone(), refusal.to_string())]);
                self.note_refused(route, &refusal);
                self.release_barriers(&refused);
            }
            Publishing::History => self.note_history(route),
            Publishing::Owned | Publishing::Waiting => {}
        }
    }

    /// Commit the signed writes still waiting for what they need, in the turn.
    fn commit_signed_waiting(&mut self) {
        if self.pending_signed.is_empty() {
            return;
        }
        let waiting = std::mem::take(&mut self.pending_signed);
        let mut refused = BTreeMap::new();
        let mut changed = false;
        for (route, requests) in waiting {
            let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
                continue;
            };
            for envelope in requests {
                match self.publish_request(&route, workspace, envelope) {
                    Publishing::Refused(refusal) => {
                        self.note_refused(&route, &refusal);
                        refused.insert(route.clone(), refusal.to_string());
                        changed = true;
                        break;
                    }
                    Publishing::History => self.note_history(&route),
                    Publishing::Owned => changed = true,
                    Publishing::Waiting => {}
                }
            }
        }
        if changed {
            self.release_barriers(&refused);
        }
    }

    /// Take `envelope`, this device's signed write of `route`, into its workspace: admitted where
    /// the replica does not hold it yet, and kept in the record as a frame its link carries until
    /// the host answers it.
    fn publish_request(
        &mut self,
        route: &DocumentRoute,
        workspace: tilo_desktop_model::WorkspaceId,
        envelope: collab_types::SignedOp,
    ) -> Publishing {
        let now = crate::clock::system_time(tilo_clock::unix_now_ns());
        let retry = envelope.clone();
        match self.engine.owner().admit_writes(
            workspace,
            vec![tilo_peer_state::owner::WorkspaceDocumentWrite {
                app: route.app_id.clone(),
                document: route.document.clone(),
                readers: None,
                envelope,
            }],
            now,
        ) {
            Ok(()) => {
                self.owes(route);
                self.landed(route, Some(self.config.cert.device_public_key));
                self.count_ownership();
                Publishing::Owned
            }
            Err(
                refusals @ (tilo_peer_state::owner::WriteRefusal::Waiting(_)
                | tilo_peer_state::owner::WriteRefusal::Unsigned(_)),
            ) => {
                tracing::debug!(app = %route.app_id, record = %route.document, %refusals,
                    "a signed write waits to be published");
                self.pending_signed
                    .entry(route.clone())
                    .or_default()
                    .push(retry);
                Publishing::Waiting
            }
            Err(refusal @ tilo_peer_state::owner::WriteRefusal::HistoryFull { .. }) => {
                tracing::debug!(app = %route.app_id, record = %route.document, %refusal,
                    "a signed write waits for its record's history to be compacted");
                self.pending_signed
                    .entry(route.clone())
                    .or_default()
                    .push(retry);
                Publishing::History
            }
            Err(refusal) => {
                tracing::warn!(app = %route.app_id, record = %route.document, %refusal,
                    "the workspace refused a write of this device for good");
                Publishing::Refused(refusal)
            }
        }
    }

    /// Draft, seal, sign and admit `mutation` of `route` through this device's holder, then
    /// publish the signed op it answered.
    async fn commit_held_write(
        &mut self,
        route: &DocumentRoute,
        mutation: &collab_types::DocumentMutation,
    ) -> Option<Result<(), String>> {
        let keys = self.keys_of(route)?;
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
            const INVALID: &str = "A document names an invalid workspace.";
            self.fail("protocol-rejected", INVALID);
            return Some(Err(INVALID.to_owned()));
        };
        let snapshot = matches!(mutation, collab_types::DocumentMutation::Snapshot(_));
        let changes = match mutation {
            collab_types::DocumentMutation::Snapshot(bytes)
            | collab_types::DocumentMutation::Incremental(bytes) => bytes,
        };
        let founding = snapshot && self.holds_none_of(route);
        let written = self
            .engine
            .document_writes()
            .write(
                &route.cluster_id,
                changes,
                tilo_peer_state::workspace_documents::DocumentWriting {
                    app: &route.app_id,
                    document: &route.document,
                    readers: None,
                    founding,
                    snapshot,
                    sealed: Some(&keys),
                },
            )
            .await;
        match written {
            Ok(receipt) => match self.publish_request(route, workspace, receipt.envelope) {
                Publishing::Owned | Publishing::Waiting => Some(Ok(())),
                Publishing::History => {
                    self.note_history(route);
                    Some(Ok(()))
                }
                Publishing::Refused(refusal) => Some(self.refused_write(route, &refusal)),
            },
            Err(tilo_peer_state::owner::WriteRefusal::HistoryFull { .. }) => {
                self.history_full(route)
            }
            Err(tilo_peer_state::owner::WriteRefusal::Unsigned(error)) => {
                tracing::warn!(%error, "writes wait for this device to sign them");
                self.keep_for(route, crate::Waits::Signature);
                self.fail(
                    SIGNING_FAILED,
                    "This device could not sign your latest changes. They are kept on this device.",
                );
                None
            }
            Err(tilo_peer_state::owner::WriteRefusal::Waiting(prerequisite)) => {
                self.keep_for(route, crate::Waits::Prerequisite(prerequisite.to_string()));
                tracing::debug!(app = %route.app_id, record = %route.document, %prerequisite,
                    "writes wait for what the workspace has not admitted here yet");
                None
            }
            Err(
                refusal @ (tilo_peer_state::owner::WriteRefusal::Refused(_)
                | tilo_peer_state::owner::WriteRefusal::Declined { .. }
                | tilo_peer_state::owner::WriteRefusal::ChangeRefused(_)),
            ) => Some(self.refused_write(route, &refusal)),
        }
    }

    fn history_full(&mut self, route: &DocumentRoute) -> Option<Result<(), String>> {
        self.keep_for(route, crate::Waits::History);
        self.note_history(route);
        None
    }

    fn note_history(&self, route: &DocumentRoute) {
        tracing::warn!(app = %route.app_id, record = %route.document,
            "writes wait for their record's history to be compacted");
        self.fail(
            RECORD_FULL,
            "This record is too large to share your latest changes. They are kept on this device.",
        );
    }

    /// Say that the workspace refused this device's write to `route` for good.
    fn refused_write(
        &self,
        route: &DocumentRoute,
        refusal: &tilo_peer_state::owner::WriteRefusal,
    ) -> Result<(), String> {
        self.note_refused(route, refusal);
        Err(refusal.to_string())
    }

    fn note_refused(&self, route: &DocumentRoute, refusal: &tilo_peer_state::owner::WriteRefusal) {
        if let tilo_peer_state::owner::WriteRefusal::ChangeRefused(refused) = refusal {
            tracing::debug!(app = %route.app_id, document = %route.document, %refused,
                "the workspace refused a write of this device before it went");
            self.reports.emit(&crate::Event::Refused {
                route: route.clone(),
                changes: refused.changes.clone(),
                refusal: refused.refusal.clone(),
            });
        } else {
            tracing::warn!(app = %route.app_id, record = %route.document, %refusal,
                "the workspace refused a write of this device for good");
        }
        self.fail("write-refused", refusal.to_string());
    }

    /// Send `workspace_id`'s frames of this device's own that its link has not sent. A document
    /// write stays in the record until the host answers it, so one unanswered when the link or
    /// the device goes is sent again on the next link; any other frame leaves the record once a
    /// link carried it, as nothing answers it.
    fn send_ownership(&mut self, workspace_id: WorkspaceId) {
        let Some(link) = self.links.get(&workspace_id) else {
            return;
        };
        let owner = self.engine.owner();
        for (id, bytes, ..) in self.ownership(workspace_id) {
            if !self.sent.insert((workspace_id, id)) {
                continue;
            }
            let message = match neighbour_wire::NeighbourMsg::decode(&bytes) {
                Ok(message) => message,
                Err(error) => {
                    tracing::warn!(%error, "a frame of this device's own did not decode");
                    continue;
                }
            };
            let answered = matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. });
            if link.sync.send(message).is_err() {
                self.sent.remove(&(workspace_id, id));
            } else if !answered && let Err(error) = owner.sent(workspace_id, &bytes) {
                self.fail_on(FailureKind::Storage, "storage-transaction-failed", error);
            }
        }
        self.count_ownership();
    }

    /// Count what this device's ownership still owes its hosts, and follow the writes that left
    /// it unanswered.
    fn count_ownership(&mut self) {
        let pending = self
            .workspaces
            .iter()
            .map(|workspace_id| self.engine.owner().frames(*workspace_id).len())
            .sum();
        self.reports
            .change(|status| status.outgoing_pending = pending);
        self.follow_ownership();
    }

    fn follow_ownership(&mut self) {
        if self.barriers.is_empty() {
            return;
        }
        let every = self
            .barriers
            .iter()
            .any(|(_, _, _, scope, _)| matches!(scope, Scope::Session))
            .then(|| self.owed(&Scope::Session));
        let (waiting, done): (Vec<_>, Vec<_>) = std::mem::take(&mut self.barriers)
            .into_iter()
            .map(|(mut left, refused, complete, scope, heard)| {
                let own = (!matches!(scope, Scope::Session)).then(|| self.owed(&scope));
                if let Some(owed) = own.as_ref().or(every.as_ref())
                    && !left.is_subset(owed)
                {
                    left.clone_from(owed);
                }
                (left, refused, complete, scope, heard)
            })
            .partition(|(left, ..)| !left.is_empty());
        self.barriers = waiting;
        for (_, refused, complete, ..) in done {
            let _ = complete.send(refused.map_or(Ok(()), Err));
        }
    }

    /// Complete `done` once every write said so far that `scope` answers for is durable at this
    /// device's holder.
    fn barrier(&mut self, done: Completion, scope: Scope, heard: Hears) {
        let held: BTreeSet<DocumentRoute> = self
            .waiting_routes()
            .into_iter()
            .filter(|route| scope.covers(route))
            .collect();
        let unheard: Vec<DocumentRoute> = self
            .unheard_refusals
            .keys()
            .filter(|route| scope.covers(route) && hears(heard.as_ref(), route))
            .cloned()
            .collect();
        let refused = unheard
            .iter()
            .filter_map(|route| self.unheard_refusals.remove(route))
            .next_back();
        if let Some(refused) = refused {
            let _ = done.send(Err(refused));
        } else if held.is_empty() {
            self.await_ownership(done, None, scope, heard);
        } else {
            self.held_barriers.push((held, done, scope, heard));
        }
    }

    /// The routes this session still holds a write on.
    fn waiting_routes(&self) -> BTreeSet<DocumentRoute> {
        let mut routes: BTreeSet<DocumentRoute> = self.held.lock().keys().cloned().collect();
        routes.extend(self.pending_signed.keys().cloned());
        routes
    }

    /// Complete `done` once every write in this device's ownership is answered.
    fn await_ownership(
        &mut self,
        done: Completion,
        refused: Option<String>,
        scope: Scope,
        heard: Hears,
    ) {
        let owed = self.owed(&scope);
        if owed.is_empty() {
            let _ = done.send(refused.map_or(Ok(()), Err));
        } else {
            self.barriers.push((owed, refused, done, scope, heard));
        }
    }

    /// Every write in this device's ownership, sendable now or not, that `scope` answers for.
    fn owed(&self, scope: &Scope) -> BTreeSet<[u8; 32]> {
        self.workspaces
            .iter()
            .filter(|workspace_id| scope.reaches(**workspace_id))
            .flat_map(|workspace_id| {
                let owed: Vec<[u8; 32]> = self
                    .ownership(*workspace_id)
                    .into_iter()
                    .map(|(id, ..)| id)
                    .collect();
                match scope {
                    Scope::Session | Scope::Workspaces(_) => owed,
                    Scope::Documents(documents) => self.owed_to(*workspace_id, documents, owed),
                }
            })
            .collect()
    }

    /// Those of `owed` that a barrier of `documents` waits for.
    fn owed_to(
        &self,
        workspace: WorkspaceId,
        documents: &BTreeSet<DocumentRoute>,
        mut owed: Vec<[u8; 32]>,
    ) -> Vec<[u8; 32]> {
        if owed.is_empty() {
            return owed;
        }
        for entry in self.ownership(workspace) {
            if written_to(&entry.1).is_some_and(|(route, _)| !documents.contains(&route)) {
                owed.retain(|id| *id != entry.0);
            }
        }
        owed
    }

    /// Whether this device's replica holds nothing of `route`'s document.
    fn holds_none_of(&self, route: &DocumentRoute) -> bool {
        tilo_desktop_model::WorkspaceId::parse(&route.cluster_id).is_some_and(|workspace| {
            self.engine.owner().with_current(workspace, |view| {
                Ok(view.peer().holds_document(&route.app_id, &route.document))
            }) == Ok(false)
        })
    }

    fn keep_for(&mut self, route: &DocumentRoute, waits: crate::Waits) {
        self.kept_why.insert(route.clone(), waits);
    }

    fn owes(&mut self, route: &DocumentRoute) {
        self.kept_why.remove(route);
        self.owed_since
            .entry(route.clone())
            .or_insert_with(tilo_clock::now);
    }

    /// The frames of this device's own that `workspace`'s record keeps, oldest first: the hash
    /// its host answers it by, its bytes, the channel it was written to and how long the
    /// workspace retains it.
    fn ownership(&self, workspace: WorkspaceId) -> Vec<HeldEntry> {
        self.engine
            .owner()
            .frames(workspace)
            .into_iter()
            .filter_map(|bytes| {
                let message = neighbour_wire::NeighbourMsg::decode(&bytes).ok()?;
                let (channel, retention) = neighbour_wire::retention(&message, false)?;
                let id = *blake3::hash(&postcard::to_allocvec(&message).ok()?).as_bytes();
                Some((id, bytes, channel, retention))
            })
            .collect()
    }

    /// The documents of `scope`'s workspaces that hold writes of this device not committed to
    /// its replica yet, each with whether the first of them is a change.
    fn held_in(&self, scope: Option<&BTreeSet<WorkspaceId>>) -> Vec<(DocumentRoute, bool)> {
        self.held
            .lock()
            .iter()
            .filter(|(route, _)| within(scope, route))
            .map(|(route, writes)| {
                let changes = writes.first().is_some_and(|write| {
                    matches!(write, collab_types::DocumentMutation::Incremental(_))
                });
                (route.clone(), changes)
            })
            .collect()
    }

    /// What this session still keeps of what this device wrote to `workspaces`.
    fn keeps(&mut self, workspaces: &BTreeSet<WorkspaceId>) -> Vec<Kept> {
        let now = tilo_clock::now();
        let scope = Some(workspaces);
        let mut kept: Vec<Kept> = self
            .held_in(scope)
            .into_iter()
            .map(|(route, changes)| {
                let waits = if self.keys_of(&route).is_none() {
                    crate::Waits::Keys
                } else if changes && self.holds_none_of(&route) {
                    crate::Waits::Document
                } else {
                    self.kept_why
                        .get(&route)
                        .cloned()
                        .unwrap_or(crate::Waits::Commit)
                };
                let age = self
                    .held_since
                    .get(&route)
                    .map_or(std::time::Duration::ZERO, |since| {
                        now.saturating_duration_since(*since)
                    });
                Kept { route, waits, age }
            })
            .collect();
        let owed: BTreeSet<DocumentRoute> = self
            .workspaces
            .iter()
            .filter(|workspace_id| workspaces.contains(*workspace_id))
            .flat_map(|workspace_id| self.ownership(*workspace_id))
            .filter_map(|entry| written_to(&entry.1).map(|(route, _)| route))
            .collect();
        self.owed_since
            .retain(|route, _| !within(scope, route) || owed.contains(route));
        kept.extend(owed.into_iter().map(|route| {
            let age = self
                .owed_since
                .get(&route)
                .map_or(std::time::Duration::ZERO, |since| {
                    now.saturating_duration_since(*since)
                });
            Kept {
                route,
                waits: crate::Waits::Holder,
                age,
            }
        }));
        kept
    }

    /// Move each held barrier whose routes hold nothing any more on to the ownership.
    fn release_barriers(&mut self, refused: &BTreeMap<DocumentRoute, String>) {
        let heard_by = |(routes, _, _, heard): &HeldBarrier| {
            routes
                .iter()
                .find(|route| refused.contains_key(*route) && hears(heard.as_ref(), route))
                .and_then(|route| refused.get(route))
                .cloned()
        };
        for (route, why) in refused {
            if !self
                .held_barriers
                .iter()
                .any(|(routes, _, _, heard)| routes.contains(route) && hears(heard.as_ref(), route))
            {
                self.unheard_refusals.insert(route.clone(), why.clone());
            }
        }
        let (heard, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held_barriers)
            .into_iter()
            .partition(|barrier| heard_by(barrier).is_some());
        self.held_barriers = waiting;
        for barrier in heard {
            if let Some(why) = heard_by(&barrier) {
                let _ = barrier.1.send(Err(why));
            }
        }
        let held = self.waiting_routes();
        let (waiting, released): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held_barriers)
            .into_iter()
            .partition(|(routes, ..)| routes.iter().any(|route| held.contains(route)));
        self.held_barriers = waiting;
        for (_, done, scope, heard) in released {
            self.await_ownership(done, None, scope, heard);
        }
    }

    /// Retire every write the host answered from ownership, in one owner turn per workspace,
    /// then tell whoever waited for them.
    fn retire_answered(&mut self) {
        if self.answered.is_empty() {
            return;
        }
        let answered = std::mem::take(&mut self.answered);
        let owner = self.engine.owner();
        for answer in &answered {
            if let Err(error) = owner.sent(answer.workspace, &answer.frame) {
                self.fail_on(FailureKind::Storage, "storage-transaction-failed", error);
            }
        }
        self.outbound_due = true;
        for AnsweredWrite {
            id,
            outcome,
            withdrawn,
            ..
        } in answered
        {
            let route = match &withdrawn {
                Some(crate::Event::Refused { route, .. }) => Some(route.clone()),
                _ => None,
            };
            if let Some(refused) = withdrawn {
                self.engine.arrivals().landed();
                self.reports.emit(&refused);
            }
            let refusal = outcome.err().map(|refusal| refusal_text(&refusal));
            let mut heard = false;
            let (waiting, done): (Vec<_>, Vec<_>) = std::mem::take(&mut self.barriers)
                .into_iter()
                .map(|(mut owed, mut refused, complete, scope, hearing)| {
                    if owed.remove(&id)
                        && route
                            .as_ref()
                            .is_none_or(|route| hears(hearing.as_ref(), route))
                    {
                        heard = true;
                        if refused.is_none() {
                            refused.clone_from(&refusal);
                        }
                    }
                    (owed, refused, complete, scope, hearing)
                })
                .partition(|(owed, refused, ..)| !owed.is_empty() && refused.is_none());
            self.barriers = waiting;
            for (_, refused, complete, ..) in done {
                let _ = complete.send(refused.map_or(Ok(()), Err));
            }
            if !heard && let (Some(route), Some(refusal)) = (route, refusal) {
                self.unheard_refusals.insert(route, refusal);
            }
        }
        self.count_ownership();
    }

    /// Send a remote-action frame to device `to`: the host's action service carries it, and its
    /// answer comes back as what the session heard.
    fn remote_action(&self, workspace_id: WorkspaceId, to: [u8; 32], frame: &[u8]) {
        let Some(link) = self.actions.clone() else {
            self.reports.emit(&Event::Remote(Heard::Undeliverable {
                workspace_id,
                to,
                refusal: tilo_server_protocol::refusal::Refusal::NotReady,
            }));
            return;
        };
        let Some(message) = tilo_actions::remote::RemoteActionFrame::decode(frame)
            .ok()
            .map(|decoded| decoded.message)
        else {
            self.reports.emit(&Event::Remote(Heard::Undeliverable {
                workspace_id,
                to,
                refusal: tilo_server_protocol::refusal::Refusal::Invalid(
                    "the action frame does not decode".into(),
                ),
            }));
            return;
        };
        let signer = Arc::clone(&self.signer);
        let device = self.config.cert.device_public_key;
        let user = self.config.cert.user_public_key;
        let reports = Arc::clone(&self.reports);
        let cert = self.config.cert.clone();
        // A request naming no workspace asks the host itself: it proves its device by the
        // invocation it carries, which this device signs, and the link names the host scope.
        let host_scope = workspace_id == HOST_ITSELF;
        n0_future::task::spawn(async move {
            let (scope, message) = if host_scope {
                (
                    String::new(),
                    signed_host_message(message, &cert, signer.as_ref()).await,
                )
            } else {
                (workspace_id.to_string(), message)
            };
            match ask_action(&link, signer, device, &scope, to, message).await {
                Ok(frame) => reports.emit(&Event::Remote(Heard::Action {
                    workspace_id,
                    user,
                    from: to,
                    frame,
                })),
                Err(refusal) => reports.emit(&Event::Remote(Heard::Undeliverable {
                    workspace_id,
                    to,
                    refusal,
                })),
            }
        });
    }

    /// Ask the workspace on `workspace_id` for content its records reference.
    fn want_content(&self, workspace_id: WorkspaceId, request: &[u8]) {
        let Some(link) = self.links.get(&workspace_id) else {
            return;
        };
        if let Ok(message) = neighbour_wire::NeighbourMsg::decode(request) {
            let _ = link.sync.send(message);
        }
    }
}

/// `message`'s invocation signed by this device for the host scope, when it carries none: a
/// request that names no workspace proves its device by the invocation, where no workspace's
/// roster names it. Anything else, and one that cannot be signed, goes as it is.
async fn signed_host_message(
    message: tilo_actions::remote::RemoteActionMessage,
    cert: &DeviceCert,
    signer: &dyn tilo_server_client::DeviceSigner,
) -> tilo_actions::remote::RemoteActionMessage {
    use tilo_actions::remote::RemoteActionMessage;
    let RemoteActionMessage::Invoke(mut request) = message else {
        return message;
    };
    if request.signed.is_none()
        && let Ok(signed) = tilo_server_protocol::invocation::sign_invocation(
            "",
            &request,
            cert,
            signer,
            tilo_clock::unix_now_ns(),
        )
        .await
    {
        request.signed = Some(Box::new(signed));
    }
    RemoteActionMessage::Invoke(request)
}

/// What this session reports when the host would not take a change it read.
fn refusal_text(refusal: &tilo_server_protocol::refusal::Refusal) -> String {
    format!("the workspace refused the change: {refusal}")
}

/// Ask `to`, a device of `workspace`, `message` through the host's action service, and answer the
/// frame it replies with. An empty `workspace` asks the host itself: its own operations, which
/// the host admits for a device of its person by the signed invocation the request carries. A
/// scope a host admits a device with no certificate to (`tilo.device`'s tickets) is asked the same
/// way, before the device holds any session there.
///
/// # Errors
///
/// The device could not sign or encode the request, or the link carried no answer.
pub async fn ask_action(
    link: &tilo_server_client::link::Link,
    signer: Arc<dyn tilo_server_client::DeviceSigner>,
    device: [u8; 32],
    workspace: &str,
    to: [u8; 32],
    message: tilo_actions::remote::RemoteActionMessage,
) -> Result<Vec<u8>, tilo_server_protocol::refusal::Refusal> {
    use neighbour_wire::actions::{ActionServiceFrame, DeviceAction, SignedDeviceAction};
    let signed_action = SignedDeviceAction::signed(
        &DeviceAction {
            workspace: workspace.to_owned(),
            request_id: uuid::Uuid::new_v4(),
            source: device,
            target: to,
            message,
            placement: None,
        },
        tilo_clock::unix_now_ns(),
        signer.as_ref(),
    )
    .await
    .map_err(|error| {
        tilo_server_protocol::refusal::Refusal::Other(format!(
            "this device could not sign the action: {error}"
        ))
    })?;
    let bytes = postcard::to_allocvec(&ActionServiceFrame::Action(Box::new(signed_action)))
        .map_err(|error| {
            tilo_server_protocol::refusal::Refusal::Invalid(format!(
                "the action does not encode: {error}"
            ))
        })?;
    let (mut send, mut recv) = link
        .open_bi()
        .await
        .map_err(|_| tilo_server_protocol::refusal::Refusal::NotReady)?;
    tilo_server_client::frame::write_frame(&mut send, &bytes)
        .await
        .map_err(|_| tilo_server_protocol::refusal::Refusal::NotReady)?;
    send.finish();
    let answer = tilo_server_client::frame::read_frame(&mut recv, MAX_ACTION_FRAME)
        .await
        .map_err(|_| tilo_server_protocol::refusal::Refusal::NotReady)?;
    Ok(answer)
}

/// How long the host holds one ask about a ticket call before it answers with how it stands.
const TICKET_WAIT_MS: u32 = 20_000;

/// Call one of `tilo.device`'s ticket actions, `action` with `argument`, on the host `link`
/// reaches (`host`, its device key), in the scope a device with no certificate is admitted to
/// ([`tilo_device_identity::login::SCOPE`]), as the key `signer` holds (`device`): a tab that is
/// signing in, or logging in with a ticket, holds no certificate there yet. Answers the action's
/// value once it settled, or the host's refusal in its own words. A call the host takes time over,
/// a certify the person still decides, is followed until `follow_for` passes.
///
/// # Errors
///
/// The device could not sign the call, the link carried no answer, or the call did not settle in
/// time.
pub async fn ask_ticket(
    link: &tilo_server_client::link::Link,
    signer: Arc<dyn tilo_server_client::DeviceSigner>,
    device: [u8; 32],
    host: [u8; 32],
    action: &str,
    argument: serde_json::Map<String, serde_json::Value>,
    follow_for: std::time::Duration,
) -> Result<Result<serde_json::Value, String>, tilo_server_protocol::refusal::Refusal> {
    use tilo_actions::remote::{
        InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
        RemoteActionTarget,
    };
    use tilo_device_identity::login::{PROVIDER, RESOURCE, SCOPE};
    use tilo_server_protocol::refusal::Refusal;
    let invocation = tilo_actions::InvocationId::new();
    let mut message = RemoteActionMessage::Invoke(InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(RESOURCE, action),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: Some(TICKET_WAIT_MS),
        appearance: None,
        signed: None,
    });
    let until = tilo_clock::now()
        .checked_add(follow_for)
        .unwrap_or_else(tilo_clock::now);
    loop {
        let frame = ask_action(link, Arc::clone(&signer), device, SCOPE, host, message).await?;
        let answer = RemoteActionFrame::decode(&frame)
            .map_err(|error| Refusal::Invalid(format!("the host's answer: {error}")))?
            .message;
        let status = match answer {
            RemoteActionMessage::Receipt(receipt) => receipt.status,
            RemoteActionMessage::Update {
                state: Some(state), ..
            } => state.receipt().status,
            RemoteActionMessage::Update { state: None, .. } => {
                return Err(Refusal::Other("the host forgot the ticket call".to_owned()));
            }
            _ => return Err(Refusal::Invalid("the host answered no receipt".to_owned())),
        };
        match status {
            ReceiptStatus::Completed { output } => {
                return Ok(Ok(output.value.unwrap_or(serde_json::Value::Null)));
            }
            ReceiptStatus::Failed { error } => return Ok(Err(error.message)),
            ReceiptStatus::Rejected { error } => return Ok(Err(error.to_string())),
            ReceiptStatus::Cancelled { message } => {
                return Ok(Err(
                    message.unwrap_or_else(|| "the call was stopped".to_owned())
                ));
            }
            ReceiptStatus::Accepted if tilo_clock::now() < until => {
                message = RemoteActionMessage::Observe {
                    correlation: 0,
                    invocation,
                    wait_ms: Some(TICKET_WAIT_MS),
                    seen: None,
                };
            }
            ReceiptStatus::Accepted => {
                return Err(Refusal::Other(
                    "the ticket call did not settle in time".to_owned(),
                ));
            }
        }
    }
}

/// The one link a device that holds no session with a host keeps to call its ticket actions on
/// ([`ask_ticket`]): every call goes over it, so a page that asks every few seconds (who waits to
/// be signed in) makes one link, not one each. A browser holds a socket for every WebTransport
/// session a page makes, and `WebKit` holds it after the session closed for as long as its
/// network process lives. A link that dropped is dialled again on the next call.
pub struct TicketLink {
    dialer: tilo_server_client::link::Dialer,
    link: tokio::sync::Mutex<Option<tilo_server_client::link::Link>>,
}

impl TicketLink {
    /// Keep a link to the host `host` names, dialled on the first call.
    #[must_use]
    pub fn new(host: tilo_server_client::ServerAddress) -> Self {
        Self {
            dialer: tilo_server_client::link::Dialer::new(host, None),
            link: tokio::sync::Mutex::new(None),
        }
    }

    /// [`ask_ticket`] over the link kept here, dialling it first when there is none.
    ///
    /// # Errors
    ///
    /// As [`ask_ticket`], and the host not reached.
    pub async fn ask(
        &self,
        signer: Arc<dyn tilo_server_client::DeviceSigner>,
        device: [u8; 32],
        host: [u8; 32],
        action: &str,
        argument: serde_json::Map<String, serde_json::Value>,
        follow_for: std::time::Duration,
    ) -> Result<Result<serde_json::Value, String>, tilo_server_protocol::refusal::Refusal> {
        let mut again = true;
        loop {
            let link = {
                let mut kept = self.link.lock().await;
                if let Some(link) = kept.as_ref() {
                    link.clone()
                } else {
                    let link = self
                        .dialer
                        .link(tilo_server_client::link::Purpose::Actions)
                        .await
                        .map_err(|error| {
                            tilo_server_protocol::refusal::Refusal::Other(error.to_string())
                        })?;
                    *kept = Some(link.clone());
                    link
                }
            };
            let answer = ask_ticket(
                &link,
                Arc::clone(&signer),
                device,
                host,
                action,
                argument.clone(),
                follow_for,
            )
            .await;
            match answer {
                // The kept link dropped: dial it again, once.
                Err(tilo_server_protocol::refusal::Refusal::NotReady) if again => {
                    again = false;
                    link.close("");
                    *self.link.lock().await = None;
                }
                answer => return answer,
            }
        }
    }

    /// Close the kept link.
    pub async fn close(&self) {
        if let Some(link) = self.link.lock().await.take() {
            link.close("");
        }
        self.dialer.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A visitor's page joins a workspace of another person's host without asking that host to
    /// place it, which the host refuses to anyone but its own person.
    #[test]
    fn only_the_hosts_person_asks_it_to_place_a_workspace() {
        let cert = |user: u8, device: u8| DeviceCert {
            user_public_key: [user; 32],
            device_public_key: [device; 32],
            device_label: String::new(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        let host = cert(1, 2);
        assert!(places_workspaces(&cert(1, 3), &host));
        assert!(!places_workspaces(&cert(4, 5), &host));
    }

    fn status(workspaces: &[(WorkspaceId, bool)]) -> PeerStatus {
        PeerStatus {
            phase: TransportPhase::Connected,
            authenticated: true,
            workspaces: workspaces.iter().copied().collect(),
            ..PeerStatus::connecting([0; 32])
        }
    }

    #[test]
    fn workspaces_not_admitted_within_the_timeout_are_overdue_once_dialled() {
        let since = 1_000;
        let waiting = status(&[
            (WorkspaceId::from_u128(1), true),
            (WorkspaceId::from_u128(2), false),
        ]);
        assert_eq!(
            admission_state(&waiting, None, since + ADMISSION_TIMEOUT_MS),
            Admission::Waiting
        );
        assert_eq!(
            admission_state(&waiting, Some(since), since + ADMISSION_TIMEOUT_MS - 1),
            Admission::Waiting
        );
        assert_eq!(
            admission_state(&waiting, Some(since), since + ADMISSION_TIMEOUT_MS),
            Admission::Overdue
        );
        let admitted = status(&[
            (WorkspaceId::from_u128(1), true),
            (WorkspaceId::from_u128(2), true),
        ]);
        assert_eq!(
            admission_state(&admitted, Some(since), since + ADMISSION_TIMEOUT_MS),
            Admission::Admitted
        );
        assert_eq!(
            admission_state(&status(&[]), Some(since), u64::MAX),
            Admission::Admitted
        );
    }

    #[test]
    fn a_changed_server_stops_the_page_and_a_proxys_page_never_counts() {
        let code = |error| match refusal(&error) {
            Refusal::Final(code, _) => code,
            Refusal::Unexplained => "unexplained",
            Refusal::Reachability => "reachability",
        };
        assert_eq!(code(LinkError::ServerChanged), "host-changed");
        assert_eq!(
            code(LinkError::Malformed("unexpected end of input".into())),
            "reachability"
        );
        assert_eq!(
            code(LinkError::Unavailable("status 502".into())),
            "reachability"
        );
        assert_eq!(code(LinkError::Sign("locked".into())), "reachability");
        assert_eq!(code(LinkError::Refused("io: closed".into())), "unexplained");
    }

    /// A page that lists nobody says why: each workspace's live session names what it
    /// waits for.
    #[test]
    fn a_live_session_that_was_not_reached_is_named_in_the_channel_diagnostics() {
        let mut live_sessions = live_session::LiveSessions::default();
        live_sessions.later(WorkspaceId::from_u128(7), 12_000);
        assert!(live_sessions.describe().contains("not joined"));
        assert_eq!(live_sessions.missed(WorkspaceId::from_u128(7)), 1);
        assert_eq!(
            live_sessions.missed(WorkspaceId::from_u128(7)),
            live_session::MISSES_BEFORE_NOTE
        );
        live_sessions.refused(WorkspaceId::from_u128(7), 60_000);
        assert!(live_sessions.describe().contains("does not admit"));
        let mut unasked = live_session::LiveSessions::default();
        unasked.note_unasked(
            WorkspaceId::from_u128(8),
            "This page holds no channel keys for its workspace yet.",
        );
        assert!(unasked.describe().contains("not asked"));
        unasked.began(WorkspaceId::from_u128(8), 1_000, 1, &BTreeSet::new());
        assert!(!unasked.describe().contains("not asked"));
    }
}

/// Whether `cert`'s device may ask the host certified as `host` to place a
/// workspace: only a device of the host's own person may.
#[cfg(test)]
fn places_workspaces(cert: &DeviceCert, host: &DeviceCert) -> bool {
    cert.user_public_key == host.user_public_key
}

/// The state this session's readers read, as the engine's owner holds it.
impl Task {
    /// The members of `workspace_id`'s workspace by person id, with the workspace's name.
    #[allow(
        dead_code,
        reason = "the live session and the diagnostics read it through the members' helpers"
    )]
    fn members(&self, workspace_id: WorkspaceId) -> Option<(String, Option<BTreeSet<String>>)> {
        self.with_workspace(workspace_id, |view| {
            let peer = view.peer();
            let doc = peer.admitted_workspace();
            Ok((
                peer.workspace_id().to_owned(),
                doc.and_then(tilo_peer_state::members::people),
            ))
        })?
        .ok()
    }

    /// What `workspace_id`'s governance calls each of its people, by person (root key, hex).
    fn member_names(&self, workspace_id: WorkspaceId) -> Option<BTreeMap<String, String>> {
        self.with_workspace(workspace_id, |view| {
            let list = view
                .peer()
                .governance()
                .governed_members(tilo_clock::wall())?;
            Ok(list
                .members
                .iter()
                .filter_map(|member| {
                    let person = member.id.person_id()?.to_owned();
                    let name = member.name.trim();
                    (!name.is_empty()).then(|| (person, name.to_owned()))
                })
                .collect())
        })?
        .ok()
    }

    /// Whose each of `workspace_id`'s member devices is, as its governance admitted them: device
    /// (hex) to person (root key, hex).
    fn member_devices(&self, workspace_id: WorkspaceId) -> Option<BTreeMap<String, String>> {
        self.with_workspace(workspace_id, |view| {
            let list = view
                .peer()
                .governance()
                .governed_members(tilo_clock::wall())?;
            Ok(list
                .members
                .iter()
                .filter(|member| !member.removed)
                .filter_map(|member| {
                    let person = member.id.person_id()?.to_owned();
                    let devices = list.devices.get(&member.id)?;
                    Some(
                        devices
                            .iter()
                            .map(move |device| (hex::encode(device), person.clone())),
                    )
                })
                .flatten()
                .collect())
        })?
        .ok()
    }

    /// What each call of `workspace_id` allows, by channel, as the workspace keeps it.
    fn call_rules(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<BTreeMap<String, collab_types::calls::CallRule>> {
        self.with_workspace(workspace_id, |view| Ok(view.peer().governance().calls()))?
            .ok()
    }

    /// The devices that hold the keys of `route`'s workspace, as the channels' directory names
    /// them.
    fn key_holders(
        &self,
        route: &DocumentRoute,
    ) -> Option<BTreeSet<collab_types::Ed25519PublicKey>> {
        use tilo_workspace_keys::KeyDirectory as _;
        let workspace = WorkspaceId::parse(&route.cluster_id)?;
        crate::workspace_keys::OwnerDirectory::reader(self.engine.owner().to_owned())
            .allowed_devices(workspace, tilo_clock::unix_now())
    }

    /// Where `workspace_id`'s host listens, read off the signed hint the workspace's own
    /// governance keeps for it.
    fn host(&self, workspace_id: WorkspaceId) -> Option<tilo_server_client::ServerAddress> {
        let hint = self
            .with_workspace(workspace_id, |view| {
                let host = view.workspace_host().device_public_key;
                Ok(view
                    .peer()
                    .governance()
                    .keeping_devices()
                    .into_iter()
                    .find(|hint| hint.host.device_public_key == host))
            })?
            .ok()??;
        let payload =
            tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::from_transport_hint(
                &hint,
            )
            .ok()?;
        let relay = match payload.packet_relay_url.as_str() {
            "iroh:direct" => None,
            url => Some(url.parse().ok()?),
        };
        Some(tilo_server_client::ServerAddress::through(
            payload.endpoint_addr,
            relay,
        ))
    }
}

/// The failure code of an entry the host's action service could not be asked through.
const ENTRY_UNREACHABLE: &str = "not-connected";

/// Why an entry was not answered: the failure code the page reads and what to say.
struct EntryFailure {
    code: &'static str,
    message: String,
}

impl EntryFailure {
    /// The host's action service could not be asked.
    const fn unreachable(message: String) -> Self {
        Self {
            code: ENTRY_UNREACHABLE,
            message,
        }
    }
}

/// The entry's own failure code, as its page reads it: the entry's code, or `not-admitted` when
/// the workspace refused the device the entry let in.
fn refusal_code(refusal: &tilo_server_protocol::entering::EnterRefusal) -> &'static str {
    use tilo_server_protocol::directory::EntryRefusalCode;
    use tilo_server_protocol::entering::EnterRefusal;
    match refusal {
        EnterRefusal::Workspace(_) => "not-admitted",
        EnterRefusal::Entry { refused, .. } => match refused.code {
            EntryRefusalCode::Banned => "banned",
            EntryRefusalCode::Removed => "removed",
            EntryRefusalCode::InvitationOnly => "invitation-only",
            EntryRefusalCode::Full => "full",
            EntryRefusalCode::Refused => "refused",
        },
    }
}

/// The entry's refusal, as the session reports it.
fn refused_entry(refusal: &tilo_server_protocol::entering::EnterRefusal) -> crate::Entry {
    use tilo_server_protocol::entering::EnterRefusal;
    crate::Entry::Refused {
        code: refusal_code(refusal),
        message: refusal.reason().to_owned(),
        retry_after_secs: match refusal {
            EnterRefusal::Entry {
                retry_after_secs, ..
            } => *retry_after_secs,
            EnterRefusal::Workspace(_) => None,
        },
        entry: match refusal {
            EnterRefusal::Entry { refused, .. } => Some(refused.clone()),
            EnterRefusal::Workspace(_) => None,
        },
    }
}

/// The host's `entered` answer as the session's entry state.
fn entry_state(entered: &tilo_server_protocol::entering::Entered) -> crate::Entry {
    use tilo_server_protocol::entering::Entered;
    match entered {
        Entered::Admitted {
            workspace,
            workspace_id,
            name,
            role,
            host,
        } => crate::Entry::Admitted {
            workspace: workspace.clone(),
            workspace_id: *workspace_id,
            name: name.clone(),
            role: *role,
            host: host.clone(),
        },
        Entered::Refused(refusal) => refused_entry(refusal),
    }
}

/// Ask the host for this device's fetch proxy token, in `workspace_id`: the
/// token the page sends with each request it makes through the proxy
/// ([`tilo_server_protocol::workspaces::FETCH_GRANT`]), or none when the host
/// serves no proxy or its roster refuses this device.
async fn ask_fetch_grant(
    link: &tilo_server_client::link::Link,
    signer: Arc<dyn tilo_server_client::DeviceSigner>,
    cert: &DeviceCert,
    host: [u8; 32],
    workspace_id: WorkspaceId,
) -> Option<[u8; 32]> {
    use tilo_actions::remote::{ReceiptStatus, RemoteActionFrame, RemoteActionMessage};
    let workspace = workspace_id.to_string();
    let invoke = tilo_server_protocol::workspaces::fetch_grant_invoke(
        &workspace,
        tilo_actions::InvocationId::new(),
    );
    let frame = ask_action(
        link,
        signer,
        cert.device_public_key,
        &workspace,
        host,
        RemoteActionMessage::Invoke(invoke),
    )
    .await
    .ok()?;
    let message = RemoteActionFrame::decode(&frame).ok()?.message;
    let RemoteActionMessage::Receipt(receipt) = message else {
        return None;
    };
    match receipt.status {
        ReceiptStatus::Completed { output } => {
            tilo_server_protocol::workspaces::fetch_grant_token(&output.value.unwrap_or_default())
        }
        _ => None,
    }
}

/// Ask the host to admit this device through `entering`, on the actions link and in the entry
/// scope: an uncertified device reaches nothing else there. The host answers the
/// [`Entered`](tilo_server_protocol::entering::Entered) it answered a session's entry with.
async fn ask_entry(
    actions: Option<&tilo_server_client::link::Link>,
    signer: Arc<dyn tilo_server_client::DeviceSigner>,
    cert: DeviceCert,
    host: [u8; 32],
    entering: &tilo_server_protocol::entering::Entering,
) -> Result<tilo_server_protocol::entering::Entered, EntryFailure> {
    use tilo_actions::remote::{ReceiptStatus, RemoteActionFrame, RemoteActionMessage};
    let Some(link) = actions else {
        return Err(EntryFailure::unreachable(
            "the host's action service is not reachable".into(),
        ));
    };
    let invoke = tilo_server_protocol::workspaces::entry_invoke(
        entering,
        &cert,
        tilo_actions::InvocationId::new(),
    )
    .map_err(|reason| EntryFailure::unreachable(format!("the entry does not encode: {reason}")))?;
    let frame = ask_action(
        link,
        signer,
        cert.device_public_key,
        tilo_server_protocol::workspaces::ENTRY_SCOPE,
        host,
        RemoteActionMessage::Invoke(invoke),
    )
    .await
    .map_err(|refusal| EntryFailure::unreachable(refusal.to_string()))?;
    let message = RemoteActionFrame::decode(&frame)
        .map_err(|error| {
            EntryFailure::unreachable(format!("the host's answer does not decode: {error}"))
        })?
        .message;
    let RemoteActionMessage::Receipt(receipt) = message else {
        return Err(EntryFailure::unreachable(
            "the host answered no receipt".into(),
        ));
    };
    match receipt.status {
        ReceiptStatus::Completed { output } => {
            tilo_server_protocol::workspaces::entered_reply(&output.value.unwrap_or_default())
                .map_err(EntryFailure::unreachable)
        }
        ReceiptStatus::Rejected { error } => Err(EntryFailure {
            code: "refused",
            message: error.to_string(),
        }),
        ReceiptStatus::Failed { error } => Err(EntryFailure {
            code: "failed",
            message: error.message,
        }),
        ReceiptStatus::Cancelled { message } => Err(EntryFailure {
            code: "cancelled",
            message: message.unwrap_or_else(|| "the host gave the entry up".into()),
        }),
        ReceiptStatus::Accepted => Err(EntryFailure::unreachable(
            "the host did not answer the entry".into(),
        )),
    }
}
