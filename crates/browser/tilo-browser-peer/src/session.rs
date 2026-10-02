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
//!   backoff, re-subscribes every joined room (the host replays retained
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
use tilo_relay_client::{CoeditPayload, DeviceSigner};
use tilo_relay_protocol::Envelope;
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

use crate::carrier::{HttpClient, HttpsLink, LinkError};
use crate::custody::{OutboxEntry, PeerRecord, PeerStore};
use crate::document::{
    Ciphers, ContentCipher, DocumentRoute, OpenedDocument, Reassembly, open_frame, sign_frame,
};

/// Longest wait between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Keep-alive interval on an idle session.
const PING_INTERVAL: Duration = Duration::from_secs(15);
/// How often overdue channel access requests are sent again.
const CHANNEL_TICK: Duration = Duration::from_secs(1);
/// Sealed documents held per route while this device waits for its keys.
const MAX_HELD_DOCUMENTS: usize = 4096;
/// Consecutive refused handshakes after which the host is taken to have
/// rejected this device (revoked or unknown) rather than to be unreachable.
const REFUSALS_BEFORE_FAILING: u32 = 3;
/// How long a connected session waits for the host to admit its rooms before
/// it reports `admission-timeout`. Checked on the keep-alive tick.
const ADMISSION_TIMEOUT_MS: u64 = 30_000;
/// Failures kept at once; the oldest goes first.
const MAX_FAILURES: usize = 16;

/// How to reach the user's host.
#[derive(Clone)]
pub enum Host {
    /// Its HTTPS origin, through this client (the page's `fetch`).
    Https(Arc<dyn HttpClient>),
    /// Its iroh endpoint (native peers).
    #[cfg(feature = "iroh")]
    Iroh {
        /// The host's relay endpoint.
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
    /// `certificate-expired`, `certificate-rejected`, `identity-revoked`,
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
    /// Admission of the joined rooms (`refresh-checkpoint`).
    Admission,
    /// One desktop surface's renderer (`retry-desktop`).
    Surface(String),
    /// An asset one desktop surface could not load (`retry-asset`).
    Asset {
        /// The surface.
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
    /// Commits the host acknowledged this session.
    pub durable_operations: u64,
    /// Host order of the latest acknowledged commit.
    pub last_seq: Option<u64>,
    /// Local store state, as reported with [`PeerHandle::set_local_state`].
    pub local: Option<LocalState>,
}

/// Everything a snapshot is derived from.
#[derive(Clone, Debug)]
pub struct PeerStatus {
    /// Transport phase.
    pub phase: TransportPhase,
    /// Whether the current connection completed the handshake.
    pub authenticated: bool,
    /// Consecutive failed attempts.
    pub attempt: u32,
    /// When the next attempt runs (ms since the epoch).
    pub next_attempt_at_ms: Option<u64>,
    /// Last envelope from the host (ms since the epoch).
    pub last_contact_at_ms: Option<u64>,
    /// Joined rooms and whether the host answered `Ready`.
    pub rooms: BTreeMap<u64, bool>,
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
    /// This page's channel state, one line per room, certificate and
    /// route, for diagnostics.
    pub channels: String,
}

/// Something the session produced for its consumers.
#[derive(Clone, Debug)]
pub enum PeerEvent {
    /// A verified document frame from another device (or this device's own,
    /// replayed from the host after a reconnect).
    Document(Box<OpenedDocument>),
    /// A document that failed verification and was dropped.
    DocumentRejected(String),
    /// The host admitted a room subscription.
    Ready {
        /// The room's workspace.
        workspace_id: u64,
    },
    /// A commit became durable on the host.
    Committed {
        /// Its route.
        route: DocumentRoute,
        /// Host order.
        seq: u64,
    },
    /// A live frame another member published, opened with its record's
    /// channel keys.
    Live {
        /// The room's workspace.
        workspace_id: u64,
        /// Host-stamped publisher.
        from: [u8; 32],
        /// Canonical live frame.
        frame: Vec<u8>,
    },
    /// A remote-action frame (`tilo_actions::remote::RemoteActionFrame`
    /// bytes) another device of the room addressed to this one: a request
    /// to answer, or the answer to one this device sent.
    RemoteAction {
        /// The room's workspace.
        workspace_id: u64,
        /// Host-stamped sending device.
        from: [u8; 32],
        /// Opaque frame.
        frame: Vec<u8>,
    },
    /// The host could not deliver a remote-action frame this device sent.
    RemoteActionUndeliverable {
        /// The room's workspace.
        workspace_id: u64,
        /// The addressed device.
        to: [u8; 32],
        /// Why.
        reason: String,
    },
    /// Live membership changed.
    LiveMember {
        /// The room's workspace.
        workspace_id: u64,
        /// The member.
        device: [u8; 32],
        /// Joined or left.
        active: bool,
    },
    /// The host holds a long sealed history of `route` it cannot read and
    /// asks this device for a snapshot. Answer with
    /// [`PeerHandle::compact`] after applying every [`PeerEvent::Document`]
    /// received before this event: the snapshot is taken to contain them.
    /// A consumer without the document ignores it.
    CompactionRequested {
        /// The route.
        route: DocumentRoute,
        /// Echo in [`PeerHandle::compact`].
        request: [u8; 16],
    },
    /// What the host said about this device's calls.
    Voice(crate::voice::VoiceEvent),
}

enum Command {
    Join(u64),
    Publish(DocumentRoute, Vec<Vec<u8>>),
    InstallCipher(DocumentRoute, Box<dyn ContentCipher>),
    OpenRoute(DocumentRoute),
    PinGovernor(String, [u8; 32]),
    LiveJoin(u64),
    Live(u64, Vec<u8>),
    SealLive(u64, Vec<u8>, LiveCompletion),
    OpenLive(u64, Vec<u8>, LiveCompletion),
    RemoteAction(u64, [u8; 32], Vec<u8>),
    Barrier(Completion),
    LiveGrant(u64, Option<String>, GrantCompletion),
    VoiceCalls(u64),
    VoiceCall(u64, [u8; 16], tilo_relay_protocol::voice::CallOperation),
    VoiceJoin(u64, [u8; 32]),
    VoiceLeave(u64),
    LegacyInput(
        tilo_relay_protocol::legacy_window::InputRequest,
        InputCompletion,
    ),
    Rooms(tilo_relay_protocol::rooms::RoomsRequest, RoomsCompletion),
    Compact([u8; 16], Vec<u8>),
    Reconnect,
    Refresh,
    Close,
}

type Wake = Arc<dyn Fn() + Send + Sync>;
/// Called once with the outcome of a [`PeerHandle::barrier_then`].
pub type Completion = Box<dyn FnOnce(Result<(), String>) + Send>;
/// Called once with a live grant, see [`PeerHandle::live_grant_then`].
pub type GrantCompletion =
    Box<dyn FnOnce(Result<tilo_relay_protocol::web::LiveGrant, String>) + Send>;
/// Called once with a live frame sealed or opened with its record's channel
/// keys, or `None` when this device holds none; see
/// [`PeerHandle::seal_live_then`] and [`PeerHandle::open_live_then`].
pub type LiveCompletion = Box<dyn FnOnce(Option<Vec<u8>>) + Send>;
/// Called once with whether the host took a legacy window's input, see
/// [`PeerHandle::legacy_input_then`].
pub type InputCompletion = Box<dyn FnOnce(Result<bool, String>) + Send>;
/// Called once with the host's answer, see [`PeerHandle::rooms_then`].
pub type RoomsCompletion =
    Box<dyn FnOnce(Result<tilo_relay_protocol::rooms::RoomsReply, String>) + Send>;

type Queue = Arc<Mutex<VecDeque<PeerEvent>>>;

struct Shared {
    status: PeerStatus,
    queues: Vec<std::sync::Weak<Mutex<VecDeque<PeerEvent>>>>,
    wakes: Vec<Wake>,
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
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect()
    }
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

/// Where the joined rooms' admission stands `now_ms`, for a subscription
/// sent at `since_ms`.
fn admission_state(status: &PeerStatus, since_ms: Option<u64>, now_ms: u64) -> Admission {
    if status.rooms.values().all(|ready| *ready) {
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
                rooms: BTreeMap::new(),
                routes: BTreeMap::new(),
                failures: Vec::new(),
                outgoing_pending: 0,
                device: config.cert.device_public_key,
                generation: 0,
                channels: String::new(),
            },
            queues: Vec::new(),
            wakes: Vec::new(),
        }));
        let events: Queue = Arc::new(Mutex::new(VecDeque::new()));
        lock(&shared).queues.push(Arc::downgrade(&events));
        let record = store.load().unwrap_or_default();
        let mut channels = crate::channels::Channels::new(
            config.cert.device_public_key,
            config.cert.user_public_key,
        );
        let saved = store
            .load_channels()
            .and_then(|bytes| postcard::from_bytes::<SavedPeerChannels>(&bytes).ok())
            .unwrap_or_default();
        channels.restore(saved.channels);
        let task = Task {
            config,
            signer,
            store,
            shared: shared.clone(),
            rooms: BTreeSet::new(),
            live: BTreeSet::new(),
            outbox: record.outbox.iter().cloned().collect(),
            record,
            ciphers: Ciphers::new(),
            channels,
            channel_changes: crate::channels::Changes::default(),
            opened: BTreeMap::new(),
            refused: (0, String::new()),
            held_inbound: BTreeMap::new(),
            held_outbound: saved.held.into_iter().collect(),
            reassembly: Reassembly::default(),
            positions: BTreeMap::new(),
            doc_seq: 0,
            barriers: Vec::new(),
            held_barriers: Vec::new(),
            admission_since: None,
            delivered: BTreeMap::new(),
            received: BTreeSet::new(),
            compactions: BTreeMap::new(),
            voice: crate::voice::Participations::default(),
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

    fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "peer session is closed".to_string())
    }

    /// Subscribe to a room.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn join(&self, workspace_id: u64) -> Result<(), String> {
        self.send(Command::Join(workspace_id))
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
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn compact(&self, request: [u8; 16], snapshot: Vec<u8>) -> Result<(), String> {
        self.send(Command::Compact(request, snapshot))
    }

    /// Seal `route`'s documents with `cipher` from now on.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn install_cipher(
        &self,
        route: DocumentRoute,
        cipher: Box<dyn ContentCipher>,
    ) -> Result<(), String> {
        self.send(Command::InstallCipher(route, cipher))
    }

    /// Take part in `route`'s encrypted channel: when a steward governs it,
    /// ask for access so documents on it can be opened and sealed. Routes
    /// are also opened by publishing on them or receiving their documents.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn open_route(&self, route: DocumentRoute) -> Result<(), String> {
        self.send(Command::OpenRoute(route))
    }

    /// Name the device that governs `room` (a room id), as the room itself
    /// says: its channels are stewarded by that device only, whatever a
    /// genesis frame claims.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn pin_governor(&self, room: &str, governor: [u8; 32]) -> Result<(), String> {
        self.send(Command::PinGovernor(room.to_owned(), governor))
    }

    /// Join a room's live plane (after [`Self::join`]).
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn live_join(&self, workspace_id: u64) -> Result<(), String> {
        self.send(Command::LiveJoin(workspace_id))
    }

    /// Publish one canonical live frame, sealed with the channel keys of the
    /// record it names. Dropped while disconnected (live state is never
    /// queued), and dropped when it names no record or this device holds
    /// none of that record's keys: a live frame never leaves in the clear.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn publish_live(&self, workspace_id: u64, frame: Vec<u8>) -> Result<(), String> {
        self.send(Command::Live(workspace_id, frame))
    }

    /// Seal one canonical live frame of `workspace_id` with the channel keys
    /// of the record it names, for a carrier other than this session (the
    /// page's live plane); `done` receives the sealed frame, or `None`.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
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
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn open_live_then(
        &self,
        workspace_id: u64,
        frame: Vec<u8>,
        done: LiveCompletion,
    ) -> Result<(), String> {
        self.send(Command::OpenLive(workspace_id, frame, done))
    }

    /// Send one remote-action frame to device `to` of the room (the host's
    /// own device reaches its app-serving plane). Dropped while
    /// disconnected: an action that ran late is worse than one that did not
    /// run, so the caller retries with the same invocation id.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn send_remote_action(
        &self,
        workspace_id: u64,
        to: [u8; 32],
        frame: Vec<u8>,
    ) -> Result<(), String> {
        self.send(Command::RemoteAction(workspace_id, to, frame))
    }

    /// A receipt that completes once every document published before this
    /// call is durable on the host.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn barrier(&self) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
        let (reply, receipt) = std::sync::mpsc::sync_channel(1);
        self.barrier_then(Box::new(move |result| {
            let _ = reply.send(result);
        }))?;
        Ok(receipt)
    }

    /// Call `done` once every document published before this call is
    /// durable on the host, or with the refusal that prevents it.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn barrier_then(&self, done: Completion) -> Result<(), String> {
        self.send(Command::Barrier(done))
    }

    /// Ask the host for a live-plane grant for `workspace_id` over the
    /// current HTTPS session; `done` receives it.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn live_grant_then(&self, workspace_id: u64, done: GrantCompletion) -> Result<(), String> {
        self.send(Command::LiveGrant(workspace_id, None, done))
    }

    /// [`Self::live_grant_then`] for agent `agent_id` this device runs: the
    /// grant publishes under the agent's namespace
    /// ([`tilo_relay_protocol::web::agent_publisher`]).
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn agent_live_grant_then(
        &self,
        workspace_id: u64,
        agent_id: String,
        done: GrantCompletion,
    ) -> Result<(), String> {
        self.send(Command::LiveGrant(workspace_id, Some(agent_id), done))
    }

    /// Ask the host for the calls of `workspace_id`; the answer arrives as
    /// [`VoiceEvent::Calls`](crate::voice::VoiceEvent::Calls).
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn voice_calls(&self, workspace_id: u64) -> Result<(), String> {
        self.send(Command::VoiceCalls(workspace_id))
    }

    /// Start, end or moderate a call of `workspace_id`; the answer arrives
    /// as [`VoiceEvent::CallResult`](crate::voice::VoiceEvent::CallResult)
    /// for `request`.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn voice_call(
        &self,
        workspace_id: u64,
        request: [u8; 16],
        operation: tilo_relay_protocol::voice::CallOperation,
    ) -> Result<(), String> {
        self.send(Command::VoiceCall(workspace_id, request, operation))
    }

    /// Take part in `call` of `workspace_id` until [`Self::voice_leave`]:
    /// the session keeps the participation alive across reconnects and
    /// hands on its grants ([`crate::voice`]).
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn voice_join(&self, workspace_id: u64, call: [u8; 32]) -> Result<(), String> {
        self.send(Command::VoiceJoin(workspace_id, call))
    }

    /// Leave the call of `workspace_id`.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn voice_leave(&self, workspace_id: u64) -> Result<(), String> {
        self.send(Command::VoiceLeave(workspace_id))
    }

    /// Send input for a legacy window a member streams
    /// ([`crate::carrier::legacy_input`]) over the current HTTPS session.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn legacy_input_then(
        &self,
        request: tilo_relay_protocol::legacy_window::InputRequest,
        done: InputCompletion,
    ) -> Result<(), String> {
        self.send(Command::LegacyInput(request, done))
    }

    /// Send `request` to the host's room administration over the current
    /// HTTPS session; `done` receives the answer.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn rooms_then(
        &self,
        request: tilo_relay_protocol::rooms::RoomsRequest,
        done: RoomsCompletion,
    ) -> Result<(), String> {
        self.send(Command::Rooms(request, done))
    }

    /// Drop the connection and reconnect now, also after a failure.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn reconnect(&self) -> Result<(), String> {
        self.send(Command::Reconnect)
    }

    /// Ask the host for every joined room's shared state again: the rooms
    /// are subscribed anew, the host replays their retained checkpoints and
    /// answers `Ready`. Offline, this reconnects now.
    ///
    /// # Errors
    ///
    /// Returns an error once the session is closed.
    pub fn refresh(&self) -> Result<(), String> {
        self.send(Command::Refresh)
    }

    /// End the session.
    pub fn close(&self) {
        let _ = self.send(Command::Close);
    }

    fn change(&self, change: impl FnOnce(&mut PeerStatus)) {
        let wakes = {
            let mut shared = lock(&self.shared);
            change(&mut shared.status);
            shared.status.generation = shared.status.generation.wrapping_add(1);
            shared.wakes.clone()
        };
        for wake in &wakes {
            wake();
        }
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
                        at_ms: crate::clock::now_ms(),
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
                    at_ms: crate::clock::now_ms(),
                    subject,
                },
            );
        });
        Ok(())
    }

    /// Clear the failure of `subject`, if any.
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

/// A sealed frame held for its keys, with its authenticated author.
type HeldFrame = (Vec<u8>, [u8; 32]);

struct Task {
    config: PeerConfig,
    signer: Arc<dyn DeviceSigner>,
    store: Arc<dyn PeerStore>,
    shared: Arc<Mutex<Shared>>,
    rooms: BTreeSet<u64>,
    live: BTreeSet<u64>,
    outbox: VecDeque<OutboxEntry>,
    record: PeerRecord,
    ciphers: Ciphers,
    /// This device's membership of the rooms' encrypted channels.
    channels: crate::channels::Channels,
    /// What channel state changed since the session last acted on it.
    channel_changes: crate::channels::Changes,
    /// Documents opened per route, and those refused with the last reason,
    /// for the diagnostics.
    opened: BTreeMap<(DocumentRoute, [u8; 4]), u64>,
    refused: (u64, String),
    /// Sealed documents that arrived before this device held their keys,
    /// with their authenticated authors.
    held_inbound: BTreeMap<DocumentRoute, VecDeque<HeldFrame>>,
    /// Frames published on an encrypted route before this device held its
    /// keys; they are sealed and committed once the keys arrive.
    held_outbound: BTreeMap<DocumentRoute, Vec<Vec<u8>>>,
    reassembly: Reassembly,
    /// The host's last position per room, presented when resubscribing so
    /// the host replays only what came after it. Kept for the session task's
    /// lifetime only: it describes what this page has received.
    positions: BTreeMap<u64, Vec<u8>>,
    doc_seq: u64,
    barriers: Vec<([u8; 16], Completion)>,
    /// Barriers called while frames were held, with the routes those frames
    /// are held on: each waits for them to enter the outbox, then for the
    /// outbox as it stands then.
    held_barriers: Vec<(BTreeSet<DocumentRoute>, Completion)>,
    /// When the rooms not yet admitted were subscribed (ms since the epoch).
    admission_since: Option<u64>,
    /// Retained entry ids of every document delivered or published per
    /// route: what a snapshot of the route contains.
    delivered: BTreeMap<DocumentRoute, BTreeSet<[u8; 32]>>,
    /// Entry ids delivered since this page last subscribed whole. A resumed
    /// subscription keeps them; a whole one asks for them again (a refresh
    /// wants the rooms whole), so it clears them.
    received: BTreeSet<[u8; 32]>,
    /// Open compaction requests: room, route and the entries delivered when
    /// the request arrived.
    compactions: BTreeMap<[u8; 16], Compaction>,
    /// This device's participation in the rooms' calls.
    voice: crate::voice::Participations,
}

/// An open compaction request: room, route and the entries it covers.
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
        if !self.rooms.contains(&workspace_id) || route.workspace_id().ok() != Some(workspace_id) {
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
        let wakes = {
            let mut shared = lock(&self.shared);
            change(&mut shared.status);
            shared.status.generation = shared.status.generation.wrapping_add(1);
            shared.wakes.clone()
        };
        for wake in &wakes {
            wake();
        }
    }

    fn emit(&self, event: &PeerEvent) {
        let wakes = {
            let mut shared = lock(&self.shared);
            shared.queues.retain(|queue| {
                let Some(queue) = queue.upgrade() else {
                    return false;
                };
                queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push_back(event.clone());
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
                    at_ms: crate::clock::now_ms(),
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

    /// Subscribe every joined room again, from now on waiting for its
    /// `Ready`. Returns `false` when the session could not take them.
    fn resubscribe(&self, client: &Link) -> bool {
        let mut sent_ok = true;
        for workspace_id in &self.rooms {
            sent_ok &= client
                .send(Envelope::Subscribe {
                    workspace_id: *workspace_id,
                })
                .is_ok();
        }
        self.update(|status| {
            for ready in status.rooms.values_mut() {
                *ready = false;
            }
        });
        sent_ok
    }

    /// Report `admission-timeout` once the rooms subscribed at
    /// `admission_since` are still not admitted, and clear it once they are.
    /// A timeout is reported once per subscription.
    fn check_admission(&mut self) {
        let status = self.status();
        match admission_state(&status, self.admission_since, crate::clock::now_ms()) {
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
            if self.config.cert.is_expired_at(crate::clock::now_ns()) {
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
            match connect(&self.config, self.signer.as_ref()).await {
                Ok(client) => {
                    attempt = 0;
                    refusals = 0;
                    match self.serve(client, &mut commands).await {
                        Exit::Closed => return self.closed(),
                        Exit::Reconnect => continue,
                        Exit::Lost(reason) => self.fail("transport-ended", reason),
                    }
                }
                Err(error) => {
                    // A host that answered but refused the handshake has
                    // rejected this device; anything else is reachability.
                    refusals = if matches!(error, LinkError::Refused(_) | LinkError::Malformed(_)) {
                        refusals.saturating_add(1)
                    } else {
                        0
                    };
                    if refusals >= REFUSALS_BEFORE_FAILING {
                        self.fail(
                            "certificate-rejected",
                            "The host no longer accepts this browser. Certify it again.",
                        );
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
                status.next_attempt_at_ms = Some(crate::clock::now_ms().saturating_add(delay_ms));
            });
            let sleep = n0_future::time::sleep(delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    biased;
                    command = commands.recv() => match command {
                        None | Some(Command::Close) => return self.closed(),
                        Some(Command::Reconnect) => break,
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

    async fn serve(
        &mut self,
        mut client: Link,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> Exit {
        if !self.resume_session(&client) {
            return Exit::Lost("host session saturated during resume".into());
        }
        self.update(|status| {
            status.phase = TransportPhase::Connected;
            status.authenticated = true;
            status.attempt = 0;
            status.last_contact_at_ms = Some(crate::clock::now_ms());
            status.failures.retain(|failure| {
                !failure.code.starts_with("transport") && !failure.code.starts_with("certificate")
            });
            for ready in status.rooms.values_mut() {
                *ready = false;
            }
        });
        self.admission_since = Some(crate::clock::now_ms());
        let mut ping = n0_future::time::interval(PING_INTERVAL);
        let mut channel_tick = n0_future::time::interval(CHANNEL_TICK);
        let mut heartbeat = n0_future::time::interval(tilo_relay_protocol::voice::HEARTBEAT);
        loop {
            if !self.settle_channels(Some(&client)).await {
                return Exit::Lost("host session saturated".into());
            }
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
                        // A refresh asks for the rooms whole: no position.
                        self.positions.clear();
                        self.received.clear();
                        if !self.resubscribe(&client) {
                            return Exit::Lost("host session saturated".into());
                        }
                        self.admission_since = Some(crate::clock::now_ms());
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
                        let admitted = matches!(envelope, Envelope::Ready { .. });
                        if let Envelope::Ready { workspace_id, .. } = &envelope
                            && !self.announce(&client, *workspace_id)
                        {
                            return Exit::Lost("host session saturated".into());
                        }
                        self.inbound(envelope);
                        if admitted {
                            self.check_admission();
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
                    let changes = self.channels.tick(crate::clock::now_ms());
                    self.absorb(changes);
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

    /// Apply one command; `client` is the live session, if any. Returns
    /// `false` when the session could not take a durable envelope.
    async fn apply(&mut self, command: Command, client: Option<&Link>) -> bool {
        match command {
            Command::Join(workspace_id) => {
                self.rooms.insert(workspace_id);
                if client.is_some() {
                    self.admission_since
                        .get_or_insert_with(crate::clock::now_ms);
                }
                self.update(|status| {
                    status.rooms.entry(workspace_id).or_insert(false);
                });
                self.received.clear();
                client
                    .is_none_or(|client| client.send(self.subscribe(client, workspace_id)).is_ok())
            }
            Command::LiveJoin(workspace_id) => {
                self.live.insert(workspace_id);
                client.is_none_or(|client| client.send(Envelope::LiveJoin { workspace_id }).is_ok())
            }
            command @ (Command::Live(..) | Command::SealLive(..) | Command::OpenLive(..)) => {
                self.apply_live(command, client);
                true
            }
            Command::RemoteAction(workspace_id, to, frame) => {
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
                true
            }
            Command::InstallCipher(route, cipher) => {
                self.ciphers.insert(route, cipher);
                true
            }
            Command::OpenRoute(route) => {
                let changes = self.channels.open(&route, crate::clock::now_ms());
                self.absorb(changes);
                true
            }
            Command::PinGovernor(room, governor) => {
                let changes = self
                    .channels
                    .pin_governor(&room, governor, crate::clock::now_ms());
                self.absorb(changes);
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
            Command::LiveGrant(workspace_id, agent, done) => {
                Self::request_live_grant(workspace_id, agent, done, client);
                true
            }
            Command::LegacyInput(request, done) => {
                Self::request_legacy_input(request, done, client);
                true
            }
            Command::VoiceCalls(workspace_id) => self.voice_calls(workspace_id, client),
            Command::VoiceCall(room, request, op) => self.voice_call(room, request, op, client),
            Command::VoiceJoin(workspace_id, call) => self.voice_join(workspace_id, call, client),
            Command::VoiceLeave(workspace_id) => self.voice_leave(workspace_id, client),
            Command::Rooms(request, done) => {
                Self::request_rooms(request, done, client);
                true
            }
            // Handled by the caller's loop.
            Command::Reconnect | Command::Refresh | Command::Close => true,
        }
    }

    /// Restore this session's rooms, pending commits and call participations.
    fn resume_session(&mut self, client: &Link) -> bool {
        let mut sent_ok = true;
        // A resuming subscription can replay a frame before its covering position.
        // Keep duplicate detection until the host requests a whole subscription.
        if !client.resumable() {
            self.received.clear();
        }
        for workspace_id in &self.rooms {
            sent_ok &= client.send(self.subscribe(client, *workspace_id)).is_ok();
        }
        for workspace_id in &self.live {
            sent_ok &= client
                .send(Envelope::LiveJoin {
                    workspace_id: *workspace_id,
                })
                .is_ok();
        }
        for entry in &self.outbox {
            sent_ok &= client.send(commit(entry, &self.config.cert)).is_ok();
        }
        for join in self.voice.resume() {
            sent_ok &= client.send(join).is_ok();
        }
        sent_ok
    }

    fn voice_calls(&self, workspace_id: u64, client: Option<&Link>) -> bool {
        if let Some(client) = client {
            client.send(Envelope::VoiceCalls { workspace_id }).is_ok()
        } else {
            self.emit(&PeerEvent::Voice(crate::voice::VoiceEvent::Calls {
                workspace_id,
                result: Err("not connected to the host".into()),
            }));
            true
        }
    }

    fn voice_call(
        &self,
        workspace_id: u64,
        request: [u8; 16],
        operation: tilo_relay_protocol::voice::CallOperation,
        client: Option<&Link>,
    ) -> bool {
        if let Some(client) = client {
            client
                .send(Envelope::VoiceCall {
                    workspace_id,
                    request,
                    operation,
                })
                .is_ok()
        } else {
            self.emit(&PeerEvent::Voice(crate::voice::VoiceEvent::CallResult {
                workspace_id,
                request,
                result: Err("not connected to the host".into()),
            }));
            true
        }
    }

    /// Offline, the next connection sends the join.
    fn voice_join(&mut self, workspace_id: u64, call: [u8; 32], client: Option<&Link>) -> bool {
        let out = self.voice.join(workspace_id, call);
        client.is_none_or(|client| {
            out.into_iter()
                .all(|envelope| client.send(envelope).is_ok())
        })
    }

    fn voice_leave(&mut self, workspace_id: u64, client: Option<&Link>) -> bool {
        let out = self.voice.leave(workspace_id);
        match (client, out) {
            (Some(client), Some(leave)) => client.send(leave).is_ok(),
            _ => true,
        }
    }

    fn request_live_grant(
        workspace_id: u64,
        agent: Option<String>,
        done: GrantCompletion,
        client: Option<&Link>,
    ) {
        match client {
            Some(Link::Https(link)) => {
                let http = link.client().clone();
                let session = link.session().to_owned();
                n0_future::task::spawn(async move {
                    let grant = live_grant(http.as_ref(), &session, workspace_id, agent).await;
                    done(grant.map_err(|error| error.to_string()));
                });
            }
            #[cfg(feature = "iroh")]
            Some(Link::Iroh(_)) => done(Err("live grants are served over HTTPS".into())),
            None => done(Err("not connected to the host".into())),
        }
    }

    fn request_legacy_input(
        request: tilo_relay_protocol::legacy_window::InputRequest,
        done: InputCompletion,
        client: Option<&Link>,
    ) {
        match client {
            Some(Link::Https(link)) => {
                let http = link.client().clone();
                let session = link.session().to_owned();
                n0_future::task::spawn(async move {
                    let sent =
                        crate::carrier::legacy_input(http.as_ref(), &session, &request).await;
                    done(sent.map_err(|error| error.to_string()));
                });
            }
            #[cfg(feature = "iroh")]
            Some(Link::Iroh(_)) => done(Err("legacy input is sent over HTTPS".into())),
            None => done(Err("not connected to the host".into())),
        }
    }

    fn request_rooms(
        request: tilo_relay_protocol::rooms::RoomsRequest,
        done: RoomsCompletion,
        client: Option<&Link>,
    ) {
        match client {
            Some(Link::Https(link)) => {
                let http = link.client().clone();
                let session = link.session().to_owned();
                n0_future::task::spawn(async move {
                    let reply = crate::carrier::rooms(http.as_ref(), &session, &request).await;
                    done(reply.map_err(|error| error.to_string()));
                });
            }
            #[cfg(feature = "iroh")]
            Some(Link::Iroh(_)) => done(Err("rooms are administered over HTTPS".into())),
            None => done(Err("not connected to the host".into())),
        }
    }

    /// Send, seal or open one live frame. Live state is lossy: nothing here
    /// is queued, and nothing leaves unsealed.
    fn apply_live(&mut self, command: Command, client: Option<&Link>) {
        match command {
            Command::Live(workspace_id, frame) => {
                if let Some(client) = client
                    && let Some(frame) = self.seal_live(workspace_id, &frame)
                {
                    let _ = client.send(Envelope::Live {
                        workspace_id,
                        from_device_public_key: self.config.cert.device_public_key,
                        frame,
                    });
                }
            }
            Command::SealLive(workspace_id, frame, done) => {
                done(self.seal_live(workspace_id, &frame));
            }
            Command::OpenLive(workspace_id, frame, done) => {
                done(self.open_live(workspace_id, &frame));
            }
            _ => {}
        }
    }

    /// Seal canonical live `frame` of `workspace_id` with the content cipher
    /// of the record it names, or `None`: a frame naming no record, or one
    /// whose record this device holds no keys for, is never sent.
    fn seal_live(&mut self, workspace_id: u64, frame: &[u8]) -> Option<Vec<u8>> {
        let frame = tilo_relay_protocol::live::LiveFrame::decode(frame).ok()?;
        let (_, _, record) = frame.seal_tags()?;
        let cipher = self.ciphers.iter_mut().find_map(|(route, cipher)| {
            let names = tilo_relay_protocol::live::LiveRecord {
                app: route.app_id.clone(),
                record: route.record.clone(),
            };
            (names == *record && route.workspace_id().ok() == Some(workspace_id)).then_some(cipher)
        })?;
        tilo_relay_protocol::live::SealedLiveFrame::seal(
            &frame,
            |artifact, retention, cleartext| cipher.seal(artifact, retention, cleartext),
        )
        .ok()
    }

    /// Open sealed live `frame` of `workspace_id` with the content cipher of
    /// its channel, returning the canonical frame, or `None` when it does not
    /// open with keys this device holds.
    fn open_live(&mut self, workspace_id: u64, frame: &[u8]) -> Option<Vec<u8>> {
        let sealed = tilo_relay_protocol::live::SealedLiveFrame::decode(frame).ok()?;
        let channel = sealed.channel_id();
        let cipher = self.ciphers.iter_mut().find_map(|(route, cipher)| {
            (crate::channels::Channels::channel(route) == channel
                && route.workspace_id().ok() == Some(workspace_id))
            .then_some(cipher)
        })?;
        sealed
            .open(|artifact, metadata, payload| cipher.open(artifact, metadata, payload))
            .ok()?
            .encode()
            .ok()
    }

    async fn publish(
        &mut self,
        route: DocumentRoute,
        frames: Vec<Vec<u8>>,
        client: Option<&Link>,
    ) -> bool {
        let Ok(workspace_id) = route.workspace_id() else {
            self.fail("protocol-rejected", "A document names an invalid room.");
            return true;
        };
        let changes = self.channels.open(&route, crate::clock::now_ms());
        self.absorb(changes);
        if !self.ciphers.contains_key(&route) {
            // Never in the clear: the frames wait for this device's keys.
            self.held_outbound.entry(route).or_default().extend(frames);
            self.persist_channels(true);
            return true;
        }
        let mut delivered = true;
        for frame in frames {
            let now = crate::clock::now_ns();
            self.doc_seq = now.max(self.doc_seq.saturating_add(1));
            let signed = match sign_frame(
                &route,
                frame,
                self.signer.as_ref(),
                self.ciphers.get_mut(&route),
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
        let now = crate::clock::now_ms();
        self.update(|status| status.last_contact_at_ms = Some(now));
        match envelope {
            Envelope::Ready { workspace_id, .. } => {
                self.update(|status| {
                    status.rooms.insert(workspace_id, true);
                });
                self.emit(&PeerEvent::Ready { workspace_id });
            }
            Envelope::NotAuthorised { reason, .. } => {
                self.fail("permission-required", reason);
            }
            Envelope::Delta {
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
                self.inbound_body(body, author_device_public_key);
            }
            Envelope::Position {
                workspace_id,
                position,
            } => {
                self.positions.insert(workspace_id, position);
            }
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
            Envelope::Committed { op, seq, .. } => self.acknowledge(op, Some(seq)),
            Envelope::CommitRefused { op, reason, .. } => {
                self.fail("protocol-rejected", reason);
                self.acknowledge(op, None);
            }
            Envelope::Live {
                workspace_id,
                from_device_public_key,
                frame,
            } => {
                if let Some(frame) = self.open_live(workspace_id, &frame) {
                    self.emit(&PeerEvent::Live {
                        workspace_id,
                        from: from_device_public_key,
                        frame,
                    });
                }
            }
            Envelope::RemoteAction {
                workspace_id,
                from_device_public_key,
                frame,
                ..
            } => self.emit(&PeerEvent::RemoteAction {
                workspace_id,
                from: from_device_public_key,
                frame,
            }),
            Envelope::RemoteActionUndeliverable {
                workspace_id,
                to_device_public_key,
                reason,
            } => self.emit(&PeerEvent::RemoteActionUndeliverable {
                workspace_id,
                to: to_device_public_key,
                reason,
            }),
            Envelope::LiveMember {
                workspace_id,
                device_public_key,
                active,
            } => self.emit(&PeerEvent::LiveMember {
                workspace_id,
                device: device_public_key,
                active,
            }),
            _ => {}
        }
    }

    /// One retained message from the host: channel governance, a member's
    /// certificate, or a document.
    fn inbound_body(&mut self, body: Vec<u8>, author: [u8; 32]) {
        let now = crate::clock::now_ms();
        match postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body) {
            Ok(neighbour_wire::NeighbourMsg::IdentityHello { cert, .. }) => {
                let changes = self.channels.note_cert(author, *cert, now);
                self.absorb(changes);
            }
            Ok(neighbour_wire::NeighbourMsg::MlsGovernance { room, frame, .. }) => {
                let changes = self.channels.receive_frame(author, &room, *frame, now);
                self.absorb(changes);
            }
            Ok(neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                cluster_id,
                record_id,
                envelope,
                ..
            }) => {
                let route = DocumentRoute {
                    app_id,
                    cluster_id,
                    record: record_id,
                };
                // A room the host resumes after it lagged can replay an entry
                // its live deliveries already carried; the page takes it once.
                if self
                    .received
                    .contains(&tilo_relay_protocol::compaction::entry_id(&body))
                {
                    return;
                }
                let changes = self.channels.open(&route, now);
                self.absorb(changes);
                if envelope.encryption.is_some() && !self.ciphers.contains_key(&route) {
                    let held = self.held_inbound.entry(route).or_default();
                    if held.len() >= MAX_HELD_DOCUMENTS {
                        held.pop_front();
                    }
                    held.push_back((body, author));
                    return;
                }
                self.open_document(&body, author);
            }
            _ => {}
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
            held: self
                .held_outbound
                .iter()
                .map(|(route, frames)| (route.clone(), frames.clone()))
                .collect(),
        };
        let result = postcard::to_stdvec(&saved)
            .map_err(|error| error.to_string())
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
            if changes.keys.is_empty() && changes.outgoing.is_empty() {
                self.persist_channels(false);
                let mut channels = self.channels.describe();
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
                    let _ = write!(
                        channels,
                        "\nheld for keys {}/{}: {}",
                        route.app_id,
                        route.record,
                        held.len()
                    );
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
                self.ciphers
                    .insert(route.clone(), Box::new(crate::channels::KeysCipher(keys)));
                for (body, author) in self.held_inbound.remove(&route).unwrap_or_default() {
                    self.open_document(&body, author);
                }
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
                let signer = Arc::clone(&self.signer);
                let device = self.config.cert.device_public_key;
                if let Some(envelope) = sign_governance(signer.as_ref(), device, outgoing).await {
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
    held: Vec<(DocumentRoute, Vec<Vec<u8>>)>,
}

/// Sign a governance message with the device key and wrap it for the host,
/// which retains it for the steward.
async fn sign_governance(
    signer: &dyn DeviceSigner,
    device: [u8; 32],
    outgoing: crate::channels::Outgoing,
) -> Option<Envelope> {
    {
        let (workspace_id, message) = match outgoing {
            crate::channels::Outgoing::Request {
                workspace_id,
                room,
                mut request,
            } => {
                let signature = signer.sign(request.signing_bytes().ok()?).await.ok()?;
                request.signature = collab_types::Signature::from_bytes(signature);
                (
                    workspace_id,
                    neighbour_wire::NeighbourMsg::MlsAccessRequest {
                        room,
                        request: Box::new(request),
                        delivery: rand::random(),
                    },
                )
            }
            crate::channels::Outgoing::Ack {
                workspace_id,
                room,
                mut ack,
            } => {
                let signature = signer.sign(ack.signing_bytes().ok()?).await.ok()?;
                ack.signature = collab_types::Signature::from_bytes(signature);
                (
                    workspace_id,
                    neighbour_wire::NeighbourMsg::MlsGovernanceAck {
                        room,
                        ack: Box::new(ack),
                        delivery: rand::random(),
                    },
                )
            }
        };
        let (channel, _) = neighbour_wire::retention(&message, false)?;
        Some(Envelope::Delta {
            workspace_id,
            author_device_public_key: device,
            authored_at_ns: crate::clock::now_ns(),
            delta: CoeditPayload::Checkpoint {
                channel,
                body: postcard::to_allocvec(&message).ok()?,
            }
            .encode()
            .ok()?,
            signature: [0; 64],
        })
    }
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
    /// (`None`, it can never become durable).
    fn acknowledge(&mut self, op: [u8; 16], seq: Option<u64>) {
        let Some(index) = self.outbox.iter().position(|entry| entry.op == op) else {
            return;
        };
        let Some(entry) = self.outbox.remove(index) else {
            return;
        };
        self.persist();
        let route = entry.route;
        let emitted = route.clone();
        self.update(|status| {
            status.outgoing_pending = status.outgoing_pending.saturating_sub(1);
            let route_status = status.routes.entry(route).or_default();
            route_status.pending_operations = route_status.pending_operations.saturating_sub(1);
            if let Some(seq) = seq {
                route_status.durable_operations = route_status.durable_operations.saturating_add(1);
                route_status.last_seq = Some(seq);
            }
        });
        if let Some(seq) = seq {
            self.emit(&PeerEvent::Committed {
                route: emitted,
                seq,
            });
        }
        let outstanding: BTreeSet<[u8; 16]> = self.outbox.iter().map(|entry| entry.op).collect();
        let result = seq
            .map(|_| ())
            .ok_or_else(|| "the host refused a document change".to_string());
        let (waiting, done): (Vec<_>, Vec<_>) = std::mem::take(&mut self.barriers)
            .into_iter()
            .partition(|(target, _)| outstanding.contains(target));
        self.barriers = waiting;
        for (target, complete) in done {
            complete(if target == op { result.clone() } else { Ok(()) });
        }
    }
}

/// This device's certificate for a room's other members. A native shell
/// accepts a member's edits only from a device whose certificate it holds,
/// and a browser has no gossip neighbours to hand it over, so it goes through
/// the host, which authenticates the author and keeps it for later joiners.
fn identity_hello(workspace_id: u64, cert: &DeviceCert) -> Option<Envelope> {
    let hello = neighbour_wire::NeighbourMsg::IdentityHello {
        cert: Box::new(cert.clone()),
        revocations: Vec::new(),
    };
    let (channel, _) = neighbour_wire::retention(&hello, false)?;
    let body = postcard::to_allocvec(&hello).ok()?;
    Some(Envelope::Delta {
        workspace_id,
        author_device_public_key: cert.device_public_key,
        authored_at_ns: crate::clock::now_ns(),
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
        authored_at_ns: crate::clock::now_ns(),
        delta: payload.encode().unwrap_or_default(),
    }
}

/// The session to the host, over whichever carrier reaches it.
#[allow(
    clippy::large_enum_variant,
    reason = "one link per session, never moved in bulk"
)]
enum Link {
    Https(HttpsLink),
    #[cfg(feature = "iroh")]
    Iroh(tilo_relay_client::RelayClient),
}

impl Link {
    fn resumable(&self) -> bool {
        match self {
            Self::Https(link) => link.resumable(),
            #[cfg(feature = "iroh")]
            Self::Iroh(_) => false,
        }
    }

    fn compacts(&self) -> bool {
        match self {
            Self::Https(link) => link.compacts(),
            #[cfg(feature = "iroh")]
            Self::Iroh(_) => false,
        }
    }

    fn send(&self, envelope: Envelope) -> Result<(), Box<Envelope>> {
        match self {
            Self::Https(link) => link.send(envelope),
            #[cfg(feature = "iroh")]
            Self::Iroh(client) => client.send(envelope),
        }
    }

    async fn recv(&mut self) -> Option<Envelope> {
        match self {
            Self::Https(link) => link.recv().await,
            #[cfg(feature = "iroh")]
            Self::Iroh(client) => client.recv().await,
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
            Self::Iroh(client) => client.close().await,
        }
    }
}

async fn connect(config: &PeerConfig, signer: &dyn DeviceSigner) -> Result<Link, LinkError> {
    match &config.host {
        Host::Https(client) => HttpsLink::connect(
            client.clone(),
            &config.cert,
            config.host_cert.device_public_key,
            &config.revocations,
            signer,
        )
        .await
        .map(Link::Https),
        #[cfg(feature = "iroh")]
        Host::Iroh { addr, relay_url } => {
            use tilo_relay_client::{ClientError, RelayClient};
            let mut builder = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal);
            if let Some(url) = relay_url {
                builder = builder.relay_mode(iroh::RelayMode::Custom(url.clone().into()));
            }
            let endpoint = builder
                .bind()
                .await
                .map_err(|error| LinkError::Unavailable(error.to_string()))?;
            let host = match relay_url {
                Some(url) => addr.clone().with_relay_url(url.clone()),
                None => addr.clone(),
            };
            RelayClient::connect_with_signer(
                endpoint,
                host,
                Some(config.cert.user_public_key),
                Some(config.host_cert.device_public_key),
                config.cert.clone(),
                signer,
                config.revocations.clone(),
            )
            .await
            .map(Link::Iroh)
            .map_err(|error| match error {
                ClientError::Iroh(reason) => LinkError::Unavailable(reason),
                ClientError::Sign(reason) => LinkError::Sign(reason),
                other => LinkError::Refused(other.to_string()),
            })
        }
    }
}

/// A live grant for this device, or for agent `agent` it runs.
async fn live_grant(
    http: &dyn crate::HttpClient,
    session: &str,
    workspace_id: u64,
    agent: Option<String>,
) -> Result<tilo_relay_protocol::web::LiveGrant, crate::carrier::LinkError> {
    match agent {
        None => crate::carrier::live_grant(http, session, workspace_id).await,
        Some(agent) => crate::carrier::agent_live_grant(http, session, workspace_id, agent).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(rooms: &[(u64, bool)]) -> PeerStatus {
        PeerStatus {
            phase: TransportPhase::Connected,
            authenticated: true,
            attempt: 0,
            next_attempt_at_ms: None,
            last_contact_at_ms: None,
            rooms: rooms.iter().copied().collect(),
            routes: BTreeMap::new(),
            failures: Vec::new(),
            outgoing_pending: 0,
            device: [0; 32],
            generation: 0,
            channels: String::new(),
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
    fn rooms_not_admitted_within_the_timeout_are_overdue_once_subscribed() {
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
}
