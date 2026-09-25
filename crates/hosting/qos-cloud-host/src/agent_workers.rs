//! Ordinary certified Cloud workers. Store is the only durable document owner;
//! the shared broker journal retains generic invocation uncertainty only.
use crate::agent_runtime_adapter::RuntimeOwner;
use collab_types::{DocumentMutation, caps::Actor};
use ed25519_dalek::Signer;
use qos_action_broker::{
    collab_broker::{AppActionBroker, BrokerHandle, CrdtRoute},
    collab_broker_capnp::{PublishedInput, PublishedSurface},
    collab_outbound::{PeerPublishSink, PeerPublisher},
    host_documents::{HostDocuments, PendingHostDocument},
};
use qos_agent_control_protocol::GrantAction;
use qos_peer_state::{NeighbourMsg, StatePeer, owner_commit::OwnerCommitService};
use qos_user_identity::DeviceCert;
use std::{
    cell::RefCell,
    os::{fd::IntoRawFd, unix::fs::DirBuilderExt},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};
use tokio::sync::mpsc;
use ui::{
    application::{ComponentCloseReport, HostContext, LaunchTarget, RestoreInput},
    collaboration::{CertifiedEndpointIdentity, CollaborationManager, RouteSpec},
    host_worker::HostWorker,
    permissions::PermissionContext,
    portable_storage::{PortableStorage, StorageKey},
    record::{RecordCheckout, RecordId, RecordOwners, RecordStore},
};

struct NetworkSink {
    sender: mpsc::Sender<NeighbourMsg>,
    documents: HostDocuments,
    owner: Arc<RuntimeOwner>,
    peer: u64,
}
impl NetworkSink {
    fn send(&self, message: NeighbourMsg) {
        if self.sender.try_send(message).is_err() {
            self.owner.stop_readiness();
            self.documents.fail("worker outbound queue unavailable");
        }
    }
}
impl PeerPublishSink for NetworkSink {
    fn app_document(&self, _: String, _: String, _: String, _: collab_types::SignedOp) {
        self.owner.stop_readiness();
        self.documents
            .fail("unexpected legacy worker document publication");
    }
    fn app_presence(
        &self,
        app_id: String,
        cluster_id: String,
        presence: collab_types::PeerPresence,
    ) {
        self.send(NeighbourMsg::AppPresenceV2 {
            peer: self.peer,
            app_id,
            cluster_id,
            presence,
        });
    }
    fn scrollback_request(&self, artifact: collab_types::ArtifactId, before_seq: u64, max: u16) {
        self.send(NeighbourMsg::ScrollbackRequest {
            artifact,
            before_seq,
            max,
            requester: self.peer,
        });
    }
}
struct SocketDirectory(PathBuf);
impl SocketDirectory {
    fn new() -> Result<Self, String> {
        // Keep Unix socket paths short even when the durable Store path is long.
        let path = Path::new("/tmp").join(format!("qos-cloud-worker-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}
impl Drop for SocketDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Worker {
    app: String,
    record: String,
    checkout: RecordCheckout,
    records: RecordStore,
    runner: HostWorker,
    heads: Vec<ui::automerge::ChangeHash>,
    #[cfg(test)]
    authorizer: ui::host_worker::PublishedInvocationAuthorizer,
}
struct Snapshot {
    generation: u64,
    peer: Rc<StatePeer>,
    private_v2_ready: bool,
}
/// The last committed peer the workers observed under the Store lock.
///
/// Opening Store and restoring the governed peer on every 10 ms runtime tick
/// saturated the agent owner thread and starved every other Store user. A
/// tick reuses this peer without Store custody only while `verified` holds:
/// the Store files are unchanged since the locked observation (any commit by
/// any process changes them), the policy time is inside the peer's exact
/// interval (no certificate issue/expiry or revocation instant passed), and
/// the locked proof is younger than `store_hint::STORE_RECHECK`. Otherwise the
/// tick opens Store; an unchanged generation inside the interval still reuses
/// the restored peer. The time-dependent owner checks run on every tick.
struct PeerCache {
    generation: u64,
    verified: Option<crate::agent_runtime_adapter::store_hint::Verified>,
    at_ns: u64,
    until_ns: u64,
    peer: Rc<StatePeer>,
    private_v2_ready: bool,
    /// Worker permission context for exactly this restored peer; its sampler
    /// keeps its own locked-verdict hint across ticks.
    permission: Option<PermissionContext>,
}
/// How long a last good publication stands in for a not-ready recomputation
/// while the transport is live and no owner failure occurred: a committed
/// refresh (new generation) or a worker surface republish in progress. Any
/// real failure path still withdraws at once, and governance changes are
/// never masked: the retained publication keeps its original governance
/// revision, which admission compares with the current caller.
const PUBLICATION_HOLD: std::time::Duration = std::time::Duration::from_secs(10);
/// Longest one reconcile pass keeps mounting new workers (at least one per
/// pass). Mounting a worker takes tens to hundreds of milliseconds; a whole
/// workspace (dozens of records) mounted in one owner tick held that thread
/// for seconds, and every command queued behind it.
const MOUNT_BUDGET: std::time::Duration = std::time::Duration::from_millis(25);
/// A worker whose record lost its last window (and is used by no active
/// agent task) stays mounted this long, so a quick reopen is instant; then
/// it unmounts. Records ever opened but closed would otherwise each keep a
/// headless worker (and its pump CPU) for the life of the host.
const UNMOUNT_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
/// Records that need a worker at one Store generation and agent task set.
struct WantedRecords {
    generation: u64,
    tasks: Vec<(String, Vec<GrantAction>)>,
    records: std::collections::BTreeSet<(String, String)>,
}
/// Retry back-off of a skipped worker record.
const MOUNT_RETRY_MIN: std::time::Duration = std::time::Duration::from_secs(1);
const MOUNT_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);
/// Why one worker mount failed: a refused or failed connection skips only
/// that record; anything else (owner, custody, component setup with its
/// recovery reports) keeps failing the whole reconcile as before.
enum MountError {
    Record(String),
    Fatal(String),
}
impl From<String> for MountError {
    fn from(error: String) -> Self {
        Self::Fatal(error)
    }
}
impl From<&str> for MountError {
    fn from(error: &str) -> Self {
        Self::Fatal(error.into())
    }
}
/// Test only (this thread): the mount pass budget of the next started
/// workers, and records whose worker connection is refused.
#[cfg(test)]
type TestMount = (
    std::time::Duration,
    std::collections::BTreeSet<(String, String)>,
    std::time::Duration,
);
#[cfg(test)]
std::thread_local! {
    pub(crate) static TEST_MOUNT: RefCell<TestMount> =
        RefCell::new((std::time::Duration::MAX, Default::default(), UNMOUNT_GRACE));
}
fn mount_retry_delay(attempt: u32) -> std::time::Duration {
    MOUNT_RETRY_MIN
        .saturating_mul(1u32 << attempt.min(8))
        .min(MOUNT_RETRY_MAX)
}
/// One live agent task, from the host's own ledger (never from the agent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActiveAgentTask {
    pub(crate) task: String,
    pub(crate) agent: String,
    /// Display name shown beside the agent's caret.
    pub(crate) name: String,
    pub(crate) grants: Vec<GrantAction>,
}
/// Inert publication facts only. The lifecycle combines these with real
/// managed-runtime, gateway, transport and explicitly selected schema readiness.
#[derive(Clone)]
pub(crate) struct WorkerPublications {
    pub(crate) room: String,
    pub(crate) host_device: [u8; 32],
    pub(crate) governance_revision: String,
    pub(crate) publication_revision: String,
    pub(crate) grants: Vec<GrantAction>,
    pub(crate) ready: bool,
    pub(crate) native_v5_ready: bool,
    /// Observed with the same Store generation as the publication, so the
    /// lifecycle needs no separate Store open to publish provider state.
    pub(crate) private_v2_ready: bool,
    pub(crate) machine: String,
}
pub(crate) struct AgentWorkers {
    // Broker joins its owned thread before the private socket directory drops.
    broker: BrokerHandle,
    owner: Arc<RuntimeOwner>,
    documents: HostDocuments,
    root: PathBuf,
    certificate: DeviceCert,
    presentation: Vec<u8>,
    room: String,
    principal: String,
    cache: PortableStorage,
    owners: RecordOwners,
    views: Rc<ui::application_view::InstanceViewStore>,
    workers: Vec<Worker>,
    recovery: Vec<ComponentCloseReport>,
    generation: Option<u64>,
    machine: String,
    peer_cache: RefCell<Option<PeerCache>>,
    last_good: RefCell<Option<(std::time::Instant, WorkerPublications)>>,
    transport_ready: bool,
    closed: bool,
    /// Managed sender for enrolled task children (None when unavailable).
    steps: Option<crate::agent_sender::AgentSteps>,
    /// `(app, record, participant)` views joined for live agent tasks.
    participants: std::collections::BTreeSet<(String, String, String)>,
    /// Host-known agent id of each live task (never from the agent).
    task_agents: std::collections::BTreeMap<String, String>,
    /// Validated records of the current generation still waiting for their
    /// worker. Mounting is bounded per call ([`MOUNT_BUDGET`]) so a large
    /// workspace never holds the owner thread for seconds.
    pending: std::collections::VecDeque<(String, String)>,
    /// Records whose worker connection was refused or failed before mount
    /// (for example the broker's host-worker capacity): skipped, not fatal,
    /// and retried after `(attempt, due)` with back-off.
    skipped: std::collections::BTreeMap<(String, String), (u32, std::time::Instant)>,
    /// Work bound of one mount pass (tests may set it).
    mount_budget: std::time::Duration,
    /// Live agent tasks from the last participant sync (host ledger).
    agent_tasks: Vec<ActiveAgentTask>,
    /// Records that need a worker, derived per Store generation: every
    /// record with an open window in the room, and every record an active
    /// agent task was granted or opened. `None` until first computed.
    wanted: Option<WantedRecords>,
    /// Mounted workers no longer wanted, since when: unmounted after
    /// [`UNMOUNT_GRACE`] (cancelled when wanted again).
    unwanted_since: std::collections::BTreeMap<(String, String), std::time::Instant>,
    /// Grace before an unwanted worker unmounts (tests may set it).
    unmount_grace: std::time::Duration,
    /// Test only: time spent in `reconcile` + worker pumps (no Store work).
    /// Test only: cumulative (worker-set bookkeeping, mounting) time.
    #[cfg(test)]
    timing: (std::time::Duration, std::time::Duration),
    #[cfg(test)]
    step_service: Arc<crate::agent_sender::CloudStepService>,
    _socket: SocketDirectory,
}
impl AgentWorkers {
    pub(crate) fn start(
        root: &Path,
        owner: Arc<RuntimeOwner>,
        outbound: mpsc::Sender<NeighbourMsg>,
    ) -> Result<(Self, mpsc::Receiver<PendingHostDocument>), String> {
        owner.policy_context().mark_started();
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let (snapshot, certificate, presentation, machine) = {
            let store = owner.store()?;
            // Source preserves the configured spelling (for example /var), while
            // this worker uses a canonical path. Compare the actual directory to
            // the already-admitted held Store descriptor, not those spellings.
            let worker_directory = std::fs::File::open(&root).map_err(|e| e.to_string())?;
            let worker_identity =
                atomic_file::FileIdentity::of(&worker_directory).map_err(|e| e.to_string())?;
            let owner_identity =
                atomic_file::FileIdentity::of(&store.directory).map_err(|e| e.to_string())?;
            if worker_identity != owner_identity {
                return Err("worker root differs from Store owner".into());
            }
            let peer = store.peer().map_err(|e| e.to_string())?;
            check_peer(&peer, store.policy_now().map_err(|e| e.to_string())?)?;
            let certificate = peer.authority().clone();
            let presentation = runtime_collab::CertifiedJoinIdentity {
                workspace_id: peer.room().into(),
                device_certificate: serde_json::to_vec(&certificate).map_err(|e| e.to_string())?,
                proof: store
                    .key()
                    .sign(&runtime_identity::collaboration_join_signing_bytes(
                        peer.room(),
                    ))
                    .to_bytes()
                    .to_vec(),
            }
            .encode()
            .map_err(|e| e.to_string())?;
            (
                Snapshot {
                    generation: store.state.generation,
                    peer: Rc::new(peer),
                    private_v2_ready: matches!(
                        store.state.private_agent_ledger,
                        Some(qos_peer_state::owner_commit::PrivateAgentState::V2(_))
                    ),
                },
                certificate,
                presentation,
                store.state.machine_id.clone(),
            )
        }; // No Store lock may span shared owner health, broker or worker calls.
        let actions = root.join("actions");
        private_directory(&actions)?;
        if actions
            .join("recovery.json")
            .try_exists()
            .map_err(|e| e.to_string())?
        {
            return Err("unresolved application recovery; explicit recovery required".into());
        }
        let socket = SocketDirectory::new()?;
        let (documents, requests) = HostDocuments::new(Arc::new(owner.ordinary()));
        let (step_service, step_requests) = crate::agent_sender::CloudStepService::channel();
        let step_service = Arc::new(step_service);
        let publisher = PeerPublisher::with_owner_commit(
            Arc::new(NetworkSink {
                sender: outbound,
                documents: documents.clone(),
                owner: owner.clone(),
                peer: qos_peer_state::peer_handle(&certificate.device_public_key),
            }),
            Arc::new(documents.clone()),
        );
        let broker = qos_action_broker::collab_broker::spawn_native_peer_with_services(
            socket.0.join("broker.sock"),
            owner.clone(),
            publisher,
            qos_action_broker::peer_actions::NativePeerBrokerConfig {
                journal_root: actions.join("journal"),
                certificate_authority: owner.clone(),
            },
            qos_action_broker::services::HostServices {
                enrolled_agents: Some(owner.clone()),
                enrolled_steps: Some(step_service.clone()),
                // The workspace desktop: people and agent tasks open
                // applications through the same owner-checked command.
                desktop_commands: Some(Arc::new(crate::desktop_commands::CloudDesktopCommands {
                    room: snapshot.peer.room().to_owned(),
                    owner: owner.clone(),
                })),
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
        let room = snapshot.peer.room().to_owned();
        let (_, _, principal) = snapshot.peer.permissions(
            certificate.device_public_key,
            crate::now_time(owner.policy_time()?.now_ns().map_err(|e| e.to_string())?),
        )?;
        let cache = PortableStorage::new(
            "cloud-agent-workers",
            &hex::encode(certificate.device_public_key),
        )?;
        let views = Rc::new(
            ui::application_view::InstanceViewStore::portable(cache.clone(), &room)
                .map_err(|e| e.to_string())?,
        );
        let owners = RecordOwners::portable(cache.clone());
        let mut this = Self {
            broker,
            owner,
            documents,
            root,
            certificate,
            presentation,
            room,
            principal,
            cache,
            owners,
            views,
            workers: vec![],
            recovery: vec![],
            generation: None,
            machine,
            peer_cache: RefCell::new(None),
            last_good: RefCell::new(None),
            transport_ready: false,
            closed: false,
            steps: None,
            participants: Default::default(),
            task_agents: Default::default(),
            pending: Default::default(),
            skipped: Default::default(),
            // Existing tests observe a fully mounted start; the bound and
            // refusals are exercised explicitly by their own tests.
            #[cfg(not(test))]
            mount_budget: MOUNT_BUDGET,
            #[cfg(test)]
            mount_budget: TEST_MOUNT.with(|test| test.borrow().0),
            agent_tasks: Vec::new(),
            wanted: None,
            unwanted_since: Default::default(),
            #[cfg(not(test))]
            unmount_grace: UNMOUNT_GRACE,
            #[cfg(test)]
            unmount_grace: TEST_MOUNT.with(|test| test.borrow().2),
            #[cfg(test)]
            timing: Default::default(),
            #[cfg(test)]
            step_service,
            _socket: socket,
        };
        if let Err(error) = this.reconcile(&snapshot) {
            return Err(match this.close() {
                Ok(()) => error,
                Err(recovery) => format!("{error}; {recovery}"),
            });
        }
        // Install the managed sender and reconcile every retained child with
        // its durable parent step before any new step can be accepted. An
        // unavailable sender leaves children retained for explicit recovery;
        // step requests are then refused, never re-executed.
        match this.app_broker().and_then(|broker| {
            crate::agent_sender::AgentSteps::install(
                &this.owner,
                &this.root,
                &broker,
                step_requests,
            )
        }) {
            Ok(steps) => this.steps = Some(steps),
            Err(error) => {
                tracing::warn!(%error, "managed sender unavailable; enrolled steps refused")
            }
        }
        Ok((this, requests))
    }
    pub(crate) fn broker(&self) -> &BrokerHandle {
        &self.broker
    }
    pub(crate) fn app_broker(&self) -> Result<AppActionBroker, String> {
        self.broker
            .app_action_broker()
            .ok_or_else(|| "worker broker unavailable".into())
    }
    /// Only the lifecycle's acknowledged live relay connection can set true.
    /// Queued signing work waits for reconnection; withdrawing readiness grants
    /// no new admission or dispatch and does not replay an existing effect.
    pub(crate) fn set_transport_ready(&mut self, ready: bool) {
        self.transport_ready = ready && !self.closed;
        if !self.transport_ready {
            self.last_good.borrow_mut().take();
            self.owner.stop_readiness();
        }
    }
    pub(crate) fn fail(&self, reason: impl Into<String>) -> String {
        self.last_good.borrow_mut().take();
        self.owner.stop_readiness();
        self.documents.fail(reason)
    }
    /// Store generation of the last worker observation (a cheap change signal
    /// for the lifecycle's provider publication).
    pub(crate) fn observed_generation(&self) -> Option<u64> {
        self.peer_cache
            .borrow()
            .as_ref()
            .map(|cache| cache.generation)
    }
    fn snapshot(&self) -> Result<Snapshot, String> {
        // The installed policy time fails once the Store Source is retired or
        // the runtime closed, so the unlocked path observes that as well.
        let time = self.owner.policy_time()?;
        let now_ns = time.now_ns().map_err(|e| e.to_string())?;
        let reused = self.peer_cache.borrow().as_ref().and_then(|cache| {
            cache
                .verified
                .is_some_and(|verified| verified.current(&self.root, now_ns))
                .then(|| Snapshot {
                    generation: cache.generation,
                    peer: cache.peer.clone(),
                    private_v2_ready: cache.private_v2_ready,
                })
        });
        let snapshot = match reused {
            Some(snapshot) => snapshot,
            None => self.snapshot_locked(&time, now_ns)?,
        };
        check_peer(&snapshot.peer, crate::now_time(now_ns))?;
        if snapshot.peer.room() != self.room || snapshot.peer.authority() != &self.certificate {
            return Err("worker owner scope changed".into());
        }
        Ok(snapshot)
    }
    /// Store custody only for this observation; the lock is released on return.
    fn snapshot_locked(
        &self,
        time: &qos_action_broker::peer_actions::PolicyTime,
        now_ns: u64,
    ) -> Result<Snapshot, String> {
        let store = self.owner.store_for(time)?;
        let generation = store.state.generation;
        let verified = crate::agent_runtime_adapter::store_hint::Verified::under_lock(
            &self.root,
            &store.state,
            now_ns,
        );
        let private_v2_ready = matches!(
            store.state.private_agent_ledger,
            Some(qos_peer_state::owner_commit::PrivateAgentState::V2(_))
        );
        let mut cache = self.peer_cache.borrow_mut();
        if let Some(cache) = cache.as_mut().filter(|cache| {
            cache.generation == generation && cache.at_ns <= now_ns && now_ns < cache.until_ns
        }) {
            cache.verified = verified;
            cache.private_v2_ready = private_v2_ready;
            return Ok(Snapshot {
                generation,
                peer: cache.peer.clone(),
                private_v2_ready,
            });
        }
        let peer = Rc::new(store.peer().map_err(|e| e.to_string())?);
        let until_ns =
            crate::agent_runtime_adapter::store_hint::peer_reuse_until(&store.state, now_ns);
        drop(store);
        *cache = Some(PeerCache {
            generation,
            verified,
            at_ns: now_ns,
            until_ns,
            peer: peer.clone(),
            private_v2_ready,
            permission: None,
        });
        Ok(Snapshot {
            generation,
            peer,
            private_v2_ready,
        })
    }
    /// The worker permission context for this exact restored peer, built once
    /// per restore instead of on every tick.
    fn permission_for(&self, snapshot: &Snapshot) -> Result<PermissionContext, String> {
        let mut cache = self.peer_cache.borrow_mut();
        let slot = cache
            .as_mut()
            .filter(|cache| Rc::ptr_eq(&cache.peer, &snapshot.peer));
        match slot {
            Some(cache) => {
                if cache.permission.is_none() {
                    cache.permission =
                        Some(permission(&self.owner, &snapshot.peer, &self.certificate)?);
                }
                Ok(cache.permission.clone().expect("permission retained"))
            }
            None => permission(&self.owner, &snapshot.peer, &self.certificate),
        }
    }
    /// Validate and refresh the current generation's records, then mount at
    /// most one bounded batch of new workers.
    fn reconcile(&mut self, snapshot: &Snapshot) -> Result<(), String> {
        #[cfg(test)]
        let began = std::time::Instant::now();
        if self.generation != Some(snapshot.generation) {
            self.refresh(snapshot)?;
        }
        self.schedule(snapshot)?;
        #[cfg(test)]
        let mounting = std::time::Instant::now();
        let mounted = self.mount_pending(snapshot);
        #[cfg(test)]
        {
            self.timing.0 += mounting - began;
            self.timing.1 += mounting.elapsed();
        }
        mounted
    }
    /// Which records need a worker now (see [`WantedRecords`]). Recomputed
    /// only when the Store generation or the agent task set changes.
    fn wanted_records(
        &mut self,
        snapshot: &Snapshot,
    ) -> Result<&std::collections::BTreeSet<(String, String)>, String> {
        let tasks: Vec<(String, Vec<GrantAction>)> = self
            .agent_tasks
            .iter()
            .map(|task| (task.task.clone(), task.grants.clone()))
            .collect();
        let current = self
            .wanted
            .as_ref()
            .is_some_and(|w| w.generation == snapshot.generation && w.tasks == tasks);
        if !current {
            let mut records = visible_records(&snapshot.peer);
            if !tasks.is_empty() {
                let rows = snapshot.peer.rows();
                for (task, grants) in &tasks {
                    // Records the task's retained grants cover.
                    if let Ok(scopes) = qos_agent_control_protocol::scopes_from_grants(grants) {
                        for row in &rows {
                            let Ok(record) =
                                qos_agent_control_protocol::canonical_record(&row.record_id)
                            else {
                                continue;
                            };
                            if scopes
                                .iter()
                                .any(|s| s.app_id == row.app_id && s.covers_record(&record))
                            {
                                records.insert((row.app_id.clone(), row.record_id.clone()));
                            }
                        }
                    }
                    // Records the task opened itself (its desktop receipts).
                    let opened =
                        qos_action_broker::services::WorkspaceAuthority::task_opened_records(
                            &*self.owner,
                            task,
                        )
                        .unwrap_or_default();
                    for opened in opened {
                        let record = if opened.record_id == RecordId::package_primary().as_str() {
                            String::new()
                        } else {
                            opened.record_id.clone()
                        };
                        records.insert((opened.app_id, record));
                    }
                }
            }
            self.wanted = Some(WantedRecords {
                generation: snapshot.generation,
                tasks,
                records,
            });
        }
        Ok(&self.wanted.as_ref().expect("computed above").records)
    }
    /// Queue wanted records without a worker, start the grace period of
    /// mounted workers no longer wanted, and unmount those whose grace ran
    /// out (only while idle: no queued write, no busy record).
    fn schedule(&mut self, snapshot: &Snapshot) -> Result<(), String> {
        let wanted = self.wanted_records(snapshot)?.clone();
        let rows: std::collections::BTreeSet<(String, String)> = snapshot
            .peer
            .rows()
            .into_iter()
            .filter(|row| qos_session_peer::canonical_records::resolve(&row.app_id).is_some())
            .map(|row| (row.app_id, row.record_id))
            .collect();
        for key in wanted.intersection(&rows) {
            let mounted = self
                .workers
                .iter()
                .any(|w| w.app == key.0 && w.record == key.1);
            if !mounted && !self.pending.contains(key) && !self.skipped.contains_key(key) {
                self.pending.push_back(key.clone());
            }
        }
        self.pending.retain(|key| wanted.contains(key));
        self.skipped.retain(|key, _| wanted.contains(key));
        let now = std::time::Instant::now();
        let mut expired = vec![];
        for worker in &self.workers {
            let key = (worker.app.clone(), worker.record.clone());
            if wanted.contains(&key) {
                self.unwanted_since.remove(&key);
                continue;
            }
            let since = *self.unwanted_since.entry(key.clone()).or_insert(now);
            if now.duration_since(since) >= self.unmount_grace {
                expired.push(key);
            }
        }
        self.unwanted_since.retain(|key, _| {
            self.workers
                .iter()
                .any(|w| w.app == key.0 && w.record == key.1)
        });
        if expired.is_empty() {
            return Ok(());
        }
        let pending: std::collections::BTreeSet<StorageKey> = self
            .cache
            .pending_write_sizes()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        for (app, record) in expired {
            let Some(index) = self
                .workers
                .iter()
                .position(|w| w.app == app && w.record == record)
            else {
                continue;
            };
            let worker = &self.workers[index];
            let committed = snapshot.peer.document_heads(&app, &record);
            let idle = !pending.contains(&storage_key(&app, &self.room, &record))
                && committed.as_ref() == Some(&worker.heads)
                && worker
                    .records
                    .can_merge_admitted(&worker.checkout)
                    .unwrap_or(false);
            if !idle {
                continue; // Retried on a later pass, once settled.
            }
            let mut worker = self.workers.remove(index);
            self.unwanted_since.remove(&(app.clone(), record.clone()));
            let reports = worker.runner.close();
            for mut report in reports {
                let _ = report.retry_record();
                let _ = report.retry_view();
                let unresolved = report.record_error.is_some()
                    || report.view_error.is_some()
                    || report.teardown_error.is_some()
                    || report.record_retry.is_some()
                    || report.record_retry_target.is_some()
                    || report.view_retry.is_some();
                if unresolved {
                    // Same custody as a whole close: retained for recovery.
                    self.recovery.push(report);
                }
            }
            self.participants
                .retain(|(a, r, _)| !(a == &app && r == &record));
            tracing::info!(%app, workers = self.workers.len(), "host worker unmounted (record closed)");
        }
        Ok(())
    }
    /// Whether validated records still wait for their first mount attempt.
    pub(crate) fn mounting(&self) -> bool {
        !self.pending.is_empty()
    }
    fn refresh(&mut self, snapshot: &Snapshot) -> Result<(), String> {
        // Only records that have or need a worker are validated and merged:
        // a closed record costs nothing per generation (validating every
        // record ever opened made each open slower as closed records grew).
        let mut needed = self.wanted_records(snapshot)?.clone();
        needed.extend(
            self.workers
                .iter()
                .map(|w| (w.app.clone(), w.record.clone())),
        );
        // Validate every needed canonical record before changing any live owner.
        let validation = PortableStorage::new("cloud-worker-validation", &self.room)?;
        let mut validators = RecordOwners::portable(validation.clone());
        let mut plans = vec![];
        for row in snapshot.peer.rows() {
            if !needed.contains(&(row.app_id.clone(), row.record_id.clone())) {
                continue;
            }
            // Every application with an installed checked record factory runs
            // headless here (Writer, Chat, Notes, Calculator, ...): a record a
            // person or agent opens gets its worker at the next generation.
            let Some(definition) = qos_session_peer::canonical_records::resolve(&row.app_id)
                .map(|entry| entry.definition())
            else {
                continue;
            };
            let checkout = checkout(definition, &row.record_id)?;
            if definition.app_id == chat::definition().app_id && !checkout.id.is_package_primary() {
                return Err("Chat requires its canonical primary route".into());
            }
            let bytes = snapshot
                .peer
                .document(&row.app_id, &row.record_id)
                .ok_or("canonical worker bytes missing")?;
            validation.hydrate(
                storage_key(&row.app_id, &self.room, &row.record_id),
                bytes.clone(),
                Some(self.certificate.device_public_key.to_vec()),
            )?;
            validators
                .open(&row.app_id, &self.room)
                .map_err(|e| e.to_string())?
                .try_open_record(&checkout)
                .map_err(|e| e.to_string())?;
            if let Some(worker) = self
                .workers
                .iter()
                .find(|w| w.app == row.app_id && w.record == row.record_id)
            {
                if !worker.records.can_merge_admitted(&checkout)? {
                    return Err("worker record is busy; committed refresh pending".into());
                }
            }
            plans.push((row.app_id, row.record_id, definition, checkout, bytes));
        }
        if self.workers.iter().any(|worker| {
            !plans
                .iter()
                .any(|(app, record, ..)| app == &worker.app && record == &worker.record)
        }) {
            return Err("mounted worker lost canonical record custody".into());
        }
        for (app, record, definition, checkout, bytes) in plans {
            let heads = snapshot
                .peer
                .document_heads(&app, &record)
                .ok_or("canonical worker heads missing")?;
            if let Some(worker) = self
                .workers
                .iter_mut()
                .find(|w| w.app == app && w.record == record)
            {
                if worker.heads != heads {
                    if worker
                        .records
                        .merge_admitted(&worker.checkout, &DocumentMutation::Snapshot(bytes))?
                        .is_none()
                    {
                        return Err("worker record owner changed during refresh".into());
                    }
                    worker.heads = heads;
                }
                continue;
            }
            // New workers are queued by `schedule`, for wanted records only.
            let _ = (app, record, definition, checkout, bytes, heads);
        }
        // Records gone from this generation need no worker any more.
        let current: std::collections::BTreeSet<(String, String)> = snapshot
            .peer
            .rows()
            .into_iter()
            .map(|row| (row.app_id, row.record_id))
            .collect();
        self.pending.retain(|key| current.contains(key));
        self.skipped.retain(|key, _| current.contains(key));
        self.generation = Some(snapshot.generation);
        Ok(())
    }
    /// Mount pending workers until the pass budget is spent. A refused or
    /// failed connection skips only its record (retried with back-off).
    fn mount_pending(&mut self, snapshot: &Snapshot) -> Result<(), String> {
        let began = std::time::Instant::now();
        let now = began;
        for (key, (_, due)) in &self.skipped {
            if *due <= now && !self.pending.contains(key) {
                self.pending.push_back(key.clone());
            }
        }
        let mut mounted = false;
        while let Some((app, record)) = self.pending.front().cloned() {
            if mounted && began.elapsed() >= self.mount_budget {
                break;
            }
            self.pending.pop_front();
            if !self
                .wanted
                .as_ref()
                .is_some_and(|w| w.records.contains(&(app.clone(), record.clone())))
            {
                continue;
            }
            if self
                .workers
                .iter()
                .any(|w| w.app == app && w.record == record)
            {
                self.skipped.remove(&(app, record));
                continue;
            }
            let Some(definition) =
                qos_session_peer::canonical_records::resolve(&app).map(|entry| entry.definition())
            else {
                continue;
            };
            let (Some(bytes), Some(heads)) = (
                snapshot.peer.document(&app, &record),
                snapshot.peer.document_heads(&app, &record),
            ) else {
                continue;
            };
            let checkout = checkout(definition, &record)?;
            mounted = true;
            match self.mount(
                snapshot,
                app.clone(),
                record.clone(),
                definition,
                checkout,
                bytes,
                heads,
            ) {
                Ok(()) => {
                    self.skipped.remove(&(app.clone(), record));
                    tracing::info!(%app, workers = self.workers.len(), "host worker mounted");
                }
                Err(MountError::Fatal(error)) => return Err(error),
                Err(MountError::Record(error)) => {
                    let entry = self
                        .skipped
                        .entry((app.clone(), record))
                        .or_insert((0, now));
                    entry.0 = entry.0.saturating_add(1);
                    let delay = mount_retry_delay(entry.0 - 1);
                    entry.1 = std::time::Instant::now() + delay;
                    tracing::warn!(
                        %app,
                        attempt = entry.0,
                        retry_in_ms = delay.as_millis() as u64,
                        %error,
                        "host worker unavailable; its record is skipped and retried"
                    );
                }
            }
        }
        Ok(())
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "one worker mount needs each of its inputs"
    )]
    fn mount(
        &mut self,
        snapshot: &Snapshot,
        app: String,
        record: String,
        definition: &'static ui::application::ApplicationDefinition,
        checkout: RecordCheckout,
        bytes: Vec<u8>,
        heads: Vec<ui::automerge::ChangeHash>,
    ) -> Result<(), MountError> {
        // Connect and join first: a refusal here happens before any
        // storage hydration or component setup, so a later retry of this
        // record starts from the same clean state.
        let room = self.room.parse().map_err(|_| "invalid room UUID")?;
        let route = if record.is_empty() {
            RouteSpec::app(&app, room)
        } else {
            RouteSpec::record(&app, room, &record)
        };
        #[cfg(test)]
        if TEST_MOUNT.with(|test| test.borrow().1.contains(&(app.clone(), record.clone()))) {
            return Err(MountError::Record(
                "injected host worker connection refusal".into(),
            ));
        }
        // A refused or failed connection/join leaves no component and no
        // recovery report: only this record is skipped (and retried).
        let (stream, authorizer) = self
            .broker
            .connect_local_worker_with_authorizer(
                CrdtRoute::record(&app, &self.room, &record),
                &self.certificate,
            )
            .map_err(MountError::Record)?;
        let manager = CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            &app,
            room,
            (!record.is_empty()).then_some(record.clone()),
            None,
            CertifiedEndpointIdentity::with_host_documents(
                self.presentation.clone(),
                self.documents.bind(route, self.certificate.clone()),
            ),
        )
        .map_err(|e| MountError::Record(format!("worker join: {e:?}")))?;
        // A remount (the record was unmounted after its window closed)
        // finds its bytes already in this cache; the committed bytes are
        // merged into the mounted worker below instead.
        let key = storage_key(&app, &self.room, &record);
        let remount = self.cache.get(&key).is_some();
        if !remount {
            self.cache.hydrate(
                key,
                bytes.clone(),
                Some(self.certificate.device_public_key.to_vec()),
            )?;
        }
        let records = self
            .owners
            .open(&app, &self.room)
            .map_err(|e| e.to_string())?;
        let instance = u64::from_be_bytes(
            blake3::hash(format!("{}/{app}/{record}", self.room).as_bytes()).as_bytes()[..8]
                .try_into()
                .expect("fixed hash"),
        );
        let view_path = self
            .root
            .join("actions/views")
            .join(format!("{instance}.bin"));
        let view_key = StorageKey::View {
            workspace: self.room.clone(),
            instance,
        };
        if self.cache.get(&view_key).is_none()
            && view_path.try_exists().map_err(|e| e.to_string())?
        {
            if !std::fs::symlink_metadata(&view_path)
                .map_err(|e| e.to_string())?
                .is_file()
            {
                return Err("worker view must be a regular file".into());
            }
            self.cache.hydrate(
                StorageKey::View {
                    workspace: self.room.clone(),
                    instance,
                },
                std::fs::read(view_path).map_err(|e| e.to_string())?,
                None,
            )?;
        }
        let restore = self
            .views
            .load(instance)
            .map_err(|e| e.to_string())?
            .map_or(RestoreInput::None, RestoreInput::Local);
        let permissions = Rc::new(RefCell::new(permission(
            &self.owner,
            &snapshot.peer,
            &self.certificate,
        )?));
        let target = LaunchTarget::Record {
            checkout: checkout.clone(),
            reveal: None,
        };
        let context = HostContext::prepare(
            definition,
            records.clone(),
            self.views.clone(),
            hex::encode(self.certificate.device_public_key),
            instance,
            permissions.clone(),
            ui::permissions::UserId(self.principal.clone()),
            qos_actions::ActionContext::default(),
            &target,
        )
        .map_err(|e| self.setup_error(e))?;
        let mut runner = HostWorker::mount(
            definition,
            ui::application::PreparedComponentInputs {
                context,
                target,
                restore,
                runner_inputs: ui::application::ComponentRunnerInputs {
                    theme: ui::Theme::dark(),
                    text_renderer: ui::TextRenderer::new(),
                },
            },
            manager,
            permissions,
        )
        .map_err(|e| self.setup_error(e))?;
        #[cfg(test)]
        let observed_authorizer = authorizer.clone();
        if let Err(error) = runner.install_published_invocation_authorizer(authorizer) {
            self.recovery.extend(runner.close());
            return Err(MountError::Fatal(error));
        }
        if remount
            && records
                .merge_admitted(&checkout, &DocumentMutation::Snapshot(bytes))?
                .is_none()
        {
            self.recovery.extend(runner.close());
            return Err(MountError::Fatal(
                "remounted worker record owner changed".into(),
            ));
        }
        self.workers.push(Worker {
            app,
            record,
            checkout,
            records,
            runner,
            heads,
            #[cfg(test)]
            authorizer: observed_authorizer,
        });
        Ok(())
    }
    fn setup_error(&mut self, error: ui::launch::LaunchError) -> String {
        let message = error.to_string();
        ui::host_worker::retain_setup_error(error, &mut self.recovery);
        message
    }
    /// A definite document refusal leaves the signer healthy; an uncertain
    /// outcome latched by the handoff service fails here.
    pub(crate) fn document_health(&self) -> Result<(), String> {
        if self.closed {
            return Err("workers closed".into());
        }
        self.documents.health()
    }
    pub(crate) fn committed(&mut self) -> Result<(), String> {
        let result = (|| {
            if self.closed {
                return Err("workers closed".into());
            }
            self.documents.health()?;
            let snapshot = self.snapshot()?;
            self.reconcile(&snapshot)?;
            self.acknowledge(&snapshot.peer)
        })();
        result.map_err(|reason| self.fail(reason))
    }
    pub(crate) fn pump(&mut self) -> Result<(), String> {
        use qos_action_broker::agent_timing::us;
        let began = std::time::Instant::now();
        let mut stages = (0u64, 0u64, 0u64, 0u64);
        let result = (|| {
            if self.closed {
                return Err("workers closed".into());
            }
            self.documents.health()?;
            let snapshot = self.snapshot()?;
            stages.0 = us(began);
            let stage = std::time::Instant::now();
            self.reconcile(&snapshot)?;
            stages.1 = us(stage);
            let stage = std::time::Instant::now();
            if !self.workers.is_empty() {
                let permission = self.permission_for(&snapshot)?;
                let task_agents = &self.task_agents;
                for worker in &mut self.workers {
                    worker.runner.pump(permission.clone())?;
                    // Agent participant carets/selections, from their own
                    // views, forwarded live under the host-known agent id.
                    let updates = worker.runner.take_participant_presence_changes();
                    if !updates.is_empty() {
                        crate::agent_live::on_participant_presence(
                            &worker.app,
                            &worker.record,
                            updates,
                            &|task| task_agents.get(task).cloned(),
                        );
                    }
                }
            }
            stages.2 = us(stage);
            self.acknowledge(&snapshot.peer)
        })();
        let result = result.map_err(|reason| self.fail(reason));
        if result.is_ok() {
            let stage = std::time::Instant::now();
            self.pump_steps();
            stages.3 = us(stage);
        }
        if began.elapsed() >= std::time::Duration::from_millis(10) {
            qos_action_broker::agent_timing::span(
                "workers_pump",
                None,
                began.elapsed(),
                &[
                    ("snapshot_us", stages.0),
                    ("reconcile_us", stages.1),
                    ("workers_us", stages.2),
                    ("steps_us", stages.3),
                ],
            );
        }
        result
    }
    /// Whether the enrolled step service holds a pending or recovering step.
    pub(crate) fn has_pending_steps(&self) -> bool {
        self.steps.as_ref().is_some_and(|steps| steps.busy())
    }
    /// Drive the enrolled step service once: begin queued steps and settle
    /// pending ones whose child became terminal. Also called right after a
    /// worker document commit, so the step it completed is committed and
    /// answered in the same owner tick instead of the next one.
    pub(crate) fn pump_steps(&mut self) {
        if self.closed {
            return;
        }
        if let (Some(steps), Ok(broker)) = (
            self.steps.as_mut(),
            self.broker.app_action_broker().ok_or(()),
        ) {
            steps.pump(&self.owner, &broker);
        }
    }
    fn acknowledge(&self, peer: &StatePeer) -> Result<(), String> {
        for write in self.cache.pending_writes() {
            let route = match &write.key {
                StorageKey::Primary { package, workspace } if workspace == &self.room => {
                    Some((package.as_str(), ""))
                }
                StorageKey::Record {
                    package,
                    workspace,
                    record,
                } if workspace == &self.room => Some((package.as_str(), record.as_str())),
                StorageKey::View {
                    workspace,
                    instance,
                } if workspace == &self.room => {
                    let directory = self.root.join("actions/views");
                    private_directory(&directory)?;
                    atomic_file::write_with_mode(
                        &directory.join(format!("{instance}.bin")),
                        &write.value.bytes,
                        0o600,
                    )
                    .map_err(|e| e.to_string())?;
                    self.cache.acknowledge(&write, Ok(()))?;
                    None
                }
                _ => return Err("worker persistence crossed owner namespace".into()),
            };
            if let Some((app, record)) = route {
                let committed = peer
                    .document(app, record)
                    .ok_or("worker lost committed record")?;
                ui::host_worker::acknowledge_document(&self.cache, &write, &committed, true)?;
            }
        }
        Ok(())
    }
    /// Agent task participant views follow the live agent tasks: a task's
    /// view is created on every worker whose record its retained grants
    /// cover (so its presence exists from the spawn) and dropped when the
    /// task is no longer active (security review MUST-12). `active` is
    /// every live, unfinished task (host ledger facts only); the participant
    /// is host-derived from the admitted task id, labelled with the agent's
    /// display name.
    #[allow(
        dead_code,
        reason = "called from the runtime tick once the adapter lists live tasks (integration diff)"
    )]
    pub(crate) fn sync_agent_participants(
        &mut self,
        active: &[ActiveAgentTask],
    ) -> Result<(), String> {
        let mut wanted = std::collections::BTreeSet::new();
        let mut labels = std::collections::BTreeMap::new();
        // Worker set input: an agent's granted/opened records get workers.
        self.agent_tasks = active.to_vec();
        self.task_agents = active
            .iter()
            .map(|t| (t.task.clone(), t.agent.clone()))
            .collect();
        for ActiveAgentTask {
            task, name, grants, ..
        } in active
        {
            let participant = qos_actions::ParticipantId::for_agent_task(task)?;
            labels.insert(participant.as_str().to_owned(), name.clone());
            let scopes = qos_agent_control_protocol::scopes_from_grants(grants)
                .map_err(|e| e.to_string())?;
            for worker in &self.workers {
                let Ok(record) = qos_agent_control_protocol::canonical_record(&worker.record)
                else {
                    continue;
                };
                if scopes
                    .iter()
                    .any(|s| s.app_id == worker.app && s.covers_record(&record))
                {
                    wanted.insert((
                        worker.app.clone(),
                        worker.record.clone(),
                        participant.as_str().to_owned(),
                    ));
                }
            }
        }
        for (app, record, participant) in self.participants.difference(&wanted) {
            let participant = qos_actions::ParticipantId::new(participant.clone())?;
            if let Some(worker) = self
                .workers
                .iter_mut()
                .find(|w| &w.app == app && &w.record == record)
            {
                worker.runner.leave_participant(&participant);
            }
        }
        for (app, record, participant) in &wanted {
            let participant = qos_actions::ParticipantId::new(participant.clone())?;
            if let Some(worker) = self
                .workers
                .iter_mut()
                .find(|w| &w.app == app && &w.record == record)
            {
                // Idempotent. The label is re-applied on every sync: a view is
                // created only at the worker's next document sync after the
                // join, and a label set before that is dropped.
                worker.runner.join_participant(&participant);
                if let Some(label) = labels.get(participant.as_str()) {
                    worker.runner.set_participant_label(&participant, label);
                }
            }
        }
        self.participants = wanted;
        Ok(())
    }
    /// Operator diagnostic (no document content): which worker surfaces the
    /// publication saw and how many delegable Document operations each
    /// contributed. Logged only when the shape changes.
    fn log_publication_shape(&self, included: &[PublishedSurface], grants: &[GrantAction]) {
        static LAST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        let workers: Vec<String> = self
            .workers
            .iter()
            .map(|w| format!("{}#{}", w.app, w.record))
            .collect();
        let mut seen: Vec<String> = self
            .broker
            .published_action_surfaces()
            .into_iter()
            .filter(|s| s.route.cluster_id == self.room)
            .map(|s| {
                let document = s
                    .actions
                    .iter()
                    .filter(|a| a.operation_kind == qos_actions::OperationKind::Document)
                    .count();
                format!(
                    "{}#{} peer={} live={} route_app={} included={} actions={} document={}",
                    s.app_id,
                    s.route.record_id,
                    s.peer_id,
                    s.live,
                    s.route.app_id,
                    included.iter().any(|i| i.peer_id == s.peer_id),
                    s.actions.len(),
                    document,
                )
            })
            .collect();
        seen.sort();
        let shape = format!(
            "workers={workers:?} surfaces={seen:?} grants={}",
            grants.len()
        );
        if let Ok(mut last) = LAST.lock()
            && last.as_deref() != Some(shape.as_str())
        {
            tracing::info!(shape = %shape, "agent worker publication shape");
            *last = Some(shape);
        }
    }
    pub(crate) fn publications(&self) -> Result<WorkerPublications, String> {
        self.documents.health().map_err(|e| self.fail(e))?;
        let snapshot = self.snapshot().map_err(|e| self.fail(e))?;
        let mut surfaces: Vec<PublishedSurface> = vec![];
        for surface in self.broker.published_action_surfaces() {
            if surface.live
                && surface.route.cluster_id == self.room
                && surface.app_id == surface.route.app_id
                && self.workers.iter().any(|worker| {
                    worker.app == surface.app_id && worker.record == surface.route.record_id
                })
                && self
                    .broker
                    .is_current_host_worker(surface.peer_id, &surface.route, &self.certificate)
                    .map_err(|reason| self.fail(reason))?
            {
                surfaces.push(surface);
            }
        }
        surfaces.sort_by(|a, b| {
            (&a.app_id, &a.route.record_id, a.peer_id).cmp(&(
                &b.app_id,
                &b.route.record_id,
                b.peer_id,
            ))
        });
        let mut grants = vec![];
        let mut revision = vec![];
        for surface in &surfaces {
            let mut actions: Vec<_> = surface
                .actions
                .iter()
                .filter(|a| a.operation_kind == qos_actions::OperationKind::Document)
                .collect();
            actions.sort_by(|a, b| (&a.resource, &a.action).cmp(&(&b.resource, &b.action)));
            for action in actions {
                grants.push(GrantAction {
                    app_id: surface.app_id.clone(),
                    record_id: if surface.route.record_id.is_empty() {
                        RecordId::package_primary().as_str().to_owned()
                    } else {
                        surface.route.record_id.clone()
                    },
                    resource: action.resource.clone(),
                    action: action.action.clone(),
                });
                let input = match &action.input {
                    PublishedInput::None => serde_json::Value::Null,
                    PublishedInput::Text {
                        prompt,
                        placeholder,
                        wants_content,
                    } => {
                        serde_json::json!({"prompt":prompt,"placeholder":placeholder,"wants_content":wants_content})
                    }
                };
                revision.push(serde_json::json!({"peer":surface.peer_id,"app":surface.app_id,"record":surface.route.record_id,"resource":action.resource,"action":action.action,"kind":action.resource_kind,"label":action.resource_label,"action_label":action.action_label,"description":action.description,"params":action.params,"contract":action.contract,"input":input}));
            }
        }
        grants.dedup();
        self.log_publication_shape(&surfaces, &grants);
        if qos_action_broker::agent_control::scoped::scoped_grants_enabled() {
            // v2: one bounded scope per record instead of one tuple per
            // published op; the frozen scope set is independent of document
            // size (no 64-tuple cap) and of the owner's current selection.
            grants = if qos_action_broker::agent_control::scoped::narrow_grant_scope() {
                publication_scopes(&surfaces)?
            } else {
                member_rights_scopes(&surfaces)?
            };
            // The desktop's open-application action, when this broker serves
            // it: an agent opens apps the way a person does, and works in the
            // record it opened for the rest of its task.
            if !grants.is_empty()
                && self
                    .broker
                    .published_action_surfaces()
                    .iter()
                    .any(|surface| {
                        surface.live
                            && surface.route.cluster_id == self.room
                            && surface.app_id == qos_agent_control_protocol::DESKTOP_APP_ID
                    })
                && let Some(scope) = desktop_open_scope()
            {
                grants.push(
                    qos_agent_control_protocol::scope_carrier(&scope).map_err(|e| e.to_string())?,
                );
            }
        }
        let live = !self.closed && self.transport_ready;
        // Not ready while validated records still wait for their first
        // mount (the offer would change as they come up); a skipped (refused)
        // record does not hold readiness back.
        let ready = live
            && self.generation == Some(snapshot.generation)
            && self.pending.is_empty()
            && !grants.is_empty()
            && self.workers.iter().all(|w| {
                surfaces
                    .iter()
                    .any(|s| s.app_id == w.app && s.route.record_id == w.record)
            });
        let publication = WorkerPublications {
            room: self.room.clone(),
            host_device: self.certificate.device_public_key,
            governance_revision: qos_peer_state::owner_commit::governance_revision(&snapshot.peer)?,
            publication_revision: blake3::hash(
                &serde_json::to_vec(&revision).map_err(|e| e.to_string())?,
            )
            .to_hex()
            .to_string(),
            grants: if ready { grants } else { vec![] },
            ready,
            native_v5_ready: snapshot.peer.admission_v5_selected(),
            private_v2_ready: snapshot.private_v2_ready,
            machine: self.machine.clone(),
        };
        let mut last_good = self.last_good.borrow_mut();
        if ready {
            *last_good = Some((std::time::Instant::now(), publication.clone()));
            return Ok(publication);
        }
        // A committed refresh or a worker surface republish is in progress, with
        // a live transport and no owner failure: keep the last good publication
        // (with its original governance revision) instead of flapping readiness.
        if live && snapshot.peer.admission_v5_selected() {
            if let Some((at, held)) = last_good.as_ref() {
                if at.elapsed() < PUBLICATION_HOLD
                    && held.room == publication.room
                    && held.host_device == publication.host_device
                {
                    return Ok(held.clone());
                }
            }
        }
        *last_good = None;
        self.owner.stop_readiness();
        Ok(publication)
    }
    pub(crate) fn close(&mut self) -> Result<(), String> {
        // Retained children stay durable in the sender file and task ledger.
        self.steps = None;
        self.set_transport_ready(false);
        self.documents
            .fail("workers closing; prior outcome must be reconciled");
        if !self.closed {
            self.closed = true;
            for worker in &mut self.workers {
                self.recovery.extend(worker.runner.close());
            }
        }
        // Ask every original owner to transfer pending bytes before checking any failure.
        for report in &mut self.recovery {
            let _ = report.retry_record();
            let _ = report.retry_view();
        }
        let result = (|| {
            let snapshot = self.snapshot()?;
            self.acknowledge(&snapshot.peer)?;
            let mut errors = vec![];
            for report in &mut self.recovery {
                let _ = report.retry_record();
                let _ = report.retry_view();
                errors.extend(
                    [
                        &report.record_error,
                        &report.view_error,
                        &report.teardown_error,
                    ]
                    .into_iter()
                    .filter_map(|e| e.clone()),
                );
                if report.record_retry.is_some()
                    || report.record_retry_target.is_some()
                    || report.view_retry.is_some()
                {
                    errors.push("unresolved component retry custody".into());
                }
            }
            if !self.cache.pending_writes().is_empty() {
                errors.push("uncommitted worker bytes".into());
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(errors.join("; "))
            }
        })();
        if let Err(reason) = result {
            let preserved = self.preserve_recovery(&reason);
            return Err(match preserved {
                Ok(()) => format!("{reason}; custody preserved in application recovery"),
                Err(error) => format!("{reason}; FAILED to preserve application recovery: {error}"),
            });
        }
        Ok(())
    }
    fn preserve_recovery(&self, reason: &str) -> Result<(), String> {
        let pending = self.cache.pending_writes().into_iter().map(|write| {
            let key = match write.key {
                StorageKey::Primary { package, workspace } => serde_json::json!({"kind":"primary","app":package,"room":workspace}),
                StorageKey::Record { package, workspace, record } => serde_json::json!({"kind":"record","app":package,"room":workspace,"record":record}),
                StorageKey::View { workspace, instance } => serde_json::json!({"kind":"view","room":workspace,"instance":instance}),
                StorageKey::Admission { workspace } => serde_json::json!({"kind":"admission","room":workspace}),
                StorageKey::PrivateActions { workspace } => serde_json::json!({"kind":"private_actions","room":workspace}),
            };
            serde_json::json!({"key":key,"actor":write.value.actor,"bytes":write.value.bytes,"revision":write.revision})
        }).collect::<Vec<_>>();
        let reports = self.recovery.iter().map(|report| serde_json::json!({"view":report.recovery,"view_error":report.view_error,"record_error":report.record_error,"teardown_error":report.teardown_error})).collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"room":self.room,"device":self.certificate.device_public_key,"outcome":"unknown","error":reason,"pending":pending,"reports":reports})).map_err(|e| e.to_string())?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err("recovery artifact exceeds bounded capacity".into());
        }
        atomic_file::write_with_mode(&self.root.join("actions/recovery.json"), &bytes, 0o600)
            .map_err(|e| e.to_string())
    }
}
/// The recommended default scopes (security review §4) for the live host
/// worker surfaces, as v2 carriers. Writer: one record, `document + view +
/// query`, bulk review denied. Chat: posting is `disclose`, never implied by
/// an edit scope, so each live channel gets its own named, channel-exact
/// scope. Only manifest-delegable apps; at most `MAX_GRANT_SCOPES` scopes.
pub(crate) fn publication_scopes(
    surfaces: &[PublishedSurface],
) -> Result<Vec<GrantAction>, String> {
    use qos_agent_control_protocol::{
        CHAT_APP_ID, GrantScope, GrantVerb, MAX_GRANT_SCOPES, OperationClass, PackagePin,
        RecordScope, ResourceScope, canonical_record, scope_carrier,
    };
    let registry = qos_action_broker::agent_control::scoped::delegation_registry();
    let mut scopes: Vec<GrantScope> = vec![];
    let pin = |app: &str| -> Option<PackagePin> {
        Some(PackagePin {
            package_id: registry.package_for_app(app)?.to_owned(),
            version: registry.manifest(app)?.version.clone(),
            schema_digest: qos_action_broker::agent_control::scoped::schema_digest(app)?,
        })
    };
    for surface in surfaces {
        if !registry.is_delegable_app(&surface.app_id) {
            continue;
        }
        let Ok(record) = canonical_record(&surface.route.record_id) else {
            continue;
        };
        let Some(package) = pin(&surface.app_id) else {
            continue;
        };
        let base = GrantScope {
            app_id: surface.app_id.clone(),
            record: RecordScope::Exact(record),
            verb: GrantVerb::Write,
            classes: vec![
                OperationClass::Document,
                OperationClass::View,
                OperationClass::Query,
            ],
            resource: ResourceScope::Any,
            actions: None,
            deny_actions: vec!["accept-all".into(), "reject-all".into()],
            package: Some(package),
            budgets: None,
            explicit_workspace_consent: false,
        };
        if surface.app_id == CHAT_APP_ID {
            let mut channels: Vec<&str> = surface
                .actions
                .iter()
                .filter(|a| a.resource_kind == "chat.channel")
                .map(|a| a.resource.as_str())
                .collect();
            channels.sort_unstable();
            channels.dedup();
            for channel in channels {
                scopes.push(GrantScope {
                    classes: vec![
                        OperationClass::Disclose,
                        OperationClass::View,
                        OperationClass::Query,
                    ],
                    resource: ResourceScope::Exact(channel.to_owned()),
                    deny_actions: vec![],
                    ..base.clone()
                });
            }
        } else {
            scopes.push(base);
        }
    }
    scopes.dedup();
    scopes.truncate(MAX_GRANT_SCOPES);
    scopes
        .iter()
        .map(|scope| scope_carrier(scope).map_err(|e| e.to_string()))
        .collect()
}

/// The default agent scope (user decision 2026-09-24): the agent is a
/// participant with its spawner's member rights. One scope per delegable app
/// names every live worker record of that app (at most 32 per scope, never a
/// workspace wildcard) with every non-filesystem delegable class. What the
/// agent may actually do is decided at every step by the same permission
/// function as people, for the agent (the spawner's current role when it has
/// no roster entry of its own) and the spawner, on the live route's record,
/// plus the host rules no member scope can lift: host filesystem effects,
/// confirmation-requiring and bulk operations are refused unattended.
pub(crate) fn member_rights_scopes(
    surfaces: &[PublishedSurface],
) -> Result<Vec<GrantAction>, String> {
    use qos_agent_control_protocol::{
        GrantScope, GrantVerb, MAX_GRANT_SCOPES, MAX_SCOPE_RECORDS, OperationClass, PackagePin,
        RecordScope, ResourceScope, canonical_record, scope_carrier,
    };
    let registry = qos_action_broker::agent_control::scoped::delegation_registry();
    let mut records: std::collections::BTreeMap<&str, std::collections::BTreeSet<String>> =
        Default::default();
    for surface in surfaces {
        if !registry.is_delegable_app(&surface.app_id) {
            continue;
        }
        if let Ok(record) = canonical_record(&surface.route.record_id) {
            records.entry(&surface.app_id).or_default().insert(record);
        }
    }
    let mut scopes = vec![];
    for (app, records) in records {
        let (Some(package_id), Some(manifest), Some(digest)) = (
            registry.package_for_app(app),
            registry.manifest(app),
            qos_action_broker::agent_control::scoped::schema_digest(app),
        ) else {
            continue;
        };
        let records: Vec<String> = records.into_iter().collect();
        for chunk in records.chunks(MAX_SCOPE_RECORDS) {
            scopes.push(GrantScope {
                app_id: app.to_owned(),
                record: match chunk {
                    [one] => RecordScope::Exact(one.clone()),
                    many => RecordScope::AnyOf(many.to_vec()),
                },
                verb: GrantVerb::Write,
                classes: vec![
                    OperationClass::Document,
                    OperationClass::View,
                    OperationClass::Query,
                    OperationClass::Disclose,
                ],
                resource: ResourceScope::Any,
                actions: None,
                deny_actions: vec![],
                package: Some(PackagePin {
                    package_id: package_id.to_owned(),
                    version: manifest.version.clone(),
                    schema_digest: digest.clone(),
                }),
                budgets: None,
                explicit_workspace_consent: false,
            });
        }
    }
    scopes.truncate(MAX_GRANT_SCOPES);
    scopes
        .iter()
        .map(|scope| scope_carrier(scope).map_err(|e| e.to_string()))
        .collect()
}

/// The frozen scope of the desktop's open-application action: `open` on
/// `desktop:app:*` of the room's desktop, class `app.open` only.
pub(crate) fn desktop_open_scope() -> Option<qos_agent_control_protocol::GrantScope> {
    use qos_agent_control_protocol::{
        DESKTOP_APP_ID, GrantScope, GrantVerb, OPEN_APP_ACTION, OPEN_APP_RESOURCE_PREFIX,
        OperationClass, PRIMARY_RECORD_ALIAS, PackagePin, RecordScope, ResourceScope,
    };
    let registry = qos_action_broker::agent_control::scoped::delegation_registry();
    let manifest = registry.manifest(DESKTOP_APP_ID)?;
    Some(GrantScope {
        app_id: DESKTOP_APP_ID.into(),
        record: RecordScope::Exact(PRIMARY_RECORD_ALIAS.into()),
        verb: GrantVerb::Write,
        classes: vec![OperationClass::AppOpen],
        resource: ResourceScope::Prefix(OPEN_APP_RESOURCE_PREFIX.into()),
        actions: Some(vec![OPEN_APP_ACTION.into()]),
        deny_actions: vec![],
        package: Some(PackagePin {
            package_id: manifest.package_id.clone(),
            version: manifest.version.clone(),
            schema_digest: qos_action_broker::agent_control::scoped::schema_digest(DESKTOP_APP_ID)?,
        }),
        budgets: None,
        explicit_workspace_consent: false,
    })
}

/// Offer label derived from the frozen grants only (MUST-19): what records,
/// which classes, and what is explicitly absent. v1 tuple offers keep their
/// generic label.
#[allow(
    dead_code,
    reason = "offer label in agent_runtime.rs publish_runtime_state (integration diff)"
)]
pub(crate) fn consent_label(grants: &[GrantAction]) -> Option<String> {
    let scopes = qos_agent_control_protocol::scopes_from_grants(grants).ok()?;
    if scopes.iter().any(|s| s.is_degenerate()) {
        return None;
    }
    // The desktop's open-application scope is named separately below.
    let opens_apps = scopes
        .iter()
        .any(|s| s.app_id == qos_agent_control_protocol::DESKTOP_APP_ID);
    let work: Vec<_> = scopes
        .iter()
        .filter(|s| s.app_id != qos_agent_control_protocol::DESKTOP_APP_ID)
        .collect();
    let member_rights = !work.is_empty()
        && work.iter().all(|s| {
            s.classes
                .contains(&qos_agent_control_protocol::OperationClass::Document)
                && s.classes
                    .contains(&qos_agent_control_protocol::OperationClass::Disclose)
                && s.resource == qos_agent_control_protocol::ResourceScope::Any
        });
    if member_rights {
        let records: usize = work.iter().map(|s| s.records().len()).sum();
        let open = if opens_apps { " It may open apps." } else { "" };
        let minutes = scopes
            .iter()
            .map(|s| s.budgets().wall_clock_secs)
            .min()
            .unwrap_or(0)
            / 60;
        return Some(
            format!(
                "Allow the agent to act with your member rights on {records} workspace \
             record(s): anything you can do there.{open} It cannot open or save files, \
             use the network, or run bulk or confirmation-required changes. Stops after \
             {minutes} min or when you revoke it."
            )
            .chars()
            .take(256)
            .collect(),
        );
    }
    let text = qos_agent_control_protocol::consent_text("the agent", &scopes, |app, record| {
        let app = match app {
            qos_agent_control_protocol::WRITER_APP_ID => "document",
            qos_agent_control_protocol::CHAT_APP_ID => "chat",
            other => other,
        };
        if record == qos_agent_control_protocol::PRIMARY_RECORD_ALIAS {
            format!("the workspace {app}")
        } else {
            format!("{app} {record}")
        }
    });
    Some(text.chars().take(256).collect())
}

fn check_peer(peer: &StatePeer, now: web_time::SystemTime) -> Result<(), String> {
    if !peer.admission_v5_selected() || peer.incomplete_reason().is_some() {
        return Err("complete explicitly selected V5 worker owner required".into());
    }
    peer.permissions(peer.authority().device_public_key, now)
        .map(|_| ())
}
fn permission(
    owner: &Arc<RuntimeOwner>,
    peer: &StatePeer,
    certificate: &DeviceCert,
) -> Result<PermissionContext, String> {
    use qos_action_broker::peer_actions::PrivatePolicyBasis;
    let time = owner.policy_time()?;
    let now = time.now_ns().map_err(|e| e.to_string())?;
    let (role, caps, person) =
        peer.permissions(certificate.device_public_key, crate::now_time(now))?;
    let basis = PrivatePolicyBasis::from_peer(peer).map_err(|e| e.to_string())?;
    let owner = owner.clone();
    let certificate = certificate.clone();
    let retained_role = role;
    let retained_person = person.clone();
    // Every decision samples. The full check runs under the Store lock; its
    // verdict is reused only while the Store files are unchanged, the policy
    // time stays inside the checked peer's exact interval and the locked
    // proof is younger than STORE_RECHECK (see `store_hint`). The original
    // policy time is validated on every sample either way.
    let verified: std::sync::Mutex<Option<crate::agent_runtime_adapter::store_hint::Verified>> =
        std::sync::Mutex::new(None);
    let sampler = Arc::new(move || {
        owner.validate_policy_time(&time)?;
        let now = time.now_ns().map_err(|e| e.to_string())?;
        if verified
            .lock()
            .ok()
            .and_then(|verified| *verified)
            .is_some_and(|verified| verified.current(owner.root(), now))
        {
            return Ok(now / 1_000_000_000);
        }
        if let Ok(mut verified) = verified.lock() {
            *verified = None; // Re-proved below; any failure leaves no verdict.
        }
        let store = owner.store_for(&time)?;
        let peers = store.peer_shared().map_err(|e| e.to_string())?;
        let current = &peers.1;
        let now = store.policy_now_ns().map_err(|e| e.to_string())?;
        let (role, _, person) =
            current.permissions(certificate.device_public_key, crate::now_time(now))?;
        if !current.certificates().contains(&certificate)
            || role != retained_role
            || person != retained_person
            || PrivatePolicyBasis::from_peer(&current)
                .map_err(|e| e.to_string())?
                .revision()
                != basis.revision()
        {
            return Err("worker policy changed; rebuild current permission context".into());
        }
        let proof = crate::agent_runtime_adapter::store_hint::Verified::under_lock(
            owner.root(),
            &store.state,
            now,
        );
        drop(store);
        if let Ok(mut verified) = verified.lock() {
            *verified = proof;
        }
        Ok(now / 1_000_000_000)
    });
    Ok(PermissionContext::from_capabilities(
        Actor::person(Some(role), person).in_room(peer.room().to_string()),
        caps,
    )
    .with_policy_time_sampler(sampler))
}

/// Records with an open window in the room: every tab of the committed
/// desktop topology, resolved to its application record exactly as the
/// desktop delivery does (explicit record binding, else the per-tab or
/// package-primary record of that application).
fn visible_records(peer: &StatePeer) -> std::collections::BTreeSet<(String, String)> {
    let mut visible = std::collections::BTreeSet::new();
    let (Some(room), Some(doc)) = (
        qos_desktop::RoomId::parse(peer.room()),
        peer.admitted_room(),
    ) else {
        return visible;
    };
    let mut tree = qos_desktop::WorkspaceTree::default();
    if qos_desktop::projection::RoomProjection::apply_admitted_room(&mut tree, room, doc).is_err() {
        return visible;
    }
    let routes = qos_desktop::projection::RoomProjection::record_routes(doc);
    let rows = peer.rows();
    for tab in tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
    {
        let Some(tab_app) = qos_desktop::resolver::canonical_app_id(&tab.tile) else {
            continue;
        };
        for row in &rows {
            let Some(definition) = qos_session_peer::canonical_records::resolve(&row.app_id)
                .map(|entry| entry.definition())
            else {
                continue;
            };
            if !ui::application::lookup_compatible_first_party_app_ids(&row.app_id, &tab_app) {
                continue;
            }
            let record = routes.get(&tab.id).cloned().unwrap_or_else(|| {
                let checkout = qos_desktop::records::component_record(
                    &row.app_id,
                    tab.id,
                    definition.record_kind.clone(),
                );
                if checkout.id.is_package_primary() {
                    String::new()
                } else {
                    checkout.id.as_str().to_owned()
                }
            });
            if row.record_id == record {
                visible.insert((row.app_id.clone(), row.record_id.clone()));
            }
        }
    }
    visible
}
fn checkout(
    definition: &ui::application::ApplicationDefinition,
    record: &str,
) -> Result<RecordCheckout, String> {
    if record.is_empty() {
        Ok(RecordCheckout::package_primary(
            definition.record_kind.clone(),
        ))
    } else {
        Ok(RecordCheckout::new(
            RecordId::parse(record).ok_or("invalid canonical worker record")?,
            definition.record_kind.clone(),
        ))
    }
}
fn storage_key(app: &str, room: &str, record: &str) -> StorageKey {
    if record.is_empty() {
        StorageKey::Primary {
            package: app.into(),
            workspace: room.into(),
        }
    } else {
        StorageKey::Record {
            package: app.into(),
            workspace: room.into(),
            record: record.into(),
        }
    }
}
fn private_directory(path: &Path) -> Result<(), String> {
    atomic_file::provision_directory(path, atomic_file::Privacy::OwnerOnly)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod enrolled_handoff_tests;
#[cfg(test)]
pub(crate) mod tests;
