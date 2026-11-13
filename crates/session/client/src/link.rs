//! The client's transport half: what this device says to its workspaces, its replicas of them, and
//! what its session with its host heard and how it fares — what any thread holds. A page's presence
//! boards ([`crate::Client`]) are built on it; an agent, a program and the relay's own tests hold
//! it alone.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_core::remote::Heard;

use crate::engine::Engine;
use crate::plane::{Closed, Cmd, Event, Outbox};
use tilo_relay_protocol::WorkspaceId;

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
    /// When each of a joined workspace's people was admitted, by workspace, by person id: the
    /// order people who share a name are numbered in, as the workspace's roster numbers them.
    pub room_joined: BTreeMap<String, BTreeMap<String, u64>>,
    /// What the host answered the entry this session asked for
    /// (its door entry), once it did.
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
            local: BTreeMap::new(),
            failures: Vec::new(),
            outgoing_pending: 0,
            device,
            generation: 0,
            channels: String::new(),
            room_members: BTreeMap::new(),
            room_devices: BTreeMap::new(),
            room_joined: BTreeMap::new(),
            entry: None,
        }
    }
}

/// The host's answer to a session's entry through a workspace's door.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// The workspace admitted this device.
    Admitted {
        /// The workspace's UUID.
        workspace: String,
        /// Its relay workspace.
        workspace_id: WorkspaceId,
        /// Its name.
        name: String,
        /// What the person may do there.
        role: tilo_relay_protocol::workspaces::Role,
    },
    /// The door or the workspace did not let it in: the door's `code`
    /// (`banned`, `removed`, `invitation-only`, `full`, `refused`), or
    /// `not-admitted` when the workspace refused the device the door let in;
    /// what was said, and how long the door asked to wait before the next try.
    Refused {
        code: &'static str,
        message: String,
        retry_after_secs: Option<u64>,
        /// The door's own refusal as it answered it: its code, and what it
        /// says about a wait list the workspace keeps, so the page offers to
        /// leave an address there.
        door: Option<tilo_relay_protocol::directory::DoorRefused>,
    },
}

impl PeerStatus {
    /// Take `failure` in: a subject other than [`FailureSubject::Session`] holds one failure at a
    /// time, the session one per code, and the oldest goes once there are too many.
    pub fn upsert(&mut self, failure: Failure) {
        self.failures.retain(|existing| {
            if failure.subject == FailureSubject::Session {
                existing.subject != FailureSubject::Session || existing.code != failure.code
            } else {
                existing.subject != failure.subject
            }
        });
        self.failures.push(failure);
        if self.failures.len() > MAX_FAILURES {
            self.failures.remove(0);
        }
    }
}

type Queue = Arc<Mutex<VecDeque<Event>>>;
type Wake = Arc<dyn Fn() + Send + Sync>;

/// What a session reports to everyone holding its [`Link`]: its status, the events it heard, and
/// who to wake when either changed. The transport writes it; the link reads it.
pub struct Reports(Mutex<Reported>);

struct Reported {
    status: PeerStatus,
    queues: Vec<Weak<Mutex<VecDeque<Event>>>>,
    wakes: Vec<Wake>,
    /// Wakes that last as long as their consumer's event queue (an asker's), dropped
    /// with it.
    listeners: Vec<(Weak<Mutex<VecDeque<Event>>>, Wake)>,
    /// The current session's bearer token for its host's fetch proxy (`tilo_net::proxy`, at
    /// `/peer/v1/fetch`); kept apart from [`PeerStatus`], which diagnostics show.
    fetch_proxy: Option<String>,
    /// What the page shows where its live plane would be, when it has none: that no plane could
    /// be reached ([`Link::live_note`]).
    live_note: Option<String>,
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
    /// What a session of `device` that has not connected yet reports.
    #[must_use]
    pub fn connecting(device: [u8; 32]) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Reported {
            status: PeerStatus::connecting(device),
            queues: Vec::new(),
            wakes: Vec::new(),
            listeners: Vec::new(),
            fetch_proxy: None,
            live_note: None,
        })))
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

    /// Apply `change` to the status, bump its generation and wake the watchers once the lock is
    /// released.
    pub fn change(&self, change: impl FnOnce(&mut PeerStatus)) {
        let wakes = {
            let mut reported = self.lock();
            change(&mut reported.status);
            reported.status.generation = reported.status.generation.wrapping_add(1);
            reported.wakes_now()
        };
        for wake in &wakes {
            wake();
        }
    }

    /// Hand `event` to every consumer's queue and wake them.
    pub fn emit(&self, event: &Event) {
        let wakes = {
            let mut reported = self.lock();
            reported.queues.retain(|queue| {
                let Some(queue) = queue.upgrade() else {
                    return false;
                };
                queue.lock_recover().push_back(event.clone());
                true
            });
            reported.wakes_now()
        };
        for wake in &wakes {
            wake();
        }
    }

    /// The bearer token the host's fetch proxy takes, as this session holds it.
    pub fn set_fetch_proxy(&self, token: Option<String>) {
        self.lock().fetch_proxy = token;
    }

    /// Put the note the page shows where its live plane would be (in place of the members) in
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
            .push((Arc::downgrade(&events.0), wake));
    }

    /// A new, independent event queue.
    fn events(&self) -> Events {
        let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
        self.lock().queues.push(Arc::downgrade(&queue));
        Events(queue)
    }
}

/// One consumer's view of the session's events: every event produced after it was created,
/// independent of other consumers.
pub struct Events(Queue);

impl Events {
    /// Take every event produced since the last call.
    #[must_use]
    pub fn drain(&self) -> Vec<Event> {
        self.0.lock_recover().drain(..).collect()
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
    reports: Arc<Reports>,
    /// This link's own event queue, which [`Self::drain`] reads.
    events: Arc<Events>,
}

impl Link {
    /// A link through `outbox`, whose transport keeps `engine` and reports on `reports`.
    #[must_use]
    pub fn new(
        outbox: Arc<dyn Outbox + Send + Sync>,
        engine: Arc<Engine>,
        reports: Arc<Reports>,
    ) -> Self {
        let events = Arc::new(reports.events());
        Self {
            outbox,
            engine,
            reports,
            events,
        }
    }

    /// Where this link says what it says, for a task that must be `Send` where the link is not
    /// (a page's, whose engine stays on its thread): it carries the commands that touch no replica.
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
    /// replica's custody keeps them.
    #[must_use]
    pub fn snapshot(&self) -> crate::health::PeerSnapshot {
        let owner = self.engine.owner();
        let writes =
            self.engine
                .workspaces()
                .into_iter()
                .flat_map(|workspace| {
                    owner.record_writes(workspace).into_iter().map(
                        move |((app_id, record), writes)| {
                            (
                                DocumentRoute {
                                    app_id,
                                    cluster_id: workspace.to_string(),
                                    record,
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
    /// `None` while it has a plane: no plane could be reached.
    #[must_use]
    pub fn live_note(&self) -> Option<String> {
        self.reports.lock().live_note.clone()
    }

    /// End the session.
    pub fn close(&self) {
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
                    subject: FailureSubject::Storage,
                });
            } else if state == LocalState::Durable
                && !status
                    .local
                    .values()
                    .any(|state| *state == LocalState::Failed)
            {
                status
                    .failures
                    .retain(|failure| failure.subject != FailureSubject::Storage);
            }
        });
    }

    /// Report a failure of something the page owns: its store, a desktop surface, a surface's
    /// asset. It replaces the subject's previous one.
    ///
    /// # Errors
    ///
    /// Rejects [`FailureSubject::Session`] and [`FailureSubject::Admission`], which only the
    /// session reports, and codes outside [`PAGE_FAILURE_CODES`].
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
        self.reports.change(|status| {
            status.upsert(Failure {
                code,
                message,
                at_ms: tilo_clock::unix_now_ms(),
                subject,
            });
        });
        Ok(())
    }

    /// Clear every failure of `subject`.
    pub fn clear_failure(&self, subject: &FailureSubject) {
        self.clear(|failure| failure == subject);
    }

    /// Clear every failure of desktop surface `surface` and of its assets.
    pub fn clear_surface(&self, surface: &str) {
        self.clear(|subject| match subject {
            FailureSubject::Surface(id) | FailureSubject::Asset { surface: id, .. } => {
                id == surface
            }
            _ => false,
        });
    }

    /// Clear the failures whose subject `of` names, waking the watchers only when there were any.
    fn clear(&self, of: impl Fn(&FailureSubject) -> bool) {
        if !self
            .reports
            .read(|status| status.failures.iter().any(|failure| of(&failure.subject)))
        {
            return;
        }
        self.reports.change(|status| {
            status.failures.retain(|failure| !of(&failure.subject));
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

    /// Whose each member device of `workspace` is, as [`PeerStatus::room_devices`] says: device
    /// (hex) to person (root key, hex), read without copying the rest of the status. Empty until
    /// the workspace's governance admitted its members.
    #[must_use]
    pub fn room_devices(&self, workspace: &str) -> BTreeMap<String, String> {
        self.reports
            .read(|status| status.room_devices.get(workspace).cloned())
            .unwrap_or_default()
    }

    /// Take every event produced since the last call on this link's own queue, which its clones
    /// share. Other consumers use [`Self::events`].
    #[must_use]
    pub fn drain(&self) -> Vec<Event> {
        self.events.drain()
    }

    /// A new, independent event queue.
    #[must_use]
    pub fn events(&self) -> Events {
        self.reports.events()
    }

    /// Call `wake` whenever status or events change.
    pub fn on_change(&self, wake: Wake) {
        self.reports.lock().wakes.push(wake);
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
        if let Cmd::InstallChannelKeys(route, keys) = &command
            && let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id)
        {
            self.engine.hand_keys(
                workspace,
                collab_types::ChannelRoute::new(&route.app_id, &route.record),
                keys.clone(),
            );
        }
        // Held before the session, which reads held writes under this lock, can look: read from
        // now on, and said to the session as the route alone.
        if let Cmd::Write(route, frames) = command {
            let mut held = self.engine.held_writes().lock();
            self.outbox.send(Cmd::Write(route.clone(), Vec::new()))?;
            held.entry(route).or_default().extend(frames);
            drop(held);
            return Ok(());
        }
        self.outbox.send(command)
    }
}

/// This session as what asking another device's actions needs ([`Heard`]): the frames it carries
/// to a device's key, and what it heard back, on a queue of its own, so any number of askers on
/// one session each hear every answer.
impl From<&Link> for Arc<dyn tilo_session_core::remote::RemoteSession> {
    fn from(link: &Link) -> Self {
        Arc::new(Asking {
            outbox: Arc::clone(&link.outbox),
            reports: Arc::clone(&link.reports),
            events: link.events(),
        })
    }
}

/// One asker of another device's actions over a session.
struct Asking {
    outbox: Arc<dyn Outbox + Send + Sync>,
    reports: Arc<Reports>,
    events: Events,
}

impl tilo_session_core::remote::RemoteSession for Asking {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut status = PeerStatus::connecting([0; 32]);
        for (code, subject) in [
            ("transport-ended", FailureSubject::Session),
            ("transport-unavailable", FailureSubject::Session),
            ("transport-ended", FailureSubject::Session),
            ("storage-transaction-failed", FailureSubject::Storage),
            ("storage-quota", FailureSubject::Storage),
            ("gpu-device-lost", FailureSubject::Surface("a".into())),
            ("gpu-device-lost", FailureSubject::Surface("b".into())),
        ] {
            status.upsert(failure(code, subject));
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
                FailureSubject::Asset {
                    surface: "a".into(),
                    asset: index.to_string(),
                },
            ));
        }
        assert_eq!(status.failures.len(), MAX_FAILURES);
    }
}
