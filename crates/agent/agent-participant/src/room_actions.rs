//! Page actions on any record of a room, answered by the host.
//!
//! An agent answers `actionsV1` only for the records it has open. A person's
//! page wants the whole room: every record's actions, whether or not anyone
//! has it open, the way the old broker offered them. [`RoomActions`] is that
//! service on the host. It serves the `tilo_actions::remote` vocabulary
//! addressed to the host's own device:
//!
//! - **Catalogue.** One browsable provider per record of the room
//!   (`record:<app>:<record>`, the token agents use) and the room's own
//!   provider (`workspace:<room>`) offering `app.<app>#open`. A query naming
//!   no provider lists rows for the records open on the room's shared
//!   desktop, the most recently changed next, up to [`DEFAULT_ROWS_RECORDS`];
//!   naming a record's provider lists every action that record publishes.
//! - **Invoke.** The record is mounted headless ([`HostedApp`]) from what the
//!   service opened of it, the action runs through the app's own door
//!   (availability, confirmation and argument binding exactly as for a
//!   person), and the change is sealed and signed as the host device's own,
//!   so every page receives it as an ordinary durable change.
//! - **Tickets.** Invocation ids are idempotent: a retry reports the original
//!   invocation, a different request under a used id is a conflict, and a
//!   refused invocation is forgotten (nothing ran). An invocation is
//!   accepted at once and runs on the service's own thread; `Observe`
//!   reports where it stands, and `Cancel` stops one that has not started.
//!
//! Only the host user's own devices are served: the principal must be the
//! person whose root the host chains to. Agents use their own mounts.
//!
//! The service joins each room as the host's own device ([`RoomMember`], from
//! the [`RoomRecordsLink`] the host supplies): a member of the room's
//! channels like any other, admitted by the room's steward. It reads only
//! records it holds keys for and writes nothing in the clear.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;

use collab_types::DocumentMutation;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHostError, Caller, CallerPrincipal,
    CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueResource, CatalogueRow, Completer,
    Confirmation, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionMessage, application_open_target, pending, workspace_provider,
    workspace_provider_room,
};
use tilo_actions::{
    ActionFailure, ActionOutput, ConfirmationPolicy, InvocationId, InvocationUpdate, PROMPTED_PARAM,
};
use tilo_browser_peer::{
    DocumentRoute, MemoryPeerStore, PeerConfig, PeerEvent, PeerEvents, PeerHandle,
};
use tilo_user_identity::RevocationList;
use ui::widget::ResourceOperationStart;

use crate::apps::{AppCatalogue, HostedApp, Seed};
use crate::host::{CatalogueFactory, HostEndpoint};
use crate::participant::actions::{RECORD_PROVIDER, record_provider};

/// Records whose rows a catalogue naming no provider lists.
pub const DEFAULT_ROWS_RECORDS: usize = 12;
/// Records kept mounted between requests; the least recently used unmounts.
const MAX_MOUNTED: usize = 24;
/// How long a request waits for the service to join a room and open what
/// the room's steward admitted it to.
const JOIN_WAIT: Duration = Duration::from_secs(30);
/// Invocations remembered for retries, status and history.
const REMEMBERED: usize = 1024;

/// How the service takes part in the host's rooms: as the host's own
/// device, a member of each room's channels like any other. It opens only
/// what a room's steward admitted it to and seals everything it writes.
#[derive(Clone)]
pub struct RoomMember {
    /// The host's endpoint and certificate; the member is the host device.
    pub endpoint: HostEndpoint,
    /// The host device's key, which signs the member's changes.
    pub key: SigningKey,
    /// The runtime the member's sessions run on.
    pub runtime: tokio::runtime::Handle,
}

/// What the service needs of its host. Methods block; they are called from
/// the service's own thread, never from the host's runtime.
pub trait RoomRecordsLink: Send + Sync {
    /// The user root the host chains to: the only person served.
    fn user_public_key(&self) -> [u8; 32];
    /// How the service joins the host's rooms.
    fn member(&self) -> RoomMember;
}

struct Entry {
    caller: Caller,
    request: InvokeRequest,
    update: InvocationUpdate,
}

#[derive(Default)]
struct Ledger {
    entries: HashMap<InvocationId, Entry>,
    order: VecDeque<InvocationId>,
    running: Option<InvocationId>,
}

impl Ledger {
    fn insert(&mut self, caller: &Caller, request: InvokeRequest) {
        while self.order.len() >= REMEMBERED {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        let invocation = request.invocation;
        self.order.push_back(invocation);
        self.entries.insert(
            invocation,
            Entry {
                caller: caller.clone(),
                request,
                update: InvocationUpdate::Accepted { invocation },
            },
        );
    }

    fn forget(&mut self, id: &InvocationId) {
        self.entries.remove(id);
        self.order.retain(|candidate| candidate != id);
    }

    fn owned(&self, caller: &Caller, id: &InvocationId) -> Option<&Entry> {
        self.entries.get(id).filter(|entry| entry.caller == *caller)
    }
}

enum Job {
    Catalogue {
        workspace_id: u64,
        query: CatalogueQuery,
        done: Completer<Result<CataloguePage, ActionHostError>>,
    },
    Invoke {
        workspace_id: u64,
        invocation: InvocationId,
        done: Completer<InvocationReceipt>,
    },
}

struct Shared {
    ledger: Mutex<Ledger>,
    link: OnceLock<Arc<dyn RoomRecordsLink>>,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
}

/// The host's page actions over every record of its rooms.
#[derive(Clone)]
pub struct RoomActions {
    shared: Arc<Shared>,
    catalogue: CatalogueFactory,
}

impl RoomActions {
    /// A service mounting apps from `catalogue`; it answers once
    /// [`Self::attach`] gives it the host's records.
    #[must_use]
    pub fn new(catalogue: CatalogueFactory) -> Self {
        Self {
            shared: Arc::new(Shared {
                ledger: Mutex::new(Ledger::default()),
                link: OnceLock::new(),
                jobs: Mutex::new(None),
            }),
            catalogue,
        }
    }

    /// Start serving `link`'s rooms on the service's own thread. A second
    /// call is ignored.
    ///
    /// # Errors
    ///
    /// Returns why the thread did not start.
    pub fn attach(&self, link: Arc<dyn RoomRecordsLink>) -> Result<(), String> {
        if self.shared.link.set(Arc::clone(&link)).is_err() {
            return Ok(());
        }
        let (sender, jobs) = mpsc::channel();
        let shared = Arc::clone(&self.shared);
        let catalogue = Arc::clone(&self.catalogue);
        std::thread::Builder::new()
            .name("room-actions".into())
            .spawn(move || Worker::new(link.as_ref(), catalogue(), shared).run(&jobs))
            .map_err(|error| error.to_string())?;
        *self
            .shared
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sender);
        Ok(())
    }

    /// Stop the service thread; later requests are refused as unavailable.
    pub fn shutdown(&self) {
        self.shared
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    fn authorized(&self, caller: &Caller) -> bool {
        let Some(link) = self.shared.link.get() else {
            return false;
        };
        let owner = hex::encode(link.user_public_key());
        matches!(caller.principal(), CallerPrincipal::Person(person) if *person == owner)
    }

    /// Whether `caller` started invocation `id` here.
    #[must_use]
    pub fn owns(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.ledger().owned(caller, id).is_some()
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.shared
            .ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    #[allow(
        clippy::result_large_err,
        reason = "a job the worker cannot take goes back to its sender whole"
    )]
    fn submit(&self, job: Job) -> Result<(), Job> {
        let jobs = self
            .shared
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match jobs.as_ref() {
            Some(sender) => sender.send(job).map_err(|error| error.0),
            None => Err(job),
        }
    }

    /// Answer one request `caller` sent to the host for `workspace_id`, or
    /// `None` for a message that is not a request.
    pub fn serve(
        &self,
        workspace_id: u64,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        let unavailable = || ActionHostError::Unavailable {
            reason: "the host's room actions are not running".into(),
        };
        Some(match message {
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                let respond = move |result| RemoteActionMessage::CatalogueResponse {
                    correlation,
                    result,
                };
                if !self.authorized(caller) {
                    return Some(Pending::ready(respond(Ok(CataloguePage::from_rows(
                        "0".into(),
                        Vec::new(),
                        &query,
                    )))));
                }
                let (done, answer) = pending();
                let job = Job::Catalogue {
                    workspace_id,
                    query,
                    done,
                };
                if let Err(Job::Catalogue { done, .. }) = self.submit(job) {
                    done.complete(Err(unavailable()));
                }
                answer.map(respond)
            }
            RemoteActionMessage::Invoke(request) => {
                let invocation = request.invocation;
                let receipt = |status| {
                    Pending::ready(RemoteActionMessage::Receipt(InvocationReceipt {
                        invocation,
                        status,
                    }))
                };
                if !self.authorized(caller) {
                    return Some(receipt(ReceiptStatus::Rejected {
                        error: ActionHostError::UnknownAction,
                    }));
                }
                {
                    let mut ledger = self.ledger();
                    if let Some(entry) = ledger.entries.get(&invocation) {
                        if entry.caller != *caller || entry.request != request {
                            return Some(receipt(ReceiptStatus::Rejected {
                                error: ActionHostError::InvocationConflict,
                            }));
                        }
                        let state = InvocationState::new(invocation, entry.update.clone());
                        return Some(receipt(state.receipt().status));
                    }
                    ledger.insert(caller, request);
                }
                let (done, answer) = pending();
                let job = Job::Invoke {
                    workspace_id,
                    invocation,
                    done,
                };
                if let Err(Job::Invoke { done, .. }) = self.submit(job) {
                    self.ledger().forget(&invocation);
                    done.complete(InvocationReceipt {
                        invocation,
                        status: ReceiptStatus::Rejected {
                            error: unavailable(),
                        },
                    });
                }
                answer.map(RemoteActionMessage::Receipt)
            }
            RemoteActionMessage::Observe { invocation } => {
                let state = self
                    .ledger()
                    .owned(caller, &invocation)
                    .map(|entry| InvocationState::new(invocation, entry.update.clone()));
                Pending::ready(RemoteActionMessage::Update { invocation, state })
            }
            RemoteActionMessage::Cancel { invocation } => {
                Pending::ready(RemoteActionMessage::CancelResponse {
                    invocation,
                    result: self.cancel(caller, &invocation),
                })
            }
            RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. }
            | RemoteActionMessage::RecordRequest { .. }
            | RemoteActionMessage::RecordResponse { .. } => return None,
        })
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
        let mut ledger = self.ledger();
        let running = ledger.running == Some(*id);
        let entry = ledger
            .entries
            .get_mut(id)
            .filter(|entry| entry.caller == *caller)
            .ok_or(ActionHostError::UnknownInvocation)?;
        if entry.update.is_terminal() {
            return Err(ActionHostError::NotCancellable {
                reason: "the invocation has finished".into(),
            });
        }
        if running {
            return Err(ActionHostError::NotCancellable {
                reason: "the action is already running".into(),
            });
        }
        entry.update = InvocationUpdate::Cancelled {
            invocation: *id,
            message: Some("cancelled before it started".into()),
        };
        drop(ledger);
        Ok(())
    }
}

/// The host's whole app-serving plane: the agent supervisor's vocabulary
/// (provider `agents`) and the room's page actions (everything else).
pub struct HostActions {
    /// Answers `agents` requests.
    pub supervisor: crate::control::SupervisorHost,
    /// Answers requests about records and the room.
    pub rooms: RoomActions,
}

impl HostActions {
    /// Answer one request `caller` sent to the host for `workspace_id`, or
    /// `None` for a message that is not a request.
    pub fn serve(
        &self,
        workspace_id: u64,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        use tilo_relay_protocol::agents::AGENTS_PROVIDER;
        let supervisor = match &message {
            RemoteActionMessage::CatalogueRequest { query, .. } => {
                query.provider.as_deref() == Some(AGENTS_PROVIDER)
            }
            RemoteActionMessage::Invoke(request) => request.target.provider == AGENTS_PROVIDER,
            RemoteActionMessage::Observe { invocation }
            | RemoteActionMessage::Cancel { invocation } => !self.rooms.owns(caller, invocation),
            RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. }
            | RemoteActionMessage::RecordRequest { .. }
            | RemoteActionMessage::RecordResponse { .. } => return None,
        };
        if supervisor {
            tilo_actions::remote::serve(&self.supervisor, caller, message)
        } else {
            self.rooms.serve(workspace_id, caller, message)
        }
    }
}

/// One record mounted on the service thread.
struct Mounted {
    app: HostedApp,
    /// Digests of the bodies already merged into it.
    seen: HashSet<[u8; 32]>,
    used: u64,
}

/// One change of a record: a digest naming it, and its bytes.
type Change = ([u8; 32], Vec<u8>);

#[derive(Default)]
struct RoomState {
    room: Option<String>,
    /// Each record's changes in the order the member took them.
    records: BTreeMap<DocumentRoute, Vec<Change>>,
    /// When each record last changed, as a position in that order.
    changed: HashMap<DocumentRoute, usize>,
    revision: usize,
}

/// The service's membership of one room.
struct Room {
    peer: PeerHandle,
    events: PeerEvents,
    /// Signalled whenever the session's status or events change.
    wake: Arc<(Mutex<bool>, Condvar)>,
    ready: bool,
    /// Every change the member opened or wrote, in order.
    log: Vec<(DocumentRoute, Change)>,
}

impl Room {
    fn join(member: &RoomMember, workspace_id: u64) -> Result<Self, String> {
        let _runtime = member.runtime.enter();
        let peer = PeerHandle::start(
            PeerConfig {
                host: member.endpoint.host.clone(),
                cert: member.endpoint.host_cert.clone(),
                host_cert: member.endpoint.host_cert.clone(),
                revocations: RevocationList::new(),
            },
            Arc::new(member.key.clone()),
            Arc::new(MemoryPeerStore::default()),
        );
        let events = peer.events();
        let wake = Arc::new((Mutex::new(false), Condvar::new()));
        let signal = Arc::clone(&wake);
        peer.on_change(Arc::new(move || {
            let (flag, condvar) = &*signal;
            *flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
            condvar.notify_all();
        }));
        peer.join(workspace_id)?;
        Ok(Self {
            peer,
            events,
            wake,
            ready: false,
            log: Vec::new(),
        })
    }

    fn pump(&mut self, workspace_id: u64) {
        for event in self.events.drain() {
            match event {
                PeerEvent::Ready {
                    workspace_id: ready,
                } if ready == workspace_id => {
                    self.ready = true;
                }
                PeerEvent::Document(document) => {
                    if tilo_relay_protocol::room_workspace_id(&document.route.cluster_id).ok()
                        != Some(workspace_id)
                    {
                        continue;
                    }
                    let Some(mutation) = document.mutation() else {
                        continue;
                    };
                    let bytes = bytes_of(&mutation);
                    let digest = change_digest(&document.author, &bytes);
                    self.log.push((document.route, (digest, bytes)));
                }
                _ => {}
            }
        }
    }

    /// Whether a record of the room arrived sealed and opens once the member
    /// holds its keys.
    fn keys_pending(&self, workspace_id: u64) -> bool {
        self.peer.status().routes.iter().any(|(route, status)| {
            status.awaiting_keys > 0
                && tilo_relay_protocol::room_workspace_id(&route.cluster_id).ok()
                    == Some(workspace_id)
        })
    }

    /// Take the session's events until `settled`, or until `deadline`.
    fn settle(&mut self, workspace_id: u64, deadline: Instant, settled: impl Fn(&Self) -> bool) {
        loop {
            {
                let (flag, _) = &*self.wake;
                *flag.lock().unwrap_or_else(PoisonError::into_inner) = false;
            }
            self.pump(workspace_id);
            if settled(self) {
                return;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return;
            };
            let (flag, condvar) = &*self.wake;
            let guard = flag.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = condvar
                .wait_timeout_while(guard, left, |changed| !*changed)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

fn bytes_of(mutation: &DocumentMutation) -> Vec<u8> {
    match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes.clone(),
    }
}

fn change_digest(author: &[u8; 32], bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(author);
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

struct Worker {
    member: RoomMember,
    catalogue: AppCatalogue,
    shared: Arc<Shared>,
    participant: String,
    rooms: HashMap<u64, Room>,
    mounted: BTreeMap<DocumentRoute, Mounted>,
    clock: u64,
}

fn record_of(provider: &str, room: &str) -> Option<DocumentRoute> {
    let rest = provider.strip_prefix(RECORD_PROVIDER)?.strip_prefix(':')?;
    let (app_id, record) = rest.split_once(':')?;
    Some(DocumentRoute {
        app_id: app_id.to_owned(),
        cluster_id: room.to_owned(),
        record: record.to_owned(),
    })
}

/// What a mounted record publishes that its door runs now. A `Launchable`
/// action is delivered by launching the app, not by a record already open,
/// so it is not offered here.
fn live_actions(app: &mut HostedApp) -> Vec<tilo_actions::ActionDescriptor> {
    app.survey()
        .into_iter()
        .filter(|descriptor| {
            descriptor.contract.discovery != tilo_actions::DiscoveryScope::Launchable
        })
        .collect()
}

/// How an invocation that did not succeed ended.
enum Outcome {
    /// Nothing ran; the id is forgotten.
    Rejected(ActionHostError),
    Failed(ActionFailure),
}

fn door_outcome(error: &tilo_actions::door::DoorError) -> Outcome {
    use tilo_actions::door::DoorError;
    match error {
        DoorError::Unknown(_) | DoorError::UnregisteredType(_) => {
            Outcome::Rejected(ActionHostError::UnknownAction)
        }
        DoorError::ConfirmationRequired => Outcome::Rejected(ActionHostError::ConfirmationRequired),
        DoorError::MissingArgument(_)
        | DoorError::UnexpectedArgument(_)
        | DoorError::InvalidArguments(_) => Outcome::Rejected(ActionHostError::InvalidRequest {
            reason: error.to_string(),
        }),
        DoorError::Unavailable { reason, .. } => Outcome::Rejected(ActionHostError::Unavailable {
            reason: reason.clone(),
        }),
        DoorError::Failed(message) => Outcome::Failed(ActionFailure {
            code: error.code().to_owned(),
            message: message.clone(),
            details: None,
            retryable: false,
        }),
    }
}

impl Worker {
    fn new(link: &dyn RoomRecordsLink, catalogue: AppCatalogue, shared: Arc<Shared>) -> Self {
        Self {
            participant: hex::encode(link.user_public_key()),
            member: link.member(),
            catalogue,
            shared,
            rooms: HashMap::new(),
            mounted: BTreeMap::new(),
            clock: 0,
        }
    }

    fn run(mut self, jobs: &mpsc::Receiver<Job>) {
        while let Ok(job) = jobs.recv() {
            match job {
                Job::Catalogue {
                    workspace_id,
                    query,
                    done,
                } => done.complete(self.catalogue_page(workspace_id, &query)),
                Job::Invoke {
                    workspace_id,
                    invocation,
                    done,
                } => done.complete(self.invoke(workspace_id, invocation)),
            }
        }
        for (_, mounted) in std::mem::take(&mut self.mounted) {
            mounted.app.close();
        }
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.shared
            .ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The member's view of `workspace_id`, joining the room first, and
    /// every change the mounted records have not merged yet.
    fn refresh(&mut self, workspace_id: u64) -> Result<RoomState, ActionHostError> {
        if !self.rooms.contains_key(&workspace_id) {
            let room = Room::join(&self.member, workspace_id)
                .map_err(|reason| ActionHostError::Unavailable { reason })?;
            self.rooms.insert(workspace_id, room);
        }
        let room =
            self.rooms
                .get_mut(&workspace_id)
                .ok_or_else(|| ActionHostError::Unavailable {
                    reason: "the room was not joined".into(),
                })?;
        let deadline = Instant::now()
            .checked_add(JOIN_WAIT)
            .unwrap_or_else(Instant::now);
        room.settle(workspace_id, deadline, |room| {
            room.ready && !room.keys_pending(workspace_id)
        });
        if !room.ready {
            return Err(ActionHostError::Unavailable {
                reason: "the host has not joined the room yet".into(),
            });
        }
        let mut state = RoomState::default();
        for (position, (route, change)) in room.log.iter().enumerate() {
            state.room.get_or_insert_with(|| route.cluster_id.clone());
            state.changed.insert(route.clone(), position);
            state
                .records
                .entry(route.clone())
                .or_default()
                .push(change.clone());
        }
        state.revision = room.log.len();
        for (route, mounted) in &mut self.mounted {
            let Some(changes) = state.records.get(route) else {
                continue;
            };
            for (digest, bytes) in changes {
                if mounted.seen.insert(*digest)
                    && let Err(error) = mounted
                        .app
                        .apply(&DocumentMutation::Incremental(bytes.clone()))
                {
                    tracing::warn!(%error, "room actions: a change did not merge");
                }
            }
        }
        Ok(state)
    }

    /// The record at `route`, mounted from what the room retains.
    fn mount(&mut self, route: &DocumentRoute, state: &RoomState) -> Result<&mut Mounted, String> {
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        if !self.mounted.contains_key(route) {
            let changes = state
                .records
                .get(route)
                .ok_or_else(|| format!("the room has no record {}", route.record))?;
            while self.mounted.len() >= MAX_MOUNTED {
                let Some(oldest) = self
                    .mounted
                    .iter()
                    .min_by_key(|(_, mounted)| mounted.used)
                    .map(|(route, _)| route.clone())
                else {
                    break;
                };
                if let Some(mounted) = self.mounted.remove(&oldest) {
                    mounted.app.close();
                }
            }
            let app = HostedApp::open(
                &self.catalogue,
                &route.app_id,
                &route.cluster_id,
                &self.participant,
                false,
                Seed::Existing {
                    record: ui::record::RecordId::parse(&route.record)
                        .ok_or_else(|| format!("{} is not a record id", route.record))?,
                    changes: changes.iter().map(|(_, bytes)| bytes.clone()).collect(),
                },
            )
            .map_err(|error| error.to_string())?;
            self.mounted.insert(
                route.clone(),
                Mounted {
                    app,
                    seen: changes.iter().map(|(digest, _)| *digest).collect(),
                    used: clock,
                },
            );
        }
        let mounted = self
            .mounted
            .get_mut(route)
            .ok_or_else(|| "the record did not mount".to_string())?;
        mounted.used = clock;
        Ok(mounted)
    }

    /// Records of the room an app of the catalogue opens: those on the
    /// shared desktop first, then the most recently changed.
    fn records(&self, state: &RoomState) -> Vec<DocumentRoute> {
        let desktop: BTreeMap<DocumentRoute, Vec<Vec<u8>>> = state
            .records
            .iter()
            .map(|(route, changes)| {
                (
                    route.clone(),
                    changes.iter().map(|(_, bytes)| bytes.clone()).collect(),
                )
            })
            .collect();
        let on_desktop = state
            .room
            .as_deref()
            .map(|room| crate::participant::desktop_records(&desktop, room))
            .unwrap_or_default();
        let mut routes: Vec<DocumentRoute> = state
            .records
            .keys()
            .filter(|route| self.catalogue.get(&route.app_id).is_some())
            .cloned()
            .collect();
        routes.sort_by_key(|route| {
            (
                !on_desktop.contains(&route.record),
                std::cmp::Reverse(state.changed.get(route).copied().unwrap_or(0)),
            )
        });
        routes
    }

    fn catalogue_page(
        &mut self,
        workspace_id: u64,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, ActionHostError> {
        let state = self.refresh(workspace_id)?;
        let revision = state.revision.to_string();
        let Some(room) = state.room.clone() else {
            return Ok(CataloguePage::from_rows(revision, Vec::new(), query));
        };
        let routes = self.records(&state);
        let workspace = workspace_provider(&room);
        let wanted: Vec<DocumentRoute> = match query.provider.as_deref() {
            Some(provider) if provider == workspace => Vec::new(),
            Some(provider) => {
                let route = record_of(provider, &room)
                    .filter(|route| routes.contains(route))
                    .ok_or(ActionHostError::UnknownAction)?;
                vec![route]
            }
            None => routes.iter().take(DEFAULT_ROWS_RECORDS).cloned().collect(),
        };
        let mut rows = Vec::new();
        for route in &wanted {
            let provider = record_provider(route);
            let descriptors = match self.mount(route, &state) {
                Ok(mounted) => live_actions(&mut mounted.app),
                Err(error) => {
                    tracing::warn!(%error, record = %route.record, "room actions: a record did not mount");
                    continue;
                }
            };
            rows.extend(descriptors.into_iter().map(|descriptor| CatalogueRow {
                provider: provider.clone(),
                app_id: route.app_id.clone(),
                parent_resource: String::new(),
                resource: CatalogueResource {
                    id: descriptor.address.resource.clone(),
                    kind: descriptor.resource_kind.clone(),
                    label: descriptor.resource_label.clone(),
                    presentation: None,
                },
                action: Some(descriptor),
                has_children: false,
                visibility: None,
            }));
        }
        if query
            .provider
            .as_deref()
            .is_none_or(|provider| provider == workspace)
        {
            rows.extend(self.open_rows(&room));
        }
        let mut page = CataloguePage::from_rows(revision, rows, query);
        if query.provider.is_none() {
            page.providers = routes
                .iter()
                .map(|route| CatalogueProvider {
                    provider: record_provider(route),
                    app_id: route.app_id.clone(),
                    room: room.clone(),
                    record_id: route.record.clone(),
                    browsable: true,
                })
                .chain(std::iter::once(CatalogueProvider {
                    provider: workspace,
                    app_id: String::new(),
                    room: room.clone(),
                    record_id: String::new(),
                    browsable: false,
                }))
                .collect();
        }
        Ok(page)
    }

    fn open_rows(&self, room: &str) -> Vec<CatalogueRow> {
        self.catalogue
            .iter()
            .map(|definition| {
                let app = definition.app_id;
                let target = application_open_target(room, app);
                CatalogueRow {
                    provider: target.provider.clone(),
                    app_id: app.to_owned(),
                    parent_resource: String::new(),
                    resource: CatalogueResource {
                        id: target.address.resource.clone(),
                        kind: "applications.app".into(),
                        label: app.to_owned(),
                        presentation: None,
                    },
                    action: Some(tilo_actions::door::ActionDescriptor {
                        address: target.address,
                        resource_kind: "applications.app".into(),
                        resource_label: app.to_owned(),
                        action_label: format!("Open {app}"),
                        description: Some(format!("Open {app} on a new record in this room.")),
                        locus: tilo_actions::ExecutionLocus::ServerPreferred,
                        kind: tilo_actions::OperationKind::Effect,
                        contract: tilo_actions::ActionContract::default(),
                        params: Vec::new(),
                        input: tilo_actions::InputSpec::None,
                        view: None,
                    }),
                    has_children: false,
                    visibility: None,
                }
            })
            .collect()
    }

    fn invoke(&mut self, workspace_id: u64, invocation: InvocationId) -> InvocationReceipt {
        let request = {
            let mut ledger = self.ledger();
            let Some(entry) = ledger.entries.get(&invocation) else {
                return InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Rejected {
                        error: ActionHostError::UnknownInvocation,
                    },
                };
            };
            if entry.update.is_terminal() {
                // Cancelled before it started.
                return InvocationState::new(invocation, entry.update.clone()).receipt();
            }
            let request = entry.request.clone();
            ledger.running = Some(invocation);
            request
        };
        let outcome = self.perform(workspace_id, &request);
        let target = &request.target;
        match &outcome {
            Ok(_) => {
                tracing::info!(provider = %target.provider, resource = %target.address.resource,
                action = %target.address.action, "room actions: succeeded");
            }
            Err(Outcome::Failed(error)) => tracing::info!(provider = %target.provider,
                resource = %target.address.resource, action = %target.address.action,
                code = %error.code, message = %error.message, "room actions: failed"),
            Err(Outcome::Rejected(error)) => tracing::info!(provider = %target.provider,
                resource = %target.address.resource, action = %target.address.action,
                code = error.code(), %error, "room actions: refused"),
        }
        let mut ledger = self.ledger();
        ledger.running = None;
        let update = match outcome {
            Ok(output) => InvocationUpdate::Succeeded { invocation, output },
            Err(Outcome::Failed(error)) => InvocationUpdate::Failed { invocation, error },
            Err(Outcome::Rejected(error)) => {
                ledger.forget(&invocation);
                return InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Rejected { error },
                };
            }
        };
        if let Some(entry) = ledger.entries.get_mut(&invocation) {
            entry.update = update.clone();
        }
        drop(ledger);
        InvocationState::new(invocation, update).receipt()
    }

    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn perform(
        &mut self,
        workspace_id: u64,
        request: &InvokeRequest,
    ) -> Result<ActionOutput, Outcome> {
        let state = self.refresh(workspace_id).map_err(Outcome::Rejected)?;
        if request
            .revision
            .as_ref()
            .is_some_and(|revision| *revision != state.revision.to_string())
        {
            return Err(Outcome::Rejected(ActionHostError::Refused {
                reason: "the room changed since that revision".into(),
            }));
        }
        let room = state.room.clone();
        let target = &request.target;
        if let Some(room) = workspace_provider_room(&target.provider) {
            if tilo_relay_protocol::room_workspace_id(room).ok() != Some(workspace_id) {
                return Err(Outcome::Rejected(ActionHostError::UnknownAction));
            }
            let app = target
                .address
                .resource
                .strip_prefix(APPLICATION_RESOURCE_PREFIX)
                .filter(|_| target.address.action == APPLICATION_OPEN_ACTION)
                .filter(|app| self.catalogue.get(app).is_some())
                .ok_or(Outcome::Rejected(ActionHostError::UnknownAction))?
                .to_owned();
            return self.open_new(workspace_id, room, &app);
        }
        let room = room.ok_or(Outcome::Rejected(ActionHostError::UnknownAction))?;
        let route = record_of(&target.provider, &room)
            .filter(|route| self.catalogue.get(&route.app_id).is_some())
            .ok_or(Outcome::Rejected(ActionHostError::UnknownAction))?;
        let arguments = match &request.argument {
            InvokeArgument::None => serde_json::Map::new(),
            InvokeArgument::Text(text) => {
                let mut arguments = serde_json::Map::new();
                arguments.insert(PROMPTED_PARAM.into(), text.clone().into());
                arguments
            }
            InvokeArgument::Json(arguments) => arguments.clone(),
        };
        let mounted = self
            .mount(&route, &state)
            .map_err(|_| Outcome::Rejected(ActionHostError::UnknownAction))?;
        let descriptor = live_actions(&mut mounted.app)
            .into_iter()
            .find(|descriptor| descriptor.address == target.address)
            .ok_or(Outcome::Rejected(ActionHostError::UnknownAction))?;
        let confirmed = match (&descriptor.contract.confirmation, &request.confirmation) {
            (ConfirmationPolicy::Phrase { phrase }, Some(Confirmation::Phrase(given))) => {
                phrase == given
            }
            (ConfirmationPolicy::Phrase { .. }, _) => false,
            (_, confirmation) => confirmation.is_some(),
        };
        let started = mounted
            .app
            .invoke(&target.address, &arguments, confirmed)
            .map_err(|error| door_outcome(&error))?;
        let mutation = mounted.app.take_mutation();
        if let Some(mutation) = mutation {
            self.commit(workspace_id, &route, &mutation)
                .map_err(|reason| {
                    Outcome::Failed(ActionFailure {
                        code: "commit_failed".into(),
                        message: reason,
                        details: None,
                        retryable: true,
                    })
                })?;
        }
        Ok(match started {
            ResourceOperationStart::Completed(output) => output,
            ResourceOperationStart::Accepted => ActionOutput::default(),
        })
    }

    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    fn open_new(
        &mut self,
        workspace_id: u64,
        room: &str,
        app: &str,
    ) -> Result<ActionOutput, Outcome> {
        let refused = |reason: String| Outcome::Rejected(ActionHostError::Refused { reason });
        let mut hosted = HostedApp::open(
            &self.catalogue,
            app,
            room,
            &self.participant,
            false,
            Seed::New,
        )
        .map_err(|error| refused(error.to_string()))?;
        let route = DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: room.to_owned(),
            record: hosted.record().to_owned(),
        };
        if let Some(mutation) = hosted.take_mutation() {
            self.commit(workspace_id, &route, &mutation)
                .map_err(refused)?;
        }
        hosted.close();
        Ok(ActionOutput {
            value: Some(serde_json::json!({
                "provider": record_provider(&route),
                "record": route.record,
            })),
            resources: Vec::new(),
        })
    }

    /// Publish `mutation` as the member: sealed with the record's channel
    /// keys, or held until the room's steward admits the member to it.
    fn commit(
        &mut self,
        workspace_id: u64,
        route: &DocumentRoute,
        mutation: &DocumentMutation,
    ) -> Result<(), String> {
        let room = self
            .rooms
            .get_mut(&workspace_id)
            .ok_or("the room was not joined")?;
        room.peer.publish(route.clone(), mutation)?;
        let bytes = bytes_of(mutation);
        let digest = change_digest(&self.member.endpoint.host_cert.device_public_key, &bytes);
        room.log.push((route.clone(), (digest, bytes)));
        if let Some(mounted) = self.mounted.get_mut(route) {
            mounted.seen.insert(digest);
        }
        Ok(())
    }
}
