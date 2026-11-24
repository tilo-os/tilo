//! The client's transport half: what this device says to its workspaces, its replicas of them, and
//! what its session with its host heard and how it fares — what any thread holds. A page's presence
//! boards ([`crate::Client`]) are built on it; an agent, a program and the server's own tests hold
//! it alone.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_actions::Heard;

use crate::engine::Engine;
use crate::network::{Closed, Cmd, Event, Outbox};
use tilo_server_protocol::WorkspaceId;

/// Failures kept at once; the oldest goes first.
const MAX_FAILURES: usize = 16;

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
    /// Refused; retries only on [`Cmd::Reconnect`].
    Failed,
    /// Disposed.
    Closed,
}

/// A failure in `SessionHealth` v1 vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// `FailureCode`: `transport-unavailable`, `transport-ended`,
    /// `certificate-expired`, `certificate-rejected`, `not-admitted`,
    /// `device-revoked`, `host-changed`, `workspace-deleted`, `agent-run-stopped`,
    /// `protocol-rejected`, `permission-required`, `write-refused`, `record-history-full`, `signing-failed`,
    /// `storage-transaction-failed`, `admission-timeout`, and the codes the
    /// page reports (see [`Link::report_failure`]).
    pub code: &'static str,
    /// Safe product copy.
    pub message: String,
    /// When it happened (ms since the epoch).
    pub at_ms: u64,
    /// What failed. A kind other than [`FailureKind::Session`] holds at
    /// most one failure at a time.
    pub kind: FailureKind,
}

/// What a [`Failure`] is about. It decides the `SessionHealth` scope and the
/// recovery the failure offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailureKind {
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

/// Failure codes the page may report for a kind it owns.
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
    pub workspaces: BTreeMap<WorkspaceId, bool>,
    /// The joined workspaces whose governance admits this device: their records read as a
    /// member's ([`crate::WorkspaceState::Serving`]).
    pub serving: BTreeSet<WorkspaceId>,
    /// Each record's local store state, as reported with [`Link::set_local_state`].
    pub local: BTreeMap<DocumentRoute, LocalState>,
    /// Current failures, newest last.
    pub failures: Vec<Failure>,
    /// Commits not yet acknowledged, over every route.
    pub outgoing_pending: usize,
    /// This device.
    pub device: [u8; 32],
    /// Bumped on every change.
    pub generation: u64,
    /// How many connections this session has had: what a request refused on
    /// one connection waits to pass before it is asked again.
    pub connections: u64,
    /// This page's key state, one line per workspace, certificate and
    /// route, for diagnostics.
    pub key_state: String,
    /// The members each joined workspace's governance lists, by workspace, by
    /// person id: what someone no longer among them made on the workspace's
    /// desktop is released ([`tilo_desktop_model::ownership::released`]).
    pub workspace_members: BTreeMap<String, BTreeSet<String>>,
    /// Whose each member device of a joined workspace is, by workspace: device (hex) to person
    /// (root key, hex), as the workspace's governance admitted them. Who is here is shown by
    /// person from it, so someone the live session never certified to this page is still theirs.
    pub workspace_devices: BTreeMap<String, BTreeMap<String, String>>,
    /// What each call of a joined workspace allows, by workspace, by channel, as the workspace
    /// keeps it: who a call lists, and whose media it carries.
    pub workspace_calls: BTreeMap<String, BTreeMap<String, collab_types::calls::CallRule>>,
    /// What each joined workspace's governance calls its people, by workspace, by person id
    /// (root key, hex): how a page names someone who is not here now, never by their key.
    pub workspace_names: BTreeMap<String, BTreeMap<String, String>>,
    /// What the host answered the entry this session asked for, once it did.
    pub entry: Option<Entry>,
}

impl PeerStatus {
    /// A session of `device` that has not connected yet.
    #[must_use]
    pub fn connecting(device: [u8; 32]) -> Self {
        Self {
            phase: TransportPhase::Connecting,
            authenticated: false,
            attempt: 0,
            next_attempt_at_ms: None,
            last_contact_at_ms: None,
            workspaces: BTreeMap::new(),
            serving: BTreeSet::new(),
            local: BTreeMap::new(),
            failures: Vec::new(),
            outgoing_pending: 0,
            device,
            generation: 0,
            connections: 0,
            key_state: String::new(),
            workspace_members: BTreeMap::new(),
            workspace_devices: BTreeMap::new(),
            workspace_calls: BTreeMap::new(),
            workspace_names: BTreeMap::new(),
            entry: None,
        }
    }
}

/// The host's answer to a session's entry through a workspace's entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// The workspace admitted this device.
    Admitted {
        /// The workspace's UUID.
        workspace: String,
        /// Its server workspace.
        workspace_id: WorkspaceId,
        /// Its name.
        name: String,
        /// What the person may do there.
        role: tilo_server_protocol::workspaces::Role,
        /// The host's signed address for the workspace ([`tilo_server_client::login::Admission::host`]).
        host: Option<Vec<u8>>,
    },
    /// The entry or the workspace did not let it in: the entry's `code`
    /// (`banned`, `removed`, `invitation-only`, `full`, `refused`), or
    /// `not-admitted` when the workspace refused the device the entry let in;
    /// what was said, and how long the entry asked to wait before the next try.
    Refused {
        code: &'static str,
        message: String,
        retry_after_secs: Option<u64>,
        /// The entry's own refusal as it answered it: its code, and what it
        /// says about a wait list the workspace keeps, so the page offers to
        /// leave an address there.
        entry: Option<tilo_server_protocol::directory::EntryRefused>,
    },
}

impl PeerStatus {
    /// Take `failure` in: a kind other than [`FailureKind::Session`] holds one failure at a
    /// time, the session one per code, and the oldest goes once there are too many.
    pub fn upsert(&mut self, failure: Failure) {
        self.failures.retain(|existing| {
            if failure.kind == FailureKind::Session {
                existing.kind != FailureKind::Session || existing.code != failure.code
            } else {
                existing.kind != failure.kind
            }
        });
        self.failures.push(failure);
        if self.failures.len() > MAX_FAILURES {
            self.failures.remove(0);
        }
    }

    /// Why the workspace `ws` cannot answer what this session asks of it now: while the session
    /// is not connected, its newest failure; while it is but the workspace has not let this
    /// device in, the workspace's refusal. The failure's code says the cause a person is told;
    /// its message stays the technical side. `None` while the workspace can answer, and while no
    /// failure was heard (a first connection still under way is slow, not broken).
    #[must_use]
    pub fn unanswerable(&self, ws: WorkspaceId) -> Option<&Failure> {
        let newest = |of: FailureKind| {
            self.failures
                .iter()
                .rev()
                .find(|failure| failure.kind == of)
        };
        match self.phase {
            TransportPhase::Connected if self.workspaces.get(&ws) == Some(&true) => None,
            TransportPhase::Connected => newest(FailureKind::Admission),
            _ => newest(FailureKind::Session),
        }
    }
}

type Queue = Arc<Mutex<VecDeque<Event>>>;
/// Which events a consumer's queue takes: a consumer is handed only the events it reads, so a
/// session that hears a busy workspace (its members' presence, many times a second) keeps none
/// of them for a consumer that would throw them away.
pub type Takes = fn(&Event) -> bool;
type Wake = Arc<dyn Fn() + Send + Sync>;

/// What serves a request another device addressed to this session's device on this
/// session with its host: a listing of a window this session holds, a page action
/// on it. The host routes such a frame here; it is this device's to answer.
pub trait InboundActions: Send + Sync {
    /// Serve the frame `from`, certified for `user`, sent this device in `workspace`.
    /// The answer goes back on this session, to `from`.
    fn take(&self, workspace: WorkspaceId, user: [u8; 32], from: [u8; 32], frame: Vec<u8>);
}

/// What a session reports to everyone holding its [`Link`]: its status, the events it heard, and
/// who to wake when either changed. The transport writes it; the link reads it.
pub struct Reports(Mutex<Reported>);

struct Reported {
    status: PeerStatus,
    queues: Vec<(Weak<Mutex<VecDeque<Event>>>, Takes)>,
    wakes: Vec<Wake>,
    /// Wakes that last as long as their consumer's event queue (an asker's), dropped
    /// with it.
    listeners: Vec<(Weak<Mutex<VecDeque<Event>>>, Wake)>,
    /// Wakes for one queue, told only when that queue takes an event: a
    /// reader that follows one kind of event and nothing else.
    takers: Vec<(Weak<Mutex<VecDeque<Event>>>, Wake)>,
    /// The current session's bearer token for its host's fetch proxy (`tilo_net::proxy`, at
    /// `/peer/v1/fetch`); kept apart from [`PeerStatus`], which diagnostics show.
    fetch_proxy: Option<String>,
    /// What the page shows where its live session would be, when it has none: that no peer network could
    /// be reached ([`Link::live_note`]).
    live_note: Option<String>,
    /// What serves what another device asks this one through the host
    /// ([`InboundActions`]): a host's routes reach the session here.
    inbound: Option<Arc<dyn InboundActions>>,
    /// Told when a queue that takes the members' presence comes or goes
    /// ([`Reports::reads_presence`]).
    presence_moved: Option<Wake>,
    /// Raised as the session is closed ([`Link::close`]): a dial under way hears it at once,
    /// where the close command waits in the queue until the dial ends.
    closing: tokio::sync::watch::Sender<bool>,
}

impl Reported {
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

impl Reports {
    /// Serve what another device addresses to this device from now on.
    pub fn serve_inbound(&self, inbound: Arc<dyn InboundActions>) {
        self.lock().inbound = Some(inbound);
    }

    /// What serves what another device addresses to this device, once the session
    /// can answer it.
    #[must_use]
    pub fn inbound(&self) -> Option<Arc<dyn InboundActions>> {
        self.lock().inbound.clone()
    }

    /// What a session of `device` that has not connected yet reports.
    #[must_use]
    pub fn connecting(device: [u8; 32]) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Reported {
            status: PeerStatus::connecting(device),
            queues: Vec::new(),
            wakes: Vec::new(),
            listeners: Vec::new(),
            takers: Vec::new(),
            fetch_proxy: None,
            live_note: None,
            inbound: None,
            presence_moved: None,
            closing: tokio::sync::watch::Sender::new(false),
        })))
    }

    /// Say the session is being closed, to a dial under way ([`Self::closed`]).
    pub fn close(&self) {
        self.lock().closing.send_replace(true);
    }

    /// Once the session is being closed ([`Self::close`]).
    pub fn closed(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut closing = self.lock().closing.subscribe();
        async move {
            let _ = closing.wait_for(|closing| *closing).await;
        }
    }

    fn lock(&self) -> MutexGuard<'_, Reported> {
        self.0.lock_recover()
    }

    /// The status as it is now.
    #[must_use]
    pub fn status(&self) -> PeerStatus {
        self.lock().status.clone()
    }

    /// What `read` reads of the status, without copying the rest of it.
    pub fn read<R>(&self, read: impl FnOnce(&PeerStatus) -> R) -> R {
        read(&self.lock().status)
    }

    /// Note that the server said something at `at_ms`. What a connection's liveness is read from
    /// (the session's keep-alive): kept here without publishing a status change for every frame
    /// the server sends.
    pub(crate) fn note_contact(&self, at_ms: u64) {
        self.lock().status.last_contact_at_ms = Some(at_ms);
    }

    /// Apply `change` to the status, bump its generation, publish the states it moved
    /// ([`crate::states`]) and wake the watchers once the lock is released.
    pub fn change(&self, change: impl FnOnce(&mut PeerStatus)) {
        let moved = {
            let mut reported = self.lock();
            let before = crate::states::of(&reported.status);
            change(&mut reported.status);
            reported.status.generation = reported.status.generation.wrapping_add(1);
            crate::states::changes(&before, &reported.status)
        };
        if moved.is_empty() {
            let wakes = self.lock().wakes_now();
            for wake in &wakes {
                wake();
            }
        }
        // Each state's event wakes everyone, the status's change with it.
        for event in &moved {
            self.emit(event);
        }
    }

    /// Hand `event` to every consumer's queue and wake them.
    pub fn emit(&self, event: &Event) {
        let (wakes, told) = {
            let mut reported = self.lock();
            let mut took: Vec<Queue> = Vec::new();
            reported.queues.retain(|(queue, takes)| {
                let Some(queue) = queue.upgrade() else {
                    return false;
                };
                if takes(event) {
                    queue.lock_recover().push_back(event.clone());
                    took.push(queue);
                }
                true
            });
            reported
                .takers
                .retain(|(queue, _)| queue.strong_count() > 0);
            let told: Vec<Wake> = reported
                .takers
                .iter()
                .filter(|(queue, _)| {
                    queue
                        .upgrade()
                        .is_some_and(|queue| took.iter().any(|took| Arc::ptr_eq(took, &queue)))
                })
                .map(|(_, wake)| Arc::clone(wake))
                .collect();
            (reported.wakes_now(), told)
        };
        for wake in wakes.iter().chain(told.iter()) {
            wake();
        }
    }

    /// The bearer token the host's fetch proxy takes, as this session holds it.
    pub fn set_fetch_proxy(&self, token: Option<String>) {
        self.lock().fetch_proxy = token;
    }

    /// Put the note the page shows where its live session would be (in place of the members) in
    /// place, waking every watcher when it changed.
    pub fn set_live_note(&self, note: Option<&str>) {
        let wakes = {
            let mut reported = self.lock();
            let note = note.map(str::to_owned);
            if reported.live_note == note {
                return;
            }
            reported.live_note = note;
            reported.wakes.clone()
        };
        for wake in &wakes {
            wake();
        }
    }

    /// Call `wake` on every change for as long as `events` is read.
    fn listen(&self, events: &Events, wake: Wake) {
        self.lock()
            .listeners
            .push((Arc::downgrade(&events.queue), wake));
    }

    /// Call `wake` when `events` takes an event, for as long as it is read.
    fn listen_taking(&self, events: &Events, wake: Wake) {
        self.lock()
            .takers
            .push((Arc::downgrade(&events.queue), wake));
    }

    /// A new, independent queue of the events `takes` takes.
    fn events(&self, takes: Takes) -> Events {
        let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
        let moved = {
            let mut reported = self.lock();
            reported.queues.push((Arc::downgrade(&queue), takes));
            let moved = reported.presence_moved.clone();
            drop(reported);
            moved.filter(|_| takes(&PRESENCE))
        };
        if let Some(moved) = &moved {
            moved();
        }
        Events { queue, moved }
    }

    /// Whether a consumer's queue takes the members' presence now: while none does, the
    /// session's peer networks read none of it.
    #[must_use]
    pub fn reads_presence(&self) -> bool {
        self.lock()
            .queues
            .iter()
            .any(|(queue, takes)| queue.strong_count() > 0 && takes(&PRESENCE))
    }

    /// Call `moved` when a queue that takes the members' presence comes or goes.
    pub fn on_presence_readers(&self, moved: Wake) {
        self.lock().presence_moved = Some(moved);
    }
}

/// What a queue that takes the members' presence takes.
const PRESENCE: Event = Event::LiveSession {
    ws: crate::Topic::nil(),
    from: [0; 32],
    track: tilo_live::live::Track::Presence,
    sequence: 0,
    body: Vec::new(),
};

/// One consumer's view of the session's events: every event it takes produced after it was
/// created, independent of other consumers. It holds what it took until drained, so a consumer
/// drains it as long as it holds it, and drops it once it stops reading.
pub struct Events {
    queue: Queue,
    /// Told when this queue, which takes the members' presence, goes.
    moved: Option<Wake>,
}

impl Drop for Events {
    fn drop(&mut self) {
        if let Some(moved) = self.moved.take() {
            // Gone before the session counts the queues again.
            drop(std::mem::take(&mut self.queue));
            moved();
        }
    }
}

impl Events {
    /// Another handle on the same queue, which keeps it alive as long as it is held.
    fn queue(&self) -> Queue {
        Arc::clone(&self.queue)
    }

    /// Take every event produced since the last call.
    #[must_use]
    pub fn drain(&self) -> Vec<Event> {
        self.queue.lock_recover().drain(..).collect()
    }
}

/// The client's transport half, which any thread holds: where this device says what it says to
/// its workspaces, its replicas of them, and what its session reports. Cheap to clone; the session
/// ends with its last link.
#[derive(Clone)]
pub struct Link {
    /// The transport's own sender, which every host's is: any thread says through it.
    outbox: Arc<dyn Outbox + Send + Sync>,
    /// This device's replicas of its workspaces, and what reads their records.
    engine: Arc<Engine>,
    /// This device's writes its session has not drafted and admitted yet: held until the
    /// session's holder can write them ([`crate::Cmd::Write`]).
    held: Held,
    reports: Arc<Reports>,
}

/// The writes this device made that its session has not drafted and admitted yet, by record: a
/// write waits here for its record's keys, or for the record it changes, and is read by no one
/// until the session's holder admits it. Whoever reports on a record counts them: a folder is not
/// said durable while one is here, and a read does not treat a record this device wrote as one it
/// has not.
#[derive(Clone, Default)]
pub struct Held(
    Arc<
        std::sync::Mutex<
            std::collections::BTreeMap<DocumentRoute, Vec<collab_types::DocumentMutation>>,
        >,
    >,
);

impl Held {
    /// The held writes, to read or change.
    pub(crate) fn lock(
        &self,
    ) -> std::sync::MutexGuard<
        '_,
        std::collections::BTreeMap<DocumentRoute, Vec<collab_types::DocumentMutation>>,
    > {
        self.0.lock_recover()
    }

    /// Whether this device holds any write of `route`.
    #[must_use]
    pub fn contains(&self, route: &DocumentRoute) -> bool {
        self.lock().contains_key(route)
    }

    /// Hold `mutation` for `route` and say it to `outbox`, whose session drafts and admits it: the
    /// one way a write of this device is taken up, whichever handle of the session makes it.
    pub(crate) fn hold(
        &self,
        route: DocumentRoute,
        mutation: &collab_types::DocumentMutation,
        outbox: &dyn Outbox,
    ) -> Result<(), String> {
        self.lock()
            .entry(route.clone())
            .or_default()
            .push(mutation.clone());
        Ok(outbox.send(Cmd::Write(route))?)
    }
}

impl Link {
    /// A link through `outbox`, whose transport keeps `engine` and reports on `reports`, holding
    /// what `held` names until the session drafts and admits it.
    #[must_use]
    pub fn new(
        outbox: Arc<dyn Outbox + Send + Sync>,
        engine: Arc<Engine>,
        held: Held,
        reports: Arc<Reports>,
    ) -> Self {
        Self {
            outbox,
            engine,
            held,
            reports,
        }
    }

    /// Whether this device holds a write of `route` its session has not drafted and admitted yet.
    #[must_use]
    pub fn holding(&self, route: &DocumentRoute) -> bool {
        self.held.contains(route)
    }

    /// Where this link says what it says, for a task that must be `Send` where the link is not
    /// (a page's, whose engine stays on its thread): it carries the commands that touch no replica,
    /// and the writes it holds for their record's keys, which the session drafts and admits.
    #[must_use]
    pub fn outbox(&self) -> &Arc<dyn Outbox + Send + Sync> {
        &self.outbox
    }

    /// This device's replicas of its workspaces: what reads a record whose writes
    /// [`Event::Landed`] said arrived.
    #[must_use]
    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// This session's health: its status, and this device's writes to each record as its
    /// replica's ownership keeps them.
    #[must_use]
    pub fn snapshot(&self) -> crate::health::PeerSnapshot {
        let owner = self.engine.owner();
        let writes = self
            .engine
            .workspaces()
            .into_iter()
            .flat_map(|workspace| {
                owner.document_writes(workspace).into_iter().map(
                    move |((app_id, record), writes)| {
                        (
                            DocumentRoute {
                                app_id,
                                cluster_id: workspace.to_string(),
                                document: record,
                            },
                            writes,
                        )
                    },
                )
            })
            .collect();
        crate::health::PeerSnapshot::from_status(&self.status(), &writes)
    }

    /// Why this page shows no live presence, in the words it shows in its members' place, or
    /// `None` while it has a peer network: no peer network could be reached.
    #[must_use]
    pub fn live_note(&self) -> Option<String> {
        self.reports.lock().live_note.clone()
    }

    /// End the session.
    pub fn close(&self) {
        self.reports.close();
        let _ = self.send(Cmd::Stop);
    }

    /// Record the page store's durability of `route`. A failed write is the storage failure; it
    /// clears once no route's store state is failed.
    pub fn set_local_state(&self, route: DocumentRoute, state: LocalState) {
        self.reports.change(|status| {
            status.local.insert(route, state);
            if state == LocalState::Failed {
                status.upsert(Failure {
                    code: "storage-transaction-failed",
                    message: "Changes could not be saved on this device.".into(),
                    at_ms: tilo_clock::unix_now_ms(),
                    kind: FailureKind::Storage,
                });
            } else if state == LocalState::Durable
                && !status
                    .local
                    .values()
                    .any(|state| *state == LocalState::Failed)
            {
                status
                    .failures
                    .retain(|failure| failure.kind != FailureKind::Storage);
            }
        });
    }

    /// Report a failure of something the page owns: its store, a desktop surface, a surface's
    /// asset. It replaces the previous one of its kind.
    ///
    /// # Errors
    ///
    /// Rejects [`FailureKind::Session`] and [`FailureKind::Admission`], which only the
    /// session reports, and codes outside [`PAGE_FAILURE_CODES`].
    pub fn report_failure(
        &self,
        kind: FailureKind,
        code: &str,
        message: impl Into<String>,
    ) -> Result<(), String> {
        if matches!(kind, FailureKind::Session | FailureKind::Admission) {
            return Err("the session reports its own failures".into());
        }
        let code = PAGE_FAILURE_CODES
            .iter()
            .copied()
            .find(|known| *known == code)
            .ok_or_else(|| format!("unknown failure code {code}"))?;
        let message = message.into();
        self.reports.change(|status| {
            status.upsert(Failure {
                code,
                message,
                at_ms: tilo_clock::unix_now_ms(),
                kind,
            });
        });
        Ok(())
    }

    /// Clear every failure of `kind`.
    pub fn clear_failure(&self, kind: &FailureKind) {
        self.clear(|failure| failure == kind);
    }

    /// Clear every failure of desktop surface `surface` and of its assets.
    pub fn clear_surface(&self, surface: &str) {
        self.clear(|kind| match kind {
            FailureKind::Surface(id) | FailureKind::Asset { surface: id, .. } => id == surface,
            _ => false,
        });
    }

    /// Clear the failures whose kind `of` names, waking the watchers only when there were any.
    fn clear(&self, of: impl Fn(&FailureKind) -> bool) {
        if !self
            .reports
            .read(|status| status.failures.iter().any(|failure| of(&failure.kind)))
        {
            return;
        }
        self.reports.change(|status| {
            status.failures.retain(|failure| !of(&failure.kind));
        });
    }

    /// The bearer token the host's fetch proxy takes (`tilo_net::proxy`, at `/peer/v1/fetch`)
    /// while this session holds one; `None` otherwise, and from a host that serves no proxy. It
    /// changes with every session, so callers read it for each request.
    #[must_use]
    pub fn fetch_proxy_session(&self) -> Option<String> {
        self.reports.lock().fetch_proxy.clone()
    }

    /// Current status.
    #[must_use]
    pub fn status(&self) -> PeerStatus {
        self.reports.status()
    }

    /// How many of this device's writes its hosts have not admitted yet, as the session counted
    /// them last: read without copying the status or reading what ownership holds, so a caller
    /// may ask after every change it hears of.
    #[must_use]
    pub fn outgoing_pending(&self) -> usize {
        self.reports.read(|status| status.outgoing_pending)
    }

    /// Whose each member device of `workspace` is, as [`PeerStatus::workspace_devices`] says: device
    /// (hex) to person (root key, hex), read without copying the rest of the status. Empty until
    /// the workspace's governance admitted its members.
    #[must_use]
    pub fn workspace_devices(&self, workspace: &str) -> BTreeMap<String, String> {
        self.reports
            .read(|status| status.workspace_devices.get(workspace).cloned())
            .unwrap_or_default()
    }

    /// What each call of `workspace` allows, by channel, as [`PeerStatus::workspace_calls`] says.
    #[must_use]
    pub fn call_rules(&self, workspace: &str) -> BTreeMap<String, collab_types::calls::CallRule> {
        self.reports
            .read(|status| status.workspace_calls.get(workspace).cloned())
            .unwrap_or_default()
    }

    /// A new, independent queue of every event the session produces from now on. A link keeps
    /// no queue of its own: what nobody reads is not kept.
    #[must_use]
    pub fn events(&self) -> Events {
        self.reports.events(|_| true)
    }

    /// A new, independent queue of the events `takes` takes, from now on.
    #[must_use]
    pub fn events_where(&self, takes: Takes) -> Events {
        self.reports.events(takes)
    }

    /// Call `wake` whenever status or events change.
    pub fn on_change(&self, wake: Wake) {
        self.reports.lock().wakes.push(wake);
    }

    /// Call `wake` when `events` takes an event, and at no other time: a
    /// reader that follows one kind of event, such as the person's workspace
    /// list, rather than every change of the session.
    pub fn on_event(&self, events: &Events, wake: Wake) {
        self.reports.listen_taking(events, wake);
    }

    /// What `read` first answers of the session's status, woken by each change of it rather
    /// than looking again on a timer; `None` when it answered nothing within `within`.
    pub async fn until<R>(
        &self,
        within: web_time::Duration,
        mut read: impl FnMut(&PeerStatus) -> Option<R>,
    ) -> Option<R> {
        let started = tilo_clock::now();
        loop {
            // Listened for before the status is read, so a change in between still wakes it.
            let (changed, change) = crate::answer::<()>();
            let changed = Mutex::new(Some(changed));
            let listening = self.reports.events(|_| false);
            self.reports.listen(
                &listening,
                Arc::new(move || {
                    let changed = changed.lock_recover().take();
                    if let Some(changed) = changed {
                        let _ = changed.send(());
                    }
                }),
            );
            if let Some(found) = self.reports.read(&mut read) {
                return Some(found);
            }
            let left = within.checked_sub(started.elapsed())?;
            let woken = async { change.await.is_ok() };
            let late = async {
                n0_future::time::sleep(left).await;
                false
            };
            if !n0_future::future::or(woken, late).await {
                return self.reports.read(&mut read);
            }
            drop(listening);
        }
    }

    /// Serve what another device addresses to this device on this session, from
    /// now on: a window this session holds is reached through the host that way.
    pub fn serve_inbound(&self, inbound: Arc<dyn InboundActions>) {
        self.reports.serve_inbound(inbound);
    }
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link").finish_non_exhaustive()
    }
}

impl Outbox for Link {
    fn send(&self, command: Cmd) -> Result<(), Closed> {
        // The replica's readers read with them from now on, not once the session got to them.
        if let Cmd::InstallGroupKeys(route, keys) = &command
            && let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id)
        {
            self.engine.hand_keys(workspace, keys.clone());
        }
        self.outbox.send(command)
    }

    /// Hold `mutation` before the session, which drafts and admits held writes, can look: whoever
    /// reports on the record counts this device's write from now, not once the session got to it.
    fn write(
        &self,
        route: DocumentRoute,
        mutation: &collab_types::DocumentMutation,
    ) -> Result<(), String> {
        self.held.hold(route, mutation, self.outbox.as_ref())
    }
}

/// This session as what asking another device's actions needs ([`Heard`]): the frames it carries
/// to a device's key, and what it heard back, on a queue of its own, so any number of askers on
/// one session each hear every answer.
impl From<&Link> for Arc<dyn tilo_session_actions::RemoteSession> {
    fn from(link: &Link) -> Self {
        Arc::new(Asking {
            outbox: Arc::clone(&link.outbox),
            reports: Arc::clone(&link.reports),
            events: link.events_where(|event| matches!(event, Event::Remote(_))),
            presence: OnceLock::new(),
            boards: Mutex::new(BTreeMap::new()),
            publisher: link.engine.identity().device_cert.device_public_key,
        })
    }
}

/// One asker of another device's actions over a session.
struct Asking {
    outbox: Arc<dyn Outbox + Send + Sync>,
    reports: Arc<Reports>,
    events: Events,
    /// What the members of the session's workspaces say of themselves, read once something asks
    /// who is here: an asker that shows nobody, such as an agent's bridge, reads none of it, and
    /// the live sessions read no presence for it.
    presence: OnceLock<Events>,
    /// Who is in each workspace, as their presence was heard: the board a page keeps too.
    boards: Mutex<BTreeMap<WorkspaceId, tilo_live::presence::Board>>,
    /// This device, which hears its own presence as nobody else's.
    publisher: [u8; 32],
}

impl tilo_session_actions::RemoteSession for Asking {
    fn send_remote_action(
        &self,
        workspace_id: WorkspaceId,
        device: [u8; 32],
        frame: Vec<u8>,
    ) -> Result<(), String> {
        self.outbox
            .send(Cmd::RemoteAction(workspace_id, device, frame))
            .map_err(|_| "peer session is closed".to_string())
    }

    fn take_heard(&self) -> Vec<Heard> {
        self.events
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                Event::Remote(heard) => Some(heard),
                _ => None,
            })
            .collect()
    }

    fn on_change(&self, wake: Wake) {
        self.reports.listen(&self.events, wake);
    }

    fn carrying(&self) -> bool {
        self.reports
            .read(|status| status.phase == TransportPhase::Connected)
    }

    fn connections(&self) -> u64 {
        self.reports.read(|status| status.connections)
    }

    /// Who is in `workspace_id`, as their presence reached this session since something first
    /// asked, and the window each works in.
    fn here(&self, workspace_id: WorkspaceId) -> Option<Vec<tilo_actions::remote::Here>> {
        let now = crate::presence::now_ms();
        let presence = self.presence.get_or_init(|| {
            self.reports.events(|event| {
                matches!(
                    event,
                    Event::LiveSession {
                        track: tilo_live::live::Track::Presence,
                        ..
                    }
                )
            })
        });
        let mut boards = self.boards.lock_recover();
        for event in presence.drain() {
            if let Event::LiveSession { ws, from, body, .. } = event {
                boards
                    .entry(ws)
                    .or_insert_with(|| {
                        tilo_live::presence::Board::new(ws.to_string(), self.publisher)
                    })
                    .heard(from, &body, now);
            }
        }
        let board = boards.entry(workspace_id).or_insert_with(|| {
            tilo_live::presence::Board::new(workspace_id.to_string(), self.publisher)
        });
        let here = Some(
            board
                .here()
                .map(|member| {
                    let shown = tilo_live::presence::shown(member, now);
                    tilo_actions::remote::Here {
                        who: shown
                            .user
                            .clone()
                            .or_else(|| shown.member.as_ref().map(ToString::to_string))
                            .unwrap_or_else(|| shown.publisher.clone()),
                        name: shown.label.clone(),
                        kind: if shown.agent { "agent" } else { "person" }.into(),
                        works_in: shown.focus.map(|focus| tilo_actions::remote::HereWindow {
                            app: focus.app,
                            document: focus.document,
                        }),
                        devices: vec![shown.publisher],
                    }
                })
                .collect(),
        );
        drop(boards);
        here
    }

    fn session_state(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<tilo_actions::remote::SessionState> {
        self.reports
            .read(|status| status.session_state(workspace_id))
    }

    fn state_changed(&self) -> Option<tilo_actions::remote::Pending<()>> {
        let (woken, changed) = tilo_actions::remote::pending();
        let listening = self
            .reports
            .events(|event| matches!(event, Event::Workspace { .. } | Event::Sync(_)));
        // The wake keeps the queue it listens on until a state's event reaches it, then lets
        // both go: every other change of the session passes it by. A wait whose caller left
        // lets both go at the next change too, so a session whose state stands still (one
        // that lost its server) does not keep a listener for every watch that gave up on it.
        let held = Mutex::new(Some((woken, listening.queue())));
        self.reports.listen(
            &listening,
            Arc::new(move || {
                let mut held = held.lock_recover();
                let Some((woken, queue)) = held.as_ref() else {
                    return;
                };
                let moved = !queue.lock_recover().is_empty();
                if (moved || woken.is_abandoned())
                    && let Some((woken, _queue)) = held.take()
                {
                    woken.complete(());
                }
            }),
        );
        Some(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(code: &'static str, kind: FailureKind) -> Failure {
        Failure {
            code,
            message: String::new(),
            at_ms: 0,
            kind,
        }
    }

    #[test]
    fn a_kind_holds_one_failure_and_session_failures_one_per_code() {
        let mut status = PeerStatus::connecting([0; 32]);
        for (code, kind) in [
            ("transport-ended", FailureKind::Session),
            ("transport-unavailable", FailureKind::Session),
            ("transport-ended", FailureKind::Session),
            ("storage-transaction-failed", FailureKind::Storage),
            ("storage-quota", FailureKind::Storage),
            ("gpu-device-lost", FailureKind::Surface("a".into())),
            ("gpu-device-lost", FailureKind::Surface("b".into())),
        ] {
            status.upsert(failure(code, kind));
        }
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
            status.upsert(failure(
                "asset-unavailable",
                FailureKind::Asset {
                    surface: "a".into(),
                    asset: index.to_string(),
                },
            ));
        }
        assert_eq!(status.failures.len(), MAX_FAILURES);
    }

    #[test]
    fn a_workspace_that_cannot_answer_says_what_failed() {
        let ws = WorkspaceId::parse("6f1c2a52-3c1e-4c55-9d8e-2b7f4f0e9a10").unwrap();
        let said = |code, message: &str, kind| Failure {
            message: message.to_owned(),
            ..failure(code, kind)
        };
        let mut status = PeerStatus::connecting([0; 32]);
        assert_eq!(
            status.unanswerable(ws),
            None,
            "a first connection still under way is slow, not broken"
        );
        status.upsert(said(
            "transport-unavailable",
            "Couldn't reach the workspace's server",
            FailureKind::Session,
        ));
        status.phase = TransportPhase::Offline;
        let cause = status.unanswerable(ws).expect("the newest session failure");
        assert_eq!(cause.code, "transport-unavailable");
        assert_eq!(cause.message, "Couldn't reach the workspace's server");
        status.phase = TransportPhase::Connected;
        assert_eq!(
            status.unanswerable(ws),
            None,
            "connected and not refused: the answer is on its way"
        );
        status.upsert(said(
            "not-admitted",
            "This workspace hasn't let this device in",
            FailureKind::Admission,
        ));
        let refusal = status.unanswerable(ws).expect("the workspace's refusal");
        assert_eq!(refusal.code, "not-admitted");
        assert_eq!(refusal.message, "This workspace hasn't let this device in");
        status.workspaces.insert(ws, true);
        assert_eq!(status.unanswerable(ws), None, "the workspace is ready");
    }
}
