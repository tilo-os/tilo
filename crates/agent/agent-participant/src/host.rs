//! Agents on a host: each agent's participant on its own thread, every grant
//! and agent kept under a state directory, picked up again when the host
//! restarts.
//!
//! [`AgentHost`] owns an [`AgentSupervisor`] whose spawner starts one thread
//! per agent. The thread mounts its apps, plans with the host's
//! [`ModelChoice`] and pumps the participant until the agent stops or the
//! host shuts down, sleeping between pumps until its session or steering
//! changes or its presence keep-alive is due. A keeper thread settles expired
//! grants (the agent stops and the host refuses its device) and writes the
//! supervisor's snapshot to `agents.json` whenever it changed, sleeping
//! until a change or the next expiry. The file
//! holds the agents' device keys and is written owner-only.
//!
//! On start the snapshot is restored ([`AgentSupervisor::restore`]):
//! approved grants are registered with the host again, agents that were
//! neither stopped nor failed start again with their queued tasks (a task
//! that was running when the host went down is marked failed), and paused
//! agents come back paused. A host shutting down leaves its agents'
//! statuses as they were, so the next start resumes them.
//!
//! Each room's content (the bytes its records name by hash) is kept under
//! `content/<room>` in the state directory, in this process: an agent's
//! app workers are handed what an operation names and give back what they
//! store ([`ConfinedReplica`](crate::ConfinedReplica)). A harness puts
//! content in and reads it back by address ([`AgentHost::put_content`],
//! [`AgentHost::content`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tilo_browser_peer::Host;
use tilo_content::ContentStore;
use tilo_relay_protocol::agents::AgentStatus;
use tilo_user_identity::DeviceCert;

use crate::apps::AppCatalogue;
use crate::control::{
    AgentControl, AgentHostLink, AgentIdentity, AgentSupervisor, SupervisorSnapshot,
};
use crate::model::ModelChoice;
use crate::participant::{AgentParticipant, ParticipantConfig};

/// The file under the state directory.
const STATE_FILE: &str = "agents.json";
/// The least time between two writes of the state: a burst of changes (an
/// agent's steps) is written as one.
const WRITE_SPACING: Duration = Duration::from_millis(500);
/// The directory under the state directory each room's content is kept in.
const CONTENT_DIR: &str = "content";

/// Builds the apps an agent may open, on the agent's own thread.
pub type CatalogueFactory = Arc<dyn Fn() -> AppCatalogue + Send + Sync>;

/// How agents reach the host they run on.
#[derive(Clone)]
pub struct HostEndpoint {
    /// The host's endpoint.
    pub host: Host,
    /// Its certificate.
    pub host_cert: DeviceCert,
}

impl HostEndpoint {
    /// The host at its iroh endpoint `addr`, the way a host's own agents
    /// reach it.
    #[must_use]
    pub const fn iroh(addr: iroh_base::EndpointAddr, host_cert: DeviceCert) -> Self {
        Self {
            host: Host::Iroh {
                addr,
                relay_url: None,
            },
            host_cert,
        }
    }
}

struct Shared {
    state_dir: PathBuf,
    model: ModelChoice,
    catalogue: CatalogueFactory,
    endpoint: OnceLock<HostEndpoint>,
    /// The runtime agents' sessions run their I/O on.
    runtime: OnceLock<tokio::runtime::Handle>,
    stopping: AtomicBool,
    /// Each agent's thread, with its control to wake it for shutdown.
    threads: Mutex<Vec<(JoinHandle<()>, Arc<AgentControl>)>>,
    /// Each room's content store, by the room's uuid.
    content: Mutex<BTreeMap<String, ContentStore>>,
    /// Each agent's session state, by its agent id: kept while the host
    /// runs, so an agent stopped and started again keeps its channel keys.
    sessions: Mutex<BTreeMap<String, Arc<tilo_browser_peer::MemoryPeerStore>>>,
}

impl Shared {
    /// The content store of `room`, under the state directory. One room's
    /// store never answers for another's.
    fn room_content(&self, room: &str) -> Result<ContentStore, String> {
        let room = uuid::Uuid::parse_str(room)
            .map_err(|_| "the room is not a uuid".to_string())?
            .hyphenated()
            .to_string();
        let mut stores = self.content.lock().unwrap_or_else(PoisonError::into_inner);
        let store = stores
            .entry(room)
            .or_insert_with_key(|room| {
                ContentStore::in_dir(self.state_dir.join(CONTENT_DIR).join(room))
            })
            .clone();
        drop(stores);
        Ok(store)
    }
}

/// Every agent of one host.
pub struct AgentHost {
    supervisor: Arc<AgentSupervisor>,
    shared: Arc<Shared>,
    ticker: Option<JoinHandle<()>>,
    written: Arc<Mutex<String>>,
    /// A runtime of its own when started outside one.
    owned_runtime: Option<tokio::runtime::Runtime>,
}

impl AgentHost {
    /// Agents of `user_public_key`, kept under `state_dir`, planning with
    /// `model`. Serve [`Self::supervisor`] to browsers (it answers the
    /// agent vocabulary), then [`Self::start`] once the host is up.
    #[must_use]
    pub fn new(
        user_public_key: [u8; 32],
        state_dir: PathBuf,
        model: ModelChoice,
        catalogue: CatalogueFactory,
    ) -> Self {
        let shared = Arc::new(Shared {
            state_dir,
            model,
            catalogue,
            endpoint: OnceLock::new(),
            runtime: OnceLock::new(),
            stopping: AtomicBool::new(false),
            threads: Mutex::new(Vec::new()),
            content: Mutex::new(BTreeMap::new()),
            sessions: Mutex::new(BTreeMap::new()),
        });
        let spawning = shared.clone();
        let supervisor = AgentSupervisor::new(
            user_public_key,
            Arc::new(move |identity, control| spawn_agent(&spawning, identity, control)),
        );
        Self {
            supervisor,
            shared,
            ticker: None,
            written: Arc::new(Mutex::new(String::new())),
            owned_runtime: None,
        }
    }

    /// The supervisor browsers steer agents through.
    #[must_use]
    pub const fn supervisor(&self) -> &Arc<AgentSupervisor> {
        &self.supervisor
    }

    /// Where the grants and agents are kept.
    #[must_use]
    pub fn state_file(&self) -> PathBuf {
        self.shared.state_dir.join(STATE_FILE)
    }

    /// Start: connect the host, restore what the state directory holds,
    /// start the agents that were running, and keep checking expiry and
    /// writing the state. Agents' sessions run their I/O on the calling
    /// Tokio runtime, or on one of the host's own outside a runtime.
    ///
    /// # Errors
    ///
    /// Returns why the state directory or file could not be read.
    pub fn start(
        &mut self,
        endpoint: HostEndpoint,
        link: Arc<dyn AgentHostLink>,
    ) -> Result<(), String> {
        std::fs::create_dir_all(&self.shared.state_dir)
            .map_err(|error| format!("create {}: {error}", self.shared.state_dir.display()))?;
        let _ = self.shared.endpoint.set(endpoint);
        let runtime = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle
        } else {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("agent-io")
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            let handle = runtime.handle().clone();
            self.owned_runtime = Some(runtime);
            handle
        };
        let _ = self.shared.runtime.set(runtime);
        self.supervisor.set_link(link);
        let snapshot = read_state(&self.state_file())?;
        let resume = self.supervisor.restore(snapshot, tilo_clock::unix_now_ns());
        for (identity, control) in resume {
            tracing::info!(agent = %identity.delegation.agent_id, "agent resumes after a host restart");
            if let Err(error) = spawn_agent(&self.shared, identity, control.clone()) {
                control.fail(&error);
            }
        }
        persist(&self.supervisor, &self.state_file(), &self.written);
        self.keep()
    }

    /// Keep the state written and the grants expired, on a thread that
    /// sleeps until the supervisor's snapshot changes, the next grant
    /// expires or the host stops — never on an interval.
    fn keep(&mut self) -> Result<(), String> {
        let supervisor = self.supervisor.clone();
        let shared = self.shared.clone();
        let written = self.written.clone();
        let file = self.state_file();
        self.ticker = Some(
            std::thread::Builder::new()
                .name("agent-host".into())
                .spawn(move || {
                    let changes = supervisor.changes().clone();
                    loop {
                        // Read before the snapshot: a change after this
                        // point wakes the wait below.
                        let seen = changes.generation();
                        for expired in supervisor.expire(tilo_clock::unix_now_ns()) {
                            tracing::info!(grant = %expired, "agent grant expired");
                        }
                        persist(&supervisor, &file, &written);
                        let wrote = Instant::now();
                        if shared.stopping.load(Ordering::Acquire) {
                            break;
                        }
                        let until_expiry = supervisor.next_expiry_ns().map(|at| {
                            Duration::from_nanos(at.saturating_sub(tilo_clock::unix_now_ns()))
                        });
                        changes.wait_past(seen, until_expiry);
                        if shared.stopping.load(Ordering::Acquire) {
                            break;
                        }
                        if let Some(rest) = WRITE_SPACING.checked_sub(wrote.elapsed()) {
                            std::thread::sleep(rest);
                        }
                    }
                })
                .map_err(|error| error.to_string())?,
        );
        Ok(())
    }

    /// Keep `bytes` in `room`'s content store, where its agents' apps read
    /// them, and answer their content address (`tilo.content:<hash>`), the
    /// way an operation's arguments name them.
    ///
    /// # Errors
    ///
    /// Returns why the room is not one, or why the store refused the bytes.
    pub fn put_content(&self, room: &str, bytes: &[u8]) -> Result<String, String> {
        let hash = self
            .shared
            .room_content(room)?
            .put(bytes)
            .map_err(|error| error.to_string())?;
        tilo_actions::content_resource(&hash).ok_or_else(|| "the store named no content".into())
    }

    /// The content `room` keeps at `address` (a content address or a bare
    /// hash): what was put, and what its agents' apps stored. `None` when
    /// the room holds none there.
    #[must_use]
    pub fn content(&self, room: &str, address: &str) -> Option<Vec<u8>> {
        let hash = tilo_actions::content_hash(address).unwrap_or(address);
        if !tilo_content::valid_hash(hash) {
            return None;
        }
        self.shared
            .room_content(room)
            .ok()?
            .read(&hash.to_ascii_lowercase())
    }

    /// Write the state now.
    pub fn flush(&self) {
        persist(&self.supervisor, &self.state_file(), &self.written);
    }

    /// Stop every agent thread, leaving each agent's status as it was so
    /// the next start resumes it, and write the state a last time.
    pub fn shutdown(mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.supervisor.changes().raise();
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        let threads = std::mem::take(
            &mut *self
                .shared
                .threads
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for (_, control) in &threads {
            control.wake();
        }
        for (thread, _) in threads {
            let _ = thread.join();
        }
        persist(&self.supervisor, &self.state_file(), &self.written);
        if let Some(runtime) = self.owned_runtime.take() {
            runtime.shutdown_background();
        }
    }
}

fn spawn_agent(
    shared: &Arc<Shared>,
    identity: AgentIdentity,
    control: Arc<AgentControl>,
) -> Result<(), String> {
    let endpoint = shared
        .endpoint
        .get()
        .cloned()
        .ok_or("the agent host has not started")?;
    let room = control
        .room()
        .or_else(|| identity.delegation.scope.rooms.first().cloned())
        .ok_or("the agent has no room")?;
    let thread_shared = shared.clone();
    let name = format!("agent-{}", identity.delegation.agent_id);
    let thread_control = control.clone();
    let handle = std::thread::Builder::new()
        .name(name)
        .spawn(move || run_agent(&thread_shared, endpoint, room, identity, &thread_control))
        .map_err(|error| error.to_string())?;
    let mut threads = shared
        .threads
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    threads.retain(|(thread, _)| !thread.is_finished());
    threads.push((handle, control));
    drop(threads);
    Ok(())
}

fn run_agent(
    shared: &Shared,
    endpoint: HostEndpoint,
    room: String,
    identity: AgentIdentity,
    control: &Arc<AgentControl>,
) {
    let agent_id = identity.delegation.agent_id.clone();
    let Some(runtime) = shared.runtime.get() else {
        control.fail("the agent host has not started");
        return;
    };
    let _runtime = runtime.enter();
    // The agent holds records as replicas of their resources and runs no
    // app's code, so it reads nothing of this machine's files or secrets;
    // it plans with the host's model.
    let planner = Box::new(shared.model.planner());
    let catalogue = (shared.catalogue)();
    let content = match shared.room_content(&room) {
        Ok(content) => content,
        Err(error) => {
            control.fail(&error);
            return;
        }
    };
    let session = Arc::clone(
        shared
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(agent_id.clone())
            .or_default(),
    );
    let mut participant = match AgentParticipant::start_with_store(
        ParticipantConfig {
            host: endpoint.host,
            host_cert: endpoint.host_cert,
            room,
            content,
        },
        identity,
        control.clone(),
        catalogue,
        planner,
        session,
    ) {
        Ok(participant) => participant,
        Err(error) => {
            tracing::warn!(agent = %agent_id, %error, "agent could not join its room");
            control.fail(&error);
            return;
        }
    };
    tracing::info!(agent = %agent_id, model = %shared.model.describe(), "agent joined its room");
    participant.run_until(None, |_| {
        shared.stopping.load(Ordering::Acquire) || control.status() == AgentStatus::Stopped
    });
    if control.status() == AgentStatus::Stopped {
        // One more pump publishes the cleared presence.
        participant.pump();
    }
    participant.close();
    tracing::info!(agent = %agent_id, "agent left its room");
}

fn read_state(file: &Path) -> Result<SupervisorSnapshot, String> {
    match std::fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("{} is not agent state: {error}", file.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(SupervisorSnapshot::default())
        }
        Err(error) => Err(format!("read {}: {error}", file.display())),
    }
}

/// Write the snapshot if it changed since the last write: to a private
/// temporary file, then renamed over the state file.
fn persist(supervisor: &AgentSupervisor, file: &Path, written: &Mutex<String>) {
    let Ok(text) = serde_json::to_string_pretty(&supervisor.snapshot()) else {
        return;
    };
    let mut last = written.lock().unwrap_or_else(PoisonError::into_inner);
    if *last == text {
        return;
    }
    let temporary = file.with_extension("json.tmp");
    let result =
        write_private(&temporary, text.as_bytes()).and_then(|()| std::fs::rename(&temporary, file));
    match result {
        Ok(()) => *last = text,
        Err(error) => tracing::warn!(%error, file = %file.display(), "agent state not written"),
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut out = options.open(path)?;
    out.write_all(bytes)?;
    out.sync_all()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_relay_protocol::agents::GrantState;

    /// Wait up to `limit` for `done`, checking every 10 ms.
    fn within(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        done()
    }

    fn grant_state(file: &Path) -> Option<GrantState> {
        read_state(file)
            .ok()?
            .grants
            .first()
            .map(|grant| grant.state)
    }

    #[test]
    fn the_idle_host_sleeps_until_its_state_changes_or_a_grant_expires() {
        let state = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[1; 32]);
        let mut host = AgentHost::new(
            root.verifying_key().to_bytes(),
            state.path().to_owned(),
            ModelChoice::Scripted,
            Arc::new(AppCatalogue::native),
        );
        #[cfg(target_os = "linux")]
        {
            use tilo_test_env::idle;
            let (kept, keeper) = idle::spawned_thread("agent-host", || host.keep());
            kept.unwrap();
            // Its first write (a synced file and a rename) is work, not
            // idling: measure once it is done.
            let first = host.state_file();
            assert!(
                within(Duration::from_secs(5), || first.exists()),
                "the first state was not written"
            );
            idle::assert_stays_asleep(&keeper, "agent host with nothing to write");
            let window = Duration::from_secs(2);
            let before = idle::context_switches(&keeper);
            std::thread::sleep(window);
            let woke = idle::context_switches(&keeper).saturating_sub(before);
            eprintln!("agent host idle wakeups: {woke} in {window:?}");
            assert_eq!(woke, 0, "the idle agent host woke");
        }
        #[cfg(not(target_os = "linux"))]
        host.keep().unwrap();

        // A change is written at once, not at the next tick.
        let expires_at = tilo_clock::unix_now_ns().saturating_add(1_500_000_000);
        let changed = Instant::now();
        host.supervisor()
            .add_grant(crate::control::tests::identity_until(&root, expires_at));
        let file = host.state_file();
        assert!(
            within(Duration::from_secs(2), || grant_state(&file)
                == Some(GrantState::Approved)),
            "the new grant was not written"
        );
        eprintln!("agent host wrote a change after {:?}", changed.elapsed());

        // With nothing else changing, the grant still expires on time.
        assert!(
            within(Duration::from_secs(4), || grant_state(&file)
                == Some(GrantState::Expired)),
            "the grant did not expire"
        );
        let late = Duration::from_nanos(tilo_clock::unix_now_ns().saturating_sub(expires_at));
        eprintln!("agent host expired the grant {late:?} after its expiry");
        assert!(late < Duration::from_millis(1_500), "expired {late:?} late");
        host.shutdown();
    }

    const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
    const OTHER_ROOM: &str = "0b8e2f64-93d1-4c55-a6f7-2e9d1c4b8a30";

    fn agent_host(state: &Path) -> AgentHost {
        AgentHost::new(
            [7; 32],
            state.to_path_buf(),
            ModelChoice::Scripted,
            Arc::new(|| AppCatalogue::new(Vec::new())),
        )
    }

    #[test]
    fn content_put_in_a_room_reads_back_by_its_address() {
        let state = tempfile::tempdir().unwrap();
        let host = agent_host(state.path());
        let address = host.put_content(ROOM, b"a picture's bytes").unwrap();
        let hash = tilo_actions::content_hash(&address).unwrap();
        assert_eq!(hash, tilo_content::hash_bytes(b"a picture's bytes"));
        assert_eq!(
            host.content(ROOM, &address).as_deref(),
            Some(&b"a picture's bytes"[..])
        );
        assert_eq!(
            host.content(&ROOM.to_uppercase(), hash).as_deref(),
            Some(&b"a picture's bytes"[..]),
            "a bare hash, in the room however its uuid is spelt"
        );
        // Kept under the state directory: a host started again reads it.
        let again = agent_host(state.path());
        assert!(again.content(ROOM, &address).is_some());
    }

    #[test]
    fn one_rooms_content_is_not_anothers() {
        let state = tempfile::tempdir().unwrap();
        let host = agent_host(state.path());
        let address = host.put_content(ROOM, b"kept in one room").unwrap();
        assert!(host.content(OTHER_ROOM, &address).is_none());
        assert!(host.put_content("../escape", b"anything").is_err());
        assert!(host.content("../escape", &address).is_none());
    }
}
