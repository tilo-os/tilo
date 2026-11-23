//! The certified server session of one device.
//!
//! One task owns the connection to the user's host and everything that must
//! be serialized with it: document signing, the commit outbox, reconnects.
//! Callers talk to it through the client's cheap [`Link`].
//!
//! - **Optimistic local edits, host order.** A published document frame is
//!   signed, appended to the outbox (persisted through the [`PeerStore`]) and
//!   sent as an `Envelope::Commit`. It stays outstanding until the host
//!   answers `Committed` with its order; the local replica already shows it.
//! - **Reconnect replays.** After any disconnect the task reconnects with
//!   backoff, re-subscribes every joined workspace (the host replays retained
//!   checkpoints and this device's queue), rejoins the live session and resends
//!   the whole outbox in order. Re-applying a document change is idempotent.
//! - **Fail closed.** An expired certificate, or a host that keeps refusing
//!   the handshake (a revoked device), ends in [`TransportPhase::Failed`]
//!   instead of retrying forever; only [`Cmd::Reconnect`] retries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use crate::{
    CallPicture, CallShown, Closed, Cmd, Event, Failure, FailureKind, Link, Outbox, PageShown,
    PeerStatus, Reports, TransportPhase, content::Kept,
};
use std::time::Duration;
use tilo_session_actions::{Heard, NOT_CONNECTED};

use tilo_server_client::{CoeditPayload, DeviceSigner};
use tilo_server_protocol::Envelope;
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

use crate::link::Held;
use crate::membership_check::{PeerEntry, PeerStore};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::LinkError;
use tilo_server_session::ServerClient;

mod agent_session;
mod connect;
mod entry;
mod governance;
mod host_voice;
mod inbound;
mod live_session;
mod opened_records;
mod outbound;

use agent_session::AgentSessionCompletion;
use connect::Dial;
pub use connect::{SessionMemberServer, member_server};
#[cfg(test)]
use connect::{connect, connect_backoff, link_error, transient};
use entry::{Refusal, refusal};
use governance::HostGovernance;
use tilo_server_protocol::WorkspaceId;

/// Longest wait between reconnect attempts: a server that comes back is reached again within
/// this (and its jitter) of being back, so a device that lost it for minutes is in again within
/// seconds, not half a minute.
const MAX_BACKOFF: Duration = Duration::from_secs(8);
/// Keep-alive interval on an idle session.
const PING_INTERVAL: Duration = Duration::from_secs(15);
/// How long the server may leave the keep-alive unanswered before this session takes the connection
/// as gone. The server answers every `Ping` with a `Pong` (and sends its own frames besides), so
/// three intervals without a frame back is a connection whose other end is gone — a stopped
/// server, or a relay or proxy still holding a connection nothing answers on. The transport under
/// it cannot be relied on to close one: a silent path stays open, and a window waiting on it would
/// wait for ever.
const SILENCE_BEFORE_LOST: Duration = Duration::from_secs(45);
/// Envelopes the session takes from the host before it settles what they changed.
const MAX_TAKEN_AT_ONCE: usize = 256;
/// How often overdue channel access requests are sent again.
const CHANNEL_TICK: Duration = Duration::from_secs(1);
/// The most envelopes the session reads in one pass before it looks at its
/// commands and timers again.
const MAX_READ_AT_ONCE: usize = 256;
/// Consecutive refusals with no reason (an iroh host closing the handshake)
/// after which the host is taken to no longer admit this device rather than
/// to be unreachable. A refusal that says why decides at once.
const REFUSALS_BEFORE_FAILING: u32 = 3;
/// The host does not admit this device's person (`not-admitted`): it never
/// knew them, forgot them (its state was reset, or it pruned them) or a workspace
/// took them out. The page joins again where the workspace's entry lets it.
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

/// How long a connected session waits for the host to admit its workspaces before
/// it reports `admission-timeout`. Checked on the keep-alive tick.
const ADMISSION_TIMEOUT_MS: u64 = 30_000;
/// How long after this device's replica refused a compaction it is behind its workspace is asked
/// for the document again, the first time; each time after, it waits twice as long, at most
/// [`BEHIND_ASKED_AT_MOST_EVERY_MS`]. Checked on the keep-alive tick.
const BEHIND_ASKED_AFTER_MS: u64 = 15_000;
/// The longest a document this device is behind in waits before it is asked for again.
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
    /// ([`tilo_server_protocol::entering`]); taken once the host answered.
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

/// A write in this device's ownership: its id, bytes, channel and retention.
type OwnershipEntry = ([u8; 32], Vec<u8>, [u8; 32], neighbour_wire::Retention);

/// The workspace id a session asks the host's own operations under: none of its workspaces, the
/// host itself (creating, listing and entering the person's workspaces). Its answers come back
/// from the host's device under the same id.
pub const HOST_ITSELF: WorkspaceId = WorkspaceId::nil();

/// What a session's task is told: the client's own commands, and the transport's own.
#[expect(
    clippy::large_enum_variant,
    reason = "nearly every command is the client's: boxing them would allocate for each to shrink the transport's few"
)]
enum Command {
    /// One of the client's commands.
    Client(Cmd),
    /// The host answered the placement of this workspace.
    Placed(WorkspaceId),
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
}

/// What sealing a live frame came to.
enum Sealing {
    Sealed(Vec<u8>),
    /// This device holds no keys for the record yet.
    NoKeys(tilo_server_protocol::live::LiveResource),
    /// Names no record, or does not seal: never sent.
    Dropped,
}

/// What opening a sealed live frame came to.
enum Opening {
    Opened(Vec<u8>),
    /// This device holds no keys for the record yet: the digest it carries.
    NoKeys([u8; 32]),
    /// Malformed, or does not open.
    Dropped,
}
/// Where a barrier's outcome goes ([`Cmd::Barrier`]).
pub(crate) type Completion = crate::Answer<Result<(), String>>;
/// Where a live frame sealed with its record's channel keys goes, or `None` when this device holds
/// none ([`Cmd::SealLive`]).
pub(crate) type LiveCompletion = crate::Answer<Option<Vec<u8>>>;
/// Where the host's answer to a workspace administration request goes ([`Cmd::Administer`]).
pub(crate) type WorkspacesCompletion =
    crate::Answer<Result<tilo_server_protocol::workspaces::WorkspacesReply, String>>;
#[derive(Debug, PartialEq, Eq)]
enum Admission {
    Admitted,
    Waiting,
    Overdue,
}

/// Where the joined workspaces' admission stands `now_ms`, for a subscription
/// sent at `since_ms`.
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

/// Whether `now_ms` is within [`SILENCE_BEFORE_LOST`] of the last frame heard from the server: what
/// says a connection is still alive where its transport reports nothing.
fn silence_is_short(last_contact_at_ms: Option<u64>, now_ms: u64) -> bool {
    let allowed = u64::try_from(SILENCE_BEFORE_LOST.as_millis()).unwrap_or(u64::MAX);
    last_contact_at_ms.is_none_or(|last| now_ms.saturating_sub(last) < allowed)
}

/// Start a certified server session with `config`'s host. It connects in the background; observe it
/// with [`Link::status`] and [`Link::on_change`]. It ends with its last link.
pub fn start_session(
    config: PeerConfig,
    signer: Arc<dyn DeviceSigner>,
    store: Arc<dyn PeerStore>,
) -> Link {
    let (task, link, receiver) = session_task(config, signer, store, None, false);
    n0_future::task::spawn(task.run(receiver));
    link
}

/// What a device's own session lends a session it opens to another host, so the device stays one
/// device there.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Default)]
pub struct Lent {
    /// The device's own key service: the session reads and asks with it, and settles no keys of
    /// its own. A device holds one place in a workspace's key group, and a second service under
    /// the same device key would take that place from the first each time it asked to be admitted.
    /// Lent none, the session holds no keys and asks for none: the device's member key in each
    /// group is its own session's.
    pub keys: Option<tilo_workspace_keys::SharedKeys>,
    /// Where the endpoint the device runs under its key lands once bound: the session dials from
    /// it. A server keeps one connection per endpoint id, so a second endpoint under the same key
    /// cut the first off; a session opened before the endpoint is bound waits for it and binds
    /// none in its place.
    pub endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
    /// The session is the device's own, not one beside it: it joins the workspaces' live
    /// sessions on the lent endpoint. A device that runs a session of its own beside this one
    /// lends none, as a second live session under its key would shadow that one's.
    pub own: bool,
}

/// [`start_session`] for a session of a device that lends it what its own session runs
/// ([`Lent`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn start_session_lent(
    config: PeerConfig,
    signer: Arc<dyn DeviceSigner>,
    store: Arc<dyn PeerStore>,
    lent: Lent,
) -> Link {
    let (mut task, link, receiver) = session_task(config, signer, store, lent.keys, true);
    if let Some(endpoint) = lent.endpoint {
        task.dial.lend(endpoint, lent.own);
    }
    n0_future::task::spawn(task.run(receiver));
    link
}

/// The session [`start_session`] runs, the link to it and its command queue,
/// before it runs: over `keys` when the device's own key service settles them. One that is
/// `lent` what its device's own session runs ([`start_session_lent`]) and is handed no keys holds
/// none.
#[expect(
    clippy::too_many_lines,
    reason = "one struct literal: the session's state, a field a line"
)]
fn session_task(
    config: PeerConfig,
    signer: Arc<dyn DeviceSigner>,
    store: Arc<dyn PeerStore>,
    keys: Option<tilo_workspace_keys::SharedKeys>,
    lent: bool,
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
    let replicas = crate::replica::Replicas::new(config.cert.clone(), Arc::clone(&store));
    let key_state = crate::membership_check::StoredKeys::load(store.as_ref());
    // The transport this session hands the groups' governance it hears over: a session that
    // settles its own keys judges them itself, and one lent the device's key service hands
    // them to that service, which judges them as it judges the frames its links bring. A
    // session lent no keys holds none and asks for none.
    let holds_no_keys = lent && keys.is_none();
    let governance = (!holds_no_keys).then(HostGovernance::default);
    let workspace_keys = keys.unwrap_or_else(|| {
        let checks = Arc::new(tilo_workspace_keys::SealChecks::default());
        tilo_workspace_keys::SharedKeys::new(if lent {
            keyless_keys(&replicas, &checks)
        } else {
            open_keys(&replicas, &key_state, vault, &signer, &checks)
        })
    });
    let engine = session_engine(&config, &signer, &replicas);
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
        replicas,
        commands: commands.downgrade(),
        config,
        signer,
        store,
        dial: Dial::for_page(&vault),
        reports: Arc::clone(&reports),
        workspaces: BTreeSet::new(),
        emptied: false,
        live_sessions_due: BTreeSet::new(),
        governance_batch: None,
        kept: BTreeMap::new(),
        placing: BTreeMap::new(),
        live_sessions: live_session::LiveSessions::default(),
        record,
        workspace_keys,
        governance,
        outbound_due: false,
        held_since: BTreeMap::new(),
        kept_why: BTreeMap::new(),
        pending_signed: BTreeMap::new(),
        owed_since: BTreeMap::new(),
        arrived: Vec::new(),
        let_go: BTreeSet::new(),
        held_inbound: BTreeMap::new(),
        behind: BTreeMap::new(),
        key_state,
        route_keys: BTreeMap::new(),
        new_keys: Vec::new(),
        opens_as_before: std::collections::HashSet::new(),
        keys_generation: 0,
        refused: (0, String::new()),
        surface_wants: neighbour_wire::content::ContentWants::default(),
        held_live: crate::held_live::HeldLive::new(),
        opened_records: BTreeMap::new(),
        opened_routes: BTreeSet::new(),
        when_opened: BTreeSet::new(),
        barriers: Vec::new(),
        sending: BTreeMap::new(),
        awaiting_surface: BTreeMap::new(),
        surfaces_arrived: false,
        compactions: Vec::new(),
        answering: Vec::new(),
        replays: BTreeSet::new(),
        held_barriers: Vec::new(),
        unheard_refusals: BTreeMap::new(),
        admission_since: None,
        delivered: BTreeMap::new(),
        received: BTreeSet::new(),
        workspaces_pending: BTreeMap::new(),
        workspaces_next: 0,
        agent_sessions_pending: BTreeMap::new(),
        agent_sessions_next: 0,
        agent_session: None,
        held_actions: HeldActions::default(),
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
/// the signer, with the membership check and founders a workspace's session runs.
fn session_engine(
    config: &PeerConfig,
    signer: &Arc<dyn DeviceSigner>,
    replicas: &crate::replica::Replicas,
) -> Arc<crate::engine::Engine> {
    Arc::new(crate::engine::Engine::holding(
        replicas.owner(),
        Arc::default(),
        tilo_identity::identity_store::DeviceIdentity::kept(
            config.cert.clone(),
            Arc::clone(signer),
        ),
        Arc::new(crate::membership_check::SessionMembership),
        Arc::new(crate::engine::Founders::default()),
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
/// through the server session it keeps with its host, and a write it holds for its record's
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
    /// The entry this session entered by refused it, as the host said: the
    /// session stops until the page acts.
    Refused(&'static str, String),
    /// The session's host never proved itself: an entering session starts
    /// before the host's proof is read, so this is its failed connect.
    Unproved(LinkError),
}

/// A document the host delivered, waiting for its workspace's replica to decide it.
struct Arrived {
    workspace_id: WorkspaceId,
    author: [u8; 32],
    message: neighbour_wire::NeighbourMsg,
    route: DocumentRoute,
    /// Its entry id, as the host names it.
    entry: [u8; 32],
    /// The app surface a change names.
    surface: Option<[u8; 32]>,
}

struct Task {
    /// This session's own command queue, for answers it waits on in the
    /// background; weak, so the session still ends with its last handle.
    commands: mpsc::WeakUnboundedSender<Command>,
    config: PeerConfig,
    signer: Arc<dyn DeviceSigner>,
    store: Arc<dyn PeerStore>,
    /// Where this session dials its host from, kept across reconnects.
    dial: Dial,
    /// What this session reports to its links.
    reports: Arc<Reports>,
    workspaces: BTreeSet<tilo_server_protocol::WorkspaceId>,
    /// Whether the host deleted the last workspace this session held: it has
    /// nothing left to be admitted to, so it stays done instead of dialling a
    /// host that only ends the connection again.
    emptied: bool,
    /// The joined workspaces whose live session may have to be joined, followed
    /// or left ([`Self::settle_live_sessions`]): those whose governance, channel
    /// keys or live session changed since it last looked, and every one on each
    /// channel tick. A page in many workspaces pays for the ones that
    /// changed, not for all of them on every frame it reads.
    live_sessions_due: BTreeSet<WorkspaceId>,
    /// Governance frames received and not admitted yet, admitted together
    /// ([`Self::admit_governance`]).
    governance_batch: Option<inbound::GovernanceBatch>,
    /// What this device keeps of each workspace's content ([`Cmd::KeepContent`]).
    kept: BTreeMap<WorkspaceId, Arc<dyn Kept>>,
    /// Workspaces of this device's person joined once the host governs them:
    /// each one's UUID and name, until the host answered its placement.
    placing: BTreeMap<WorkspaceId, (String, String)>,
    /// This page's own live session per joined workspace
    /// ([`crate::session::live_session`]).
    live_sessions: live_session::LiveSessions,
    record: PeerEntry,
    /// This device's side of the workspaces' encrypted channels: the engine every member runs,
    /// here as a reader that asks for access and follows the members' commits.
    workspace_keys: tilo_workspace_keys::SharedKeys,
    /// What the channels say to and hear from the host: the frames a link to this device would
    /// bring, taken into [`Self::workspace_keys`] exactly as a link frame is, so a session that
    /// reaches its workspace with no link still joins each record's group. `None` for a session
    /// lent no keys, which holds none and asks for none ([`start_session_lent`]).
    governance: Option<HostGovernance>,
    /// The channels' state and the writes held for their keys, in the store.
    key_state: crate::membership_check::StoredKeys,
    /// Every route this session opened, and the keys it holds for it.
    route_keys: BTreeMap<DocumentRoute, RouteKeys>,
    /// Routes whose keys arrived since the session last acted on them.
    new_keys: Vec<DocumentRoute>,
    /// Those of [`Self::new_keys`] whose sealed history the keys held before already opened
    /// whole: the keys moved on to a later epoch and still hold the earlier ones, as they do when
    /// a member is admitted, so nothing the replica holds opens now that did not before, and
    /// their records did not land.
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
    /// here, or a signature. Their replica holds them already; only their ownership is owed.
    pending_signed: BTreeMap<DocumentRoute, Vec<collab_types::SignedOp>>,
    /// Since when each document has committed writes of this device its holder has not answered.
    owed_since: BTreeMap<DocumentRoute, tilo_clock::time::Instant>,
    /// Documents the host delivered that the replica decides together ([`Self::keep_arrived`]).
    arrived: Vec<Arrived>,
    /// Records of writes the replica let go undecided, its queue of undecided writes full: the
    /// host replays them once what they waited for landed.
    let_go: BTreeSet<DocumentRoute>,
    /// What the host answered of the writes this connection sent, retired from ownership
    /// together: the workspace, the ownership entry, the answer and what a refusal withdraws.
    /// The records whose last write the replica did not take in, and why.
    held_inbound: BTreeMap<DocumentRoute, String>,
    /// The documents a compaction of which this device's replica refused: it holds only part of
    /// what the compaction covers, and could not take it with its keys either (they are not here
    /// yet, or the compaction lacks something this device holds). Nothing written to such a
    /// document after the compaction reads here, so each is asked of its workspace again
    /// ([`Self::ask_behind_again`]) until a compaction of it is taken: it waits for nothing in
    /// silence.
    behind: BTreeMap<DocumentRoute, Behind>,
    /// The on-demand records this page opened, by workspace and retained
    /// channel: opened again after every resubscription. A channel names a
    /// record by its app and id alone, so the same record of two workspaces
    /// (each one's layout, an app's own record) shares it.
    opened_records: BTreeMap<(WorkspaceId, [u8; 32]), DocumentRoute>,
    /// The routes this session was asked to open ([`Cmd::OpenRoute`]):
    /// in a workspace that sends records when opened, each is opened as a
    /// record too, so it arrives.
    opened_routes: BTreeSet<DocumentRoute>,
    /// The workspaces whose host said they send records only when opened.
    when_opened: BTreeSet<WorkspaceId>,
    /// Each waits for the writes that were in this device's ownership when it was called, and
    /// completes with the host's refusal of any of them: the writes of every workspace, or of
    /// the workspaces or the documents it was asked for alone ([`outbound::Scope`]).
    barriers: Vec<outbound::Owed>,
    /// What this connection sent its host of each workspace's ownership.
    sending: BTreeMap<tilo_desktop_model::WorkspaceId, crate::membership_check::Sending>,
    /// The write carrying this device's governance of each workspace the host last answered as
    /// durable: it stays in ownership after, and a barrier of the workspace waits for it only
    /// while it is not this one ([`outbound::Scope::Workspaces`]).
    /// The writes the host was not ready for: each is sent again once the host says the
    /// workspace is ready, and refused if the host turns it away again.
    /// Writes the owner holds for an app surface this page does not have, by that surface: the
    /// routes the host replays once it arrived.
    awaiting_surface: BTreeMap<[u8; 32], (WorkspaceId, DocumentRoute)>,
    /// Whether an app surface arrived since the session last asked for what waited for one.
    surfaces_arrived: bool,
    /// Compactions the host asked for and this session has not answered yet.
    compactions: Vec<(WorkspaceId, [u8; 16], DocumentRoute)>,
    /// Compactions committed for the host's requests, each with the entries delivered before
    /// it, answered once ownership sends them ([`Self::answer_compactions`]).
    answering: Vec<Answering>,
    /// Records whose writes the replica asked to hear again, replayed by the host.
    replays: BTreeSet<DocumentRoute>,
    /// This device's replicas, as the page's and agents' readers read them.
    engine: Arc<crate::engine::Engine>,
    /// Barriers called while frames were held, with the routes those frames
    /// are held on: each waits for them to enter the outbox, then for the
    /// outbox as it stands then, in the workspaces it was asked for.
    held_barriers: Vec<outbound::Held>,
    /// Why the workspace refused writes of each route that no barrier hearing that route waited
    /// for when they were let go: they left the device before the barrier that asks after them was
    /// called, and that barrier completes with the refusal rather than as saved.
    unheard_refusals: BTreeMap<DocumentRoute, String>,
    /// When the workspaces not yet admitted were subscribed (ms since the epoch).
    admission_since: Option<u64>,
    /// Retained entry ids of every document delivered or published per
    /// route: what a snapshot of the route contains.
    delivered: BTreeMap<DocumentRoute, BTreeSet<[u8; 32]>>,
    /// Entry ids delivered since this page last subscribed whole. A resumed
    /// subscription keeps them; a whole one asks for them again (a refresh
    /// wants the workspaces whole), so it clears them.
    received: BTreeSet<[u8; 32]>,
    /// Each joined workspace as its governance admits it.
    replicas: crate::replica::Replicas,
    /// The workspace administration this session asked its host and has not
    /// been answered yet, by the id it asked with.
    workspaces_pending: BTreeMap<u64, WorkspacesCompletion>,
    /// The id the next administration request is asked with.
    workspaces_next: u64,
    /// The agent runs this session declared and the host has not answered yet, by the
    /// id they asked with.
    agent_sessions_pending: BTreeMap<u64, AgentSessionCompletion>,
    /// The id the next agent run is declared with.
    agent_sessions_next: u64,
    /// The agent whose run this session said it is, declared again on every
    /// connection: the host keeps it per connection, and a session that came
    /// back without it would act as the person.
    agent_session: Option<String>,
    /// Action frames owed to workspaces the host has not admitted this session
    /// to yet, oldest first, with the workspace each is for: sent before the
    /// admission, they are refused by the host and the request they carry is
    /// never answered.
    held_actions: HeldActions,
    /// The workspaces whose replay the host began on this connection: their
    /// bootstrap arrived, or their `Ready` for a host that sends none. The
    /// host subscribed this connection to them, so it takes what the page
    /// says there — its certificate ([`Self::introduce`]) and its channels'
    /// governance ([`Self::send_governance`]) — while the replay runs.
    begun: BTreeSet<WorkspaceId>,
    /// The records that landed in a workspace whose governance, as this
    /// device's replica holds it, did not admit this device yet, with the
    /// author of the write that landed last: this device reads a record as the
    /// member it is, so their readers hear of them once it is one
    /// ([`Self::landed`], [`Self::follow_governance`]).
    unadmitted: BTreeMap<DocumentRoute, Option<[u8; 32]>>,
    /// The workspaces whose governance, as this device's replica holds it,
    /// admitted this device on this session.
    admitted_here: BTreeSet<WorkspaceId>,
}

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

/// One action frame held until the host admits this session to its workspace.
struct HeldAction {
    workspace_id: WorkspaceId,
    to: [u8; 32],
    frame: Vec<u8>,
}

/// Entry ids a route remembers for compaction. Past it, later entries are
/// left out of statements and simply stay retained.
const MAX_DELIVERED_ENTRIES: usize = 1 << 17;
/// A compaction request this device compacted for: the workspace, the request, the record and
/// the entries delivered for it before.
type Answering = (WorkspaceId, [u8; 16], DocumentRoute, Vec<[u8; 32]>);

/// Open compaction requests kept; the oldest goes first.
const MAX_COMPACTIONS: usize = 16;

/// Action frames held for a workspace's admission at once. Past it, a new action is refused at
/// once, saying the session waits to be let in: every one already held stays held, in order.
const MAX_HELD_ACTIONS: usize = 64;

/// The action frames waiting for the server to admit this session to their workspace, oldest
/// first. None is ever given up once held: a person who was told their action waits is never
/// told later that it was dropped for a newer one. When too many wait, the newest is refused at
/// once, while its caller is still there to hear it and ask again.
#[derive(Default)]
struct HeldActions(Vec<HeldAction>);

impl HeldActions {
    /// Hold `action` until its workspace admits this session; it comes back when as many wait
    /// as [`MAX_HELD_ACTIONS`], and nothing held was let go.
    fn hold(&mut self, action: HeldAction) -> Result<(), HeldAction> {
        if self.0.len() >= MAX_HELD_ACTIONS {
            return Err(action);
        }
        self.0.push(action);
        Ok(())
    }

    /// Every action held for `workspace_id`, in the order they were made; those for other
    /// workspaces keep waiting.
    fn take(&mut self, workspace_id: WorkspaceId) -> Vec<HeldAction> {
        let (due, waiting) = std::mem::take(&mut self.0)
            .into_iter()
            .partition(|held| held.workspace_id == workspace_id);
        self.0 = waiting;
        due
    }
}

impl Task {
    fn note_delivered(&mut self, route: &DocumentRoute, entries: &[[u8; 32]]) {
        let known = self.delivered.entry(route.clone()).or_default();
        for entry in entries {
            if known.len() >= MAX_DELIVERED_ENTRIES {
                return;
            }
            known.insert(*entry);
        }
    }

    /// Answer the host beginning a workspace's replay (its bootstrap) or
    /// admitting this session to it (its `Ready`) with what the page says
    /// there from then on. `false` when the session took no more.
    fn begin(&mut self, client: &ServerClient, envelope: &Envelope) -> bool {
        match envelope {
            Envelope::WorkspaceBootstrap { workspace_id, .. } => {
                self.introduce(client, *workspace_id)
            }
            Envelope::Ready {
                workspace_id,
                documents_when_opened,
                ..
            } => {
                if *documents_when_opened {
                    self.documents_when_opened(*workspace_id, Some(client));
                }
                self.announce(client, *workspace_id)
            }
            _ => true,
        }
    }

    /// Tell the host this device's certificate as soon as it subscribed this
    /// connection to `workspace_id`, which its bootstrap ahead of the replay
    /// says: the workspace's channel members take a device's access
    /// request only once they hold its certificate, so a page told only at
    /// `Ready` got its keys a whole replay late. Returns `false` when the
    /// session could not take it.
    fn introduce(&mut self, client: &ServerClient, workspace_id: WorkspaceId) -> bool {
        if !self.begun.insert(workspace_id) {
            return true;
        }
        identity_hello(workspace_id, &self.config.cert, &self.config.revocations)
            .is_none_or(|hello| client.send(hello).is_ok())
    }

    /// Tell the host, once `workspace_id` admitted this device, its
    /// certificate, unless the replay's start told it already. Returns
    /// `false` when the session could not take it.
    fn announce(&mut self, client: &ServerClient, workspace_id: WorkspaceId) -> bool {
        self.introduce(client, workspace_id)
    }

    /// Take `entry` into what this session announces and send its identity
    /// again to every workspace the host admitted it to. An entry that does
    /// not verify, or that names another person's root, changes nothing: a
    /// host refuses a hello whose revocations are not its sender's own.
    fn revoke(
        &mut self,
        entry: tilo_user_identity::RevocationEntry,
        client: Option<&ServerClient>,
    ) -> bool {
        if entry.user_public_key != self.config.cert.user_public_key {
            return true;
        }
        let Ok(changed) = self.config.revocations.insert(entry) else {
            return true;
        };
        let Some(client) = client else {
            return true;
        };
        if !changed {
            return true;
        }
        let admitted: Vec<WorkspaceId> = self.reports.read(|status| {
            status
                .workspaces
                .iter()
                .filter(|(_, ready)| **ready)
                .map(|(workspace_id, _)| *workspace_id)
                .collect()
        });
        for workspace_id in admitted {
            if let Some(hello) =
                identity_hello(workspace_id, &self.config.cert, &self.config.revocations)
                && client.send(hello).is_err()
            {
                return false;
            }
        }
        true
    }

    /// Note `request`, the host's ask for a snapshot of `route`: answered from this device's
    /// replica at the top of the session loop ([`Self::compact`]).
    fn compaction_requested(
        &mut self,
        workspace_id: WorkspaceId,
        request: [u8; 16],
        route: DocumentRoute,
    ) {
        if !self.workspaces.contains(&workspace_id)
            || route.workspace_id().ok() != Some(workspace_id)
        {
            return;
        }
        if self.compactions.len() >= MAX_COMPACTIONS {
            self.compactions.remove(0);
        }
        self.compactions.push((workspace_id, request, route));
    }

    /// Compact `route`'s sealed history as this device's replica holds it for request `request`:
    /// the compaction is the record's checkpoint, which every replica takes in place of exactly
    /// the writes it folds. It is answered once ownership sends it ([`Self::answer_compactions`]).
    async fn compact(
        &mut self,
        (workspace_id, request, route): (WorkspaceId, [u8; 16], DocumentRoute),
    ) {
        let covered: Vec<[u8; 32]> = self
            .delivered
            .get(&route)
            .map(|known| known.iter().copied().collect())
            .unwrap_or_default();
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
            return;
        };
        let now = crate::clock::system_time(tilo_clock::unix_now_ns());
        // A record this device cannot compact whole yet is compacted by another member.
        match self
            .replicas
            .owner()
            .compact_document(
                workspace,
                (&route.app_id, &route.document),
                self.signer.as_ref(),
                now,
            )
            .await
        {
            Ok(0) => {}
            Ok(_) => {
                if self.answering.len() >= MAX_COMPACTIONS {
                    self.answering.remove(0);
                }
                self.answering.push((workspace_id, request, route, covered));
            }
            Err(error) => {
                tracing::warn!(%error, "a sealed record the host asked for stays uncompacted");
            }
        }
    }

    /// Send the signed statement of each compaction ownership sent, of what it covers: every entry
    /// delivered for its record before it was asked. Returns `false` when the session could not
    /// take an envelope.
    async fn answer_compactions(&mut self, client: &ServerClient) -> bool {
        for (workspace_id, request, route, covered) in std::mem::take(&mut self.answering) {
            let drafted = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id)
                .and_then(|workspace| {
                    self.replicas
                        .owner()
                        .drafted_compaction(workspace, (&route.app_id, &route.document))
                        .ok()
                })
                .flatten();
            let Some(entry) = drafted else {
                self.answering.push((workspace_id, request, route, covered));
                continue;
            };
            let snapshot = vec![tilo_server_protocol::compaction::entry_id(&entry)];
            // What this device publishes, its own replica holds.
            self.note_delivered(&route, &snapshot);
            let statement = tilo_server_protocol::compaction::CoverageStatement {
                workspace_id,
                channel: neighbour_wire::app_document_group(&route.app_id, &route.document),
                request,
                snapshot,
                covered,
                heads: self.compaction_heads(&route, &entry).unwrap_or_default(),
            };
            let Ok(statement) = statement.encode() else {
                continue;
            };
            let Ok(signature) = self
                .signer
                .sign(tilo_server_protocol::compaction::signing_bytes(&statement))
                .await
            else {
                continue;
            };
            let answer = Envelope::Compacted {
                workspace_id,
                statement,
                signature,
            };
            if client.send(answer).is_err() {
                return false;
            }
        }
        true
    }

    /// The heads of the record `entry`, ownership's compaction of `route`, holds.
    fn compaction_heads(&self, route: &DocumentRoute, entry: &[u8]) -> Option<Vec<[u8; 32]>> {
        let message: neighbour_wire::NeighbourMsg =
            neighbour_wire::NeighbourMsg::decode(entry).ok()?;
        let neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } = message else {
            return None;
        };
        let ciphertext = collab_types::sealed_ciphertext(&envelope).ok()?;
        let metadata = envelope.encryption?;
        let plaintext = self
            .keys_for_ciphertext(route, metadata)?
            .open_content(envelope.artifact, &metadata, &ciphertext)
            .ok()?;
        let snapshot = collab_types::DocumentMutation::decode(&plaintext).ok()?;
        tilo_server_protocol::compaction::snapshot_heads(snapshot.data())
    }

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

    /// Subscribe to `workspace_id`: the host's live traffic of it. Its records
    /// reach a device over its link to the workspace, never a session, so a
    /// subscription names no position to replay from.
    fn subscribe(workspace_id: WorkspaceId) -> Envelope {
        Envelope::Subscribe { workspace_id }
    }

    /// Subscribe every joined workspace again, from now on waiting for its
    /// `Ready`. Returns `false` when the session could not take them.
    fn resubscribe(&mut self, client: &ServerClient) -> bool {
        self.begun.clear();
        let mut sent_ok = true;
        for workspace_id in &self.workspaces {
            sent_ok &= client
                .send(Envelope::Subscribe {
                    workspace_id: *workspace_id,
                })
                .is_ok();
        }
        self.reports.change(|status| {
            for ready in status.workspaces.values_mut() {
                *ready = false;
            }
        });
        sent_ok &= self.reopen_records(client);
        sent_ok
    }

    /// Whether the server said anything within [`SILENCE_BEFORE_LOST`]. The session pings it every
    /// [`PING_INTERVAL`] and it answers, so silence that long is a connection to redial even where
    /// its transport never says it closed.
    fn heard_recently(&self) -> bool {
        let last = self.reports.read(|status| status.last_contact_at_ms);
        silence_is_short(last, tilo_clock::unix_now_ms())
    }

    /// Report `admission-timeout` once the workspaces subscribed at
    /// `admission_since` are still not admitted, and clear it once they are.
    /// A timeout is reported once per subscription.
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
        // A read-only invited page may never publish. Keep its certificate
        // and host pin before the first connection, as well as its outbox.
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
        let mut refusals: u32 = 0;
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
            // under the same key, and a server keeps one connection per key, so a dial left
            // running would cut the new session off when it lands.
            let closing = self.reports.closed();
            let connected = tokio::select! {
                connected = self.connect() => connected,
                () = closing => return self.closed().await,
            };
            let failed = match connected {
                Ok(client) => {
                    reached_again(lost_at.take(), attempt);
                    // Waiting for admission is timed from when this page
                    // subscribes, not from the moment the host answers, so a
                    // host that holds the connection open still reports.
                    self.admission_since = Some(tilo_clock::unix_now_ms());
                    let exit = self.serve(client, &mut commands).await;
                    // A request the host had not answered is answered here:
                    // nothing waits on a connection that is gone.
                    self.requests_lost();
                    if !matches!(exit, Exit::Unproved(_)) {
                        attempt = 0;
                        refusals = 0;
                    }
                    match exit {
                        Exit::Closed => return self.closed().await,
                        Exit::Reconnect => continue,
                        Exit::Lost(reason) => {
                            tracing::warn!(%reason, "the session lost its server; it dials again");
                            lost_at = Some(tilo_clock::now());
                            self.fail("transport-ended", reason);
                            None
                        }
                        Exit::Refused(code, message) => {
                            if !self.stop_until_asked(code, message, &mut commands).await {
                                return self.closed().await;
                            }
                            continue;
                        }
                        Exit::Unproved(error) => Some(error),
                    }
                }
                Err(error) => Some(error),
            };
            if let Some(error) = failed {
                let stop = match refusal(&error) {
                    Refusal::Final(code, message) => Some((code, message)),
                    Refusal::Unexplained => {
                        refusals = refusals.saturating_add(1);
                        (refusals >= REFUSALS_BEFORE_FAILING)
                            .then(|| (NOT_ADMITTED.0, NOT_ADMITTED.1.to_owned()))
                    }
                    Refusal::Reachability => {
                        refusals = 0;
                        None
                    }
                };
                if let Some((code, message)) = stop {
                    if !self.stop_until_asked(code, message, &mut commands).await {
                        return self.closed().await;
                    }
                    attempt = 0;
                    refusals = 0;
                    continue;
                }
                tracing::info!(attempt, %error, "the session did not reach its server");
                lost_at.get_or_insert_with(tilo_clock::now);
                self.fail("transport-unavailable", error.to_string());
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

    /// Wait out this attempt's backoff, serving what the person asks offline.
    /// `false` when they closed the session instead.
    async fn wait_offline(
        &mut self,
        attempt: u32,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> bool {
        // The backoff ends when the clock the status names reaches the deadline, not when a
        // timer started here does.
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
                    Some(Command::Client(Cmd::Reconnect | Cmd::Retry)) => return true,
                    Some(Command::Client(Cmd::Refresh)) => {
                        self.received.clear();
                        return true;
                    }
                    Some(command) => self.apply_offline(command).await,
                },
                () = &mut sleep => return true,
            }
        }
    }

    /// Whether a session the host left holding nothing goes on: it stays
    /// failed until the person asks for a connection again — no dial reaches a
    /// workspace that is gone — and `false` when they closed it instead.
    async fn asked_again(&mut self, commands: &mut mpsc::UnboundedReceiver<Command>) -> bool {
        self.stop_until_asked(
            WORKSPACE_DELETED.0,
            WORKSPACE_DELETED.1.to_owned(),
            commands,
        )
        .await
    }

    async fn closed(mut self) {
        // Nothing reconnects from here: the endpoint goes with the session,
        // and the host is told so.
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
                Some(Command::Client(Cmd::Reconnect)) => return true,
                Some(Command::Client(Cmd::Refresh)) => {
                    self.received.clear();
                    return true;
                }
                Some(command) => self.apply_offline(command).await,
            }
        }
    }

    /// The session is up: connected and authenticated, its transport
    /// failures cleared, every workspace waiting to be admitted again.
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

    /// Hand the host what this session owes it before waiting for the next event: its live sessions,
    /// the app surfaces it asks for, the compactions it was asked for and its ownership. Returns
    /// `false` when the session could not take an envelope.
    async fn settle_owed(&mut self, client: &ServerClient) -> bool {
        self.settle_live_sessions();
        // App surfaces this page holds documents for go out with what the
        // session owes, and a surface that arrived decides what waited
        // for it before anything else is read.
        self.ask_surfaces(client);
        if std::mem::take(&mut self.surfaces_arrived) || !self.replays.is_empty() {
            self.replay_surfaced();
        }
        for compaction in std::mem::take(&mut self.compactions) {
            self.compact(compaction).await;
        }
        // What the host brought into a sealed record's history is this device's to fold, as its
        // own writes are before each of them.
        let now = crate::clock::system_time(tilo_clock::unix_now_ns());
        for workspace_id in self.workspaces.clone() {
            let Some(workspace) = self.replicas.workspace(workspace_id) else {
                continue;
            };
            if let Err(error) = self
                .replicas
                .owner()
                .compact_outgoing(workspace, self.signer.as_ref(), now)
                .await
            {
                tracing::warn!(%error, "sealed histories stay uncompacted");
            }
        }
        self.renew_ownership().await;
        // What this device's ownership still owes the host goes as its queue takes it:
        // every answer from the host, every command, every tick is a
        // chance to hand it more.
        for workspace_id in self.workspaces.clone() {
            if !self.send_ownership(workspace_id, client) {
                return true;
            }
        }
        // A compaction is answered once ownership sent it.
        self.answer_compactions(client).await
    }

    /// Ask the host for every joined workspace whole again, as a refresh
    /// does: no admission failure kept. `false` when the
    /// session took no more.
    fn refresh(&mut self, client: &ServerClient) -> bool {
        self.clear_admission_failure();
        self.received.clear();
        self.resubscribe(client)
    }

    async fn serve(
        &mut self,
        client: ServerClient,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> Exit {
        // The last session's fetch proxy token died with its connection; this
        // one mints its own as it starts.
        self.reports.set_fetch_proxy(None);
        if !self.resume_session(&client) {
            return Exit::Lost("host session saturated during resume".into());
        }
        self.connected();
        // Each joined workspace's live session is joined again on this connection.
        self.live_sessions_due
            .extend(self.workspaces.iter().copied());
        let mut ping = n0_future::time::interval(PING_INTERVAL);
        let mut channel_tick = n0_future::time::interval(CHANNEL_TICK);
        loop {
            if !self.settle_channels(Some(&client)).await {
                return Exit::Lost("host session saturated".into());
            }
            if !self.settle_owed(&client).await {
                return Exit::Lost("host session saturated".into());
            }
            tokio::select! {
                biased;
                command = commands.recv() => {
                    // What else was asked already is taken before the session settles: the
                    // writes it publishes commit together.
                    let mut next = command;
                    let mut taken = 0_usize;
                    loop {
                        if let Some(exit) = self.take_command(&client, next).await {
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
                envelope = client.recv() => {
                    let Some(mut envelope) = envelope else {
                        return client.handshake_failure().map_or_else(
                            || Exit::Lost("the host closed the session".into()),
                            |error| Exit::Unproved(connect::link_error(error)),
                        );
                    };
                    // The server answered: what keeps a connection alive is read from this, not from
                    // the transport, which a stopped server or a relay holding it may leave open.
                    self.reports.note_contact(tilo_clock::unix_now_ms());
                    // What else the host sent is read in the same pass, so
                    // governance it replayed as many frames is admitted at
                    // once ([`Self::admit_governance`]), before anything else
                    // is decided.
                    let mut read = 0usize;
                    loop {
                        // The person stopped the agent this session is a run of: it ends here, and
                        // connects no more, so nothing of the agent is said again.
                        if matches!(envelope, Envelope::AgentSessionStopped) {
                            self.admit_governance();
                            self.fail("agent-run-stopped", "The person stopped this agent in Tilo.");
                            client.disconnect().await;
                            return Exit::Closed;
                        }
                        if let Err(exit) = self.received(&client, envelope) {
                            return exit;
                        }
                        read = read.saturating_add(1);
                        let next = (read < MAX_READ_AT_ONCE)
                            .then(|| client.inbound.try_lock().ok()?.try_recv().ok())
                            .flatten();
                        match next {
                            Some(next) => envelope = next,
                            None => break,
                        }
                    }
                    self.admit_governance();
                },
                _ = ping.tick() => {
                    if !self.heard_recently() {
                        return Exit::Lost("the server stopped answering".into());
                    }
                    if client.send(Envelope::Ping).is_err() {
                        return Exit::Lost("host session saturated".into());
                    }
                    self.check_admission();
                    self.ask_behind_again(tilo_clock::unix_now_ms());
                }
                _ = channel_tick.tick() => {
                    self.pump_channels(false);
                    // A live session that ended or is due to be asked again.
                    self.live_sessions_due.extend(self.workspaces.iter().copied());
                }
            }
        }
    }

    /// Take one command while connected; `None` when the handles are gone. Returns how the
    /// session ends, if it does.
    async fn take_command(
        &mut self,
        client: &ServerClient,
        command: Option<Command>,
    ) -> Option<Exit> {
        match command {
            None | Some(Command::Client(Cmd::Stop)) => {
                client.disconnect().await;
                Some(Exit::Closed)
            }
            Some(Command::Client(Cmd::Reconnect)) => {
                // The next session dials from this same endpoint.
                client.disconnect().await;
                Some(Exit::Reconnect)
            }
            Some(Command::Client(Cmd::Refresh)) => {
                (!self.refresh(client)).then(|| Exit::Lost("host session saturated".into()))
            }
            Some(command) => (!self.apply(command, Some(client)).await)
                .then(|| Exit::Lost("host session saturated".into())),
        }
    }

    /// Take in `envelope`, one the host sent: content asked for, or what
    /// [`Self::inbound`] decides.
    fn received(&mut self, client: &ServerClient, envelope: Envelope) -> Result<(), Exit> {
        // Governance gathered from the frames before is the workspace's before
        // anything after it is decided.
        if !matches!(envelope, Envelope::Delta { .. }) {
            self.admit_governance();
        }
        // The host's answer to this session's entry, before anything else it
        // says.
        if let Envelope::Entered(entered) = envelope {
            return self.entered(entered).map_or(Ok(()), Err);
        }
        // A host that keeps nothing of this person forgot them: the session
        // stops as not admitted, and the page joins again through its entry.
        if let Envelope::NotAuthorised {
            refusal: tilo_server_protocol::refusal::Refusal::NotAdmitted,
            ..
        } = &envelope
        {
            return Err(Exit::Refused(NOT_ADMITTED.0, NOT_ADMITTED.1.to_owned()));
        }
        if self.content_wanted(&envelope, client) {
            return Ok(());
        }
        let admitted = match &envelope {
            Envelope::Ready { workspace_id, .. } => Some(*workspace_id),
            _ => None,
        };
        if !self.begin(client, &envelope) {
            return Err(Exit::Lost("host session saturated".into()));
        }
        self.inbound(envelope);
        if let Some(workspace_id) = admitted {
            self.admitted(workspace_id, client);
        }
        Ok(())
    }

    /// Send a remote-action frame to device `to`, or say it cannot go.
    ///
    /// A frame for a workspace this session asked to join and the host has
    /// not admitted it to yet waits for the admission
    /// ([`Self::admitted`]): sent now, the host refuses it as a subscription
    /// it does not hold, and the request it carries is never answered. One
    /// for a workspace this session did not join goes as it is — the host's
    /// refusal is the answer, and the caller need not wait for it.
    async fn remote_action(
        &mut self,
        workspace_id: WorkspaceId,
        to: [u8; 32],
        frame: Vec<u8>,
        client: Option<&ServerClient>,
    ) {
        let Some(client) = client else {
            self.reports.emit(&Event::Remote(Heard::Undeliverable {
                workspace_id,
                to,
                refusal: tilo_server_protocol::refusal::Refusal::NotReady,
            }));
            return;
        };
        // The host's own operations, which no workspace decides, go to it on the session itself.
        if workspace_id == HOST_ITSELF {
            if to != self.config.server_cert.device_public_key {
                self.reports.emit(&Event::Remote(Heard::Undeliverable {
                    workspace_id,
                    to,
                    refusal: tilo_server_protocol::refusal::Refusal::Invalid(
                        "outside a workspace, only the host itself is asked".into(),
                    ),
                }));
                return;
            }
            let _ = client.send(Envelope::HostAction { frame });
            return;
        }
        let frame = match self.signed_frame(workspace_id, frame).await {
            Ok(frame) => frame,
            Err(refusal) => {
                self.reports.emit(&Event::Remote(Heard::Undeliverable {
                    workspace_id,
                    to,
                    refusal,
                }));
                return;
            }
        };
        if self.admitted_to(workspace_id) == Some(false) {
            let held = HeldAction {
                workspace_id,
                to,
                frame,
            };
            if let Err(refused) = self.held_actions.hold(held) {
                self.reports.emit(&Event::Remote(Heard::Undeliverable {
                    workspace_id: refused.workspace_id,
                    to: refused.to,
                    refusal: tilo_server_protocol::refusal::Refusal::Other(
                        "this computer is waiting for the workspace to let it back in, with \
                         earlier actions ahead of this one; nothing was sent, ask again in a \
                         moment"
                            .into(),
                    ),
                }));
            }
            return;
        }
        self.send_action(client, workspace_id, to, frame);
    }

    /// `frame` as this device sends it: an invocation in it signed under this
    /// device's own key, so the host runs it as this device's and anyone can
    /// check who asked. An invocation an agent's own key signed goes as it is,
    /// for the host to check against that key; so does every other message, and
    /// a frame that does not decode is the host's to refuse.
    async fn signed_frame(
        &self,
        workspace_id: WorkspaceId,
        frame: Vec<u8>,
    ) -> Result<Vec<u8>, tilo_server_protocol::refusal::Refusal> {
        use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
        use tilo_server_protocol::refusal::Refusal;
        let Ok(mut decoded) = RemoteActionFrame::decode(&frame) else {
            return Ok(frame);
        };
        // An agent's call is signed by the agent's own key already, and goes as the agent's:
        // this device carries it and never signs it over as its own.
        if !matches!(&decoded.message, RemoteActionMessage::Invoke(request) if request.signed.is_none())
        {
            return Ok(frame);
        }
        tilo_server_protocol::invocation::sign_message(
            &workspace_id.to_string(),
            &mut decoded.message,
            &self.config.cert,
            self.signer.as_ref(),
            tilo_clock::unix_now_ns(),
        )
        .await
        .map_err(|error| {
            Refusal::Other(format!("this device could not sign the action: {error}"))
        })?;
        decoded.encode().map_err(|error| {
            Refusal::Invalid(format!("the signed action does not encode: {error}"))
        })
    }

    /// Whether the host admitted this session to `workspace_id`: `None` for a
    /// workspace it did not ask to join, `Some(false)` while the host has not
    /// admitted it yet, and `Some(true)` once it has.
    fn admitted_to(&self, workspace_id: WorkspaceId) -> Option<bool> {
        self.reports
            .read(|status| status.workspaces.get(&workspace_id).copied())
    }

    fn send_action(
        &self,
        client: &ServerClient,
        workspace_id: WorkspaceId,
        to: [u8; 32],
        frame: Vec<u8>,
    ) {
        let _ = client.send(Envelope::RemoteAction {
            workspace_id,
            version: tilo_server_protocol::transport::REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: to,
            from_device_public_key: self.config.cert.device_public_key,
            from_user_public_key: self.config.cert.user_public_key,
            frame,
        });
    }

    /// Send the action frames that waited for `workspace_id`'s admission, in
    /// the order they were made. What this session owes another workspace
    /// waits for that one.
    fn flush_held_actions(&mut self, workspace_id: WorkspaceId, client: &ServerClient) {
        for held in self.held_actions.take(workspace_id) {
            self.send_action(client, held.workspace_id, held.to, held.frame);
        }
    }

    /// Ask the workspace for content ([`Cmd::WantContent`]): the host
    /// fans the request out and answers with what it keeps. Nothing while
    /// disconnected.
    fn want_content(
        &self,
        workspace_id: WorkspaceId,
        request: Vec<u8>,
        client: Option<&ServerClient>,
    ) {
        let Some(client) = client else {
            return;
        };
        let _ = client.send(Envelope::DocumentContentWant {
            workspace_id,
            from_device_public_key: self.config.cert.device_public_key,
            frame: request,
        });
    }

    /// Say `frames`, one burst, as one group of `workspace_id`'s presence
    /// track, sealed under the workspace scope's keys by the live session itself.
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

    /// Apply one command while disconnected; what it publishes commits at once.
    async fn apply_offline(&mut self, command: Command) {
        self.apply(command, None).await;
        self.commit_held().await;
    }

    /// Apply one command; `client` is the live session, if any. Returns
    /// `false` when the session could not take a durable envelope.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per command, each handing off to its own method"
    )]
    async fn apply(&mut self, command: Command, client: Option<&ServerClient>) -> bool {
        match command {
            Command::Client(Cmd::Join(workspace_id)) => self.join(workspace_id, client),
            Command::Client(Cmd::JoinPlaced(workspace, name)) => {
                self.join_placed(workspace, name, client)
            }
            Command::Placed(workspace_id) => {
                if self.placing.remove(&workspace_id).is_some() {
                    self.join(workspace_id, client)
                } else {
                    true
                }
            }
            Command::Client(Cmd::OpenDocumentAt(workspace_id, route)) => {
                self.open_resource(workspace_id, route, client)
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
            Command::Client(command @ (Cmd::Live(..) | Cmd::SealLive(..))) => {
                self.apply_live(command);
                true
            }
            Command::Client(Cmd::RemoteAction(workspace_id, to, frame)) => {
                self.remote_action(workspace_id, to, frame, client).await;
                true
            }
            Command::Client(Cmd::WantContent(workspace_id, request)) => {
                self.want_content(workspace_id, request, client);
                true
            }
            Command::Client(Cmd::KeepContent(workspace_id, kept)) => {
                self.kept.insert(workspace_id, kept);
                true
            }
            Command::Client(Cmd::InstallGroupKeys(route, _)) => {
                // The engine holds them already ([`crate::Link`] hands them over): settled with
                // the next channel changes, the same way as keys the channel's own governance
                // hands this session. The channel's records are sealed with them, and so is a
                // call on one of them while this device holds no channel state of its own.
                self.route_keys.insert(route.clone(), RouteKeys::Handed);
                self.new_keys.push(route);
                self.report_new_keys();
                true
            }
            Command::Client(Cmd::OpenRoute(route)) => {
                self.open_channel(&route);
                let asked = match route.workspace_id() {
                    Ok(workspace_id) if self.when_opened.contains(&workspace_id) => {
                        self.open_record_once(workspace_id, &route, client)
                    }
                    _ => true,
                };
                self.opened_routes.insert(route);
                asked
            }
            // Its mutation is held already ([`crate::Link`]): draft and publish what waits. A
            // change needs the record it changes, so one this session holds none of is asked
            // for as an opened route is: the write goes once the record lands.
            Command::Client(Cmd::Write(route)) => {
                if let Ok(workspace_id) = route.workspace_id() {
                    self.open_record_once(workspace_id, &route, client);
                }
                self.publish(&route);
                true
            }
            // Its holder signed it and this replica admitted it: ownership takes it as it is,
            // and a record this session holds none of is asked for as a write's is.
            Command::Client(Cmd::PublishSigned(route, op)) => {
                if let Ok(workspace_id) = route.workspace_id() {
                    self.open_record_once(workspace_id, &route, client);
                }
                self.publish_signed(&route, *op);
                true
            }
            Command::Client(Cmd::Barrier(done)) => {
                self.barrier(done, outbound::Scope::Session, None);
                true
            }
            Command::Client(Cmd::BarrierOf(workspaces, done)) => {
                self.barrier(
                    done,
                    outbound::Scope::Workspaces(workspaces.into_iter().collect()),
                    None,
                );
                true
            }
            Command::Client(Cmd::BarrierFor(routes, done)) => {
                let routes: BTreeSet<DocumentRoute> = routes.into_iter().collect();
                self.barrier(
                    done,
                    outbound::Scope::Documents(routes.clone()),
                    Some(routes),
                );
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
            Command::Client(Cmd::SayForSession(workspace_id, words)) => {
                self.say_for_run(workspace_id, words, client).await;
                true
            }
            Command::Client(Cmd::CallAudio(workspace_id, frames)) => {
                self.publish_call_audio(workspace_id, frames);
                true
            }
            Command::Client(Cmd::AgentSession(label, done)) => {
                self.session_as_agent(label, done, client)
            }
            Command::Client(Cmd::AskMember(approval, done)) => {
                self.ask_member(approval, done);
                true
            }
            Command::Client(Cmd::Enter(entering, done)) => {
                self.enter(entering, done, client);
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
                self.administer(&request, done, client);
                true
            }
            Command::Client(Cmd::Revoke(entry)) => self.revoke(entry, client),
            Command::Client(Cmd::SendControl { ws, msg, target }) => {
                self.send_control(ws, &msg, target, client);
                true
            }
            // Handled by the caller's loop.
            Command::Client(Cmd::Reconnect | Cmd::Retry | Cmd::Refresh | Cmd::Stop) => true,
            // What only a desktop's own live session carries: nothing a server session says.
            Command::Client(_) => true,
        }
    }

    /// Say one shared window's control message — a person's input on it, a
    /// viewer's ask or keyframe request — to the device hosting it, through
    /// the workspace's host: a page has no direct route to any device, and
    /// the host hands it to that device alone, stamped as this one's. Input
    /// lands within its frame or not at all, so one said while disconnected
    /// is dropped.
    fn send_control(
        &self,
        workspace_id: WorkspaceId,
        msg: &neighbour_wire::NeighbourMsg,
        target: iroh_base::EndpointId,
        client: Option<&ServerClient>,
    ) {
        if !matches!(
            msg,
            neighbour_wire::NeighbourMsg::RemoteInput { .. }
                | neighbour_wire::NeighbourMsg::SurfaceKeyframeRequest { .. }
                | neighbour_wire::NeighbourMsg::SurfaceViewRequest { .. }
        ) {
            return;
        }
        let (Some(client), Ok(frame)) = (client, postcard::to_allocvec(msg)) else {
            return;
        };
        let _ = client.send(Envelope::DocumentContent {
            workspace_id,
            to_device_public_key: *target.as_bytes(),
            from_device_public_key: self.config.cert.device_public_key,
            frame,
        });
    }

    /// Subscribe to `workspace_id`, now or once connected.
    fn join(&mut self, workspace_id: WorkspaceId, client: Option<&ServerClient>) -> bool {
        // Taking part again is taking part: a page's client says it on every pass over the
        // workspaces it shows, and one still being placed is subscribed once the host governs it.
        if self.workspaces.contains(&workspace_id) || self.placing.contains_key(&workspace_id) {
            return true;
        }
        self.workspaces.insert(workspace_id);
        self.live_sessions_due.insert(workspace_id);
        self.reports.change(|status| {
            status.workspaces.entry(workspace_id).or_insert(false);
        });
        self.received.clear();
        self.begun.remove(&workspace_id);
        client.is_none_or(|client| client.send(Self::subscribe(workspace_id)).is_ok())
    }

    /// Join `workspace` once the host governs it ([`Cmd::JoinPlaced`]).
    fn join_placed(
        &mut self,
        workspace: String,
        name: String,
        client: Option<&ServerClient>,
    ) -> bool {
        let Some(workspace_id) = WorkspaceId::parse(&workspace) else {
            return true;
        };
        if self.workspaces.contains(&workspace_id) {
            return true;
        }
        if !places_workspaces(&self.config.cert, &self.config.server_cert) {
            return self.join(workspace_id, client);
        }
        self.reports.change(|status| {
            status.workspaces.entry(workspace_id).or_insert(false);
        });
        self.placing.insert(workspace_id, (workspace, name));
        match client {
            Some(client) => self.place(workspace_id, client),
            None => true,
        }
    }

    /// Ask the host to govern the workspace joined as `workspace_id`; it is
    /// subscribed once the host answers, whatever it answers.
    fn place(&mut self, workspace_id: WorkspaceId, client: &ServerClient) -> bool {
        let Some((workspace, name)) = self.placing.get(&workspace_id).cloned() else {
            return true;
        };
        let commands = self.commands.clone();
        // Refused (not this person's host, a workspace governed elsewhere) or
        // unanswered: the workspace's admission decides.
        let (placed, answered) = crate::answer();
        n0_future::task::spawn(async move {
            let _ = answered.await;
            if let Some(commands) = commands.upgrade() {
                let _ = commands.send(Command::Placed(workspace_id));
            }
        });
        self.administer(
            &tilo_server_protocol::workspaces::WorkspacesRequest::Place { workspace, name },
            placed,
            Some(client),
        );
        true
    }

    /// Restore this session's workspaces, pending commits and call participations.
    fn resume_session(&mut self, client: &ServerClient) -> bool {
        // A page only follows its channel groups: said first, so no replay
        // this session asks for carries every member's request to be
        // admitted to one, which only a device that admits others reads.
        let mut sent_ok = client.send(Envelope::KeyFollower).is_ok();
        // A live session's grant is its session's token: the live sessions of the last
        // session ended with it, and every workspace's is joined again with
        // this one ([`Self::settle_live_sessions`]).
        self.live_sessions.end_session();
        // Every subscription starts whole: the host replays the workspace's
        // retained state, and duplicate detection begins with it.
        self.received.clear();
        self.begun.clear();
        for workspace_id in self.workspaces.clone() {
            sent_ok &= client.send(Self::subscribe(workspace_id)).is_ok();
        }
        let placing = self.placing.keys().copied().collect::<Vec<_>>();
        for workspace_id in placing {
            sent_ok &= self.place(workspace_id, client);
        }
        // The agent this session runs, said after the workspaces it enters, as it was said first.
        sent_ok &= self.declare_agent_run_again(client);
        sent_ok &= self.reopen_records(client);
        // What this page recorded while it was away is owed again, oldest
        // first: as much as the transport takes now, the rest as the host's
        // answers make room ([`Self::send_ownership`]).
        self.sending.clear();
        for workspace_id in self.workspaces.clone() {
            if !self.send_ownership(workspace_id, client) {
                break;
            }
        }
        sent_ok
    }

    /// Ask the host `request` of its workspace administration: over the
    /// session it answers, under an id this session picks so the answers of a
    /// session asking several may come back in any order.
    fn administer(
        &mut self,
        request: &tilo_server_protocol::workspaces::WorkspacesRequest,
        done: WorkspacesCompletion,
        client: Option<&ServerClient>,
    ) {
        let Some(client) = client else {
            let _ = done.send(Err(NOT_CONNECTED.into()));
            return;
        };
        let request_id = self.workspaces_next;
        self.workspaces_next = self.workspaces_next.wrapping_add(1);
        let frame = match postcard::to_stdvec(request) {
            Ok(frame) => frame,
            Err(error) => {
                let _ = done.send(Err(format!("the request does not encode: {error}")));
                return;
            }
        };
        self.workspaces_pending.insert(request_id, done);
        if client
            .send(Envelope::WorkspacesRequest { request_id, frame })
            .is_err()
            && let Some(done) = self.workspaces_pending.remove(&request_id)
        {
            let _ = done.send(Err("the host's session closed".into()));
        }
    }

    /// The host answered administration request `request_id`.
    fn workspaces_reply(&mut self, request_id: u64, frame: &[u8]) {
        if let Some(done) = self.workspaces_pending.remove(&request_id) {
            let _ = done.send(answered(frame));
        }
    }

    /// The session ended with requests the host never answered: each is
    /// answered here, so nothing waits on a connection that is gone.
    fn requests_lost(&mut self) {
        for (_, done) in std::mem::take(&mut self.workspaces_pending) {
            let _ = done.send(Err("the session ended before the host answered".into()));
        }
        for (_, done) in std::mem::take(&mut self.agent_sessions_pending) {
            let _ = done.send(Err("the session ended before the host answered".into()));
        }
    }

    /// Send, seal or open one live frame. Nothing leaves unsealed: a frame
    /// whose record's keys are not here yet waits for them (bounded), and a
    /// frame that names no record is dropped.
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
                Sealing::Sealed(sealed) => {
                    // Preview frames ride the workspace's live session, the way the
                    // document's own frames ride the session: only a member
                    // of its roster opens the scope's layer, and the frame
                    // inside it is sealed with the record's keys.
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
                Sealing::NoKeys(record) if hold => self.held_live.hold_outbound(
                    workspace_id,
                    record,
                    Waiting::Send {
                        workspace_id,
                        frame,
                    },
                ),
                Sealing::NoKeys(_) | Sealing::Dropped => {}
            },
            Waiting::Seal {
                workspace_id,
                frame,
                done,
            } => match self.seal_live(workspace_id, &frame) {
                Sealing::Sealed(sealed) => {
                    let _ = done.send(Some(sealed));
                }
                Sealing::NoKeys(record) if hold => self.held_live.hold_outbound(
                    workspace_id,
                    record,
                    Waiting::Seal {
                        workspace_id,
                        frame,
                        done,
                    },
                ),
                Sealing::NoKeys(_) | Sealing::Dropped => {
                    let _ = done.send(None);
                }
            },
            Waiting::Receive {
                workspace_id,
                from,
                frame,
            } => match self.open_live(workspace_id, &frame) {
                // A run's narration is the host's own, signed outside the live session.
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
                Opening::NoKeys(_) | Opening::Dropped => {}
            },
        }
    }

    /// `route`'s keys arrived: seal, send and open the live frames that
    /// waited for them.
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

    /// Seal canonical live `frame` of `workspace_id` with the content cipher
    /// of the record it names. A frame naming no record is never sent.
    fn seal_live(&self, workspace_id: WorkspaceId, frame: &[u8]) -> Sealing {
        let Some(frame) = tilo_server_protocol::live::LiveFrame::decode(frame).ok() else {
            return Sealing::Dropped;
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
            return Sealing::NoKeys(record.clone());
        };
        tilo_server_protocol::live::SealedLiveFrame::seal(
            &frame,
            &workspace_id.to_string(),
            |artifact, retention, cleartext| {
                keys.seal_content(artifact, retention, cleartext)
                    .map_err(|error| error.to_string())
            },
        )
        .map_or(Sealing::Dropped, Sealing::Sealed)
    }

    /// Open sealed live `frame` of `workspace_id` with the content cipher of
    /// the record it names into the canonical frame.
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

    /// Hold `token` for the rest of this session: what a page sends with
    /// every read through its host's fetch proxy.
    fn grant_fetch_proxy(&self, token: &[u8; 32]) {
        self.reports.set_fetch_proxy(Some(hex::encode(token)));
    }

    /// Whether `envelope` is an ask for a workspace's content, answered to the device
    /// that asked from what this device keeps of it: a member asking this device directly, or
    /// the server publishing its ask on its presence — how a server asks its members, as it has no
    /// session with each of them — which every member that keeps the content answers. A
    /// workspace this device keeps nothing of is left.
    fn content_wanted(&self, envelope: &Envelope, client: &ServerClient) -> bool {
        let device = self.config.cert.device_public_key;
        let Some((workspace_id, from, frame)) = wanted(envelope) else {
            return false;
        };
        let Some(kept) = self.kept.get(&workspace_id).filter(|_| from != device) else {
            return true;
        };
        let pieces =
            neighbour_wire::content::answer(&device, &from, &frame, |app, workspace, hash| {
                kept.read(app, workspace, hash)
            });
        for piece in pieces {
            let _ = client.send(Envelope::DocumentContent {
                workspace_id,
                to_device_public_key: from,
                from_device_public_key: device,
                frame: piece,
            });
        }
        true
    }

    /// Take this device into `route`'s channel, with the members its
    /// workspace's governance admits.
    fn open_channel(&mut self, route: &DocumentRoute) {
        if !self.route_keys.contains_key(route) {
            self.route_keys.insert(route.clone(), RouteKeys::Waiting);
        }
        // A channel taken up now asks for access in this pass.
        if matches!(self.route_keys.get(route), Some(RouteKeys::Waiting)) && !self.read_keys(route)
        {
            self.pump_channels(false);
        } else {
            self.report_new_keys();
        }
    }

    /// Run the channels' protocol once, then take up the keys it changed;
    /// `all` first reads every waiting route's again, as after the
    /// workspace's governance moved, so what it now names asks in this pass.
    fn pump_channels(&mut self, all: bool) {
        let now = tilo_clock::now();
        if all {
            self.read_all_keys();
        }
        if let Some(governance) = &self.governance {
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
        // Every route's keys are read off one reading of the workspace's key holders: asked
        // route by route, a device with many records open re-derived the whole membership's
        // access for each of them whenever a member came or went.
        let keys = self.engine.documents_seal_keys(&routes);
        for (route, keys) in routes.iter().zip(keys) {
            self.take_keys(route, here(keys));
        }
    }

    /// Take up `route`'s keys when the engine holds newer ones than the
    /// session; answers whether the route has keys.
    fn read_keys(&mut self, route: &DocumentRoute) -> bool {
        let keys = self.keys_of(route);
        self.take_keys(route, keys)
    }

    /// Take up `keys`, `route`'s keys as the engine holds them now, when they are newer than the
    /// session's; answers whether the route has keys.
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
    /// epoch `before`, which `keys` still hold: each sealed with the workspace's content keys at
    /// that epoch or an earlier one. A write sealed otherwise, or later, may open only now.
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
                .replicas
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

    /// Say keys as they are taken up, ahead of the records they open, which
    /// land only as the session settles its channels.
    fn report_new_keys(&self) {
        if !self.new_keys.is_empty() {
            self.report_keys();
        }
    }

    /// Say the key state ([`Self::describe_keys`]) when it changed.
    fn report_keys(&self) {
        let key_state = self.describe_keys();
        if self.reports.read(|status| status.key_state != key_state) {
            self.reports.change(|status| status.key_state = key_state);
        }
    }

    /// Store channel state and held edits when either changed, so a
    /// reloaded page keeps its access and what it wrote before the keys.
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

    /// Act on channel changes: await what the device's ownership signs, send
    /// the governance the channels said, install new keys, open what waited
    /// for them, and seal and commit what was held. Returns `false` when the
    /// session could not take an envelope.
    async fn settle_channels(&mut self, client: Option<&ServerClient>) -> bool {
        let mut delivered = true;
        self.keep_arrived();
        if self.workspace_keys.signing() {
            self.workspace_keys.signatures().await;
            self.pump_channels(false);
        }
        if let Some(client) = client {
            delivered &= self.send_governance(client);
        }
        loop {
            for route in std::mem::take(&mut self.new_keys) {
                if let Ok(workspace_id) = route.workspace_id() {
                    self.live_sessions_due.insert(workspace_id);
                }
                self.release_live(&route);
                // What the replica holds sealed for these keys opens now. Keys that open only
                // what the earlier ones did leave the record as its readers read it: telling them
                // it landed had each read every record of the workspace again on an admission,
                // and open the writes of its own it had not read yet, seconds for a large tracker.
                if !self.opens_as_before.remove(&route) {
                    self.landed(&route, None);
                }
                // A compaction it could not take without them is asked for again at once.
                if self.behind.contains_key(&route) {
                    self.replays.insert(route.clone());
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
        delivered
    }

    /// The key state's diagnostics: each workspace, route, live session and held write.
    fn describe_keys(&self) -> String {
        let mut channels = self.replicas.describe();
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
        // A write the replica holds undecided says what it waits for, so a reader is told what
        // the record wants (an app version this device does not run), not only its keys.
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
}

/// This device's channels as a reader: its state in `state`, sealed under
/// `vault`, signed by its ownership, and their readers its workspaces'.
fn open_keys(
    replicas: &crate::replica::Replicas,
    state: &crate::membership_check::StoredKeys,
    vault: [u8; 32],
    signer: &Arc<dyn DeviceSigner>,
    checks: &Arc<tilo_workspace_keys::SealChecks>,
) -> tilo_workspace_keys::WorkspaceKeys {
    let directory = || {
        Box::new(crate::workspace_keys::OwnerDirectory::reader(
            replicas.owner(),
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

/// The key service of a session beside its device's own that was handed
/// none of the device's ([`start_session_lent`]): it holds no keys and asks
/// for none, as a device with no secret store does, since the device's member
/// key in each workspace's group is its engine's.
fn keyless_keys(
    replicas: &crate::replica::Replicas,
    checks: &Arc<tilo_workspace_keys::SealChecks>,
) -> tilo_workspace_keys::WorkspaceKeys {
    tilo_workspace_keys::WorkspaceKeys::without_store(
        Box::new(crate::workspace_keys::OwnerDirectory::reader(
            replicas.owner(),
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
}

/// The keys `sealing` names, when they are here.
fn here(sealing: Result<crate::engine::SealKeys, String>) -> Option<key_group::ContentKeys> {
    match sealing {
        Ok(crate::engine::SealKeys::Here(keys)) => Some(keys),
        _ => None,
    }
}

/// This device's certificate for a workspace's other members, and the
/// revocations its person's root signed. A native shell accepts a member's
/// edits only from a device whose certificate it holds, and a browser has no
/// direct neighbours to hand it over, so it goes through the host, which
/// authenticates the author and keeps it for later joiners. A host reads the
/// revocations with it: a device of the person it may no longer trust, or the
/// stand-in approver it holds for them, is refused from the moment each entry
/// names on, and what they signed before stays valid.
fn identity_hello(
    workspace_id: WorkspaceId,
    cert: &DeviceCert,
    revocations: &RevocationList,
) -> Option<Envelope> {
    let mut entries: Vec<tilo_user_identity::RevocationEntry> =
        revocations.iter().cloned().collect();
    entries.sort_by_key(|entry| entry.revoked_device_public_key);
    let hello = neighbour_wire::NeighbourMsg::IdentityHello {
        protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
        cert: Box::new(cert.clone()),
        revocations: entries,
    };
    let (channel, _) = neighbour_wire::retention(&hello, false)?;
    let body = postcard::to_allocvec(&hello).ok()?;
    Some(Envelope::Delta {
        workspace_id,
        author_device_public_key: cert.device_public_key,
        authored_at_ns: tilo_clock::unix_now_ns(),
        delta: CoeditPayload::Checkpoint { channel, body }.encode().ok()?,
        signature: [0; 64],
    })
}

/// The reply `frame` carries, or the refusal a page shows when it does not
/// decode.
fn answered<T: serde::de::DeserializeOwned>(frame: &[u8]) -> Result<T, String> {
    postcard::from_bytes(frame).map_err(|error| format!("the answer does not decode: {error}"))
}

/// Whether `cert`'s device may ask the host certified as `host` to place a
/// workspace: only a device of the host's own person may; the host refuses
/// anyone else's, so a visitor's page never asks.
fn places_workspaces(cert: &DeviceCert, host: &DeviceCert) -> bool {
    cert.user_public_key == host.user_public_key
}

/// The workspace, the device that asked and the request frame `envelope` carries, when it asks
/// this device for a workspace's content: a member's ask addressed to it, or the server's
/// ask published on its presence — whose attributed body is the request, the server asking its
/// members as a whole rather than each in a session — which every member that keeps the content
/// answers. `None` for any other envelope, and for presence that is not such an ask (a person's
/// presence passes as it always did).
fn wanted(envelope: &Envelope) -> Option<(WorkspaceId, [u8; 32], Vec<u8>)> {
    match envelope {
        Envelope::DocumentContentWant {
            workspace_id,
            from_device_public_key: from,
            frame,
        } => Some((*workspace_id, *from, frame.clone())),
        Envelope::Presence {
            workspace_id,
            payload,
        } => {
            let attributed = tilo_server_session::AttributedPresence::decode(payload)?;
            let Ok(neighbour_wire::NeighbourMsg::DocumentContentWant {
                requester,
                cluster_id,
                ..
            }) = neighbour_wire::NeighbourMsg::decode(&attributed.body)
            else {
                return None;
            };
            (requester == tilo_peer_state::peer_handle(&attributed.author)
                && cluster_id == workspace_id.to_string())
            .then_some((*workspace_id, attributed.author, attributed.body))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Actions waiting for a workspace to let the session back in are never given up: past
    /// as many as are held, the newest is refused at once, and every one held before goes out
    /// in order once the workspace admits the session. Giving up the oldest lost a person's
    /// action they had been told was on its way.
    #[test]
    fn a_held_action_is_never_dropped_for_a_newer_one() {
        let lounge = WorkspaceId::from_u128(1);
        let other = WorkspaceId::from_u128(2);
        let action = |workspace_id, n: usize| HeldAction {
            workspace_id,
            to: [0; 32],
            frame: n.to_le_bytes().to_vec(),
        };
        let mut held = HeldActions::default();
        assert!(held.hold(action(other, usize::MAX)).is_ok());
        for n in 1..MAX_HELD_ACTIONS {
            assert!(held.hold(action(lounge, n)).is_ok(), "action {n} is held");
        }
        let refused = held
            .hold(action(lounge, MAX_HELD_ACTIONS))
            .expect_err("one past the bound is refused, not held");
        assert_eq!(refused.frame, MAX_HELD_ACTIONS.to_le_bytes());
        let sent: Vec<Vec<u8>> = held
            .take(lounge)
            .into_iter()
            .map(|held| held.frame)
            .collect();
        let made: Vec<Vec<u8>> = (1..MAX_HELD_ACTIONS)
            .map(|n| n.to_le_bytes().to_vec())
            .collect();
        assert_eq!(
            sent, made,
            "every held action goes out, in the order it was made"
        );
        assert_eq!(
            held.take(other).len(),
            1,
            "another workspace's action still waits"
        );
        assert!(
            held.hold(action(lounge, 0)).is_ok(),
            "held again once they went out"
        );
    }

    /// Three keep-alive intervals without a frame from the server end the connection; anything it
    /// said within them, a `Pong` included, keeps it.
    #[test]
    fn three_keepalives_without_an_answer_end_the_connection() {
        assert!(silence_is_short(Some(0), 44_999));
        assert!(!silence_is_short(Some(0), 45_000), "three pings unanswered");
        assert!(
            silence_is_short(Some(30_000), 74_999),
            "the last answer counts"
        );
        assert!(!silence_is_short(Some(30_000), 75_000));
        assert!(
            silence_is_short(None, u64::MAX),
            "nothing heard yet is the handshake's own deadline to decide"
        );
    }

    /// A visitor's page joins a workspace of another person's host without
    /// asking that host to place it, which the host refuses to anyone but its
    /// own person; a page of the host's person still asks.
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
    fn workspaces_not_admitted_within_the_timeout_are_overdue_once_subscribed() {
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
        // An HTML error page from a proxy does not decode: reachability.
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
    /// waits for, and only asks that reached no live session count towards the note
    /// in the members' place.
    #[test]
    fn a_live_session_that_was_not_reached_is_named_in_the_channel_diagnostics() {
        let mut live_sessions = live_session::LiveSessions::default();
        live_sessions.later(WorkspaceId::from_u128(7), 12_000);
        assert_eq!(
            live_sessions.describe(),
            "live session 00000000-0000-0000-0000-000000000007: not joined. No live session was reached; \
             asking again at 12000"
        );
        assert_eq!(live_sessions.missed(WorkspaceId::from_u128(7)), 1);
        assert_eq!(
            live_sessions.missed(WorkspaceId::from_u128(7)),
            live_session::MISSES_BEFORE_NOTE
        );
        live_sessions.refused(WorkspaceId::from_u128(7), 60_000);
        assert_eq!(
            live_sessions.describe(),
            "live session 00000000-0000-0000-0000-000000000007: not joined. The workspace does not \
             admit this page's live_session; asking again at 60000"
        );
        assert_eq!(
            live_sessions.missed(WorkspaceId::from_u128(7)),
            1,
            "an answer starts the count again"
        );
        // A live session never asked for at all says what the page still lacks.
        let mut unasked = live_session::LiveSessions::default();
        unasked.note_unasked(
            WorkspaceId::from_u128(8),
            "This page holds no channel keys for its workspace yet.",
        );
        assert_eq!(
            unasked.describe(),
            "live session 00000000-0000-0000-0000-000000000008: not asked. This page holds no channel \
             keys for its workspace yet."
        );
        // Asked once, the lack is no longer what the page says.
        unasked.began(WorkspaceId::from_u128(8), 1_000, 1, &BTreeSet::new());
        assert!(!unasked.describe().contains("not asked"));
    }
}

#[cfg(test)]
mod behind_tests;
#[cfg(test)]
mod content_wanted_tests;
#[cfg(test)]
mod iroh_connect_tests;
#[cfg(test)]
mod late_admission_tests;
#[cfg(test)]
mod opened_records_tests;
#[cfg(test)]
mod resumed_governance_tests;
