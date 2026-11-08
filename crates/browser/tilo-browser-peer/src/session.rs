//! The certified relay session of one browser peer.
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
use tilo_relay_client::https::{HttpClient, HttpsLink, LinkError};

mod connect;
mod plane;
mod voice;

use connect::Dial;
#[cfg(all(test, feature = "iroh"))]
use connect::{connect, iroh_link_error};

/// Longest wait between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Keep-alive interval on an idle session.
const PING_INTERVAL: Duration = Duration::from_secs(15);
/// How long before the host ends a session (its fixed lifetime) the next one
/// starts: the renewal is then an ordinary reconnect, not a refused poll
/// followed by an offline spell and a backoff.
const RENEW_AHEAD: Duration = Duration::from_secs(60);
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
    use tilo_relay_protocol::web::SessionRefusal;
    match error {
        LinkError::Denied(SessionRefusal::NotAdmitted) => {
            Refusal::Final(NOT_ADMITTED.0, NOT_ADMITTED.1)
        }
        LinkError::Denied(SessionRefusal::DeviceRevoked) => Refusal::Final(
            "device-revoked",
            "This browser was removed from your devices, so it can’t open this workspace anymore.",
        ),
        LinkError::Denied(SessionRefusal::CertificateInvalid) => Refusal::Final(
            "certificate-rejected",
            "The workspace’s host did not accept how this browser signed in.",
        ),
        LinkError::HostChanged => {
            Refusal::Final("host-changed", "This workspace’s host was set up again.")
        }
        // A stale handshake is simply run again.
        LinkError::Denied(SessionRefusal::Handshake)
        | LinkError::Unavailable(_)
        | LinkError::Malformed(_)
        | LinkError::Sign(_) => Refusal::Reachability,
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

/// How to reach the user's host.
#[derive(Clone)]
pub enum Host {
    /// Its HTTPS origin, through this client (the page's `fetch`).
    Https(Arc<dyn HttpClient>),
    /// Its iroh endpoint.
    #[cfg(feature = "iroh")]
    Iroh {
        addr: iroh_base::EndpointAddr,
        /// An iroh relay server to dial it through, if any.
        relay_url: Option<iroh_base::RelayUrl>,
    },
}

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
    /// The page keeps this session beside the workspace in front, only to
    /// deliver for it (problem reports to the page's home workspace): the
    /// host counts nobody online by it, so it takes no seat and shows no
    /// face there ([`tilo_relay_protocol::web::SessionFinish::background`]).
    pub background: bool,
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
    /// `device-revoked`, `host-changed`,
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

/// Why a document waits undecided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldReason {
    /// Its route's keys, which carry it sealed.
    Keys,
    /// The app version it was written with, by that version's surface hash:
    /// this page asks its workspace for it, and decides the document when it
    /// arrives.
    Surface([u8; 32]),
    /// The workspace's governance: its author's certificate, the workspace's
    /// document, or the app version the workspace runs.
    Governance,
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
    /// The joined workspaces whose layout is personal, as their governance says:
    /// the page keeps its strips and windows to itself there.
    pub personal_rooms: BTreeSet<String>,
    /// The members each joined workspace's governance lists, by workspace, by
    /// person id: what someone no longer among them made on the workspace's
    /// desktop is released ([`tilo_desktop_model::ownership::released`]).
    pub room_members: BTreeMap<String, BTreeSet<String>>,
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
    /// A remote-action frame (`tilo_actions::remote::RemoteActionFrame`
    /// bytes) another device of the workspace addressed to this one: a request
    /// to answer, or the answer to one this device sent.
    RemoteAction {
        /// The workspace id.
        workspace_id: u64,
        /// Host-stamped sending device.
        from: [u8; 32],
        /// Opaque frame.
        frame: Vec<u8>,
    },
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
    /// A member asked the workspace for content a record references: answer
    /// with [`PeerHandle::send_content`] when this page keeps it. The workspace's host asks this way for a copy of what members
    /// post, so a visitor's file stays after they leave.
    ContentWanted {
        /// The workspace id.
        workspace_id: u64,
        /// Host-stamped publishing device.
        from: [u8; 32],
        /// A `NeighbourMsg::RecordContentWant`, postcard-encoded.
        request: Vec<u8>,
    },
    /// The host could not deliver a remote-action frame this device sent.
    RemoteActionUndeliverable {
        /// The workspace id.
        workspace_id: u64,
        /// The addressed device.
        to: [u8; 32],
        /// Why.
        reason: String,
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
    /// The workspace's governance changed whether it shares its members' layout
    /// ([`PeerStatus::personal_rooms`]): in a personal one the page keeps
    /// its strips and windows to itself and sends the workspace no desktop.
    WorkspaceLayout {
        /// The workspace (its cluster id).
        workspace: String,
        /// Each member's layout is their own.
        personal: bool,
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
        } => Some(PeerEvent::RemoteAction {
            workspace_id,
            from: from_device_public_key,
            frame,
        }),
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
        } => Some(PeerEvent::RemoteActionUndeliverable {
            workspace_id,
            to: to_device_public_key,
            reason,
        }),
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
    OpenLive(u64, Vec<u8>, LiveCompletion),
    RemoteAction(u64, [u8; 32], Vec<u8>),
    WantContent(u64, Vec<u8>),
    SendContent(u64, [u8; 32], Vec<u8>),
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
    /// This page needs `source` of `device` afresh: a keyframe from its next
    /// group on.
    CallPictureRestart(u64, [u8; 32], tilo_live::video::Source),
    Administer(
        tilo_relay_protocol::workspaces::WorkspacesRequest,
        RoomsCompletion,
    ),
    Here(tilo_relay_protocol::web::HereRequest, HereCompletion),
    Node(NodeCompletion),
    Compact([u8; 16], Vec<u8>),
    Reconnect,
    /// The network is back: a session waiting out its backoff tries again
    /// now; a connected or failed one stays as it is.
    Retry,
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
/// [`PeerHandle::seal_live_then`] and [`PeerHandle::open_live_then`].
pub type LiveCompletion = Box<dyn FnOnce(Option<Vec<u8>>) + Send>;
/// Called once with the session device the node runs beside the host (or
/// `None`), see [`PeerHandle::node_session_then`].
pub type NodeCompletion = Box<dyn FnOnce(Result<Option<[u8; 32]>, String>) + Send>;
/// Called once with who is here, see [`PeerHandle::here_then`].
pub type HereCompletion =
    Box<dyn FnOnce(Result<tilo_relay_protocol::web::HereReply, String>) + Send>;
/// Called once with the host's answer, see [`PeerHandle::rooms_then`].
pub type RoomsCompletion =
    Box<dyn FnOnce(Result<tilo_relay_protocol::workspaces::WorkspacesReply, String>) + Send>;
type Queue = Arc<Mutex<VecDeque<PeerEvent>>>;

struct Shared {
    status: PeerStatus,
    queues: Vec<std::sync::Weak<Mutex<VecDeque<PeerEvent>>>>,
    wakes: Vec<Wake>,
    /// The current session's token while its host serves the fetch proxy;
    /// kept apart from [`PeerStatus`], which diagnostics show.
    fetch_proxy: Option<String>,
    /// The current HTTPS session's token, for the page to end it as it
    /// leaves ([`PeerHandle::session_token`]).
    session: Option<String>,
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
        shared.wakes.clone()
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
            status: PeerStatus {
                phase: TransportPhase::Connecting,
                authenticated: false,
                attempt: 0,
                next_attempt_at_ms: None,
                last_contact_at_ms: None,
                workspaces: BTreeMap::new(),
                routes: BTreeMap::new(),
                failures: Vec::new(),
                outgoing_pending: 0,
                device: config.cert.device_public_key,
                generation: 0,
                channels: String::new(),
                personal_rooms: BTreeSet::new(),
                room_members: BTreeMap::new(),
            },
            queues: Vec::new(),
            wakes: Vec::new(),
            fetch_proxy: None,
            session: None,
            live_note: None,
        }));
        let events: Queue = Arc::new(Mutex::new(VecDeque::new()));
        lock(&shared).queues.push(Arc::downgrade(&events));
        let record = store.load().unwrap_or_default();
        let vault = store.channel_vault().unwrap_or_else(rand::random);
        let mut channels = crate::channels::Channels::new(config.cert.device_public_key, vault);
        let saved = store
            .load_channels()
            .and_then(|bytes| SavedPeerChannels::decode(&bytes))
            .unwrap_or_default();
        channels.restore(saved.channels);
        channels.restore_requests(saved.requests);
        #[cfg(feature = "toolkit")]
        crate::surfaces::decide_with_kept_surfaces();
        let task = Task {
            replicas: crate::replica::Replicas::new(config.cert.clone(), Arc::clone(&store)),
            commands: commands.downgrade(),
            config,
            signer,
            store,
            dial: Dial::for_page(&vault),
            shared: shared.clone(),
            workspaces: BTreeSet::new(),
            placing: BTreeMap::new(),
            planes: plane::Planes::default(),
            outbox: record.outbox.iter().cloned().collect(),
            resending: BTreeSet::new(),
            record,
            ciphers: Ciphers::new(),
            channels,
            channel_changes: crate::channels::Changes::default(),
            opened: BTreeMap::new(),
            refused: (0, String::new()),
            held_inbound: BTreeMap::new(),
            surface_wants: crate::content::ContentWants::default(),
            surfaces_arrived: false,
            owed_replays: BTreeMap::new(),
            held_live: crate::held_live::HeldLive::new(),
            held_outbound: saved.held.into_iter().collect(),
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
            here_pending: BTreeMap::new(),
            here_next: 0,
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

    /// Subscribe to a workspace.
    pub fn join(&self, workspace_id: u64) -> Result<(), String> {
        self.send(Command::Join(workspace_id))
    }

    /// Join `workspace` (its UUID), a workspace of this device's person, named
    /// `name`: the host is first asked to govern it
    /// ([`tilo_relay_protocol::workspaces::WorkspacesRequest::Place`]), creating it
    /// unless it does, so the workspace has governance like every other; then it
    /// is subscribed. Where the host places nothing for this device (a workspace
    /// of another person's, a host reached without its administration route),
    /// workspace is subscribed as it is, and its own admission decides.
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

    /// Open one sealed live frame of `workspace_id` that another carrier
    /// delivered; `done` receives the canonical frame, or `None` when it
    /// does not open with keys this device holds.
    pub fn open_live_then(
        &self,
        workspace_id: u64,
        frame: Vec<u8>,
        done: LiveCompletion,
    ) -> Result<(), String> {
        self.send(Command::OpenLive(workspace_id, frame, done))
    }

    /// Send one remote-action frame to device `to` of the workspace (the host's
    /// own device reaches its app-serving plane). Dropped while
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

    /// Send device `to` of workspace `workspace_id` one piece of content it asked
    /// for ([`PeerEvent::ContentWanted`]): `frame` is a postcard-encoded
    /// `NeighbourMsg::RecordContentPiece`. Dropped while disconnected: the
    /// asker asks again.
    pub fn send_content(
        &self,
        workspace_id: u64,
        to: [u8; 32],
        frame: Vec<u8>,
    ) -> Result<(), String> {
        self.send(Command::SendContent(workspace_id, to, frame))
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

    /// Post this person's own actions and ask who is here; `done` gets the answer.
    pub fn here_then(
        &self,
        request: tilo_relay_protocol::web::HereRequest,
        done: HereCompletion,
    ) -> Result<(), String> {
        self.send(Command::Here(request, done))
    }

    /// Ask the host's workspace administration `request`; `done` gets the answer.
    pub fn rooms_then(
        &self,
        request: tilo_relay_protocol::workspaces::WorkspacesRequest,
        done: RoomsCompletion,
    ) -> Result<(), String> {
        self.send(Command::Administer(request, done))
    }

    /// Ask the host which device runs the person's apps beside it (a
    /// node's session), over the current HTTPS session; `done` receives it,
    /// or `None` from a host that runs none.
    pub fn node_session_then(&self, done: NodeCompletion) -> Result<(), String> {
        self.send(Command::Node(done))
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

    /// Current status.
    pub fn status(&self) -> PeerStatus {
        lock(&self.shared).status.clone()
    }

    /// The session token the host's fetch proxy takes
    /// (`tilo_net::proxy`, at `/peer/v1/fetch`), while connected over HTTPS to
    /// a host that serves one; `None` otherwise. It changes with every
    /// reconnect, so callers read it for each request.
    pub fn fetch_proxy_session(&self) -> Option<String> {
        lock(&self.shared).fetch_proxy.clone()
    }

    /// The token of the HTTPS session the peer runs now, `None` between
    /// sessions and over iroh: what a page leaving sends the host
    /// (`/peer/v1/session/end`) so the workspace stops counting it at once.
    pub fn session_token(&self) -> Option<String> {
        lock(&self.shared).session.clone()
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
}

enum Exit {
    Closed,
    Reconnect,
    Lost(String),
}

/// A frame held for its route, with its authenticated author, whether the
/// host was replaying it, and what it waits for: its record's history is
/// admitted where a live write is not, and a frame decided again keeps that
/// verdict.
struct HeldFrame {
    body: Vec<u8>,
    author: [u8; 32],
    retained: bool,
    reason: HoldReason,
}

impl HeldFrame {
    /// Whether this frame waits for an app surface an arrival can supply.
    fn waits_for_surface(&self) -> bool {
        matches!(self.reason, HoldReason::Surface(_))
    }
}

/// Why `envelope`, a write this page cannot decide yet, waits: for the app
/// version it was written with, a change naming a surface the page holds not
/// (it asks the workspace for it), or for its workspace's governance.
fn hold_reason(route: &DocumentRoute, envelope: &collab_types::SignedOp) -> HoldReason {
    let surface = (envelope.kind == collab_types::OpKind::Change)
        .then_some(envelope.surface)
        .flatten();
    match surface {
        Some(surface) if !crate::surfaces::holds(&route.app_id, &surface) => {
            HoldReason::Surface(surface)
        }
        _ => HoldReason::Governance,
    }
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
    /// This device's membership of the workspaces' encrypted channels.
    channels: crate::channels::Channels,
    /// What channel state changed since the session last acted on it.
    channel_changes: crate::channels::Changes,
    /// Documents opened per route, and those refused with the last reason,
    /// for the diagnostics.
    opened: BTreeMap<(DocumentRoute, [u8; 4]), u64>,
    refused: (u64, String),
    /// Sealed documents that arrived before this device held their keys,
    /// with their authenticated authors.
    held_inbound: BTreeMap<DocumentRoute, InboundHold<HeldFrame>>,
    /// App surfaces this page asked its workspaces for, by the route whose
    /// writes named them: they arrive as pieces of record content.
    surface_wants: crate::content::ContentWants<()>,
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
    /// The sessions asked their host who is here and not answered yet, by the
    /// id they asked with.
    here_pending: BTreeMap<u64, HereCompletion>,
    /// The id the next question about who is here is asked with.
    here_next: u64,
}

/// An open compaction request: workspace, route and the entries it covers.
type Compaction = (u64, DocumentRoute, Vec<[u8; 32]>);

/// Entry ids a route remembers for compaction. Past it, later entries are
/// left out of statements and simply stay retained.
const MAX_DELIVERED_ENTRIES: usize = 1 << 17;
/// Open compaction requests kept; the oldest goes first.
const MAX_COMPACTIONS: usize = 16;

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

    /// Tell the host, once `workspace_id` admitted this device, its
    /// certificate and that it answers compaction requests. Returns `false`
    /// when the session could not take them.
    fn announce(&self, client: &Link, workspace_id: u64) -> bool {
        if let Some(hello) = identity_hello(workspace_id, &self.config.cert)
            && client.send(hello).is_err()
        {
            return false;
        }
        !client.compacts()
            || client
                .send(Envelope::CompactVolunteer { workspace_id })
                .is_ok()
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
        client: Option<&Link>,
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
            shared.wakes.clone()
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

    /// Subscribe every joined workspace again, from now on waiting for its
    /// `Ready`. Returns `false` when the session could not take them.
    fn resubscribe(&self, client: &Link) -> bool {
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
                    return self.closed();
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
                    let exit = self.serve_naming_proxy(client, &mut commands).await;
                    // A request the host had not answered is answered here:
                    // nothing waits on a connection that is gone.
                    self.requests_lost();
                    match exit {
                        Exit::Closed => return self.closed(),
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
                            return self.closed();
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
                        None | Some(Command::Close) => return self.closed(),
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

    fn closed(&self) {
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

    /// [`Self::serve`], naming the session's fetch proxy while it lasts
    /// ([`PeerHandle::fetch_proxy_session`]).
    async fn serve_naming_proxy(
        &mut self,
        client: Link,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> Exit {
        {
            let mut shared = lock(&self.shared);
            shared.fetch_proxy = client.fetch_proxy_session();
            shared.session = client.session_token();
        }
        let exit = self.serve(client, commands).await;
        let mut shared = lock(&self.shared);
        shared.fetch_proxy = None;
        shared.session = None;
        exit
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

    async fn serve(
        &mut self,
        mut client: Link,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> Exit {
        if !self.resume_session(&client) {
            return Exit::Lost("host session saturated during resume".into());
        }
        self.connected();
        let mut ping = n0_future::time::interval(PING_INTERVAL);
        let mut channel_tick = n0_future::time::interval(CHANNEL_TICK);
        let mut heartbeat = n0_future::time::interval(tilo_relay_protocol::voice::HEARTBEAT);
        // A page that stood still beats once when it runs again, not once
        // for every beat it missed.
        heartbeat.set_missed_tick_behavior(n0_future::time::MissedTickBehavior::Delay);
        let renewal = renewal(client.renew_after(tilo_clock::unix_now_ns()));
        tokio::pin!(renewal);
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
                        client.close().await;
                        return Exit::Closed;
                    }
                    Some(Command::Reconnect) => {
                        client.close().await;
                        return Exit::Reconnect;
                    }
                    Some(Command::Refresh) => {
                        self.clear_admission_failure();
                        // A refresh asks for the workspaces whole: no position.
                        self.positions.clear();
                        self.received.clear();
                        if !self.resubscribe(&client) {
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
                        let admitted = match &envelope {
                            Envelope::Ready { workspace_id, .. } => Some(*workspace_id),
                            _ => None,
                        };
                        if let Some(workspace_id) = admitted
                            && !self.announce(&client, workspace_id)
                        {
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
                    let changes = self.channels.tick(tilo_clock::unix_now_ms());
                    self.absorb(changes);
                }
                () = &mut renewal => {
                    client.close().await;
                    return Exit::Reconnect;
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
    fn remote_action(
        &self,
        workspace_id: u64,
        to: [u8; 32],
        frame: Vec<u8>,
        client: Option<&Link>,
    ) {
        if let Some(client) = client {
            let _ = client.send(Envelope::RemoteAction {
                workspace_id,
                version: tilo_relay_protocol::transport::REMOTE_ACTION_ENVELOPE_VERSION,
                to_device_public_key: to,
                from_device_public_key: self.config.cert.device_public_key,
                from_user_public_key: self.config.cert.user_public_key,
                frame,
            });
        } else {
            self.emit(&PeerEvent::RemoteActionUndeliverable {
                workspace_id,
                to,
                reason: "not connected to the host".into(),
            });
        }
    }

    /// Ask the workspace for content ([`PeerHandle::want_content`]): the host
    /// fans the request out and answers with what it keeps. Nothing while
    /// disconnected.
    fn want_content(&self, workspace_id: u64, request: Vec<u8>, client: Option<&Link>) {
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
    async fn apply(&mut self, command: Command, client: Option<&Link>) -> bool {
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
            command @ (Command::Live(..) | Command::SealLive(..) | Command::OpenLive(..)) => {
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
            Command::SendContent(workspace_id, to, frame) => {
                if let Some(client) = client {
                    let _ = client.send(Envelope::RecordContent {
                        workspace_id,
                        to_device_public_key: to,
                        from_device_public_key: self.config.cert.device_public_key,
                        frame,
                    });
                }
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
                self.channels.install_handed(&route, keys.clone());
                self.absorb(crate::channels::Changes {
                    keys: vec![(route, keys)],
                    ..crate::channels::Changes::default()
                });
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
            Command::Here(request, done) => self.request_here(request, done, client),
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
            Command::CallPictureRestart(workspace_id, device, source) => {
                self.restart_call_picture(workspace_id, device, source);
                true
            }
            Command::Administer(request, done) => {
                self.administer(request, done, client);
                true
            }
            Command::Node(done) => {
                Self::request_node(done, client);
                true
            }
            // Handled by the caller's loop.
            Command::Reconnect | Command::Retry | Command::Refresh | Command::Close => true,
        }
    }

    /// Subscribe to `workspace_id`, now or once connected.
    fn join(&mut self, workspace_id: u64, client: Option<&Link>) -> bool {
        self.workspaces.insert(workspace_id);
        self.update(|status| {
            status.workspaces.entry(workspace_id).or_insert(false);
        });
        self.received.clear();
        client.is_none_or(|client| client.send(self.subscribe(client, workspace_id)).is_ok())
    }

    /// Join `workspace` once the host governs it ([`PeerHandle::join_placed`]).
    fn join_placed(&mut self, workspace: String, name: String, client: Option<&Link>) -> bool {
        let Ok(workspace_id) = tilo_relay_protocol::room_workspace_id(&workspace) else {
            return true;
        };
        if self.workspaces.contains(&workspace_id) {
            return true;
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
    fn place(&self, workspace_id: u64, client: &Link) -> bool {
        let Some((workspace, name)) = self.placing.get(&workspace_id).cloned() else {
            return true;
        };
        let commands = self.commands.clone();
        let placed = move || {
            if let Some(commands) = commands.upgrade() {
                let _ = commands.send(Command::Placed(workspace_id));
            }
        };
        match client {
            Link::Https(link) => {
                let http = link.client().clone();
                let session = link.session().to_owned();
                n0_future::task::spawn(async move {
                    let request = tilo_relay_protocol::workspaces::WorkspacesRequest::Place {
                        workspace,
                        name,
                    };
                    // Refused (not this person's host, a workspace governed
                    // elsewhere) or unanswered: the workspace's admission decides.
                    let _ = tilo_relay_client::https::workspaces(http.as_ref(), &session, &request)
                        .await;
                    placed();
                });
            }
            // Workspaces are administered over HTTPS only: a native peer joins
            // workspaces placed for it otherwise.
            #[cfg(feature = "iroh")]
            Link::Iroh { .. } => placed(),
        }
        true
    }

    /// Restore this session's workspaces, pending commits and call participations.
    fn resume_session(&mut self, client: &Link) -> bool {
        let mut sent_ok = true;
        // A plane's grant is its session's token: the planes of the last
        // session ended with it, and every workspace's is joined again with
        // this one ([`Self::settle_planes`]).
        self.planes.end_session();
        // A resuming subscription can replay a frame before its covering position.
        // Keep duplicate detection until the host requests a whole subscription.
        if !client.resumable() {
            self.received.clear();
        }
        for workspace_id in &self.workspaces {
            sent_ok &= client.send(self.subscribe(client, *workspace_id)).is_ok();
        }
        for workspace_id in self.placing.keys() {
            sent_ok &= self.place(*workspace_id, client);
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
    fn flush_outbox(&mut self, client: &Link) {
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
    fn request_here(
        &mut self,
        request: tilo_relay_protocol::web::HereRequest,
        done: HereCompletion,
        client: Option<&Link>,
    ) -> bool {
        match client {
            Some(Link::Https(link)) => {
                let http = link.client().clone();
                let session = link.session().to_owned();
                n0_future::task::spawn(async move {
                    let reply =
                        tilo_relay_client::https::here(http.as_ref(), &session, &request).await;
                    done(reply.map_err(|error| error.to_string()));
                });
            }
            #[cfg(feature = "iroh")]
            Some(Link::Iroh { client, .. }) => {
                let request_id = self.here_next;
                self.here_next = self.here_next.wrapping_add(1);
                let frame = match postcard::to_stdvec(&request) {
                    Ok(frame) => frame,
                    Err(error) => {
                        done(Err(format!("the request does not encode: {error}")));
                        return true;
                    }
                };
                self.here_pending.insert(request_id, done);
                if client
                    .send(Envelope::HereRequest { request_id, frame })
                    .is_err()
                    && let Some(done) = self.here_pending.remove(&request_id)
                {
                    done(Err("the host's session closed".into()));
                }
            }
            None => done(Err("not connected to the host".into())),
        }
        true
    }

    /// The host answered who is here for request `request_id`.
    fn here_reply(&mut self, request_id: u64, frame: &[u8]) {
        if let Some(done) = self.here_pending.remove(&request_id) {
            done(answered::<Result<_, String>>(frame).and_then(std::convert::identity));
        }
    }

    /// Ask the host `request` of its workspace administration: over the
    /// session it answers, under an id this session picks so the answers of a
    /// session asking several may come back in any order.
    fn administer(
        &mut self,
        request: tilo_relay_protocol::workspaces::WorkspacesRequest,
        done: RoomsCompletion,
        client: Option<&Link>,
    ) {
        match client {
            Some(Link::Https(link)) => {
                let http = link.client().clone();
                let session = link.session().to_owned();
                n0_future::task::spawn(async move {
                    let reply =
                        tilo_relay_client::https::workspaces(http.as_ref(), &session, &request)
                            .await;
                    done(reply.map_err(|error| error.to_string()));
                });
            }
            #[cfg(feature = "iroh")]
            Some(Link::Iroh { client, .. }) => {
                let request_id = self.rooms_next;
                self.rooms_next = self.rooms_next.wrapping_add(1);
                let frame = match postcard::to_stdvec(&request) {
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
            None => done(Err("not connected to the host".into())),
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
        for (_, done) in std::mem::take(&mut self.here_pending) {
            done(Err("the session ended before the host answered".into()));
        }
    }

    fn request_node(done: NodeCompletion, client: Option<&Link>) {
        match client {
            Some(Link::Https(link)) => {
                let http = link.client().clone();
                let session = link.session().to_owned();
                n0_future::task::spawn(async move {
                    // A host that answers no `/node` runs nothing beside it.
                    let info = tilo_relay_client::https::node(http.as_ref(), &session).await;
                    done(Ok(info.ok().and_then(|info| info.session)));
                });
            }
            #[cfg(feature = "iroh")]
            Some(Link::Iroh { .. }) => done(Ok(None)),
            None => done(Err("not connected to the host".into())),
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
            Command::OpenLive(workspace_id, frame, done) => Waiting::Open {
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
            Waiting::Open {
                workspace_id,
                frame,
                done,
            } => match self.open_live(workspace_id, &frame) {
                Opening::Opened(opened) => done(Some(opened)),
                Opening::NoKeys(channel) if hold => self.held_live.hold_inbound(
                    channel,
                    Waiting::Open {
                        workspace_id,
                        frame,
                        done,
                    },
                ),
                Opening::NoKeys(_) | Opening::Dropped => done(None),
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
        let channel = crate::channels::Channels::channel(route).0.into_bytes();
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
            (crate::channels::Channels::channel(route) == channel
                && route.workspace_id().ok() == Some(workspace_id))
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
        client: Option<&Link>,
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
        client: Option<&Link>,
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

    /// Subscribe to `workspace_id`, resuming after the last position when
    /// the host supports it.
    fn subscribe(&self, client: &Link, workspace_id: u64) -> Envelope {
        if client.resumable() {
            Envelope::SubscribeFrom {
                workspace_id,
                position: self.positions.get(&workspace_id).cloned(),
            }
        } else {
            Envelope::Subscribe { workspace_id }
        }
    }

    fn inbound(&mut self, envelope: Envelope) {
        let now = tilo_clock::unix_now_ms();
        self.update(|status| status.last_contact_at_ms = Some(now));
        match envelope {
            Envelope::Ready { workspace_id, .. } => {
                self.update(|status| {
                    status.workspaces.insert(workspace_id, true);
                });
                // The replay's documents reach the page before its `Ready`,
                // as they arrived.
                self.decide_held(workspace_id);
                self.emit(&PeerEvent::Ready { workspace_id });
            }
            Envelope::NotAuthorised { reason, .. } => {
                self.fail("permission-required", reason);
            }
            Envelope::WorkspaceBootstrap {
                workspace_id,
                bootstrap,
            } => {
                match self
                    .replicas
                    .bootstrap(workspace_id, &bootstrap, web_time::SystemTime::now())
                {
                    Ok(true) => self.follow_governance(workspace_id),
                    Ok(false) => {}
                    Err(error) => self.fail("protocol-rejected", error),
                }
            }
            Envelope::Delta {
                workspace_id,
                author_device_public_key,
                delta,
                ..
            } => {
                let Some(
                    CoeditPayload::Checkpoint { body, .. }
                    | CoeditPayload::RetainedDelta { body, .. },
                ) = CoeditPayload::decode(&delta)
                else {
                    return;
                };
                self.inbound_body(workspace_id, body, author_device_public_key);
            }
            Envelope::Position {
                workspace_id,
                position,
            } => {
                self.positions.insert(workspace_id, position);
            }
            Envelope::RecordOpened {
                workspace_id,
                channel,
                frames,
            } => self.record_opened(workspace_id, channel, frames),
            Envelope::CompactRequest {
                workspace_id,
                request,
                app_id,
                cluster_id,
                record,
            } => self.compaction_requested(
                workspace_id,
                request,
                DocumentRoute {
                    app_id,
                    cluster_id,
                    record,
                },
            ),
            Envelope::WorkspacesReply { request_id, frame } => self.rooms_reply(request_id, &frame),
            Envelope::HereReply { request_id, frame } => self.here_reply(request_id, &frame),
            Envelope::Committed { op, seq, .. } => self.acknowledge(op, Ok(seq)),
            Envelope::CommitRefused { op, reason, .. } => {
                self.fail("protocol-rejected", reason.clone());
                self.acknowledge(op, Err(reason));
            }
            Envelope::RecordContentWant {
                workspace_id,
                from_device_public_key,
                frame,
            } => self.content_wanted(workspace_id, from_device_public_key, frame),
            Envelope::RecordContent { ref frame, .. } => {
                // A surface this page asked for is the peer's, not a record's
                // content: it is kept for deciding, and what waited for it is
                // decided at the top of the session loop.
                let device = self.config.cert.device_public_key;
                match self.surface_wants.piece(&device, frame) {
                    Some(arrived) => {
                        self.surfaces_arrived |=
                            crate::surfaces::keep(&arrived.hash, arrived.bytes);
                    }
                    None => {
                        if let Some(event) = addressed(envelope) {
                            self.emit(&event);
                        }
                    }
                }
            }
            envelope => {
                if let Some(event) = addressed(envelope) {
                    self.emit(&event);
                }
            }
        }
    }

    /// One member asked for `frame` of `workspace_id`'s record content: a
    /// request this device did not make is the page's to answer.
    fn content_wanted(&self, workspace_id: u64, from: [u8; 32], frame: Vec<u8>) {
        if from != self.config.cert.device_public_key {
            self.emit(&PeerEvent::ContentWanted {
                workspace_id,
                from,
                request: frame,
            });
        }
    }

    /// Take this device into `route`'s channel, with the participants its
    /// workspace's governance admits.
    fn open_channel(&mut self, route: &DocumentRoute) {
        let replicas = &self.replicas;
        let changes = self.channels.open(
            route,
            &|route| replicas.policy(route),
            tilo_clock::unix_now_ms(),
        );
        self.absorb(changes);
    }

    /// Follow what `workspace_id`'s governance says now: whether the workspace
    /// shares its members' layout, who its members are, and the channel
    /// commits that waited for a participant it now admits.
    fn follow_governance(&mut self, workspace_id: u64) {
        if let Some((workspace, layout, members)) = self.replicas.layout(workspace_id) {
            let personal = layout.is_personal();
            if self.status().room_members.get(&workspace) != members.as_ref() {
                self.update(|status| match members {
                    Some(members) => _ = status.room_members.insert(workspace.clone(), members),
                    None => _ = status.room_members.remove(&workspace),
                });
            }
            if self.status().personal_rooms.contains(&workspace) != personal {
                self.update(|status| {
                    if personal {
                        status.personal_rooms.insert(workspace.clone());
                    } else {
                        status.personal_rooms.remove(&workspace);
                    }
                });
                self.emit(&PeerEvent::WorkspaceLayout {
                    workspace,
                    personal,
                });
            }
        }
        let replicas = &self.replicas;
        let changes = self
            .channels
            .retry(&|route| replicas.policy(route), tilo_clock::unix_now_ms());
        self.absorb(changes);
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

    /// One retained or live message the host relayed to `workspace_id`:
    /// the workspace's governance (a member's certificate, the workspace document)
    /// is admitted through the workspace's replica; channel governance and
    /// documents go on.
    fn inbound_body(&mut self, workspace_id: u64, body: Vec<u8>, author: [u8; 32]) {
        let retained = lock(&self.shared).status.workspaces.get(&workspace_id) != Some(&true);
        match postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body) {
            Ok(
                message @ (neighbour_wire::NeighbourMsg::IdentityHello { .. }
                | neighbour_wire::NeighbourMsg::SyncResp { .. }),
            ) => {
                // Governance the replica refuses is not the workspace's.
                let admitted = self.replicas.admit(
                    workspace_id,
                    author,
                    retained,
                    message,
                    web_time::SystemTime::now(),
                );
                if admitted == Ok(true) {
                    self.follow_governance(workspace_id);
                }
            }
            Ok(message) => self.inbound_message(body, message, author, retained),
            Err(_) => {}
        }
    }

    /// A held document, decided again; `retained` as it arrived.
    fn held_body(&mut self, body: Vec<u8>, author: [u8; 32], retained: bool) {
        if let Ok(message) = postcard::from_bytes(&body) {
            self.inbound_message(body, message, author, retained);
        }
    }

    /// Channel governance or a document `author` wrote, `retained` while the
    /// host replays the workspace.
    fn inbound_message(
        &mut self,
        body: Vec<u8>,
        message: neighbour_wire::NeighbourMsg,
        author: [u8; 32],
        retained: bool,
    ) {
        let now = tilo_clock::unix_now_ms();
        match message {
            neighbour_wire::NeighbourMsg::MlsGovernance {
                workspace, frame, ..
            } => {
                let replicas = &self.replicas;
                let changes = self.channels.receive_frame(
                    &workspace,
                    *frame,
                    &|route| replicas.policy(route),
                    now,
                );
                self.absorb(changes);
            }
            ref message @ neighbour_wire::NeighbourMsg::AppDocument {
                ref app_id,
                ref cluster_id,
                ref record_id,
                ref envelope,
                ..
            } => {
                let route = DocumentRoute {
                    app_id: app_id.clone(),
                    cluster_id: cluster_id.clone(),
                    record: record_id.clone(),
                };
                // A workspace the host resumes after it lagged can replay an entry
                // its live deliveries already carried; the page takes it once.
                if self
                    .received
                    .contains(&tilo_relay_protocol::compaction::entry_id(&body))
                {
                    return;
                }
                let Ok(workspace_id) = route.workspace_id() else {
                    return;
                };
                // The workspace's governance decides the write before any app
                // sees it, and holds it while it cannot yet.
                match self.replicas.decide_document(
                    workspace_id,
                    author,
                    retained,
                    message,
                    web_time::SystemTime::now(),
                ) {
                    Ok(true) => {}
                    Ok(false) => {
                        // The channel carries the record's content keys, and
                        // which devices read it is its workspace's governance,
                        // not any one write: take part in it for a write that
                        // arrived here even when this page cannot decide it
                        // yet. A record whose every write is held would
                        // otherwise never ask for the keys the writes wait on.
                        self.open_channel(&route);
                        let reason = hold_reason(&route, envelope);
                        self.hold_document(
                            &route,
                            OpKey::of(envelope),
                            body,
                            author,
                            retained,
                            reason,
                        );
                        return;
                    }
                    Err(error) => {
                        self.refused = (self.refused.0.saturating_add(1), error.clone());
                        self.emit(&PeerEvent::DocumentRejected(error));
                        return;
                    }
                }
                self.open_channel(&route);
                match self.verdict(&route, envelope, retained) {
                    Ok(None) => self.open_document(&body, author),
                    Ok(Some(reason)) => {
                        let key = OpKey::of(envelope);
                        self.hold_document(&route, key, body, author, retained, reason);
                    }
                    Err(error) => {
                        self.refused = (self.refused.0.saturating_add(1), error.to_owned());
                        self.emit(&PeerEvent::DocumentRejected(error.to_owned()));
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether the host answered `Ready` for `route`'s workspace: it sent the
    /// governance it holds first, so the page's mirror of its channels is
    /// what the workspace decided.
    fn governance_known(&self, route: &DocumentRoute) -> bool {
        route.workspace_id().is_ok_and(|workspace_id| {
            lock(&self.shared).status.workspaces.get(&workspace_id) == Some(&true)
        })
    }

    /// Decide again the documents held for the routes of `workspace_id`,
    /// now that its governance is known: cleartext its channels admit
    /// opens, cleartext they require sealed is refused, the rest waits on.
    fn decide_held(&mut self, workspace_id: u64) {
        for route in self.held_routes(workspace_id) {
            self.decide_route(&route, None);
        }
    }

    /// What the write `envelope` waits for once its route's keys and the
    /// channel's governance admit it: `None` when it opens now, or the error
    /// for cleartext on a channel that requires it sealed.
    fn verdict(
        &self,
        route: &DocumentRoute,
        envelope: &collab_types::SignedOp,
        retained: bool,
    ) -> Result<Option<HoldReason>, &'static str> {
        let Some(metadata) = &envelope.encryption else {
            let admission = self.channels.admission(
                route,
                (
                    self.governance_known(route),
                    self.ciphers.contains_key(route),
                ),
                retained,
                None,
            );
            return match admission.admits_cleartext() {
                Some(true) => Ok(None),
                Some(false) => Err("cleartext on a channel that requires it sealed"),
                None => Ok(Some(HoldReason::Governance)),
            };
        };
        Ok(self
            .ciphers
            .get(route)
            .is_none_or(|cipher| !cipher.holds_epoch(metadata.epoch))
            .then_some(HoldReason::Keys))
    }

    /// Take every document held for `route` and decide it again: what still
    /// cannot be decided is held again with the reason it has now.
    fn decide_route(&mut self, route: &DocumentRoute, client: Option<&Link>) {
        let held = self
            .held_inbound
            .get_mut(route)
            .map(InboundHold::take)
            .unwrap_or_default();
        for (_, frame) in held {
            self.held_body(frame.body, frame.author, frame.retained);
        }
        self.recount(route);
        self.replay_owed(route, client);
    }

    /// Decide again what the surfaces that arrived were waited for: every
    /// document held for an app version this page has now.
    fn decide_held_surfaces(&mut self, client: &Link) {
        let routes: Vec<DocumentRoute> = self
            .held_inbound
            .iter()
            .filter(|(_, held)| held.held().any(|(_, frame)| frame.waits_for_surface()))
            .map(|(route, _)| route.clone())
            .collect();
        for route in routes {
            self.decide_route(&route, Some(client));
        }
    }

    /// Ask the workspaces for the app surfaces this page holds documents for
    /// and does not have: the workspace keeps every version its history was
    /// written with, and a member that wrote under one answers with it.
    fn ask_surfaces(&mut self, client: &Link) {
        let mut wants: BTreeMap<[u8; 32], (u64, String, String)> = BTreeMap::new();
        for (route, held) in &self.held_inbound {
            let Ok(workspace_id) = route.workspace_id() else {
                continue;
            };
            for (_, frame) in held.held() {
                if let HoldReason::Surface(surface) = frame.reason {
                    wants.entry(surface).or_insert_with(|| {
                        (workspace_id, route.app_id.clone(), route.cluster_id.clone())
                    });
                }
            }
        }
        let device = self.config.cert.device_public_key;
        let now = tilo_clock::unix_now_ms();
        for (surface, (workspace_id, app_id, cluster_id)) in wants {
            if crate::surfaces::holds(&app_id, &surface) {
                continue;
            }
            let Some(request) = self.surface_wants.ask(
                &device,
                (),
                (&app_id, &cluster_id),
                &hex::encode(surface),
                now,
            ) else {
                continue;
            };
            self.want_content(workspace_id, request, Some(client));
        }
    }

    /// Set `route`'s status from what it holds now, by the reason each
    /// document waits.
    fn recount(&self, route: &DocumentRoute) {
        let mut waiting = (0_usize, 0_usize, 0_usize);
        if let Some(held) = self.held_inbound.get(route) {
            for (_, frame) in held.held() {
                let count = match frame.reason {
                    HoldReason::Keys => &mut waiting.0,
                    HoldReason::Surface(_) => &mut waiting.1,
                    HoldReason::Governance => &mut waiting.2,
                };
                *count = count.saturating_add(1);
            }
        }
        self.update(|status| {
            let status = status.routes.entry(route.clone()).or_default();
            (
                status.awaiting_keys,
                status.awaiting_surface,
                status.awaiting_governance,
            ) = waiting;
        });
    }

    /// The host answered `Ready` for `workspace_id`: what the replay brought
    /// before it and a full hold let go of is paged again, where the route
    /// now opens it.
    fn admitted(&mut self, workspace_id: u64, client: &Link) {
        self.check_admission();
        // What this session could not ask before the workspace admitted it.
        let asked = self
            .channels
            .ask_now(workspace_id, tilo_clock::unix_now_ms());
        self.absorb(asked);
        for route in self.held_routes(workspace_id) {
            let opens = self.ciphers.contains_key(&route)
                || self
                    .channels
                    // What the host replays is the record's history, whose
                    // cleartext base opens where a live write would wait.
                    .admission(&route, (true, false), true, None)
                    .admits_cleartext()
                    == Some(true);
            if opens {
                self.replay_owed(&route, Some(client));
            }
        }
    }

    /// The routes of `workspace_id` holding documents, or owing them.
    fn held_routes(&self, workspace_id: u64) -> Vec<DocumentRoute> {
        self.held_inbound
            .keys()
            .filter(|route| route.workspace_id().ok() == Some(workspace_id))
            .cloned()
            .collect()
    }

    /// Hold a document until the reason it names goes away: its route's keys,
    /// the app surface it was written with, or its workspace's governance.
    fn hold_document(
        &mut self,
        route: &DocumentRoute,
        key: OpKey,
        body: Vec<u8>,
        author: [u8; 32],
        retained: bool,
        reason: HoldReason,
    ) {
        let held = self
            .held_inbound
            .entry(route.clone())
            .or_insert_with(|| InboundHold::new(MAX_HELD_DOCUMENTS));
        held.hold(
            key,
            HeldFrame {
                body,
                author,
                retained,
                reason,
            },
        );
        self.recount(route);
    }

    /// Ask the host to replay the records whose full holds let documents
    /// go, now that `route` can open them.
    fn replay_owed(&mut self, route: &DocumentRoute, client: Option<&Link>) {
        let owed = self
            .held_inbound
            .get(route)
            .is_some_and(|held| !held.owed_pages().is_empty());
        let (Some(client), Ok(workspace_id)) = (client, route.workspace_id()) else {
            return;
        };
        if !owed {
            return;
        }
        let channel = neighbour_wire::app_document_channel(&route.app_id, &route.record);
        if client
            .send(Envelope::OpenRecord {
                workspace_id,
                channel,
            })
            .is_ok()
        {
            self.owed_replays.insert(channel, route.clone());
        }
    }

    fn open_document(&mut self, body: &[u8], author: [u8; 32]) {
        match open_frame(body, author, &mut self.ciphers).and_then(|document| match document {
            Some(document) => self.reassembly.accept(document),
            None => Ok(None),
        }) {
            Ok(Some(document)) => {
                let from: [u8; 4] = author
                    .get(..4)
                    .and_then(|b| b.try_into().ok())
                    .unwrap_or_default();
                let opened = self
                    .opened
                    .entry((document.route.clone(), from))
                    .or_default();
                *opened = opened.saturating_add(1);
                let route = document.route.clone();
                if let Some(held) = self.held_inbound.get_mut(&route) {
                    held.settle(&OpKey::of(&document.envelope));
                }
                let entries = document.entries().to_vec();
                self.emit(&PeerEvent::Document(Box::new(document)));
                for entry in &entries {
                    if self.received.len() < MAX_DELIVERED_ENTRIES {
                        self.received.insert(*entry);
                    }
                }
                self.note_delivered(&route, &entries);
            }
            Ok(None) => {}
            Err(error) => {
                self.refused = (self.refused.0.saturating_add(1), error.to_string());
                self.emit(&PeerEvent::DocumentRejected(error.to_string()));
            }
        }
    }

    fn absorb(&mut self, changes: crate::channels::Changes) {
        self.channel_changes.extend(changes);
    }

    /// Store channel state and held edits when either changed, so a
    /// reloaded page keeps its access and what it wrote before the keys.
    fn persist_channels(&mut self, held_changed: bool) {
        let Some(channels) = self.channels.export_if_changed().or_else(|| {
            held_changed.then(|| {
                self.channels.mark_changed();
                self.channels.export_if_changed().unwrap_or_default()
            })
        }) else {
            return;
        };
        let saved = SavedPeerChannels {
            channels,
            requests: self.channels.pending_requests(),
            held: self
                .held_outbound
                .iter()
                .map(|(route, frames)| (route.clone(), frames.clone()))
                .collect(),
        };
        let result = saved
            .encode()
            .and_then(|bytes| self.store.save_channels(&bytes));
        if let Err(error) = result {
            self.fail_on(FailureSubject::Storage, "storage-transaction-failed", error);
        }
    }

    /// Act on channel changes: install new keys, open what waited for them,
    /// seal and commit what was held, and sign and send governance. Returns
    /// `false` when the session could not take an envelope.
    async fn settle_channels(&mut self, client: Option<&Link>) -> bool {
        let mut delivered = true;
        loop {
            let changes = std::mem::take(&mut self.channel_changes);
            if changes.keys.is_empty()
                && changes.outgoing.is_empty()
                && self.channel_changes.is_empty()
            {
                self.persist_channels(false);
                let mut channels = self.replicas.describe();
                channels.push('\n');
                channels.push_str(&self.channels.describe());
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
                if lock(&self.shared).status.channels != channels {
                    self.update(|status| status.channels = channels);
                }
                return delivered;
            }
            let held_before = self.held_outbound.len();
            for (route, keys) in changes.keys {
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
            for outgoing in changes.outgoing {
                let Some(client) = client else {
                    // Offline: overdue requests are sent again after the
                    // reconnect; acknowledgements go with the next frame.
                    continue;
                };
                let crate::channels::Outgoing::Request { workspace_id, .. } = &outgoing;
                if lock(&self.shared).status.workspaces.get(workspace_id) != Some(&true) {
                    // The host takes a workspace's governance only from a session
                    // it admitted to the workspace: this is asked again the
                    // moment it does (`admitted`).
                    continue;
                }
                let signer = Arc::clone(&self.signer);
                let device = self.config.cert.device_public_key;
                for envelope in sign_governance(signer.as_ref(), device, outgoing).await {
                    delivered &= client.send(envelope).is_ok();
                }
            }
        }
    }
}

/// A page's channel state and the edits it holds for the durable store,
/// beside its [`PeerRecord`].
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SavedPeerChannels {
    channels: crate::channels::SavedChannels,
    requests: Vec<collab_types::MlsAccessRequestV1>,
    held: Vec<(DocumentRoute, Vec<Vec<u8>>)>,
}

/// Tags stored channel state of this format: state of an earlier format
/// does not open, and the page starts over.
const SAVED_CHANNELS_TAG: &[u8] = b"tilo.browser-channels.v4";

impl SavedPeerChannels {
    fn encode(&self) -> Result<Vec<u8>, String> {
        let mut bytes = SAVED_CHANNELS_TAG.to_vec();
        postcard::to_io(self, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes.strip_prefix(SAVED_CHANNELS_TAG)?).ok()
    }
}

/// Sign a governance message with the device key and wrap it for the host,
/// which retains it for the workspace's participants. An access request goes
/// routed, so they decide Read on the record.
async fn sign_governance(
    signer: &dyn DeviceSigner,
    device: [u8; 32],
    outgoing: crate::channels::Outgoing,
) -> Vec<Envelope> {
    let Some(messages) = signed_governance(signer, outgoing).await else {
        return Vec::new();
    };
    messages
        .into_iter()
        .filter_map(|(workspace_id, message)| governance_envelope(device, workspace_id, &message))
        .collect()
}

/// `outgoing` signed, as the messages that carry it and their workspace.
async fn signed_governance(
    signer: &dyn DeviceSigner,
    outgoing: crate::channels::Outgoing,
) -> Option<Vec<(u64, neighbour_wire::NeighbourMsg)>> {
    let crate::channels::Outgoing::Request {
        workspace_id,
        cluster_id,
        mut request,
        route,
    } = outgoing;
    let signature = signer.sign(request.signing_bytes().ok()?).await.ok()?;
    request.signature = collab_types::Signature::from_bytes(signature);
    // A group the unit's own is asked for with the record it carries, so its
    // participants decide Read on it; the workspace's scope names no record.
    let routed = match route {
        Some(route) => neighbour_wire::NeighbourMsg::MlsRoutedAccessRequest {
            room: cluster_id,
            request: Box::new(request),
            route: Box::new(route),
            delivery: rand::random(),
        },
        None => neighbour_wire::NeighbourMsg::MlsAccessRequest {
            workspace: cluster_id,
            request: Box::new(request),
            delivery: rand::random(),
        },
    };
    Some(vec![(workspace_id, routed)])
}

/// `message`, signed governance of `workspace_id`, wrapped for the host.
fn governance_envelope(
    device: [u8; 32],
    workspace_id: u64,
    message: &neighbour_wire::NeighbourMsg,
) -> Option<Envelope> {
    let (channel, _) = neighbour_wire::retention(message, false)?;
    Some(Envelope::Delta {
        workspace_id,
        author_device_public_key: device,
        authored_at_ns: tilo_clock::unix_now_ns(),
        delta: CoeditPayload::Checkpoint {
            channel,
            body: postcard::to_allocvec(message).ok()?,
        }
        .encode()
        .ok()?,
        signature: [0; 64],
    })
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

/// Whether `reason` is the host answering "no" about a change it read, as
/// [`PeerHandle::barrier_then`] carries it. A lost, cancelled or overloaded
/// session is not an answer: only that leaves the change's outcome unknown.
pub fn is_refusal(reason: &str) -> bool {
    reason.trim_start().starts_with(REFUSAL)
}

/// What this session reports when the host would not take a change it read.
fn refusal_text(reason: &str) -> String {
    format!("{REFUSAL}: {}", reason.trim())
}

/// The phrasing this session uses for the one answer that is a refusal.
const REFUSAL: &str = "the workspace refused the change";

/// This device's certificate for a workspace's other members. A native shell
/// accepts a member's edits only from a device whose certificate it holds,
/// and a browser has no direct neighbours to hand it over, so it goes through
/// the host, which authenticates the author and keeps it for later joiners.
fn identity_hello(workspace_id: u64, cert: &DeviceCert) -> Option<Envelope> {
    let hello = neighbour_wire::NeighbourMsg::IdentityHello {
        protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
        cert: Box::new(cert.clone()),
        revocations: Vec::new(),
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

/// Resolves when a session is due for renewal `after` from now; never for a
/// session with no fixed end.
async fn renewal(after: Option<Duration>) {
    match after {
        Some(after) => n0_future::time::sleep(after).await,
        None => std::future::pending().await,
    }
}

/// How long from `now_ns` until a session that ends at `expires_at_ns` is
/// renewed: [`RENEW_AHEAD`] before its end, at once when that has passed.
fn renew_after(expires_at_ns: u64, now_ns: u64) -> Duration {
    Duration::from_nanos(expires_at_ns.saturating_sub(now_ns)).saturating_sub(RENEW_AHEAD)
}

/// The reply `frame` carries, or the refusal a page shows when it does not
/// decode.
fn answered<T: serde::de::DeserializeOwned>(frame: &[u8]) -> Result<T, String> {
    postcard::from_bytes(frame).map_err(|error| format!("the answer does not decode: {error}"))
}

/// The session to the host, over whichever carrier reaches it.
#[allow(
    clippy::large_enum_variant,
    reason = "one link per session, never moved in bulk"
)]
enum Link {
    Https(HttpsLink),
    #[cfg(feature = "iroh")]
    Iroh {
        client: tilo_relay_session::RelayClient,
    },
}

impl Link {
    /// The session whose fetch proxy this link's host serves; `None` when
    /// it serves none, as a direct Iroh link never does.
    fn fetch_proxy_session(&self) -> Option<String> {
        match self {
            Self::Https(link) => link.fetch_proxy().then(|| link.session().to_owned()),
            #[cfg(feature = "iroh")]
            Self::Iroh { .. } => None,
        }
    }

    #[cfg_attr(
        not(feature = "iroh"),
        allow(
            clippy::unnecessary_wraps,
            reason = "an iroh link has no session token"
        )
    )]
    fn session_token(&self) -> Option<String> {
        match self {
            Self::Https(link) => Some(link.session().to_owned()),
            #[cfg(feature = "iroh")]
            Self::Iroh { .. } => None,
        }
    }

    fn resumable(&self) -> bool {
        match self {
            Self::Https(link) => link.resumable(),
            #[cfg(feature = "iroh")]
            Self::Iroh { .. } => false,
        }
    }

    fn compacts(&self) -> bool {
        match self {
            Self::Https(link) => link.compacts(),
            #[cfg(feature = "iroh")]
            Self::Iroh { .. } => false,
        }
    }

    fn send(&self, envelope: Envelope) -> Result<(), Box<Envelope>> {
        match self {
            Self::Https(link) => link.send(envelope),
            #[cfg(feature = "iroh")]
            Self::Iroh { client, .. } => client.send(envelope),
        }
    }

    /// How long until this session is renewed, [`RENEW_AHEAD`] of the end
    /// its host set; `None` for a carrier whose session has no fixed end.
    #[cfg_attr(
        not(feature = "iroh"),
        allow(
            clippy::unnecessary_wraps,
            reason = "the iroh carrier's sessions have no fixed end"
        )
    )]
    fn renew_after(&self, now_ns: u64) -> Option<Duration> {
        match self {
            Self::Https(link) => Some(renew_after(link.expires_at_ns(), now_ns)),
            #[cfg(feature = "iroh")]
            Self::Iroh { .. } => None,
        }
    }

    async fn recv(&mut self) -> Option<Envelope> {
        match self {
            Self::Https(link) => link.recv().await,
            #[cfg(feature = "iroh")]
            Self::Iroh { client, .. } => client.recv().await,
        }
    }

    #[cfg_attr(
        not(feature = "iroh"),
        allow(clippy::unused_async, reason = "the iroh carrier awaits its close")
    )]
    async fn close(self) {
        match self {
            Self::Https(link) => link.close(),
            #[cfg(feature = "iroh")]
            Self::Iroh { client, .. } => client.close().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_pending_requests_and_held_edits_survive_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let route = DocumentRoute {
            app_id: "so.tilo.notes".into(),
            cluster_id: "workspace".into(),
            record: "note".into(),
        };
        let request = collab_types::MlsAccessRequestV1::unsigned(
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record),
            collab_types::Ed25519PublicKey::from_bytes([3; 32]),
            vec![1, 2, 3],
            7,
        );
        let saved = SavedPeerChannels {
            channels: crate::channels::SavedChannels::default(),
            requests: vec![request.clone()],
            held: vec![(route, vec![b"before the reload".to_vec()])],
        };
        let decoded =
            SavedPeerChannels::decode(&saved.encode()?).ok_or("stored state did not decode")?;
        if postcard::to_stdvec(&decoded.requests)? != postcard::to_stdvec(&vec![request])? {
            return Err("the pending access request changed on reopen".into());
        }
        if decoded.held != saved.held {
            return Err("the held edit changed on reopen".into());
        }
        Ok(())
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
            personal_rooms: BTreeSet::new(),
            room_members: BTreeMap::new(),
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
    fn a_refusal_that_says_why_decides_and_a_proxys_page_never_counts() {
        use tilo_relay_protocol::web::SessionRefusal;
        let code = |error| match refusal(&error) {
            Refusal::Final(code, _) => code,
            Refusal::Unexplained => "unexplained",
            Refusal::Reachability => "reachability",
        };
        assert_eq!(
            code(LinkError::Denied(SessionRefusal::NotAdmitted)),
            "not-admitted"
        );
        assert_eq!(
            code(LinkError::Denied(SessionRefusal::DeviceRevoked)),
            "device-revoked"
        );
        assert_eq!(
            code(LinkError::Denied(SessionRefusal::CertificateInvalid)),
            "certificate-rejected"
        );
        assert_eq!(code(LinkError::HostChanged), "host-changed");
        assert_eq!(
            code(LinkError::Denied(SessionRefusal::Handshake)),
            "reachability"
        );
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

    /// A session is renewed a minute before the host ends it, so a page never
    /// learns of the end from a refused poll; one already that close renews
    /// at once.
    #[test]
    fn a_session_renews_ahead_of_its_end() {
        let second = 1_000_000_000_u64;
        let now = 1_000 * second;
        assert_eq!(
            super::renew_after(now + 15 * 60 * second, now),
            Duration::from_secs(14 * 60)
        );
        assert_eq!(super::renew_after(now + 30 * second, now), Duration::ZERO);
        assert_eq!(super::renew_after(now - second, now), Duration::ZERO);
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
            "plane 7: not joined — no live plane was reached; asking again at 12000"
        );
        assert_eq!(planes.missed(7), 1);
        assert_eq!(planes.missed(7), plane::MISSES_BEFORE_NOTE);
        planes.refused(7, 60_000);
        assert_eq!(
            planes.describe(),
            "plane 7: not joined — the workspace does not admit this page's plane; asking again \
             at 60000"
        );
        assert_eq!(planes.missed(7), 1, "an answer starts the count again");
    }
}

#[cfg(all(test, feature = "iroh"))]
mod iroh_connect_tests;
