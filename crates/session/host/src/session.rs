//! The certified relay session of one device.
//!
//! One task owns the connection to the user's host and everything that must
//! be serialized with it: document signing, the commit outbox, reconnects.
//! Callers talk to it through a cheap [`PeerHandle`].
//!
//! - **Optimistic local edits, host order.** A published document frame is
//!   signed, appended to the outbox (persisted through the [`PeerStore`]) and
//!   sent as an `Envelope::Commit`. It stays outstanding until the host
//!   answers `Committed` with its order; the local replica already shows it.
//! - **Reconnect replays.** After any disconnect the task reconnects with
//!   backoff, re-subscribes every joined workspace (the host replays retained
//!   checkpoints and this device's queue), rejoins the live plane and resends
//!   the whole outbox in order. Re-applying a document change is idempotent.
//! - **Fail closed.** An expired certificate, or a host that keeps refusing
//!   the handshake (a revoked device), ends in [`TransportPhase::Failed`]
//!   instead of retrying forever; only [`PeerHandle::reconnect`] retries.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard};

use std::time::Duration;
use tilo_session_client::content::Kept;
use tilo_session_core::remote::Heard;

use collab_types::DocumentMutation;
use collab_types::inbound::{InboundHold, OpKey};
use tilo_relay_client::{CoeditPayload, DeviceSigner};
use tilo_relay_protocol::Envelope;
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

use crate::custody::{OutboxEntry, PeerRecord, PeerStore};
use crate::document::sign_frame;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::{
    Ciphers, ContentCipher, DocumentRoute, OpenedDocument, Reassembly, open_frame,
};
use tilo_relay_client::LinkError;
use tilo_relay_session::RelayClient;

mod connect;
mod governance;
mod inbound;
mod plane;
mod voice;

use connect::Dial;
#[cfg(test)]
use connect::{connect, iroh_link_error};
use governance::HostGovernance;
use inbound::{HeldFrame, HoldReason};

/// Longest wait between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Keep-alive interval on an idle session.
const PING_INTERVAL: Duration = Duration::from_secs(15);
/// How often overdue channel access requests are sent again.
const CHANNEL_TICK: Duration = Duration::from_secs(1);
/// Documents held per route while this device waits for its keys; past it
/// the oldest is owed by the host's replay of the record.
const MAX_HELD_DOCUMENTS: usize = 4096;
/// Consecutive refusals with no reason (an iroh host closing the handshake)
/// after which the host is taken to no longer admit this device rather than
/// to be unreachable. A refusal that says why decides at once.
const REFUSALS_BEFORE_FAILING: u32 = 3;
/// The host does not admit this device's person (`not-admitted`): it never
/// knew them, forgot them (its state was reset, or it pruned them) or a workspace
/// took them out. The page joins again where the workspace's door lets it.
const NOT_ADMITTED: (&str, &str) = (
    "not-admitted",
    "This workspace’s host no longer has you as a member.",
);

/// What a failed connection says about this device.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// The host was not reached, or something in front of it answered:
    /// retried with backoff, never counted against the device.
    Reachability,
    /// The host turned the device away without saying why: counted, and
    /// taken as [`NOT_ADMITTED`] after [`REFUSALS_BEFORE_FAILING`] in a row.
    Unexplained,
    /// The host said why: the session stops with this failure code and copy
    /// until the page acts on it.
    Final(&'static str, &'static str),
}

fn refusal(error: &LinkError) -> Refusal {
    match error {
        LinkError::HostChanged => {
            Refusal::Final("host-changed", "This workspace’s host was set up again.")
        }
        LinkError::Unavailable(_) | LinkError::Malformed(_) | LinkError::Sign(_) => {
            Refusal::Reachability
        }
        // A host that turns a device away closes its handshake without a
        // word, so its own reason never reaches the page.
        LinkError::Refused(_) => Refusal::Unexplained,
        // Another peer protocol version: whichever side is behind updates
        // (docs/architecture/compatibility.md). This page is the host's
        // build, so a newer host means a reload brings it up to date.
        LinkError::PeerVersion { theirs }
            if tilo_relay_protocol::auth::this_device_is_behind(*theirs) =>
        {
            Refusal::Final(
                "update-required",
                "This page is out of date. Reload it to keep working together.",
            )
        }
        LinkError::PeerVersion { .. } => Refusal::Final(
            "host-update-required",
            "This workspace’s host needs to update to keep working together.",
        ),
    }
}
/// How long a connected session waits for the host to admit its workspaces before
/// it reports `admission-timeout`. Checked on the keep-alive tick.
const ADMISSION_TIMEOUT_MS: u64 = 30_000;
/// Failures kept at once; the oldest goes first.
const MAX_FAILURES: usize = 16;

/// Where to reach the user's host: the iroh endpoint it serves its peers'
/// sessions on ([`tilo_relay_client::Host`]).
pub use tilo_relay_client::Host;

/// Where to connect and as whom.
#[derive(Clone)]
pub struct PeerConfig {
    /// The user's host (`tilo-relay`).
    pub host: Host,
    /// This device's certificate.
    pub cert: DeviceCert,
    /// The host certificate pinned at certification.
    pub host_cert: DeviceCert,
    /// Root-signed revocations this device knows about.
    pub revocations: RevocationList,
}

/// Transport phase, in `SessionHealth` v1 vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportPhase {
    /// First connection attempt.
    Connecting,
    /// Authenticated and serving.
    Connected,
    /// Lost the connection; retrying.
    Reconnecting,
    /// Waiting for the next retry.
    Offline,
    /// Refused; retries only on [`PeerHandle::reconnect`].
    Failed,
    /// Disposed.
    Closed,
}

/// A failure in `SessionHealth` v1 vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// `FailureCode`: `transport-unavailable`, `transport-ended`,
    /// `certificate-expired`, `certificate-rejected`, `not-admitted`,
    /// `device-revoked`, `host-changed`, `agent-run-stopped`,
    /// `protocol-rejected`, `permission-required`, `signature-rejected`,
    /// `storage-transaction-failed`, `admission-timeout`, and the codes the
    /// page reports (see [`PeerHandle::report_failure`]).
    pub code: &'static str,
    /// Safe product copy.
    pub message: String,
    /// When it happened (ms since the epoch).
    pub at_ms: u64,
    /// What failed. A subject other than [`FailureSubject::Session`] holds at
    /// most one failure at a time.
    pub subject: FailureSubject,
}

/// What a [`Failure`] is about. It decides the `SessionHealth` scope and the
/// recovery the failure offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailureSubject {
    /// The session itself: transport, identity, protocol.
    Session,
    /// The page's durable store (`retry-saving`).
    Storage,
    /// Admission of the joined workspaces (`refresh-checkpoint`).
    Admission,
    /// One desktop surface's renderer (`retry-desktop`).
    Surface(String),
    /// An asset one desktop surface could not load (`retry-asset`).
    Asset {
        surface: String,
        /// The surface's id for the asset.
        asset: String,
    },
}

/// Failure codes the page may report for a subject it owns.
pub const PAGE_FAILURE_CODES: &[&str] = &[
    "storage-quota",
    "storage-denied",
    "storage-transaction-failed",
    "asset-unavailable",
    "gpu-unavailable",
    "gpu-device-lost",
    "renderer-initialization-failed",
];

/// Local durability of one record as the page's store reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalState {
    /// Nothing reported yet.
    Unknown,
    /// Edits not yet written.
    Pending,
    /// A write is in flight.
    Committing,
    /// Everything written durably.
    Durable,
    /// The last write failed.
    Failed,
}

/// Per-route bookkeeping behind `RecordHealth`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteStatus {
    /// Commits sent and not yet acknowledged.
    pub pending_operations: usize,
    pub durable_operations: u64,
    /// Commits the host refused this session ([`PeerEvent::CommitRefused`]).
    pub refused_operations: u64,
    /// Host order of the latest acknowledged commit.
    pub last_seq: Option<u64>,
    /// Local store state, as reported with [`PeerHandle::set_local_state`].
    pub local: Option<LocalState>,
    /// Sealed documents that arrived before this device held the route's
    /// keys; they open once the keys do.
    pub awaiting_keys: usize,
    /// Documents that arrived written by an app version whose surface this
    /// page does not hold; they are decided once the workspace answers it.
    pub awaiting_surface: usize,
    /// Documents that arrived before the workspace's governance could decide
    /// them (its author, the workspace, or the app version it runs).
    pub awaiting_governance: usize,
}

impl RouteStatus {
    /// The documents this route holds, undecided, whatever they wait for.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.awaiting_keys
            .saturating_add(self.awaiting_surface)
            .saturating_add(self.awaiting_governance)
    }
}

/// Everything a snapshot is derived from.
#[derive(Clone, Debug)]
pub struct PeerStatus {
    pub phase: TransportPhase,
    /// Whether the current connection completed the handshake.
    pub authenticated: bool,
    /// Consecutive failed attempts.
    pub attempt: u32,
    /// When the next attempt runs (ms since the epoch).
    pub next_attempt_at_ms: Option<u64>,
    /// Last envelope from the host (ms since the epoch).
    pub last_contact_at_ms: Option<u64>,
    /// Joined workspaces and whether the host answered `Ready`.
    pub workspaces: BTreeMap<u64, bool>,
    /// Per-route status.
    pub routes: BTreeMap<DocumentRoute, RouteStatus>,
    /// Current failures, newest last.
    pub failures: Vec<Failure>,
    /// Commits not yet acknowledged, over every route.
    pub outgoing_pending: usize,
    /// This device.
    pub device: [u8; 32],
    /// Bumped on every change.
    pub generation: u64,
    /// This page's channel state, one line per workspace, certificate and
    /// route, for diagnostics.
    pub channels: String,
    /// The members each joined workspace's governance lists, by workspace, by
    /// person id: what someone no longer among them made on the workspace's
    /// desktop is released ([`tilo_desktop_model::ownership::released`]).
    pub room_members: BTreeMap<String, BTreeSet<String>>,
    /// Whose each member device of a joined workspace is, by workspace: device (hex) to person
    /// (root key, hex), as the workspace's governance admitted them. Who is here is shown by
    /// person from it, so someone the live plane never certified to this page is still theirs.
    pub room_devices: BTreeMap<String, BTreeMap<String, String>>,
}

impl PeerStatus {
    /// `device`'s session as it starts: connecting, nothing joined yet.
    fn connecting(device: [u8; 32]) -> Self {
        Self {
            phase: TransportPhase::Connecting,
            authenticated: false,
            attempt: 0,
            next_attempt_at_ms: None,
            last_contact_at_ms: None,
            workspaces: BTreeMap::new(),
            routes: BTreeMap::new(),
            failures: Vec::new(),
            outgoing_pending: 0,
            device,
            generation: 0,
            channels: String::new(),
            room_members: BTreeMap::new(),
            room_devices: BTreeMap::new(),
        }
    }
}

/// Something the session produced for its consumers.
#[derive(Clone, Debug)]
pub enum PeerEvent {
    /// A verified document frame from another device (or this device's own,
    /// replayed from the host after a reconnect).
    Document(Box<OpenedDocument>),
    /// A document that failed verification and was dropped.
    DocumentRejected(String),
    /// The host admitted a workspace subscription.
    Ready {
        /// The workspace id.
        workspace_id: u64,
    },
    /// A commit became durable on the host.
    Committed {
        route: DocumentRoute,
        /// Host order.
        seq: u64,
    },
    /// The host refused a commit of this device: nothing of it was kept
    /// or passed on, and it is no longer outstanding. Its changes are
    /// shared state nowhere but in this device's replica, and every later
    /// local change depends on them, so the record's owner withdraws them
    /// and tells whoever made them.
    CommitRefused {
        route: DocumentRoute,
        /// The Automerge hashes of the changes it carried, as its claims
        /// name them; empty for a snapshot.
        changes: Vec<[u8; 32]>,
        /// The host's reason, for logs
        /// ([`tilo_relay_protocol::refusal::plain_reason`] tells a person).
        reason: String,
    },
    /// A live frame another member published, opened with its record's
    /// channel keys.
    Live {
        /// The workspace id.
        workspace_id: u64,
        /// Host-stamped publisher.
        from: [u8; 32],
        /// Canonical live frame.
        frame: Vec<u8>,
    },
    /// A remote-action frame another device of the workspace addressed to this one (a request, or
    /// the answer to one this device sent), or word that one it sent could not be delivered.
    Remote(Heard),
    /// A piece of content a workspace's records reference, sent to this device
    /// alone by a member that keeps it (the host's own device included),
    /// in answer to [`PeerHandle::want_content`].
    RecordContent {
        /// The workspace id.
        workspace_id: u64,
        /// Host-stamped sending device.
        from: [u8; 32],
        /// A `NeighbourMsg::RecordContentPiece`, postcard-encoded.
        frame: Vec<u8>,
    },
    /// A frame another member published on the workspace's live plane
    /// ([`plane`]), opened with the scope's channel keys: what a device
    /// seals on its tracks, carried by the relay that serves the workspace
    /// rather than by this session.
    LivePlane {
        /// The workspace id.
        workspace_id: u64,
        /// The device that sealed it.
        from: [u8; 32],
        /// The track it came from ([`tilo_live::live::Track`]).
        track: tilo_live::live::Track,
        /// The object's sequence on that track.
        sequence: u32,
        /// The opened body.
        body: Vec<u8>,
    },
    /// A call's voice heard on `workspace_id`'s plane
    /// ([`plane`]): one Opus frame, opened with the scope's keys, as
    /// the device `from` published it.
    CallAudio {
        /// The workspace id.
        workspace_id: u64,
        /// The device whose voice it is.
        from: [u8; 32],
        /// The frame's place in that device's voice stream.
        sequence: u64,
        /// The first sample's place in the sender's 48 kHz timeline.
        timestamp_48k: u64,
        /// The Opus frame.
        opus: Vec<u8>,
    },
    /// One picture of a call on `workspace_id`'s plane: an encoded
    /// tilo-video/1 object of `from`'s camera or screen, opened with the
    /// scope's keys.
    CallPicture {
        /// The workspace id.
        workspace_id: u64,
        /// The device whose picture it is.
        from: [u8; 32],
        /// The camera or screen it shows.
        source: tilo_live::video::Source,
        /// Whether it starts a group: the app's decoder starts here.
        keyframe: bool,
        /// The tilo-video/1 object, header and access unit.
        object: Vec<u8>,
    },
    /// What this page's call now encodes and starts over: the layers of its
    /// camera and screen the call's devices show, and the sources they need
    /// afresh.
    CallDemand {
        /// The workspace id.
        workspace_id: u64,
        /// The layers shown of each source.
        wanted: Vec<(tilo_live::video::Source, tilo_live::video::Layer)>,
        /// The sources a device needs a keyframe of.
        keyframes: Vec<tilo_live::video::Source>,
    },
    /// The host holds a long sealed history of `route` it cannot read and
    /// asks this device for a snapshot. Answer with
    /// [`PeerHandle::compact`] after applying every [`PeerEvent::Document`]
    /// received before this event: the snapshot is taken to contain them.
    /// A consumer without the document ignores it.
    CompactionRequested {
        route: DocumentRoute,
        /// Echo in [`PeerHandle::compact`].
        request: [u8; 16],
    },
    /// What the host said about this device's calls.
    Voice(crate::voice::VoiceEvent),
    /// The host sent everything it holds of a record this device opened
    /// ([`PeerHandle::open_record`]) as [`PeerEvent::Document`]s, before
    /// this event; its changes keep arriving from now on.
    RecordOpened {
        /// The workspace id.
        workspace_id: u64,
        /// The record.
        route: DocumentRoute,
        /// How many frames the host sent.
        frames: u32,
    },
}

/// What the host addressed to this device that it only passes on: an
/// action frame or what became of one, a piece of content, a member's
/// coming or going.
fn addressed(envelope: Envelope) -> Option<PeerEvent> {
    match envelope {
        Envelope::RemoteAction {
            workspace_id,
            from_device_public_key,
            frame,
            ..
        } => Some(PeerEvent::Remote(Heard::Action {
            workspace_id,
            from: from_device_public_key,
            frame,
        })),
        Envelope::RecordContent {
            workspace_id,
            from_device_public_key,
            frame,
            ..
        } => Some(PeerEvent::RecordContent {
            workspace_id,
            from: from_device_public_key,
            frame,
        }),
        Envelope::RemoteActionUndeliverable {
            workspace_id,
            to_device_public_key,
            reason,
        } => Some(PeerEvent::Remote(Heard::Undeliverable {
            workspace_id,
            to: to_device_public_key,
            reason,
        })),
        _ => None,
    }
}

/// One Opus frame of a page's microphone, as its call publishes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallFrame {
    /// The first sample's place in the page's 48 kHz timeline.
    pub timestamp_48k: u64,
    /// The Opus frame.
    pub opus: Vec<u8>,
}

enum Command {
    Join(u64),
    /// Join the workspace (its UUID) of this device's person, named as given,
    /// once the host governs it ([`PeerHandle::join_placed`]).
    JoinPlaced(String, String),
    /// The host answered the placement of this workspace.
    Placed(u64),
    OpenRecord(u64, DocumentRoute),
    Publish(DocumentRoute, Vec<Vec<u8>>),
    InstallCipher(DocumentRoute, Box<dyn ContentCipher>),
    InstallChannelKeys(DocumentRoute, channel_crypto::ChannelContentKeys),
    OpenRoute(DocumentRoute),
    /// What asking the host for `workspace_id`'s live plane, and joining it,
    /// came to: the plane itself, except on wasm, where the page's carrier
    /// cannot travel in a command and waits in [`plane::arrived`].
    #[cfg(not(target_arch = "wasm32"))]
    PlaneJoined(u64, Result<Arc<tilo_live::plane::Plane>, plane::Failed>),
    #[cfg(target_arch = "wasm32")]
    PlaneJoined(u64),
    /// The plane of `workspace_id` heard something: drain it.
    PlaneHeard(u64),
    /// The plane of `workspace_id` ended: the relay closed it, or the plane
    /// no longer admits what it reads.
    PlaneEnded(u64),
    Live(u64, Vec<u8>),
    SealLive(u64, Vec<u8>, LiveCompletion),
    RemoteAction(u64, [u8; 32], Vec<u8>),
    WantContent(u64, Vec<u8>),
    /// What this device keeps of the workspace's content, answered to the members that ask.
    KeepContent(u64, Arc<dyn Kept>),
    Barrier(Completion),
    /// Presence frames for the workspace's plane, said as this device.
    Presence(u64, Vec<collab_types::presence::Presence>),
    /// The page's microphone frames, published on the workspace's plane as
    /// this device's voice.
    CallAudio(u64, Vec<CallFrame>),
    VoiceCalls(u64),
    VoiceCall(u64, [u8; 16], tilo_relay_protocol::voice::CallOperation),
    VoiceJoin(u64, [u8; 32]),
    VoiceLeave(u64),
    VoiceShow(u64, tilo_relay_protocol::voice::Showing),
    /// Pictures this page's camera and screen encoded, for the plane of the
    /// workspace named.
    CallPictures(u64, Vec<CallPicture>),
    /// What this page shows of each device in its call's plane: the layers of
    /// their camera and screen it draws.
    CallShowing(u64, Vec<CallShown>),
    /// The windows whose pages this page draws, by the device hosting them: each
    /// owner publishes a window's page only while a member asks for its track.
    ShowPages(u64, Vec<PageShown>),
    /// This page needs `source` of `device` afresh: a keyframe from its next
    /// group on.
    CallPictureRestart(u64, [u8; 32], tilo_live::video::Source),
    Administer(
        tilo_relay_protocol::workspaces::WorkspacesRequest,
        RoomsCompletion,
    ),
    AgentRun(String, AgentRunCompletion),
    Compact([u8; 16], Vec<u8>),
    Reconnect,
    /// The network is back: a session waiting out its backoff tries again
    /// now; a connected or failed one stays as it is.
    Retry,
    /// A revocation this device's person's root signed: announce it to
    /// every workspace this session is admitted to.
    Revoke(tilo_user_identity::RevocationEntry),
    Refresh,
    Close,
}

/// One tilo-video/1 object this page encoded for its call's plane: one layer
/// of its camera or screen.
#[derive(Clone, Debug)]
pub struct CallPicture {
    pub source: tilo_live::video::Source,
    pub layer: tilo_live::video::Layer,
    /// The object's place in the source's stream.
    pub sequence: u64,
    pub keyframe: bool,
    pub timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    /// The encoded access unit.
    pub payload: Vec<u8>,
}

/// What this page shows of one device in its call's plane.
#[derive(Clone, Debug)]
pub struct CallShown {
    pub device: [u8; 32],
    /// The camera layers it draws.
    pub camera: Vec<tilo_live::video::Layer>,
    /// The screen layers it draws.
    pub screen: Vec<tilo_live::video::Layer>,
}

/// The windows whose pages one device of a workspace hosts, as this page draws them.
#[derive(Clone, Debug, Default)]
pub struct PageShown {
    /// The device hosting the windows.
    pub device: [u8; 32],
    /// The tabs the windows stand in, the same ids on every device of the workspace.
    pub tabs: Vec<u64>,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

/// What sealing a live frame came to.
enum Sealing {
    Sealed(Vec<u8>),
    /// This device holds no keys for the record yet.
    NoKeys(tilo_relay_protocol::live::LiveRecord),
    /// Names no record, or does not seal: never sent.
    Dropped,
}

/// What opening a sealed live frame came to.
enum Opening {
    Opened(Vec<u8>),
    /// This device holds no keys for the channel yet.
    NoKeys([u8; 16]),
    /// Malformed, or does not open.
    Dropped,
}
/// Called once with the outcome of a [`PeerHandle::barrier_then`].
pub type Completion = Box<dyn FnOnce(Result<(), String>) + Send>;
/// Called once with a live frame sealed or opened with its record's channel
/// keys, or `None` when this device holds none; see
/// [`PeerHandle::seal_live_then`].
pub type LiveCompletion = Box<dyn FnOnce(Option<Vec<u8>>) + Send>;
/// Called once with whether the session is now the agent's run, see
/// [`PeerHandle::run_as_agent_then`].
pub type AgentRunCompletion = Box<dyn FnOnce(Result<(), String>) + Send>;
/// Called once with the host's answer, see [`PeerHandle::rooms_then`].
pub type RoomsCompletion =
    Box<dyn FnOnce(Result<tilo_relay_protocol::workspaces::WorkspacesReply, String>) + Send>;
type Queue = Arc<Mutex<VecDeque<PeerEvent>>>;

struct Shared {
    status: PeerStatus,
    queues: Vec<std::sync::Weak<Mutex<VecDeque<PeerEvent>>>>,
    wakes: Vec<Wake>,
    /// Wakes that last as long as their consumer's event queue
    /// ([`PeerHandle::on_event`]), dropped with it.
    listeners: Vec<(std::sync::Weak<Mutex<VecDeque<PeerEvent>>>, Wake)>,
    /// The current session's bearer token for its host's fetch proxy
    /// (`tilo_net::proxy`, at `/peer/v1/fetch`); kept apart from
    /// [`PeerStatus`], which diagnostics show.
    fetch_proxy: Option<String>,
    /// What the page shows where its live plane would be, when it has none:
    /// that no plane could be reached ([`PeerHandle::live_note`]).
    live_note: Option<String>,
}

/// A cheap handle on a running session.
#[derive(Clone)]
pub struct PeerHandle {
    commands: mpsc::UnboundedSender<Command>,
    shared: Arc<Mutex<Shared>>,
    events: Queue,
}

/// One consumer's view of the session's events: every event produced after
/// it was created, independent of other consumers.
pub struct PeerEvents(Queue);

impl PeerEvents {
    /// Take every event produced since the last call.
    pub fn drain(&self) -> Vec<PeerEvent> {
        self.0.lock_recover().drain(..).collect()
    }
}

impl Shared {
    /// Everyone to wake, forgetting the listeners whose queue is gone.
    fn wakes_now(&mut self) -> Vec<Wake> {
        self.listeners.retain(|(queue, _)| queue.strong_count() > 0);
        self.wakes
            .iter()
            .cloned()
            .chain(self.listeners.iter().map(|(_, wake)| Arc::clone(wake)))
            .collect()
    }
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock_recover()
}

/// Apply `change` to the status, bump its generation and wake the watchers
/// once the lock is released.
fn change_status(shared: &Mutex<Shared>, change: impl FnOnce(&mut PeerStatus)) {
    let wakes = {
        let mut shared = lock(shared);
        change(&mut shared.status);
        shared.status.generation = shared.status.generation.wrapping_add(1);
        shared.wakes_now()
    };
    for wake in &wakes {
        wake();
    }
}

/// Add `failure`, replacing the one it supersedes: a session failure
/// replaces one with its code, any other one its subject's.
fn upsert(status: &mut PeerStatus, failure: Failure) {
    status.failures.retain(|existing| {
        if failure.subject == FailureSubject::Session {
            existing.subject != FailureSubject::Session || existing.code != failure.code
        } else {
            existing.subject != failure.subject
        }
    });
    status.failures.push(failure);
    if status.failures.len() > MAX_FAILURES {
        status.failures.remove(0);
    }
}

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

impl PeerHandle {
    /// Start the session task. It connects in the background; observe it
    /// with [`Self::status`] and [`Self::on_change`].
    pub fn start(
        config: PeerConfig,
        signer: Arc<dyn DeviceSigner>,
        store: Arc<dyn PeerStore>,
    ) -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        let shared = Arc::new(Mutex::new(Shared {
            status: PeerStatus::connecting(config.cert.device_public_key),
            queues: Vec::new(),
            wakes: Vec::new(),
            listeners: Vec::new(),
            fetch_proxy: None,
            live_note: None,
        }));
        let events: Queue = Arc::new(Mutex::new(VecDeque::new()));
        lock(&shared).queues.push(Arc::downgrade(&events));
        let record = store.load().unwrap_or_default();
        let vault = store.channel_vault().unwrap_or_else(rand::random);
        let replicas = crate::replica::Replicas::new(config.cert.clone(), Arc::clone(&store));
        let channel_state = crate::custody::StoredChannels::load(store.as_ref());
        let gates = Arc::new(tilo_channels::Gates::default());
        let channels = open_channels(&replicas, &channel_state, vault, &signer, &gates);
        #[cfg(feature = "toolkit")]
        crate::surfaces::decide_with_kept_surfaces();
        let task = Task {
            replicas,
            commands: commands.downgrade(),
            config,
            signer,
            store,
            dial: Dial::for_page(&vault),
            shared: shared.clone(),
            workspaces: BTreeSet::new(),
            planes_due: BTreeSet::new(),
            kept: BTreeMap::new(),
            placing: BTreeMap::new(),
            planes: plane::Planes::default(),
            outbox: record.outbox.iter().cloned().collect(),
            resending: BTreeSet::new(),
            record,
            ciphers: Ciphers::new(),
            channels,
            governance: HostGovernance::default(),
            held_outbound: channel_state.held().into_iter().collect(),
            channel_state,
            gates,
            route_keys: BTreeMap::new(),
            new_keys: Vec::new(),
            keys_generation: 0,
            opened: BTreeMap::new(),
            refused: (0, String::new()),
            held_inbound: BTreeMap::new(),
            surface_wants: neighbour_wire::content::ContentWants::default(),
            surfaces_arrived: false,
            owed_replays: BTreeMap::new(),
            held_live: crate::held_live::HeldLive::new(),
            reassembly: Reassembly::default(),
            positions: BTreeMap::new(),
            opened_records: BTreeMap::new(),
            doc_seq: 0,
            barriers: Vec::new(),
            held_barriers: Vec::new(),
            admission_since: None,
            delivered: BTreeMap::new(),
            received: BTreeSet::new(),
            compactions: BTreeMap::new(),
            voice: crate::voice::Participations::default(),
            rooms_pending: BTreeMap::new(),
            rooms_next: 0,
            agent_runs_pending: BTreeMap::new(),
            agent_runs_next: 0,
            held_actions: Vec::new(),
            begun: BTreeSet::new(),
        };
        task.update(|status| {
            status.outgoing_pending = task.outbox.len();
            for entry in &task.outbox {
                let pending = &mut status
                    .routes
                    .entry(entry.route.clone())
                    .or_default()
                    .pending_operations;
                *pending = pending.saturating_add(1);
            }
        });
        n0_future::task::spawn(task.run(receiver));
        Self {
            commands,
            shared,
            events,
        }
    }

    /// Why this page shows no live presence, in the words it shows in its
    /// members' place, or `None` while it has a plane: no plane could be
    /// reached ([`plane::UNREACHABLE`]).
    pub fn live_note(&self) -> Option<String> {
        lock(&self.shared).live_note.clone()
    }

    fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "peer session is closed".to_string())
    }

    /// Add `entry`, which this device's person's root signed, to the
    /// revocations this session announces: a host reads them with this
    /// device's certificate, and the one entry takes effect from the moment it
    /// names, without waiting for the next session.
    pub fn revoke(&self, entry: tilo_user_identity::RevocationEntry) -> Result<(), String> {
        self.send(Command::Revoke(entry))
    }

    /// Subscribe to a workspace.
    pub fn join(&self, workspace_id: u64) -> Result<(), String> {
        self.send(Command::Join(workspace_id))
    }

    /// Join `workspace` (its UUID), a workspace of this device's person, named
    /// `name`: the host is first asked to govern it
    /// ([`tilo_relay_protocol::workspaces::WorkspacesRequest::Place`]), creating it
    /// unless it does, so the workspace has governance like every other; then it
    /// is subscribed. A device of another person than the host's asks nothing
    /// of the kind, which only the host's person may do: the workspace is
    /// subscribed at once and its own admission decides, as it does where the
    /// host places nothing (a host reached without its administration route).
    ///
    /// # Errors
    ///
    /// `workspace` is no workspace UUID, or the session is closed.
    pub fn join_placed(&self, workspace: &str, name: &str) -> Result<(), String> {
        tilo_relay_protocol::room_workspace_id(workspace).map_err(|error| error.to_string())?;
        self.send(Command::JoinPlaced(workspace.to_owned(), name.to_owned()))
    }

    /// Open `route`, an on-demand record of the joined workspace `workspace_id`
    /// (`neighbour_wire::record_sync`): one a join does not replay. Its
    /// history arrives as [`PeerEvent::Document`]s, then
    /// [`PeerEvent::RecordOpened`]; its changes keep arriving after, and
    /// the record is opened again whenever the session reconnects.
    pub fn open_record(&self, workspace_id: u64, route: DocumentRoute) -> Result<(), String> {
        self.send(Command::OpenRecord(workspace_id, route))
    }

    /// Publish a document mutation on `route`: split into signed frames,
    /// committed in order.
    ///
    /// # Errors
    ///
    /// Returns an error when the mutation cannot be encoded or the session
    /// is closed.
    pub fn publish(&self, route: DocumentRoute, mutation: &DocumentMutation) -> Result<(), String> {
        let frames = mutation
            .encode_frames()
            .map_err(|error| error.to_string())?;
        self.send(Command::Publish(route, frames))
    }

    /// Answer [`PeerEvent::CompactionRequested`] `request` with `snapshot`,
    /// an Automerge save of the route that contains every document this
    /// session delivered for it before the request. The snapshot is
    /// committed like any publication, sealed on an encrypted route, and
    /// the host then drops exactly the history it was told the snapshot
    /// holds.
    pub fn compact(&self, request: [u8; 16], snapshot: Vec<u8>) -> Result<(), String> {
        self.send(Command::Compact(request, snapshot))
    }

    /// Seal `route`'s documents with `cipher` from now on.
    pub fn install_cipher(
        &self,
        route: DocumentRoute,
        cipher: Box<dyn ContentCipher>,
    ) -> Result<(), String> {
        self.send(Command::InstallCipher(route, cipher))
    }

    /// Hold `keys` as `route`'s channel keys, exactly as if a participant
    /// had admitted this session to the channel: documents held for them
    /// open, the route's status stops awaiting keys, and edits held for them
    /// go out sealed. For an application endpoint of a device that takes part
    /// in the channel, which hands it the keys it already holds
    /// ([`channel_crypto::ChannelPeer::content_keys`]); any other session is
    /// admitted by a participant over the channel itself.
    pub fn install_channel_keys(
        &self,
        route: DocumentRoute,
        keys: channel_crypto::ChannelContentKeys,
    ) -> Result<(), String> {
        self.send(Command::InstallChannelKeys(route, keys))
    }

    /// Take part in `route`'s encrypted channel: ask its participants for
    /// access so documents on it can be opened and sealed. Routes
    /// are also opened by publishing on them or receiving their documents.
    pub fn open_route(&self, route: DocumentRoute) -> Result<(), String> {
        self.send(Command::OpenRoute(route))
    }

    /// Publish one canonical live frame, sealed with the channel keys of the
    /// record it names. Dropped while disconnected (live state is never
    /// queued), and dropped when it names no record or this device holds
    /// none of that record's keys: a live frame never leaves in the clear.
    pub fn publish_live(&self, workspace_id: u64, frame: Vec<u8>) -> Result<(), String> {
        self.send(Command::Live(workspace_id, frame))
    }

    /// Seal one canonical live frame of `workspace_id` with the channel keys
    /// of the record it names, for a carrier other than this session (the
    /// page's live plane); `done` receives the sealed frame, or `None`.
    pub fn seal_live_then(
        &self,
        workspace_id: u64,
        frame: Vec<u8>,
        done: LiveCompletion,
    ) -> Result<(), String> {
        self.send(Command::SealLive(workspace_id, frame, done))
    }

    /// Send one remote-action frame to device `to` of the workspace (the host's
    /// own device reaches its app-serving plane). A workspace this session
    /// has joined but not been admitted to yet holds the frame until it is:
    /// the host takes no action from a session it has not subscribed, and the
    /// request would be refused and never answered. Dropped while
    /// disconnected: an action that ran late is worse than one that did not
    /// run, so the caller retries with the same invocation id.
    pub fn send_remote_action(
        &self,
        workspace_id: u64,
        to: [u8; 32],
        frame: Vec<u8>,
    ) -> Result<(), String> {
        self.send(Command::RemoteAction(workspace_id, to, frame))
    }

    /// Ask workspace `workspace_id` for content its records reference: `request`
    /// is a postcard-encoded `NeighbourMsg::RecordContentWant`. The host
    /// fans it out to every member and answers itself from what it keeps;
    /// the pieces come back to this device alone as
    /// [`PeerEvent::RecordContent`]. Dropped while disconnected: the caller
    /// asks again.
    pub fn want_content(&self, workspace_id: u64, request: Vec<u8>) -> Result<(), String> {
        self.send(Command::WantContent(workspace_id, request))
    }

    /// Answer `workspace_id`'s members that ask for content its records reference from `kept`,
    /// for as long as this session runs.
    pub fn keep_content(&self, workspace_id: u64, kept: Arc<dyn Kept>) -> Result<(), String> {
        self.send(Command::KeepContent(workspace_id, kept))
    }

    /// Publish the page's microphone `frames` on `workspace_id`'s plane as
    /// this device's voice, in the order given. Dropped where this page holds
    /// no plane for the workspace: a call's voice is current, never replayed.
    pub fn publish_call_audio(
        &self,
        workspace_id: u64,
        frames: Vec<CallFrame>,
    ) -> Result<(), String> {
        self.send(Command::CallAudio(workspace_id, frames))
    }

    /// Say `frames` on `workspace_id`'s presence track, the way every other
    /// device says its presence. Dropped where this page holds no plane for
    /// the workspace: presence is ephemeral, and the next change says where
    /// its sender is.
    pub fn publish_presence(
        &self,
        workspace_id: u64,
        frames: Vec<collab_types::presence::Presence>,
    ) -> Result<(), String> {
        self.send(Command::Presence(workspace_id, frames))
    }

    /// A receipt that completes once every document published before this
    /// call is durable on the host.
    pub fn barrier(&self) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
        let (reply, receipt) = std::sync::mpsc::sync_channel(1);
        self.barrier_then(Box::new(move |result| {
            let _ = reply.send(result);
        }))?;
        Ok(receipt)
    }

    /// Call `done` once every document published before this call is
    /// durable on the host, or with the refusal that prevents it.
    pub fn barrier_then(&self, done: Completion) -> Result<(), String> {
        self.send(Command::Barrier(done))
    }

    /// Say this session is one run of the agent `label` names: from then on every request it makes
    /// is the agent's, for this device's person, and the person's agent door lists the run while
    /// the session lasts. `done` gets the host's answer.
    pub fn run_as_agent_then(&self, label: &str, done: AgentRunCompletion) -> Result<(), String> {
        let label: String = label
            .chars()
            .filter(|character| !character.is_control())
            .take(tilo_relay_protocol::transport::MAX_AGENT_LABEL_CHARS)
            .collect();
        self.send(Command::AgentRun(label, done))
    }

    /// Ask the host's workspace administration `request`; `done` gets the answer.
    pub fn rooms_then(
        &self,
        request: tilo_relay_protocol::workspaces::WorkspacesRequest,
        done: RoomsCompletion,
    ) -> Result<(), String> {
        self.send(Command::Administer(request, done))
    }

    /// Drop the connection and reconnect now, also after a failure.
    pub fn reconnect(&self) -> Result<(), String> {
        self.send(Command::Reconnect)
    }

    /// The network is back (the browser is online again, or the host just
    /// answered on another connection): a session waiting out its backoff
    /// tries again now instead of up to `MAX_BACKOFF` later. A connected
    /// session is left as it is.
    pub fn retry_now(&self) -> Result<(), String> {
        self.send(Command::Retry)
    }

    /// Ask the host for every joined workspace's shared state again: the workspaces
    /// are subscribed anew, the host replays their retained checkpoints and
    /// answers `Ready`. Offline, this reconnects now.
    pub fn refresh(&self) -> Result<(), String> {
        self.send(Command::Refresh)
    }

    /// End the session.
    pub fn close(&self) {
        let _ = self.send(Command::Close);
    }

    fn change(&self, change: impl FnOnce(&mut PeerStatus)) {
        change_status(&self.shared, change);
    }

    /// Record the page store's durability of `route`. A failed write is the
    /// storage failure; it clears once no route's store state is failed.
    pub fn set_local_state(&self, route: DocumentRoute, state: LocalState) {
        self.change(|status| {
            status.routes.entry(route).or_default().local = Some(state);
            if state == LocalState::Failed {
                upsert(
                    status,
                    Failure {
                        code: "storage-transaction-failed",
                        message: "Changes could not be saved on this device.".into(),
                        at_ms: tilo_clock::unix_now_ms(),
                        subject: FailureSubject::Storage,
                    },
                );
            } else if state == LocalState::Durable
                && !status
                    .routes
                    .values()
                    .any(|route| route.local == Some(LocalState::Failed))
            {
                status
                    .failures
                    .retain(|failure| failure.subject != FailureSubject::Storage);
            }
        });
    }

    /// Report a failure of something the page owns: its store, a desktop
    /// surface, a surface's asset. It replaces the subject's previous one.
    ///
    /// # Errors
    ///
    /// Rejects [`FailureSubject::Session`] and [`FailureSubject::Admission`],
    /// which only the session reports, and codes outside
    /// [`PAGE_FAILURE_CODES`].
    pub fn report_failure(
        &self,
        subject: FailureSubject,
        code: &str,
        message: impl Into<String>,
    ) -> Result<(), String> {
        if matches!(subject, FailureSubject::Session | FailureSubject::Admission) {
            return Err("the session reports its own failures".into());
        }
        let code = PAGE_FAILURE_CODES
            .iter()
            .copied()
            .find(|known| *known == code)
            .ok_or_else(|| format!("unknown failure code {code}"))?;
        let message = message.into();
        self.change(|status| {
            upsert(
                status,
                Failure {
                    code,
                    message,
                    at_ms: tilo_clock::unix_now_ms(),
                    subject,
                },
            );
        });
        Ok(())
    }

    pub fn clear_failure(&self, subject: &FailureSubject) {
        if !lock(&self.shared)
            .status
            .failures
            .iter()
            .any(|failure| failure.subject == *subject)
        {
            return;
        }
        self.change(|status| {
            status
                .failures
                .retain(|failure| failure.subject != *subject);
        });
    }

    /// Clear every failure of desktop surface `surface` and of its assets.
    pub fn clear_surface(&self, surface: &str) {
        let of_surface = |subject: &FailureSubject| match subject {
            FailureSubject::Surface(id) | FailureSubject::Asset { surface: id, .. } => {
                id == surface
            }
            _ => false,
        };
        if !lock(&self.shared)
            .status
            .failures
            .iter()
            .any(|failure| of_surface(&failure.subject))
        {
            return;
        }
        self.change(|status| {
            status
                .failures
                .retain(|failure| !of_surface(&failure.subject));
        });
    }

    /// The bearer token the host's fetch proxy takes (`tilo_net::proxy`, at
    /// `/peer/v1/fetch`) while this session holds one; `None` otherwise, and
    /// from a host that serves no proxy. It changes with every session, so
    /// callers read it for each request.
    pub fn fetch_proxy_session(&self) -> Option<String> {
        lock(&self.shared).fetch_proxy.clone()
    }

    /// Current status.
    pub fn status(&self) -> PeerStatus {
        lock(&self.shared).status.clone()
    }

    /// Whose each member device of `workspace` is, as [`PeerStatus::room_devices`] says: device
    /// (hex) to person (root key, hex), read without copying the rest of the status. Empty until
    /// the workspace's governance admitted its members.
    pub fn room_devices(&self, workspace: &str) -> BTreeMap<String, String> {
        lock(&self.shared)
            .status
            .room_devices
            .get(workspace)
            .cloned()
            .unwrap_or_default()
    }

    /// Take every event produced since the last call on this handle's own
    /// queue. Other consumers use [`Self::events`].
    pub fn drain(&self) -> Vec<PeerEvent> {
        PeerEvents(self.events.clone()).drain()
    }

    /// A new, independent event queue.
    pub fn events(&self) -> PeerEvents {
        let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
        lock(&self.shared).queues.push(Arc::downgrade(&queue));
        PeerEvents(queue)
    }

    /// Call `wake` whenever status or events change.
    pub fn on_change(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        lock(&self.shared).wakes.push(wake);
    }

    /// Call `wake` whenever status or events change, for as long as
    /// `events` is read: a consumer that comes and goes leaves no wake
    /// behind.
    pub fn on_event(&self, events: &PeerEvents, wake: Arc<dyn Fn() + Send + Sync>) {
        lock(&self.shared)
            .listeners
            .push((Arc::downgrade(&events.0), wake));
    }
}

enum Exit {
    Closed,
    Reconnect,
    Lost(String),
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
    shared: Arc<Mutex<Shared>>,
    workspaces: BTreeSet<u64>,
    /// The joined workspaces whose live plane may have to be joined, followed
    /// or left ([`Self::settle_planes`]): those whose governance, channel
    /// keys or plane changed since it last looked, and every one on each
    /// channel tick. A page in many workspaces pays for the ones that
    /// changed, not for all of them on every frame it reads.
    planes_due: BTreeSet<u64>,
    /// What this device keeps of each workspace's content ([`PeerHandle::keep_content`]).
    kept: BTreeMap<u64, Arc<dyn Kept>>,
    /// Workspaces of this device's person joined once the host governs them:
    /// each one's UUID and name, until the host answered its placement.
    placing: BTreeMap<u64, (String, String)>,
    /// This page's own live plane per joined workspace
    /// ([`crate::session::plane`]).
    planes: plane::Planes,
    outbox: VecDeque<OutboxEntry>,
    /// The outbox entries this session still owes its host on this
    /// connection: a page that recorded while it was away owes more than the
    /// transport takes at once, so they go as the host's answers make room.
    resending: BTreeSet<[u8; 16]>,
    record: PeerRecord,
    ciphers: Ciphers,
    /// This device's side of the workspaces' encrypted channels: the engine every participant runs,
    /// here as a reader that asks for access and follows the participants' commits.
    channels: tilo_channels::ChannelEncryption,
    /// What the channels say to and hear from the host.
    governance: HostGovernance,
    /// The channels' state and the writes held for their keys, in the store.
    channel_state: crate::custody::StoredChannels,
    /// The gate of every channel this device knows has a group.
    gates: Arc<tilo_channels::Gates>,
    /// Every route this session opened, and the keys it holds for it.
    route_keys: BTreeMap<DocumentRoute, RouteKeys>,
    /// Keys installed since the session last acted on them.
    new_keys: Vec<(DocumentRoute, channel_crypto::ChannelContentKeys)>,
    /// The channels' generation the keys were last read at.
    keys_generation: u64,
    /// Documents opened per route, and those refused with the last reason,
    /// for the diagnostics.
    opened: BTreeMap<(DocumentRoute, [u8; 4]), u64>,
    refused: (u64, String),
    /// Sealed documents that arrived before this device held their keys,
    /// with their authenticated authors.
    held_inbound: BTreeMap<DocumentRoute, InboundHold<HeldFrame>>,
    /// App surfaces this page asked its workspaces for, by the route whose
    /// writes named them: they arrive as pieces of record content.
    surface_wants: neighbour_wire::content::ContentWants<()>,
    /// Whether an app surface arrived since the session last decided what it
    /// held for one.
    surfaces_arrived: bool,
    /// Records the host is replaying for documents a full hold let go of.
    owed_replays: BTreeMap<[u8; 32], DocumentRoute>,
    /// Live frames waiting for their record's keys.
    held_live: crate::held_live::HeldLive,
    /// Frames published on an encrypted route before this device held its
    /// keys; they are sealed and committed once the keys arrive.
    held_outbound: BTreeMap<DocumentRoute, Vec<Vec<u8>>>,
    reassembly: Reassembly,
    /// The host's last position per workspace, presented when resubscribing so
    /// the host replays only what came after it. Kept for the session task's
    /// lifetime only: it describes what this page has received.
    positions: BTreeMap<u64, Vec<u8>>,
    /// The on-demand records this page opened, by retained channel: opened
    /// again after every resubscription.
    opened_records: BTreeMap<[u8; 32], (u64, DocumentRoute)>,
    doc_seq: u64,
    barriers: Vec<([u8; 16], Completion)>,
    /// Barriers called while frames were held, with the routes those frames
    /// are held on: each waits for them to enter the outbox, then for the
    /// outbox as it stands then.
    held_barriers: Vec<(BTreeSet<DocumentRoute>, Completion)>,
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
    /// Open compaction requests: workspace, route and the entries delivered when
    /// the request arrived.
    compactions: BTreeMap<[u8; 16], Compaction>,
    /// This device's participation in the workspaces' calls.
    voice: crate::voice::Participations,
    /// The workspace administration this session asked its host and has not
    /// been answered yet, by the id it asked with.
    rooms_pending: BTreeMap<u64, RoomsCompletion>,
    /// The id the next administration request is asked with.
    rooms_next: u64,
    /// The agent runs this session declared and the host has not answered yet, by the
    /// id they asked with.
    agent_runs_pending: BTreeMap<u64, AgentRunCompletion>,
    /// The id the next agent run is declared with.
    agent_runs_next: u64,
    /// Action frames owed to workspaces the host has not admitted this session
    /// to yet, oldest first, with the workspace each is for: sent before the
    /// admission, they are refused by the host and the request they carry is
    /// never answered.
    held_actions: Vec<HeldAction>,
    /// The workspaces whose replay the host began on this connection: their
    /// bootstrap arrived, or their `Ready` for a host that sends none. The
    /// host subscribed this connection to them, so it takes what the page
    /// says there — its certificate ([`Self::introduce`]) and its channels'
    /// governance ([`Self::send_governance`]) — while the replay runs.
    begun: BTreeSet<u64>,
}

/// An open compaction request: workspace, route, and the entries it covers.
type Compaction = (u64, DocumentRoute, Vec<[u8; 32]>);

/// One action frame held until the host admits this session to its workspace.
struct HeldAction {
    workspace_id: u64,
    to: [u8; 32],
    frame: Vec<u8>,
}

/// Entry ids a route remembers for compaction. Past it, later entries are
/// left out of statements and simply stay retained.
const MAX_DELIVERED_ENTRIES: usize = 1 << 17;
/// Open compaction requests kept; the oldest goes first.
const MAX_COMPACTIONS: usize = 16;

/// Action frames held for a workspace's admission. Past it, the oldest is
/// given up: an action that waited for a session's admission longer than this
/// many actions is better refused than delivered late.
const MAX_HELD_ACTIONS: usize = 64;

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
    fn begin(&mut self, client: &RelayClient, envelope: &Envelope) -> bool {
        match envelope {
            Envelope::WorkspaceBootstrap { workspace_id, .. } => {
                self.introduce(client, *workspace_id)
            }
            Envelope::Ready { workspace_id, .. } => self.announce(client, *workspace_id),
            _ => true,
        }
    }

    /// Tell the host this device's certificate as soon as it subscribed this
    /// connection to `workspace_id`, which its bootstrap ahead of the replay
    /// says: the workspace's channel participants take a device's access
    /// request only once they hold its certificate, so a page told only at
    /// `Ready` got its keys a whole replay late. Returns `false` when the
    /// session could not take it.
    fn introduce(&mut self, client: &RelayClient, workspace_id: u64) -> bool {
        if !self.begun.insert(workspace_id) {
            return true;
        }
        identity_hello(workspace_id, &self.config.cert, &self.config.revocations)
            .is_none_or(|hello| client.send(hello).is_ok())
    }

    /// Tell the host, once `workspace_id` admitted this device, its
    /// certificate (unless the replay's start told it already) and that it
    /// answers compaction requests. Returns `false` when the session could
    /// not take them.
    fn announce(&mut self, client: &RelayClient, workspace_id: u64) -> bool {
        if !self.introduce(client, workspace_id) {
            return false;
        }
        client
            .send(Envelope::CompactVolunteer { workspace_id })
            .is_ok()
    }

    /// Take `entry` into what this session announces and send its identity
    /// again to every workspace the host admitted it to. An entry that does
    /// not verify, or that names another person's root, changes nothing: a
    /// host refuses a hello whose revocations are not its sender's own.
    fn revoke(
        &mut self,
        entry: tilo_user_identity::RevocationEntry,
        client: Option<&RelayClient>,
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
        let admitted: Vec<u64> = lock(&self.shared)
            .status
            .workspaces
            .iter()
            .filter(|(_, ready)| **ready)
            .map(|(workspace_id, _)| *workspace_id)
            .collect();
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

    fn compaction_requested(&mut self, workspace_id: u64, request: [u8; 16], route: DocumentRoute) {
        if !self.workspaces.contains(&workspace_id)
            || route.workspace_id().ok() != Some(workspace_id)
        {
            return;
        }
        let covered = self
            .delivered
            .get(&route)
            .map(|known| known.iter().copied().collect())
            .unwrap_or_default();
        if self.compactions.len() >= MAX_COMPACTIONS
            && let Some(oldest) = self.compactions.keys().next().copied()
        {
            self.compactions.remove(&oldest);
        }
        self.compactions
            .insert(request, (workspace_id, route.clone(), covered));
        self.emit(&PeerEvent::CompactionRequested { route, request });
    }

    /// Commit `snapshot` for request `request`, then send the signed
    /// statement of what it covers. Returns `false` when the session could
    /// not take an envelope.
    async fn compact(
        &mut self,
        request: [u8; 16],
        snapshot: Vec<u8>,
        client: Option<&RelayClient>,
    ) -> bool {
        let Some(client) = client else {
            return true;
        };
        let Some((workspace_id, route, covered)) = self.compactions.remove(&request) else {
            return true;
        };
        let Ok(frames) = DocumentMutation::Snapshot(snapshot.clone()).encode_frames() else {
            return true;
        };
        let before = self.outbox.len();
        if !self.publish(route.clone(), frames, Some(client)).await {
            return false;
        }
        let snapshot_entries: Vec<[u8; 32]> = self
            .outbox
            .iter()
            .skip(before)
            .map(|entry| tilo_relay_protocol::compaction::entry_id(&entry.body))
            .collect();
        if snapshot_entries.is_empty() {
            // Held for the route's keys: nothing was committed to cover with.
            return true;
        }
        let statement = tilo_relay_protocol::compaction::CoverageStatement {
            workspace_id,
            channel: *blake3::hash(format!("app\0{}\0{}", route.app_id, route.record).as_bytes())
                .as_bytes(),
            request,
            snapshot: snapshot_entries,
            covered,
            heads: tilo_relay_protocol::compaction::snapshot_heads(&snapshot).unwrap_or_default(),
        };
        let Ok(statement) = statement.encode() else {
            return true;
        };
        let Ok(signature) = self
            .signer
            .sign(tilo_relay_protocol::compaction::signing_bytes(&statement))
            .await
        else {
            return true;
        };
        client
            .send(Envelope::Compacted {
                workspace_id,
                statement,
                signature,
            })
            .is_ok()
    }

    fn update(&self, change: impl FnOnce(&mut PeerStatus)) {
        change_status(&self.shared, change);
    }

    fn emit(&self, event: &PeerEvent) {
        let wakes = {
            let mut shared = lock(&self.shared);
            shared.queues.retain(|queue| {
                let Some(queue) = queue.upgrade() else {
                    return false;
                };
                queue.lock_recover().push_back(event.clone());
                true
            });
            shared.wakes_now()
        };
        for wake in &wakes {
            wake();
        }
    }

    fn fail(&self, code: &'static str, message: impl Into<String>) {
        self.fail_on(FailureSubject::Session, code, message);
    }

    fn fail_on(&self, subject: FailureSubject, code: &'static str, message: impl Into<String>) {
        let message = message.into();
        self.update(|status| {
            upsert(
                status,
                Failure {
                    code,
                    message,
                    at_ms: tilo_clock::unix_now_ms(),
                    subject,
                },
            );
        });
    }

    fn persist(&mut self) {
        self.record.outbox = self.outbox.iter().cloned().collect();
        if let Err(error) = self.store.save(&self.record) {
            self.fail_on(FailureSubject::Storage, "storage-transaction-failed", error);
        }
    }

    /// Subscribe to `workspace_id`, continuing from the position this page
    /// last received: the host replays only what came after it, and a page
    /// that holds no position (its first session, or a refresh) gets the
    /// workspace whole.
    fn subscribe(&self, workspace_id: u64) -> Envelope {
        Envelope::SubscribeFrom {
            workspace_id,
            position: self.positions.get(&workspace_id).cloned(),
        }
    }

    /// Subscribe every joined workspace again, from now on waiting for its
    /// `Ready`. Returns `false` when the session could not take them.
    fn resubscribe(&mut self, client: &RelayClient) -> bool {
        self.begun.clear();
        let mut sent_ok = true;
        for workspace_id in &self.workspaces {
            sent_ok &= client
                .send(Envelope::Subscribe {
                    workspace_id: *workspace_id,
                })
                .is_ok();
        }
        for (channel, (workspace_id, _)) in &self.opened_records {
            sent_ok &= client
                .send(Envelope::OpenRecord {
                    workspace_id: *workspace_id,
                    channel: *channel,
                })
                .is_ok();
        }
        self.update(|status| {
            for ready in status.workspaces.values_mut() {
                *ready = false;
            }
        });
        sent_ok
    }

    /// Report `admission-timeout` once the workspaces subscribed at
    /// `admission_since` are still not admitted, and clear it once they are.
    /// A timeout is reported once per subscription.
    fn check_admission(&mut self) {
        let status = self.status();
        match admission_state(&status, self.admission_since, tilo_clock::unix_now_ms()) {
            Admission::Admitted => {
                self.admission_since = None;
                if status
                    .failures
                    .iter()
                    .any(|failure| failure.subject == FailureSubject::Admission)
                {
                    self.clear_admission_failure();
                }
            }
            Admission::Waiting => {}
            Admission::Overdue => {
                self.admission_since = None;
                self.fail_on(
                    FailureSubject::Admission,
                    "admission-timeout",
                    "Shared state is still catching up. Refresh it to ask the host again.",
                );
            }
        }
    }

    fn clear_admission_failure(&self) {
        self.update(|status| {
            status
                .failures
                .retain(|failure| failure.subject != FailureSubject::Admission);
        });
    }

    fn status(&self) -> PeerStatus {
        lock(&self.shared).status.clone()
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
                .host_cert
                .as_ref()
                .is_some_and(|saved| !same_identity(saved, &self.config.host_cert))
        {
            return Err("The saved peer identity or host does not match this session.");
        }
        // A read-only invited page may never publish. Keep its certificate
        // and host pin before the first connection, as well as its outbox.
        self.record.cert = Some(self.config.cert.clone());
        self.record.host_cert = Some(self.config.host_cert.clone());
        self.persist();
        Ok(())
    }

    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        if let Err(message) = self.bind_record_identity() {
            self.fail("certificate-rejected", message);
            self.update(|status| status.phase = TransportPhase::Failed);
            return;
        }
        let mut attempt: u32 = 0;
        let mut refusals: u32 = 0;
        loop {
            if self.config.cert.is_expired_at(tilo_clock::unix_now_ns()) {
                self.fail("certificate-expired", "This browser's certificate expired.");
                self.update(|status| status.phase = TransportPhase::Failed);
                if !self.wait_for_reconnect(&mut commands).await {
                    return self.closed().await;
                }
                continue;
            }
            self.update(|status| {
                status.phase = if attempt == 0 {
                    TransportPhase::Connecting
                } else {
                    TransportPhase::Reconnecting
                };
                status.next_attempt_at_ms = None;
            });
            match self.connect().await {
                Ok(client) => {
                    attempt = 0;
                    refusals = 0;
                    // Waiting for admission is timed from when this page
                    // subscribes, not from the moment the host answers, so a
                    // host that holds the connection open still reports.
                    self.admission_since = Some(tilo_clock::unix_now_ms());
                    let exit = self.serve(client, &mut commands).await;
                    // A request the host had not answered is answered here:
                    // nothing waits on a connection that is gone.
                    self.requests_lost();
                    match exit {
                        Exit::Closed => return self.closed().await,
                        Exit::Reconnect => continue,
                        Exit::Lost(reason) => self.fail("transport-ended", reason),
                    }
                }
                Err(error) => {
                    let stop = match refusal(&error) {
                        Refusal::Final(code, message) => Some((code, message)),
                        Refusal::Unexplained => {
                            refusals = refusals.saturating_add(1);
                            (refusals >= REFUSALS_BEFORE_FAILING).then_some(NOT_ADMITTED)
                        }
                        Refusal::Reachability => {
                            refusals = 0;
                            None
                        }
                    };
                    if let Some((code, message)) = stop {
                        self.fail(code, message);
                        self.update(|status| {
                            status.phase = TransportPhase::Failed;
                            status.authenticated = false;
                        });
                        if !self.wait_for_reconnect(&mut commands).await {
                            return self.closed().await;
                        }
                        attempt = 0;
                        refusals = 0;
                        continue;
                    }
                    self.fail("transport-unavailable", error.to_string());
                }
            }
            attempt = attempt.saturating_add(1);
            let delay = MAX_BACKOFF.min(Duration::from_millis(
                250u64.saturating_mul(1u64 << attempt.min(7)),
            ));
            let delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
            self.update(|status| {
                status.phase = TransportPhase::Offline;
                status.authenticated = false;
                status.attempt = attempt;
                status.next_attempt_at_ms =
                    Some(tilo_clock::unix_now_ms().saturating_add(delay_ms));
            });
            let sleep = n0_future::time::sleep(delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    biased;
                    command = commands.recv() => match command {
                        None | Some(Command::Close) => return self.closed().await,
                        Some(Command::Reconnect | Command::Retry) => break,
                        Some(Command::Refresh) => {
                            self.positions.clear();
                            self.received.clear();
                            break;
                        }
                        Some(command) => {
                            self.apply(command, None).await;
                        }
                    },
                    () = &mut sleep => break,
                }
            }
        }
    }

    async fn closed(mut self) {
        // Nothing reconnects from here: the endpoint goes with the session,
        // and the host is told so.
        self.dial.shutdown().await;
        lock(&self.shared).fetch_proxy = None;
        self.update(|status| {
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
                None | Some(Command::Close) => return false,
                Some(Command::Reconnect) => return true,
                Some(Command::Refresh) => {
                    self.positions.clear();
                    self.received.clear();
                    return true;
                }
                Some(command) => {
                    self.apply(command, None).await;
                }
            }
        }
    }

    /// The session is up: connected and authenticated, its transport
    /// failures cleared, every workspace waiting to be admitted again.
    fn connected(&self) {
        self.update(|status| {
            status.phase = TransportPhase::Connected;
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

    /// Ask the host for every joined workspace whole again, as a refresh
    /// does: no position, no admission failure kept. `false` when the
    /// session took no more.
    fn refresh(&mut self, client: &RelayClient) -> bool {
        self.clear_admission_failure();
        self.positions.clear();
        self.received.clear();
        self.resubscribe(client)
    }

    async fn serve(
        &mut self,
        client: RelayClient,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> Exit {
        // The last session's fetch proxy token died with its connection; this
        // one mints its own as it starts.
        lock(&self.shared).fetch_proxy = None;
        if !self.resume_session(&client) {
            return Exit::Lost("host session saturated during resume".into());
        }
        self.connected();
        // Each joined workspace's plane is joined again on this connection.
        self.planes_due.extend(self.workspaces.iter().copied());
        let mut ping = n0_future::time::interval(PING_INTERVAL);
        let mut channel_tick = n0_future::time::interval(CHANNEL_TICK);
        let mut heartbeat = n0_future::time::interval(tilo_relay_protocol::voice::HEARTBEAT);
        // A page that stood still beats once when it runs again, not once
        // for every beat it missed.
        heartbeat.set_missed_tick_behavior(n0_future::time::MissedTickBehavior::Delay);
        loop {
            if !self.settle_channels(Some(&client)).await {
                return Exit::Lost("host session saturated".into());
            }
            self.settle_planes();
            // App surfaces this page holds documents for go out with what the
            // session owes, and a surface that arrived decides what waited
            // for it before anything else is read.
            self.ask_surfaces(&client);
            if std::mem::take(&mut self.surfaces_arrived) {
                self.decide_held_surfaces(&client);
            }
            // What the outbox still owes the host goes as its queue takes it:
            // every answer from the host, every command, every tick is a
            // chance to hand it more.
            self.flush_outbox(&client);
            tokio::select! {
                biased;
                command = commands.recv() => match command {
                    None | Some(Command::Close) => {
                        client.disconnect().await;
                        return Exit::Closed;
                    }
                    Some(Command::Reconnect) => {
                        // The next session dials from this same endpoint.
                        client.disconnect().await;
                        return Exit::Reconnect;
                    }
                    Some(Command::Refresh) => {
                        if !self.refresh(&client) {
                            return Exit::Lost("host session saturated".into());
                        }
                    }
                    Some(command) => {
                        if !self.apply(command, Some(&client)).await {
                            return Exit::Lost("host session saturated".into());
                        }
                    }
                },
                envelope = client.recv() => match envelope {
                    // The person stopped the agent this session is a run of: it ends here, and
                    // connects no more, so nothing of the agent is said again.
                    Some(Envelope::AgentRunStopped) => {
                        self.fail("agent-run-stopped", "The person stopped this agent in Tilo.");
                        client.disconnect().await;
                        return Exit::Closed;
                    }
                    Some(envelope) => {
                        if let Some((event, reply)) = self.voice.inbound(&envelope) {
                            if let Some(reply) = reply
                                && client.send(reply).is_err()
                            {
                                return Exit::Lost("host session saturated".into());
                            }
                            if let Some(event) = event {
                                self.emit(&PeerEvent::Voice(event));
                            }
                            continue;
                        }
                        if self.content_wanted(&envelope, &client) {
                            continue;
                        }
                        let admitted = match &envelope {
                            Envelope::Ready { workspace_id, .. } => Some(*workspace_id),
                            _ => None,
                        };
                        if !self.begin(&client, &envelope) {
                            return Exit::Lost("host session saturated".into());
                        }
                        self.inbound(envelope);
                        if let Some(workspace_id) = admitted {
                            self.admitted(workspace_id, &client);
                        }
                    }
                    None => return Exit::Lost("the host closed the session".into()),
                },
                _ = ping.tick() => {
                    if client.send(Envelope::Ping).is_err() {
                        return Exit::Lost("host session saturated".into());
                    }
                    self.check_admission();
                }
                _ = channel_tick.tick() => {
                    self.pump_channels(false);
                    // A plane that ended or is due to be asked again.
                    self.planes_due.extend(self.workspaces.iter().copied());
                }
                _ = heartbeat.tick() => {
                    for beat in self.voice.heartbeats() {
                        if client.send(beat).is_err() {
                            return Exit::Lost("host session saturated".into());
                        }
                    }
                }
            }
        }
    }

    /// Send a remote-action frame to device `to`, or say it cannot go.
    ///
    /// A frame for a workspace this session asked to join and the host has
    /// not admitted it to yet waits for the admission
    /// ([`Self::admitted`]): sent now, the host refuses it as a subscription
    /// it does not hold, and the request it carries is never answered. One
    /// for a workspace this session did not join goes as it is — the host's
    /// refusal is the answer, and the caller need not wait for it.
    fn remote_action(
        &mut self,
        workspace_id: u64,
        to: [u8; 32],
        frame: Vec<u8>,
        client: Option<&RelayClient>,
    ) {
        let Some(client) = client else {
            self.emit(&PeerEvent::Remote(Heard::Undeliverable {
                workspace_id,
                to,
                reason: "not connected to the host".into(),
            }));
            return;
        };
        if self.admitted_to(workspace_id) == Some(false) {
            if self.held_actions.len() >= MAX_HELD_ACTIONS {
                let dropped = self.held_actions.remove(0);
                self.emit(&PeerEvent::Remote(Heard::Undeliverable {
                    workspace_id: dropped.workspace_id,
                    to: dropped.to,
                    reason: "too many actions waited for the workspace's admission".into(),
                }));
            }
            self.held_actions.push(HeldAction {
                workspace_id,
                to,
                frame,
            });
            return;
        }
        self.send_action(client, workspace_id, to, frame);
    }

    /// Whether the host admitted this session to `workspace_id`: `None` for a
    /// workspace it did not ask to join, `Some(false)` while the host has not
    /// admitted it yet, and `Some(true)` once it has.
    fn admitted_to(&self, workspace_id: u64) -> Option<bool> {
        lock(&self.shared)
            .status
            .workspaces
            .get(&workspace_id)
            .copied()
    }

    fn send_action(&self, client: &RelayClient, workspace_id: u64, to: [u8; 32], frame: Vec<u8>) {
        let _ = client.send(Envelope::RemoteAction {
            workspace_id,
            version: tilo_relay_protocol::transport::REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: to,
            from_device_public_key: self.config.cert.device_public_key,
            from_user_public_key: self.config.cert.user_public_key,
            frame,
        });
    }

    /// Send the action frames that waited for `workspace_id`'s admission, in
    /// the order they were made. What this session owes another workspace
    /// waits for that one.
    fn flush_held_actions(&mut self, workspace_id: u64, client: &RelayClient) {
        let (due, waiting): (Vec<HeldAction>, Vec<HeldAction>) =
            std::mem::take(&mut self.held_actions)
                .into_iter()
                .partition(|held| held.workspace_id == workspace_id);
        self.held_actions = waiting;
        for held in due {
            self.send_action(client, held.workspace_id, held.to, held.frame);
        }
    }

    /// Ask the workspace for content ([`PeerHandle::want_content`]): the host
    /// fans the request out and answers with what it keeps. Nothing while
    /// disconnected.
    fn want_content(&self, workspace_id: u64, request: Vec<u8>, client: Option<&RelayClient>) {
        let Some(client) = client else {
            return;
        };
        let _ = client.send(Envelope::RecordContentWant {
            workspace_id,
            from_device_public_key: self.config.cert.device_public_key,
            frame: request,
        });
    }

    /// Say `frames` as object `sequence` of `workspace_id`'s presence track,
    /// sealed under the workspace scope's keys by the plane itself.
    fn publish_presence(
        &mut self,
        workspace_id: u64,
        frames: Vec<collab_types::presence::Presence>,
    ) {
        for frame in frames {
            let Some(joined) = self.planes.joined_mut(workspace_id) else {
                continue;
            };
            let sequence = joined.presence_seq.wrapping_add(1);
            joined.presence_seq = sequence;
            let _ = tilo_live::presence::say(&joined.plane, sequence, &frame);
        }
    }

    /// Apply one command; `client` is the live session, if any. Returns
    /// `false` when the session could not take a durable envelope.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per command, each handing off to its own method"
    )]
    async fn apply(&mut self, command: Command, client: Option<&RelayClient>) -> bool {
        match command {
            Command::Join(workspace_id) => self.join(workspace_id, client),
            Command::JoinPlaced(workspace, name) => self.join_placed(workspace, name, client),
            Command::Placed(workspace_id) => {
                if self.placing.remove(&workspace_id).is_some() {
                    self.join(workspace_id, client)
                } else {
                    true
                }
            }
            Command::OpenRecord(workspace_id, route) => {
                self.open_record(workspace_id, route, client)
            }
            #[cfg(not(target_arch = "wasm32"))]
            Command::PlaneJoined(workspace_id, joined) => {
                self.plane_joined(workspace_id, joined);
                true
            }
            #[cfg(target_arch = "wasm32")]
            Command::PlaneJoined(workspace_id) => {
                self.plane_joined(workspace_id, plane::arrived(workspace_id));
                true
            }
            Command::PlaneHeard(workspace_id) => {
                self.plane_heard(workspace_id);
                true
            }
            Command::PlaneEnded(workspace_id) => {
                self.plane_ended(workspace_id);
                true
            }
            command @ (Command::Live(..) | Command::SealLive(..)) => {
                self.apply_live(command);
                true
            }
            Command::RemoteAction(workspace_id, to, frame) => {
                self.remote_action(workspace_id, to, frame, client);
                true
            }
            Command::WantContent(workspace_id, request) => {
                self.want_content(workspace_id, request, client);
                true
            }
            Command::KeepContent(workspace_id, kept) => {
                self.kept.insert(workspace_id, kept);
                true
            }
            Command::InstallCipher(route, cipher) => {
                self.ciphers.insert(route.clone(), cipher);
                self.release_live(&route);
                true
            }
            Command::InstallChannelKeys(route, keys) => {
                // Settled with the next channel changes, the same way as
                // keys the channel's own governance hands this session. The
                // channel's records are sealed with them, and so is a call on
                // one of them while this device holds no channel state of its
                // own.
                self.route_keys
                    .insert(route.clone(), RouteKeys::Handed(keys.clone()));
                self.new_keys.push((route, keys));
                true
            }
            Command::OpenRoute(route) => {
                self.open_channel(&route);
                true
            }
            Command::Publish(route, frames) => self.publish(route, frames, client).await,
            Command::Compact(request, snapshot) => self.compact(request, snapshot, client).await,
            Command::Barrier(done) => {
                if self.held_outbound.is_empty() {
                    self.await_outbox(done);
                } else {
                    let held = self.held_outbound.keys().cloned().collect();
                    self.held_barriers.push((held, done));
                }
                true
            }
            Command::Presence(workspace_id, frames) => {
                self.publish_presence(workspace_id, frames);
                true
            }
            Command::CallAudio(workspace_id, frames) => {
                self.publish_call_audio(workspace_id, frames);
                true
            }
            Command::AgentRun(label, done) => self.run_as_agent(label, done, client),
            Command::VoiceCalls(workspace_id) => self.voice_calls(workspace_id, client),
            Command::VoiceCall(workspace, request, op) => {
                self.voice_call(workspace, request, op, client)
            }
            Command::VoiceJoin(workspace_id, call) => self.voice_join(workspace_id, call, client),
            Command::VoiceLeave(workspace_id) => self.voice_leave(workspace_id, client),
            Command::VoiceShow(ws, showing) => self.voice_show(ws, showing, client),
            Command::CallPictures(workspace_id, pictures) => {
                self.publish_call_pictures(workspace_id, pictures);
                true
            }
            Command::CallShowing(workspace_id, shown) => {
                self.show_call_pictures(workspace_id, &shown);
                true
            }
            Command::ShowPages(workspace_id, shown) => {
                self.show_pages(workspace_id, &shown);
                true
            }
            Command::CallPictureRestart(workspace_id, device, source) => {
                self.restart_call_picture(workspace_id, device, source);
                true
            }
            Command::Administer(request, done) => {
                self.administer(&request, done, client);
                true
            }
            Command::Revoke(entry) => self.revoke(entry, client),
            // Handled by the caller's loop.
            Command::Reconnect | Command::Retry | Command::Refresh | Command::Close => true,
        }
    }

    /// Subscribe to `workspace_id`, now or once connected.
    fn join(&mut self, workspace_id: u64, client: Option<&RelayClient>) -> bool {
        self.workspaces.insert(workspace_id);
        self.planes_due.insert(workspace_id);
        self.update(|status| {
            status.workspaces.entry(workspace_id).or_insert(false);
        });
        self.received.clear();
        self.begun.remove(&workspace_id);
        client.is_none_or(|client| client.send(self.subscribe(workspace_id)).is_ok())
    }

    /// Join `workspace` once the host governs it ([`PeerHandle::join_placed`]).
    fn join_placed(
        &mut self,
        workspace: String,
        name: String,
        client: Option<&RelayClient>,
    ) -> bool {
        let Ok(workspace_id) = tilo_relay_protocol::room_workspace_id(&workspace) else {
            return true;
        };
        if self.workspaces.contains(&workspace_id) {
            return true;
        }
        if !places_workspaces(&self.config.cert, &self.config.host_cert) {
            return self.join(workspace_id, client);
        }
        self.update(|status| {
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
    fn place(&mut self, workspace_id: u64, client: &RelayClient) -> bool {
        let Some((workspace, name)) = self.placing.get(&workspace_id).cloned() else {
            return true;
        };
        let commands = self.commands.clone();
        // Refused (not this person's host, a workspace governed elsewhere) or
        // unanswered: the workspace's admission decides.
        let placed = move |_: Result<tilo_relay_protocol::workspaces::WorkspacesReply, String>| {
            if let Some(commands) = commands.upgrade() {
                let _ = commands.send(Command::Placed(workspace_id));
            }
        };
        self.administer(
            &tilo_relay_protocol::workspaces::WorkspacesRequest::Place { workspace, name },
            Box::new(placed),
            Some(client),
        );
        true
    }

    /// Restore this session's workspaces, pending commits and call participations.
    fn resume_session(&mut self, client: &RelayClient) -> bool {
        let mut sent_ok = true;
        // A plane's grant is its session's token: the planes of the last
        // session ended with it, and every workspace's is joined again with
        // this one ([`Self::settle_planes`]).
        self.planes.end_session();
        // Every subscription starts whole: the host replays the workspace's
        // retained state, and duplicate detection begins with it.
        self.received.clear();
        self.begun.clear();
        for workspace_id in self.workspaces.clone() {
            sent_ok &= client.send(self.subscribe(workspace_id)).is_ok();
        }
        let placing = self.placing.keys().copied().collect::<Vec<_>>();
        for workspace_id in placing {
            sent_ok &= self.place(workspace_id, client);
        }
        for (channel, (workspace_id, _)) in &self.opened_records {
            sent_ok &= client
                .send(Envelope::OpenRecord {
                    workspace_id: *workspace_id,
                    channel: *channel,
                })
                .is_ok();
        }
        // What this page recorded while it was away is owed again, oldest
        // first: as much as the transport takes now, the rest as the host's
        // answers make room ([`Self::flush_outbox`]).
        self.resending = self.outbox.iter().map(|entry| entry.op).collect();
        self.flush_outbox(client);
        for join in self.voice.resume() {
            sent_ok &= client.send(join).is_ok();
        }
        sent_ok
    }

    /// Hand the host as much of the outbox as its queue takes, oldest first;
    /// what does not fit stays owed and is sent on the next chance the host's
    /// answers bring. A page that recorded a lot while it was away owes more
    /// than the transport holds at once, which is no reason to give up the
    /// session.
    fn flush_outbox(&mut self, client: &RelayClient) {
        let owed: Vec<[u8; 16]> = self
            .outbox
            .iter()
            .map(|entry| entry.op)
            .filter(|op| self.resending.contains(op))
            .collect();
        for op in owed {
            let Some(entry) = self.outbox.iter().find(|entry| entry.op == op) else {
                self.resending.remove(&op);
                continue;
            };
            if client.send(commit(entry, &self.config.cert)).is_err() {
                // The transport takes no more right now.
                return;
            }
            self.resending.remove(&op);
        }
    }

    /// Always `true`: the session takes it whatever the answer.
    fn run_as_agent(
        &mut self,
        label: String,
        done: AgentRunCompletion,
        client: Option<&RelayClient>,
    ) -> bool {
        let Some(client) = client else {
            done(Err("not connected to the host".into()));
            return true;
        };
        let request_id = self.agent_runs_next;
        self.agent_runs_next = self.agent_runs_next.wrapping_add(1);
        self.agent_runs_pending.insert(request_id, done);
        if client
            .send(Envelope::AgentRun { request_id, label })
            .is_err()
            && let Some(done) = self.agent_runs_pending.remove(&request_id)
        {
            done(Err("the host's session closed".into()));
        }
        true
    }

    /// The host answered an agent run this session declared.
    fn agent_run_answered(&mut self, answer: Envelope) {
        let Envelope::AgentRunAnswer {
            request_id,
            refusal,
        } = answer
        else {
            return;
        };
        if let Some(done) = self.agent_runs_pending.remove(&request_id) {
            done(refusal.map_or(Ok(()), Err));
        }
    }

    /// Ask the host `request` of its workspace administration: over the
    /// session it answers, under an id this session picks so the answers of a
    /// session asking several may come back in any order.
    fn administer(
        &mut self,
        request: &tilo_relay_protocol::workspaces::WorkspacesRequest,
        done: RoomsCompletion,
        client: Option<&RelayClient>,
    ) {
        let Some(client) = client else {
            return done(Err("not connected to the host".into()));
        };
        let request_id = self.rooms_next;
        self.rooms_next = self.rooms_next.wrapping_add(1);
        let frame = match postcard::to_stdvec(request) {
            Ok(frame) => frame,
            Err(error) => {
                return done(Err(format!("the request does not encode: {error}")));
            }
        };
        self.rooms_pending.insert(request_id, done);
        if client
            .send(Envelope::WorkspacesRequest { request_id, frame })
            .is_err()
            && let Some(done) = self.rooms_pending.remove(&request_id)
        {
            done(Err("the host's session closed".into()));
        }
    }

    /// The host answered administration request `request_id`.
    fn rooms_reply(&mut self, request_id: u64, frame: &[u8]) {
        if let Some(done) = self.rooms_pending.remove(&request_id) {
            done(answered(frame));
        }
    }

    /// The session ended with requests the host never answered: each is
    /// answered here, so nothing waits on a connection that is gone.
    fn requests_lost(&mut self) {
        for (_, done) in std::mem::take(&mut self.rooms_pending) {
            done(Err("the session ended before the host answered".into()));
        }
        for (_, done) in std::mem::take(&mut self.agent_runs_pending) {
            done(Err("the session ended before the host answered".into()));
        }
    }

    /// Send, seal or open one live frame. Nothing leaves unsealed: a frame
    /// whose record's keys are not here yet waits for them (bounded), and a
    /// frame that names no record is dropped.
    fn apply_live(&mut self, command: Command) {
        use crate::held_live::Waiting;
        let waiting = match command {
            Command::Live(workspace_id, frame) => Waiting::Send {
                workspace_id,
                frame,
            },
            Command::SealLive(workspace_id, frame, done) => Waiting::Seal {
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
                    // Preview frames ride the workspace's plane, the way the
                    // document's own frames ride the session: only a member
                    // of its roster opens the scope's layer, and the frame
                    // inside it is sealed with the record's keys.
                    if let Some(joined) = self.planes.joined_mut(workspace_id) {
                        let sequence = joined.activity_seq.wrapping_add(1);
                        joined.activity_seq = sequence;
                        let _ = joined.plane.publish(
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
                Sealing::Sealed(sealed) => done(Some(sealed)),
                Sealing::NoKeys(record) if hold => self.held_live.hold_outbound(
                    workspace_id,
                    record,
                    Waiting::Seal {
                        workspace_id,
                        frame,
                        done,
                    },
                ),
                Sealing::NoKeys(_) | Sealing::Dropped => done(None),
            },
            Waiting::Receive {
                workspace_id,
                from,
                frame,
            } => match self.open_live(workspace_id, &frame) {
                Opening::Opened(opened) => self.emit(&PeerEvent::Live {
                    workspace_id,
                    from,
                    frame: opened,
                }),
                Opening::NoKeys(channel) if hold => self.held_live.hold_inbound(
                    channel,
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
        let record = tilo_relay_protocol::live::LiveRecord {
            app: route.app_id.clone(),
            record: route.record.clone(),
        };
        let channel = unit(route).0.into_bytes();
        let released: Vec<_> = self
            .held_live
            .release(workspace_id, record, channel)
            .collect();
        for waiting in released {
            self.live_frame(waiting, false);
        }
    }

    /// Seal canonical live `frame` of `workspace_id` with the content cipher
    /// of the record it names. A frame naming no record is never sent.
    fn seal_live(&mut self, workspace_id: u64, frame: &[u8]) -> Sealing {
        let Some(frame) = tilo_relay_protocol::live::LiveFrame::decode(frame).ok() else {
            return Sealing::Dropped;
        };
        let Some((_, _, record)) = frame.seal_tags() else {
            return Sealing::Dropped;
        };
        let cipher = self.ciphers.iter_mut().find_map(|(route, cipher)| {
            let names = tilo_relay_protocol::live::LiveRecord {
                app: route.app_id.clone(),
                record: route.record.clone(),
            };
            (names == *record && route.workspace_id().ok() == Some(workspace_id)).then_some(cipher)
        });
        let Some(cipher) = cipher else {
            return Sealing::NoKeys(record.clone());
        };
        tilo_relay_protocol::live::SealedLiveFrame::seal(
            &frame,
            |artifact, retention, cleartext| cipher.seal(artifact, retention, cleartext),
        )
        .map_or(Sealing::Dropped, Sealing::Sealed)
    }

    /// Open sealed live `frame` of `workspace_id` with the content cipher of
    /// its channel into the canonical frame.
    fn open_live(&mut self, workspace_id: u64, frame: &[u8]) -> Opening {
        let Ok(sealed) = tilo_relay_protocol::live::SealedLiveFrame::decode(frame) else {
            return Opening::Dropped;
        };
        let channel = sealed.channel_id();
        let bound = self.ciphers.iter_mut().find_map(|(route, cipher)| {
            (unit(route) == channel && route.workspace_id().ok() == Some(workspace_id))
                .then_some((route, cipher))
        });
        let Some((route, cipher)) = bound else {
            return Opening::NoKeys(sealed.channel);
        };
        sealed
            .open_in_record(
                &route.cluster_id,
                &tilo_relay_protocol::live::LiveRecord {
                    app: route.app_id.clone(),
                    record: route.record.clone(),
                },
                |artifact, metadata, payload| cipher.open(artifact, metadata, payload),
            )
            .ok()
            .and_then(|frame| frame.encode().ok())
            .map_or(Opening::Dropped, Opening::Opened)
    }

    async fn publish(
        &mut self,
        route: DocumentRoute,
        frames: Vec<Vec<u8>>,
        client: Option<&RelayClient>,
    ) -> bool {
        let Ok(workspace_id) = route.workspace_id() else {
            self.fail(
                "protocol-rejected",
                "A document names an invalid workspace.",
            );
            return true;
        };
        self.open_channel(&route);
        if !self.ciphers.contains_key(&route) {
            // Never in the clear: the frames wait for this device's keys.
            self.held_outbound.entry(route).or_default().extend(frames);
            self.persist_channels(true);
            return true;
        }
        let mut delivered = true;
        for frame in frames {
            let now = tilo_clock::unix_now_ns();
            self.doc_seq = now.max(self.doc_seq.saturating_add(1));
            let Some(cipher) = self.ciphers.get_mut(&route) else {
                self.fail("signature-rejected", "the route's channel keys went away");
                return delivered;
            };
            let signed = match sign_frame(
                &route,
                frame,
                self.signer.as_ref(),
                cipher,
                self.doc_seq,
                now,
            )
            .await
            {
                Ok(signed) => signed,
                Err(error) => {
                    self.fail("signature-rejected", error.to_string());
                    return delivered;
                }
            };
            // What this device publishes, its own replica holds.
            self.note_delivered(
                &route,
                &[tilo_relay_protocol::compaction::entry_id(&signed.body)],
            );
            let entry = OutboxEntry {
                op: rand::random(),
                workspace_id,
                route: route.clone(),
                channel: signed.channel,
                checkpoint: signed.checkpoint,
                body: signed.body,
            };
            if let Some(client) = client {
                delivered &= client.send(commit(&entry, &self.config.cert)).is_ok();
            }
            self.outbox.push_back(entry);
            self.update(|status| {
                status.outgoing_pending = status.outgoing_pending.saturating_add(1);
                let pending = &mut status
                    .routes
                    .entry(route.clone())
                    .or_default()
                    .pending_operations;
                *pending = pending.saturating_add(1);
            });
        }
        self.persist();
        delivered
    }

    /// Open the on-demand record `route` of `workspace_id`, and ask for its
    /// keys at the same time. Returns `false` when the session could not
    /// take the request.
    fn open_record(
        &mut self,
        workspace_id: u64,
        route: DocumentRoute,
        client: Option<&RelayClient>,
    ) -> bool {
        let channel = neighbour_wire::app_document_channel(&route.app_id, &route.record);
        self.open_channel(&route);
        self.opened_records.insert(channel, (workspace_id, route));
        client.is_none_or(|client| {
            client
                .send(Envelope::OpenRecord {
                    workspace_id,
                    channel,
                })
                .is_ok()
        })
    }

    /// The host sent everything it holds of a record this page opened.
    fn record_opened(&mut self, workspace_id: u64, channel: [u8; 32], frames: u32) {
        if let Some(route) = self.owed_replays.remove(&channel) {
            let artifact =
                collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record);
            let given_up = self
                .held_inbound
                .get_mut(&route)
                .map(|held| held.give_up_owed(artifact))
                .unwrap_or_default();
            for owed in given_up {
                let error = format!("the host's replay has no document {owed}");
                self.refused = (self.refused.0.saturating_add(1), error.clone());
                self.emit(&PeerEvent::DocumentRejected(error));
            }
        }
        if let Some((_, route)) = self.opened_records.get(&channel) {
            self.emit(&PeerEvent::RecordOpened {
                workspace_id,
                route: route.clone(),
                frames,
            });
        }
    }

    /// Hold `token` for the rest of this session: what a page sends with
    /// every read through its host's fetch proxy.
    fn grant_fetch_proxy(&self, token: &[u8; 32]) {
        lock(&self.shared).fetch_proxy = Some(hex::encode(token));
    }

    /// Whether `envelope` is a member's ask for a workspace's record content, answered to that
    /// member alone from what this device keeps of it; a workspace it keeps nothing of is left.
    fn content_wanted(&self, envelope: &Envelope, client: &RelayClient) -> bool {
        let Envelope::RecordContentWant {
            workspace_id,
            from_device_public_key: from,
            frame,
        } = envelope
        else {
            return false;
        };
        let device = self.config.cert.device_public_key;
        let Some(kept) = self.kept.get(workspace_id).filter(|_| *from != device) else {
            return true;
        };
        let pieces =
            neighbour_wire::content::answer(&device, from, frame, |app, workspace, hash| {
                kept.read(app, workspace, hash)
            });
        for piece in pieces {
            let _ = client.send(Envelope::RecordContent {
                workspace_id: *workspace_id,
                to_device_public_key: *from,
                from_device_public_key: device,
                frame: piece,
            });
        }
        true
    }

    /// Take this device into `route`'s channel, with the participants its
    /// workspace's governance admits.
    fn open_channel(&mut self, route: &DocumentRoute) {
        if !self.route_keys.contains_key(route) {
            self.route_keys.insert(route.clone(), RouteKeys::Waiting);
        }
        // A channel taken up now asks for access in this pass.
        if matches!(self.route_keys.get(route), Some(RouteKeys::Waiting))
            && !self.read_keys(route, tilo_clock::Instant::now())
        {
            self.pump_channels(false);
        }
    }

    /// Run the channels' protocol once, then take up the keys it changed;
    /// `all` first reads every waiting route's again, as after the
    /// workspace's governance moved, so what it now names asks in this pass.
    fn pump_channels(&mut self, all: bool) {
        let now = tilo_clock::Instant::now();
        if all {
            self.read_all_keys(now);
        }
        self.channels
            .pump(&self.governance, now, tilo_clock::unix_now());
        if self.channels.generation() != self.keys_generation {
            self.read_all_keys(now);
        }
    }

    fn read_all_keys(&mut self, now: tilo_clock::Instant) {
        self.keys_generation = self.channels.generation();
        let routes: Vec<DocumentRoute> = self
            .route_keys
            .iter()
            .filter(|(_, keys)| !matches!(keys, RouteKeys::Handed(_)))
            .map(|(route, _)| route.clone())
            .collect();
        for route in routes {
            self.read_keys(&route, now);
        }
    }

    /// Take up `route`'s keys when the channels hold newer ones than the
    /// session; answers whether the route has keys.
    fn read_keys(&mut self, route: &DocumentRoute, now: tilo_clock::Instant) -> bool {
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
            return false;
        };
        let named = collab_types::ChannelRoute::new(route.app_id.as_str(), route.record.as_str());
        let (unit, access) = self.channels.access_record(workspace, &named, now);
        let tilo_channels::ChannelAccess::Ready(keys) = access else {
            return false;
        };
        let change = self.channels.channel_change(unit);
        let installed = match self.route_keys.get(route) {
            Some(RouteKeys::Engine { change: held, .. }) => *held == change,
            Some(RouteKeys::Handed(_)) => true,
            Some(RouteKeys::Waiting) | None => false,
        };
        if !installed {
            self.route_keys.insert(
                route.clone(),
                RouteKeys::Engine {
                    change,
                    keys: keys.clone(),
                },
            );
            self.new_keys.push((route.clone(), keys));
        }
        true
    }

    /// The verdict on an op for `route` from the gate of the group carrying it
    /// ([`crate::document_admission`], or [`crate::document_history_admission`] for what the host
    /// replays).
    fn admission(
        &self,
        route: &DocumentRoute,
        (governance_known, sealed): (bool, bool),
        retained: bool,
        encryption: Option<&collab_types::EncryptionMetadata>,
    ) -> collab_types::inbound::OpAdmission {
        let gate = self.gates.gate_for(&route.cluster_id, unit(route));
        if retained {
            crate::document_history_admission(gate, governance_known, sealed, route, encryption)
        } else {
            crate::document_admission(gate, governance_known, sealed, route, encryption)
        }
    }

    /// Follow what `workspace_id`'s governance says now: who its members are,
    /// and the channel commits that waited for a participant it now admits.
    fn follow_governance(&mut self, workspace_id: u64) {
        self.planes_due.insert(workspace_id);
        if let Some((workspace, members)) = self.replicas.members(workspace_id) {
            if self.status().room_members.get(&workspace) != members.as_ref() {
                self.update(|status| match members {
                    Some(members) => _ = status.room_members.insert(workspace.clone(), members),
                    None => _ = status.room_members.remove(&workspace),
                });
            }
            let devices = self.replicas.member_devices(workspace_id);
            if self.status().room_devices.get(&workspace) != devices.as_ref() {
                self.update(|status| match devices {
                    Some(devices) => _ = status.room_devices.insert(workspace.clone(), devices),
                    None => _ = status.room_devices.remove(&workspace),
                });
            }
        }
        self.pump_channels(true);
        // A record opened before the governance admitted it waits with its
        // channel unopened: opening it here asks the participants for the keys
        // the writes it holds wait on.
        let unopened: Vec<DocumentRoute> = self.held_outbound.keys().cloned().collect();
        for route in unopened {
            self.open_channel(&route);
        }
        // Writes held for a certificate or governance the workspace now has; the
        // workspace's `Ready` decides those of its replay.
        if lock(&self.shared).status.workspaces.get(&workspace_id) == Some(&true) {
            self.decide_held(workspace_id);
        }
    }

    /// Store channel state and held edits when either changed, so a
    /// reloaded page keeps its access and what it wrote before the keys.
    fn persist_channels(&self, held_changed: bool) {
        if held_changed {
            self.channel_state.hold(
                self.held_outbound
                    .iter()
                    .map(|(route, frames)| (route.clone(), frames.clone()))
                    .collect(),
            );
        }
        if let Err(error) = self.channel_state.save(self.store.as_ref()) {
            self.fail_on(FailureSubject::Storage, "storage-transaction-failed", error);
        }
    }

    /// Act on channel changes: await what the device's custody signs, send
    /// the governance the channels said, install new keys, open what waited
    /// for them, and seal and commit what was held. Returns `false` when the
    /// session could not take an envelope.
    async fn settle_channels(&mut self, client: Option<&RelayClient>) -> bool {
        let mut delivered = true;
        if self.channels.signing() {
            self.channels.signatures().await;
            self.pump_channels(false);
        }
        if let Some(client) = client {
            delivered &= self.send_governance(client);
        }
        loop {
            let keys = std::mem::take(&mut self.new_keys);
            if keys.is_empty() {
                self.persist_channels(false);
                let channels = self.describe_channels();
                if lock(&self.shared).status.channels != channels {
                    self.update(|status| status.channels = channels);
                }
                return delivered;
            }
            let held_before = self.held_outbound.len();
            for (route, keys) in keys {
                if let Ok(workspace_id) = route.workspace_id() {
                    self.planes_due.insert(workspace_id);
                }
                self.ciphers.insert(
                    route.clone(),
                    Box::new(tilo_peer_state::document_frame::KeysCipher(keys)),
                );
                self.release_live(&route);
                // Open what waited first: a reader that sees the route no
                // longer awaiting keys has its documents among the events.
                self.decide_route(&route, client);
                if let Some(frames) = self.held_outbound.remove(&route) {
                    delivered &= self.publish(route, frames, client).await;
                }
            }
            if self.held_outbound.len() != held_before {
                self.persist_channels(true);
                self.release_barriers();
            }
        }
    }

    /// The channels' diagnostics: each workspace, route, plane and held write.
    fn describe_channels(&self) -> String {
        let mut channels = self.replicas.describe();
        for (route, keys) in &self.route_keys {
            let state = match keys {
                RouteKeys::Waiting => "waiting",
                RouteKeys::Engine { .. } => "ready",
                RouteKeys::Handed(_) => "handed",
            };
            let _ = write!(
                channels,
                "\nroute {}/{} {:?}: {state};",
                route.app_id,
                route.record,
                unit(route)
            );
        }
        let planes = self.planes.describe();
        if !planes.is_empty() {
            channels.push('\n');
            channels.push_str(&planes);
        }
        for ((route, from), count) in &self.opened {
            let _ = write!(
                channels,
                "\nopened {}/{} from {}: {count}",
                route.app_id,
                route.record,
                hex::encode(from)
            );
        }
        for (route, held) in &self.held_inbound {
            let mut reasons = Vec::new();
            let mut waiting = [0_usize; 3];
            for (_, frame) in held.held() {
                let counted = match frame.reason {
                    HoldReason::Keys => &mut waiting[0],
                    HoldReason::Surface(_) => &mut waiting[1],
                    HoldReason::Governance => &mut waiting[2],
                };
                *counted = counted.saturating_add(1);
            }
            for (count, reason) in
                waiting
                    .into_iter()
                    .zip(["its keys", "its app surface", "governance"])
            {
                if count > 0 {
                    reasons.push(format!("{count} for {reason}"));
                }
            }
            if !reasons.is_empty() {
                let _ = write!(
                    channels,
                    "\nheld {}/{}: {}",
                    route.app_id,
                    route.record,
                    reasons.join(", ")
                );
            }
        }
        if self.refused.0 > 0 {
            let _ = write!(channels, "\nrefused {}: {}", self.refused.0, self.refused.1);
        }
        channels
    }
}

/// This device's channels as a reader: its state in `state`, sealed under
/// `vault`, signed by its custody, and their readers its workspaces'.
fn open_channels(
    replicas: &crate::replica::Replicas,
    state: &crate::custody::StoredChannels,
    vault: [u8; 32],
    signer: &Arc<dyn DeviceSigner>,
    gates: &Arc<tilo_channels::Gates>,
) -> tilo_channels::ChannelEncryption {
    let directory = || {
        Box::new(tilo_session_client::channels::OwnerDirectory::reader(
            replicas.owner(),
        ))
    };
    tilo_channels::ChannelEncryption::open(
        tilo_channels::ChannelEncryptionConfig {
            state: Arc::new(state.clone()),
            vault: channel_crypto::VaultKey::new(vault),
            signer: Arc::clone(signer),
            role: tilo_channels::Role::Reader,
        },
        directory(),
        Arc::clone(gates) as Arc<dyn tilo_channels::ChannelGates>,
    )
    .unwrap_or_else(|_| {
        tilo_channels::ChannelEncryption::without_store(directory(), Arc::clone(gates) as _)
    })
}

/// The keys this session holds for a route it opened.
enum RouteKeys {
    /// No keys yet.
    Waiting,
    /// The keys the channels hold, as of the channel's `change`.
    Engine {
        change: u64,
        keys: channel_crypto::ChannelContentKeys,
    },
    /// Keys a host device handed over for a record whose group this device
    /// is not admitted to itself: the workspace's own keys for the record.
    Handed(channel_crypto::ChannelContentKeys),
}

impl RouteKeys {
    fn keys(&self) -> Option<&channel_crypto::ChannelContentKeys> {
        match self {
            Self::Waiting => None,
            Self::Engine { keys, .. } | Self::Handed(keys) => Some(keys),
        }
    }
}

/// The storage unit of `route`: the channel its content is sealed under.
fn unit(route: &DocumentRoute) -> collab_types::ChannelId {
    collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record)
}

impl Task {
    /// Complete `done` once every entry now in the outbox is retired.
    fn await_outbox(&mut self, done: Completion) {
        match self.outbox.back() {
            None => done(Ok(())),
            Some(last) => self.barriers.push((last.op, done)),
        }
    }

    /// Move each held barrier whose routes hold nothing any more on to the
    /// outbox, which now carries what those routes held.
    fn release_barriers(&mut self) {
        let (waiting, released): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held_barriers)
            .into_iter()
            .partition(|(routes, _)| {
                routes
                    .iter()
                    .any(|route| self.held_outbound.contains_key(route))
            });
        self.held_barriers = waiting;
        for (_, done) in released {
            self.await_outbox(done);
        }
    }

    /// Retire outbox entry `op`: acknowledged at host order `seq`, or refused
    /// (`Err` with the host's reason; it can never become durable).
    fn acknowledge(&mut self, op: [u8; 16], outcome: Result<u64, String>) {
        let seq = outcome.as_ref().ok().copied();
        // The host read this commit and would not take it: the session is
        // fine, and saying so keeps the app connected. What the host said is
        // what the person is owed, so it travels with the refusal.
        let refusal = outcome.as_ref().err().map(|reason| refusal_text(reason));
        let Some(index) = self.outbox.iter().position(|entry| entry.op == op) else {
            return;
        };
        let Some(entry) = self.outbox.remove(index) else {
            return;
        };
        self.resending.remove(&op);
        self.persist();
        let route = entry.route;
        let emitted = route.clone();
        self.update(|status| {
            status.outgoing_pending = status.outgoing_pending.saturating_sub(1);
            let route_status = status.routes.entry(route).or_default();
            route_status.pending_operations = route_status.pending_operations.saturating_sub(1);
            match seq {
                Some(seq) => {
                    route_status.durable_operations =
                        route_status.durable_operations.saturating_add(1);
                    route_status.last_seq = Some(seq);
                }
                None => {
                    route_status.refused_operations =
                        route_status.refused_operations.saturating_add(1);
                }
            }
        });
        match outcome {
            Ok(seq) => self.emit(&PeerEvent::Committed {
                route: emitted,
                seq,
            }),
            Err(reason) => self.emit(&PeerEvent::CommitRefused {
                route: emitted,
                changes: tilo_peer_state::document_frame::claimed_changes(&entry.body),
                reason,
            }),
        }
        let outstanding: BTreeSet<[u8; 16]> = self.outbox.iter().map(|entry| entry.op).collect();
        let result = refusal.map_or(Ok(()), Err);
        let (waiting, done): (Vec<_>, Vec<_>) = std::mem::take(&mut self.barriers)
            .into_iter()
            .partition(|(target, _)| outstanding.contains(target));
        self.barriers = waiting;
        for (target, complete) in done {
            complete(if target == op { result.clone() } else { Ok(()) });
        }
    }
}

/// What this session reports when the host would not take a change it read.
fn refusal_text(reason: &str) -> String {
    format!("{REFUSAL}: {}", reason.trim())
}

/// The phrasing this session uses for the one answer that is a refusal.
const REFUSAL: &str = "the workspace refused the change";

/// This device's certificate for a workspace's other members, and the
/// revocations its person's root signed. A native shell accepts a member's
/// edits only from a device whose certificate it holds, and a browser has no
/// direct neighbours to hand it over, so it goes through the host, which
/// authenticates the author and keeps it for later joiners. A host reads the
/// revocations with it: a device of the person it may no longer trust, or the
/// stand-in approver it holds for them, is refused from the moment each entry
/// names on, and what they signed before stays valid.
fn identity_hello(
    workspace_id: u64,
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

fn commit(entry: &OutboxEntry, cert: &DeviceCert) -> Envelope {
    let payload = if entry.checkpoint {
        CoeditPayload::Checkpoint {
            channel: entry.channel,
            body: entry.body.clone(),
        }
    } else {
        CoeditPayload::RetainedDelta {
            channel: entry.channel,
            body: entry.body.clone(),
        }
    };
    Envelope::Commit {
        workspace_id: entry.workspace_id,
        op: entry.op,
        author_device_public_key: cert.device_public_key,
        authored_at_ns: tilo_clock::unix_now_ns(),
        delta: payload.encode().unwrap_or_default(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn status(workspaces: &[(u64, bool)]) -> PeerStatus {
        PeerStatus {
            phase: TransportPhase::Connected,
            authenticated: true,
            attempt: 0,
            next_attempt_at_ms: None,
            last_contact_at_ms: None,
            workspaces: workspaces.iter().copied().collect(),
            routes: BTreeMap::new(),
            failures: Vec::new(),
            outgoing_pending: 0,
            device: [0; 32],
            generation: 0,
            channels: String::new(),
            room_members: BTreeMap::new(),
            room_devices: BTreeMap::new(),
        }
    }

    fn failure(code: &'static str, subject: FailureSubject) -> Failure {
        Failure {
            code,
            message: String::new(),
            at_ms: 0,
            subject,
        }
    }

    #[test]
    fn a_subject_holds_one_failure_and_session_failures_one_per_code() {
        let mut status = status(&[]);
        upsert(
            &mut status,
            failure("transport-ended", FailureSubject::Session),
        );
        upsert(
            &mut status,
            failure("transport-unavailable", FailureSubject::Session),
        );
        upsert(
            &mut status,
            failure("transport-ended", FailureSubject::Session),
        );
        upsert(
            &mut status,
            failure("storage-transaction-failed", FailureSubject::Storage),
        );
        upsert(
            &mut status,
            failure("storage-quota", FailureSubject::Storage),
        );
        upsert(
            &mut status,
            failure("gpu-device-lost", FailureSubject::Surface("a".into())),
        );
        upsert(
            &mut status,
            failure("gpu-device-lost", FailureSubject::Surface("b".into())),
        );
        let codes: Vec<_> = status.failures.iter().map(|failure| failure.code).collect();
        assert_eq!(
            codes,
            [
                "transport-unavailable",
                "transport-ended",
                "storage-quota",
                "gpu-device-lost",
                "gpu-device-lost"
            ]
        );
        for index in 0..MAX_FAILURES {
            upsert(
                &mut status,
                failure(
                    "asset-unavailable",
                    FailureSubject::Asset {
                        surface: "a".into(),
                        asset: index.to_string(),
                    },
                ),
            );
        }
        assert_eq!(status.failures.len(), MAX_FAILURES);
    }

    #[test]
    fn workspaces_not_admitted_within_the_timeout_are_overdue_once_subscribed() {
        let since = 1_000;
        let waiting = status(&[(1, true), (2, false)]);
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
        let admitted = status(&[(1, true), (2, true)]);
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
    fn a_changed_host_stops_the_page_and_a_proxys_page_never_counts() {
        let code = |error| match refusal(&error) {
            Refusal::Final(code, _) => code,
            Refusal::Unexplained => "unexplained",
            Refusal::Reachability => "reachability",
        };
        assert_eq!(code(LinkError::HostChanged), "host-changed");
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

    /// A page that lists nobody says why: each workspace's plane names what it
    /// waits for, and only asks that reached no plane count towards the note
    /// in the members' place.
    #[test]
    fn a_plane_that_was_not_reached_is_named_in_the_channel_diagnostics() {
        let mut planes = plane::Planes::default();
        planes.later(7, 12_000);
        assert_eq!(
            planes.describe(),
            "plane 7: not joined. No live plane was reached; asking again at 12000"
        );
        assert_eq!(planes.missed(7), 1);
        assert_eq!(planes.missed(7), plane::MISSES_BEFORE_NOTE);
        planes.refused(7, 60_000);
        assert_eq!(
            planes.describe(),
            "plane 7: not joined. The workspace does not admit this page's plane; asking again \
             at 60000"
        );
        assert_eq!(planes.missed(7), 1, "an answer starts the count again");
    }
}

/// This session as what asking another device's actions needs ([`Heard`]): the frames it carries
/// to a device's key, and what it heard back, on a queue of its own, so any number of askers on
/// one session each hear every answer.
impl From<&PeerHandle> for Arc<dyn tilo_session_core::remote::RemoteSession> {
    fn from(peer: &PeerHandle) -> Self {
        Arc::new(Asking {
            peer: peer.clone(),
            events: peer.events(),
        })
    }
}

/// One asker of another device's actions over a session.
struct Asking {
    peer: PeerHandle,
    events: PeerEvents,
}

impl tilo_session_core::remote::RemoteSession for Asking {
    fn send_remote_action(
        &self,
        workspace_id: u64,
        device: [u8; 32],
        frame: Vec<u8>,
    ) -> Result<(), String> {
        self.peer.send_remote_action(workspace_id, device, frame)
    }

    fn take_heard(&self) -> Vec<Heard> {
        self.events
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                PeerEvent::Remote(heard) => Some(heard),
                _ => None,
            })
            .collect()
    }

    fn on_change(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.peer.on_event(&self.events, wake);
    }
}

#[cfg(test)]
mod iroh_connect_tests;
