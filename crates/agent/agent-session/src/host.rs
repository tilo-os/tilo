//! Agents on a host: each agent's member on its own thread, every grant
//! and agent kept under a state directory, picked up again when the host
//! restarts.
//!
//! [`AgentService`] owns an [`AgentSupervisor`] whose spawner starts one thread
//! per agent. The thread pumps the member, which works on each task with
//! the host's model ([`MakeTurns`]), until the agent stops or the
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
//! Each workspace's content (the bytes its records name by hash) is kept under
//! `content/<workspace>` in the state directory, in this process: the host's
//! one store, which its workspace actions read and write. A harness puts
//! content in and reads it back by address ([`AgentService::put_content`],
//! [`AgentService::content`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tilo_content::ContentStore;
use tilo_server_client::ServerAddress;

use tilo_content::WorkspaceStores;
use tilo_server_protocol::agents::AgentStatus;
use tilo_user_identity::DeviceCert;

use crate::apps::AppCatalogue;
use crate::control::{
    AgentControl, AgentHostLink, AgentIdentity, AgentSupervisor, RosterCreatedAt,
    SupervisorSnapshot,
};
use crate::member::{AgentMember, MemberConfig};
use crate::model::MakeTurns;
use tilo_lock::MutexRecover as _;

const STATE_FILE: &str = "agents.json";
/// The least time between two writes of the state: a burst of changes (an
/// agent's steps) is written as one.
const WRITE_SPACING: Duration = Duration::from_millis(500);
/// The directory under the state directory each workspace's content is kept in.
/// Where a test keeps the host's content, under its state directory.
#[cfg(test)]
const CONTENT_DIR: &str = "content";

/// Builds the apps a host's workspace actions open records in.
pub type CatalogueFactory = Arc<dyn Fn() -> AppCatalogue + Send + Sync>;

/// How agents reach the host they run on.
#[derive(Clone)]
pub struct ServerEndpoint {
    pub host: ServerAddress,
    /// Its certificate.
    pub server_cert: DeviceCert,
}

impl ServerEndpoint {
    /// The host at its iroh endpoint `addr`, the way a host's own agents
    /// reach it.
    #[must_use]
    pub fn iroh(addr: iroh_base::EndpointAddr, server_cert: DeviceCert) -> Self {
        Self {
            host: ServerAddress::at(addr),
            server_cert,
        }
    }

    fn member_config(self, workspace: String) -> MemberConfig {
        MemberConfig {
            host: self.host,
            server_cert: self.server_cert,
            workspace,
        }
    }
}

type MemberThread = (JoinHandle<()>, Arc<dyn Fn() + Send + Sync>);

struct Shared {
    state_dir: PathBuf,
    model: MakeTurns,
    endpoint: OnceLock<ServerEndpoint>,
    /// The link the supervisor asks the host it runs on through, once it
    /// started.
    link: OnceLock<Arc<dyn AgentHostLink>>,
    /// The runtime agents' sessions run their I/O on.
    runtime: OnceLock<tokio::runtime::Handle>,
    stopping: AtomicBool,
    /// Each agent's thread, with its control to wake it for shutdown.
    threads: Mutex<Vec<MemberThread>>,
    /// Each workspace's content store: the host's one store, which its
    /// workspace actions use too.
    content: Arc<WorkspaceStores>,
    /// Each agent's session state, by its agent id: kept while the host
    /// runs, so an agent stopped and started again keeps its channel keys.
    sessions: Mutex<BTreeMap<String, Arc<tilo_session_client::MemoryPeerStore>>>,
    /// The agents' sessions here, which sign a change an action made for each
    /// ([`crate::workspace_actions`]).
    agent_sessions: crate::control::AgentSessions,
    /// The wall clock the keeper judges grants by.
    clock: HostClock,
    /// Standalone tasks and grants left dormant when only roster service is enabled.
    dormant: OnceLock<SupervisorSnapshot>,
}

/// How the host reads the wall clock. The keeper runs on its own thread,
/// which a test's thread-scoped clock does not reach, so a test hands the
/// host its pinned time instead of pinning the whole process.
type HostClock = Arc<dyn Fn() -> u64 + Send + Sync>;

impl Shared {
    /// The content store of `workspace`, under the state directory.
    fn workspace_content(&self, workspace: &str) -> Result<ContentStore, String> {
        self.content.get(workspace)
    }
}

pub struct AgentService {
    supervisor: Arc<AgentSupervisor>,
    shared: Arc<Shared>,
    ticker: Option<JoinHandle<()>>,
    written: Arc<Mutex<String>>,
    /// A runtime of its own when started outside one.
    owned_runtime: Option<tokio::runtime::Runtime>,
}

impl AgentService {
    /// Agents of `user_public_key`, kept under `state_dir`, working with
    /// `model`, the host's workspaces' content in `content`. Serve
    /// [`Self::supervisor`] to browsers (it answers the agent vocabulary),
    /// then [`Self::start`] once the host is up.
    #[must_use]
    pub fn new(
        user_public_key: [u8; 32],
        (state_dir, content): (PathBuf, Arc<WorkspaceStores>),
        model: MakeTurns,
    ) -> Self {
        Self::with_clock(
            user_public_key,
            (state_dir, content),
            model,
            Arc::new(tilo_clock::unix_now_ns),
        )
    }

    /// [`Self::new`] with the wall clock read by `clock` (Unix nanoseconds).
    #[must_use]
    pub fn with_clock(
        user_public_key: [u8; 32],
        (state_dir, content): (PathBuf, Arc<WorkspaceStores>),
        model: MakeTurns,
        clock: HostClock,
    ) -> Self {
        let shared = Arc::new(Shared {
            content,
            state_dir,
            model,
            endpoint: OnceLock::new(),
            link: OnceLock::new(),
            runtime: OnceLock::new(),
            stopping: AtomicBool::new(false),
            threads: Mutex::new(Vec::new()),
            sessions: Mutex::new(BTreeMap::new()),
            agent_sessions: crate::control::AgentSessions::default(),
            clock,
            dormant: OnceLock::new(),
        });
        let spawning = shared.clone();
        let supervisor = AgentSupervisor::new(
            user_public_key,
            Arc::new(move |identity, control| spawn_agent(&spawning, identity, control)),
        );
        let joining = Arc::clone(&shared);
        let supervised = Arc::downgrade(&supervisor);
        supervisor.set_roster_spawner(Arc::new(move |identity, created_at| {
            spawn_roster_session(&joining, identity, created_at, supervised.clone())
        }));
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

    /// The agents' sessions here, which sign a change an action made for each.
    #[must_use]
    pub fn agent_sessions(&self) -> crate::control::AgentSessions {
        self.shared.agent_sessions.clone()
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
    pub fn start(
        &mut self,
        endpoint: ServerEndpoint,
        link: Arc<dyn AgentHostLink>,
    ) -> Result<(), String> {
        self.start_serving(endpoint, link, false)
    }

    /// Start the roster signing peers, keeping standalone task agents dormant.
    ///
    /// # Errors
    /// The state file could not be read or the host could not start.
    pub fn start_roster(
        &mut self,
        endpoint: ServerEndpoint,
        link: Arc<dyn AgentHostLink>,
    ) -> Result<(), String> {
        self.start_serving(endpoint, link, true)
    }

    fn start_serving(
        &mut self,
        endpoint: ServerEndpoint,
        link: Arc<dyn AgentHostLink>,
        roster_only: bool,
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
        let _ = self.shared.link.set(Arc::clone(&link));
        self.supervisor.set_link(link);
        let mut snapshot = read_state(&self.state_file())?;
        if roster_only {
            let (roster, standalone) = snapshot
                .grants
                .into_iter()
                .partition(|grant| grant.roster_created_at.is_some());
            let _ = self.shared.dormant.set(SupervisorSnapshot {
                grants: standalone,
                agents: std::mem::take(&mut snapshot.agents),
            });
            snapshot.grants = roster;
        }
        let resume = self.supervisor.restore(snapshot, (self.shared.clock)());
        for (identity, control) in resume {
            tracing::info!(agent = %crate::control::agent_id(&identity.delegation), "agent resumes after a host restart");
            if let Err(error) = spawn_agent(&self.shared, identity, control.clone()) {
                control.fail(&error);
            }
        }
        persist(&self.supervisor, &self.shared, &self.written);
        self.keep()
    }

    /// Keep the state written and the grants expired, on a thread that
    /// sleeps until the supervisor's snapshot changes, the next grant
    /// expires or the host stops — never on an interval.
    fn keep(&mut self) -> Result<(), String> {
        let supervisor = self.supervisor.clone();
        let shared = self.shared.clone();
        let written = self.written.clone();
        let clock = Arc::clone(&self.shared.clock);
        self.ticker = Some(
            std::thread::Builder::new()
                .name("agent-host".into())
                .spawn(move || {
                    let changes = supervisor.changes().clone();
                    loop {
                        // Read before the snapshot: a change after this
                        // point wakes the wait below.
                        let seen = changes.generation();
                        for expired in supervisor.expire((clock)()) {
                            tracing::info!(grant = %expired, "agent grant expired");
                        }
                        persist(&supervisor, &shared, &written);
                        let wrote = Instant::now();
                        if shared.stopping.load(Ordering::Acquire) {
                            break;
                        }
                        let until_expiry = supervisor
                            .next_expiry_ns()
                            .map(|at| Duration::from_nanos(at.saturating_sub((clock)())));
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

    /// Keep `bytes` in `workspace`'s content store, where its agents' apps read
    /// them, and answer their content address (`tilo.content:<hash>`), the
    /// way an operation's arguments name them.
    ///
    /// # Errors
    ///
    /// Returns why the workspace is not one, or why the store refused the bytes.
    pub fn put_content(&self, workspace: &str, bytes: &[u8]) -> Result<String, String> {
        // Sealed here, before an action names it: the host keeps and serves
        // ciphertext, and the reference the action carries holds the key.
        let hash = self
            .shared
            .workspace_content(workspace)?
            .sealing()
            .put(bytes)
            .map_err(|error| error.to_string())?;
        tilo_actions::content_resource(&hash).ok_or_else(|| "the store named no content".into())
    }

    /// The content `workspace` keeps at `address` (a content address or a bare
    /// hash): what was put, and what its agents' apps stored. `None` when
    /// the workspace holds none there.
    #[must_use]
    pub fn content(&self, workspace: &str, address: &str) -> Option<Vec<u8>> {
        let reference = tilo_actions::content_hash(address).unwrap_or(address);
        if !tilo_content::valid_reference(reference) {
            return None;
        }
        self.shared
            .workspace_content(workspace)
            .ok()?
            .read(reference)
    }

    /// Write the state now.
    pub fn flush(&self) {
        persist(&self.supervisor, &self.shared, &self.written);
    }

    /// Stop every agent thread, leaving each agent's status as it was so
    /// the next start resumes it, and write the state a last time.
    pub fn shutdown(mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.supervisor.changes().raise();
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        let threads = std::mem::take(&mut *self.shared.threads.lock_recover());
        for (_, wake) in &threads {
            wake();
        }
        for (thread, _) in threads {
            let _ = thread.join();
        }
        persist(&self.supervisor, &self.shared, &self.written);
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
        .ok_or("the agent supervisor has not started")?;
    let workspace = control
        .workspace()
        .or_else(|| {
            identity
                .delegation
                .scope
                .workspaces
                .first()
                .map(ToString::to_string)
        })
        .ok_or("the agent has no workspace")?;
    let thread_shared = shared.clone();
    let name = format!("agent-{}", crate::control::agent_id(&identity.delegation));
    let thread_control = control.clone();
    let handle = std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            run_agent(
                &thread_shared,
                endpoint,
                workspace,
                identity,
                &thread_control,
            );
        })
        .map_err(|error| error.to_string())?;
    keep_thread(shared, handle, Arc::new(move || control.wake()));
    Ok(())
}

fn keep_thread(shared: &Shared, handle: JoinHandle<()>, wake: Arc<dyn Fn() + Send + Sync>) {
    let mut threads = shared.threads.lock_recover();
    threads.retain(|(thread, _)| !thread.is_finished());
    threads.push((handle, wake));
}

fn spawn_roster_session(
    shared: &Arc<Shared>,
    identity: AgentIdentity,
    created_at: RosterCreatedAt,
    supervisor: std::sync::Weak<AgentSupervisor>,
) -> Result<(), String> {
    let endpoint = shared
        .endpoint
        .get()
        .cloned()
        .ok_or("the agent supervisor has not started")?;
    let runtime = shared
        .runtime
        .get()
        .cloned()
        .ok_or("the agent supervisor has not started")?;
    let joining = Arc::clone(shared);
    let mut threads = shared.threads.lock_recover();
    if shared.stopping.load(Ordering::Acquire) {
        return Err("the agent supervisor is stopping".into());
    }
    let thread = std::thread::Builder::new()
        .name(format!(
            "roster-{}",
            crate::control::agent_id(&identity.delegation)
        ))
        .spawn(move || {
            let _runtime = runtime.enter();
            if let Err(error) = join_roster_session(&joining, endpoint, &identity, created_at) {
                tracing::warn!(agent = %crate::control::agent_id(&identity.delegation), %error,
                    "the roster agent's signing session ended");
                if !joining.stopping.load(Ordering::Acquire)
                    && let Some(supervisor) = supervisor.upgrade()
                {
                    supervisor.roster_session_failed(identity.cert.device_public_key, &error);
                }
            }
        })
        .map_err(|error| error.to_string())?;
    let unpark = thread.thread().clone();
    threads.retain(|(thread, _)| !thread.is_finished());
    threads.push((thread, Arc::new(move || unpark.unpark())));
    drop(threads);
    Ok(())
}

fn join_roster_session(
    shared: &Shared,
    endpoint: ServerEndpoint,
    identity: &AgentIdentity,
    created_at: RosterCreatedAt,
) -> Result<(), String> {
    let link = shared
        .link
        .get()
        .ok_or("the agent supervisor has not started")?;
    let [workspace] = identity.delegation.scope.workspaces.as_slice() else {
        return Err("a roster signing session joins exactly one workspace".into());
    };
    let (admitted, reply) = std::sync::mpsc::channel();
    let (_open, lifetime) = tokio::sync::oneshot::channel();
    link.admit_roster_session(
        identity,
        created_at,
        lifetime,
        Box::new(move |result| {
            let _ = admitted.send(result);
        }),
    );
    reply
        .recv_timeout(ADMISSION_WAIT)
        .map_err(|error| error.to_string())??;
    if shared.stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    let config = endpoint.member_config(workspace.to_string());
    let thread = std::thread::current();
    let (peer, events) = crate::member::join(
        &config,
        identity,
        *workspace,
        Arc::new(tilo_session_client::MemoryPeerStore::default()),
        Arc::new(move || thread.unpark()),
    )?;
    drop(events);
    let device = identity.cert.device_public_key;
    shared.agent_sessions.joined(
        device,
        peer.clone(),
        Some((created_at, identity.delegation.clone())),
    );
    let result = loop {
        if shared.stopping.load(Ordering::Acquire) {
            break Ok(());
        }
        if matches!(
            peer.status().phase,
            tilo_session_client::TransportPhase::Failed
                | tilo_session_client::TransportPhase::Closed
        ) {
            break Err("the roster agent's signing session closed".into());
        }
        std::thread::park();
    };
    peer.close();
    shared.agent_sessions.left(&device);
    result
}

fn run_agent(
    shared: &Shared,
    endpoint: ServerEndpoint,
    workspace: String,
    identity: AgentIdentity,
    control: &Arc<AgentControl>,
) {
    let agent_id = crate::control::agent_id(&identity.delegation).to_owned();
    let Some(runtime) = shared.runtime.get() else {
        control.fail("the agent supervisor has not started");
        return;
    };
    let _runtime = runtime.enter();
    // A workspace admits only the devices its roster names, and a session it
    // refuses is not dialled again on its own: the agent waits for its place
    // on its workspaces before it joins, rather than race the host that
    // writes it.
    wait_admitted(shared, &identity);
    // The agent runs no app's code and reads nothing of this machine's files
    // or secrets: it acts through the host's actions, with the host's model.
    let session = Arc::clone(
        shared
            .sessions
            .lock_recover()
            .entry(agent_id.clone())
            .or_default(),
    );
    let device = identity.cert.device_public_key;
    let mut member = match AgentMember::start_with_store(
        endpoint.member_config(workspace),
        identity,
        control.clone(),
        Arc::clone(&shared.model),
        session,
    ) {
        Ok(member) => member,
        Err(error) => {
            tracing::warn!(agent = %agent_id, %error, "agent could not join its workspace");
            control.fail(&error);
            return;
        }
    };
    shared
        .agent_sessions
        .joined(device, member.peer().clone(), None);
    tracing::info!(agent = %agent_id, "agent joined its workspace");
    member.run_until(None, |_| {
        shared.stopping.load(Ordering::Acquire) || control.status() == AgentStatus::Stopped
    });
    if control.status() == AgentStatus::Stopped {
        // One more pump publishes the cleared presence.
        member.pump();
    }
    member.close();
    shared.agent_sessions.left(&device);
    tracing::info!(agent = %agent_id, "agent left its workspace");
}

/// How long an agent waits for its workspaces to admit it before it joins
/// anyway, so that a host which never answers still lets it try and report
/// why it could not.
const ADMISSION_WAIT: Duration = Duration::from_secs(30);

/// Wait until the host's workspaces hold `identity` as a member of their own,
/// when the host says when they do.
fn wait_admitted(shared: &Shared, identity: &AgentIdentity) {
    let Some(link) = shared.link.get() else {
        return;
    };
    let (admitted, wait) = std::sync::mpsc::channel();
    link.when_admitted(
        &identity.cert,
        &identity.delegation,
        Box::new(move || {
            let _ = admitted.send(());
        }),
    );
    if wait.recv_timeout(ADMISSION_WAIT).is_err() {
        tracing::warn!(agent = %crate::control::agent_id(&identity.delegation),
            "the agent's workspaces did not admit it in time");
    }
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

/// Write the snapshot if it changed since the last write: a private file
/// replaced whole, so a reader never sees half of it.
fn persist(supervisor: &AgentSupervisor, shared: &Shared, written: &Mutex<String>) {
    let mut last = written.lock_recover();
    let mut snapshot = supervisor.snapshot();
    if let Some(dormant) = shared.dormant.get() {
        snapshot.grants.extend_from_slice(&dormant.grants);
        snapshot.agents.clone_from(&dormant.agents);
    }
    let Ok(text) = serde_json::to_string_pretty(&snapshot) else {
        return;
    };
    if *last == text {
        return;
    }
    let file = shared.state_dir.join(STATE_FILE);
    match tilo_files::native::atomic::write_with_mode(&file, text.as_bytes(), 0o600) {
        Ok(()) => *last = text,
        Err(error) => tracing::warn!(%error, file = %file.display(), "agent state not written"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_server_protocol::agents::GrantState;
    use tilo_test_env::wait_until;

    fn grant_state(file: &Path) -> Option<GrantState> {
        read_state(file)
            .ok()?
            .grants
            .first()
            .map(|grant| grant.state)
    }

    #[test]
    fn the_idle_host_sleeps_until_its_state_changes_or_a_grant_expires() {
        // The wall clock stands still until the test moves it past the
        // grant's expiry, so a loaded runner cannot expire it early. The
        // keeper reads the clock this test hands it: it runs on its own
        // thread, which a thread-scoped pin does not reach.
        let start = tilo_clock::unix_now_ns();
        let pinned = Arc::new(std::sync::atomic::AtomicU64::new(start));
        let state = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[1; 32]);
        let clock = Arc::clone(&pinned);
        let mut host = AgentService::with_clock(
            root.verifying_key().to_bytes(),
            (state.path().to_owned(), test_content(state.path())),
            crate::ModelChoice::Scripted.make_turns(),
            Arc::new(move || clock.load(Ordering::Relaxed)),
        );
        #[cfg(target_os = "linux")]
        {
            use tilo_test_env::idle;
            let (kept, keeper) = idle::spawned_thread("agent-host", || host.keep());
            kept.unwrap();
            // Its first write (a synced file and a rename) is work, not
            // idling: measure once it is done.
            let first = host.state_file();
            wait_until("the first state write", || first.exists());
            idle::assert_stays_asleep(&keeper, "agent host with nothing to write");
            let window = Duration::from_secs(2);
            let before = idle::asleep_runs(&keeper);
            std::thread::sleep(window);
            let woke = idle::asleep_runs(&keeper).saturating_sub(before);
            eprintln!("agent host idle wakeups: {woke} in {window:?}");
            assert_eq!(woke, 0, "the idle agent host woke");
        }
        #[cfg(not(target_os = "linux"))]
        host.keep().unwrap();

        // A change is written at once, not at the next tick.
        let expires_at = start.saturating_add(1_500_000_000);
        let changed = Instant::now();
        host.supervisor()
            .add_grant(crate::control::tests::identity_until(&root, expires_at));
        let file = host.state_file();
        wait_until("the new grant's write", || {
            grant_state(&file) == Some(GrantState::Approved)
        });
        eprintln!("agent host wrote a change after {:?}", changed.elapsed());

        // With nothing else changing, the grant still expires: the host has
        // no interval, so only the wake it sets for the expiry gets it there.
        // A wake set from the wrong deadline or unit is minutes off, past
        // the hang bound.
        pinned.store(start.saturating_add(2_000_000_000), Ordering::Relaxed);
        wait_until("the grant's expiry", || {
            grant_state(&file) == Some(GrantState::Expired)
        });
        eprintln!(
            "agent host expired the grant {:?} after it was granted",
            changed.elapsed()
        );
        host.shutdown();
    }

    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
    const OTHER_WORKSPACE: &str = "0b8e2f64-93d1-4c55-a6f7-2e9d1c4b8a30";

    /// The host's content, under its state directory as a server keeps it.
    fn test_content(state: &Path) -> Arc<WorkspaceStores> {
        Arc::new(WorkspaceStores::in_dir(
            state.join(CONTENT_DIR),
            tilo_content::ContentLimits::default(),
        ))
    }

    fn agent_service(state: &Path) -> AgentService {
        AgentService::new(
            [7; 32],
            (state.to_path_buf(), test_content(state)),
            crate::ModelChoice::Scripted.make_turns(),
        )
    }

    #[test]
    fn content_put_in_a_workspace_reads_back_by_its_address() {
        let state = tempfile::tempdir().unwrap();
        let host = agent_service(state.path());
        let address = host.put_content(WORKSPACE, b"a picture's bytes").unwrap();
        let reference = tilo_actions::content_hash(&address).unwrap();
        assert!(tilo_content::is_sealed(reference), "{reference}");
        assert_eq!(
            host.content(WORKSPACE, &address).as_deref(),
            Some(&b"a picture's bytes"[..])
        );
        assert_eq!(
            host.content(&WORKSPACE.to_uppercase(), reference)
                .as_deref(),
            Some(&b"a picture's bytes"[..]),
            "a bare reference, in the workspace however its uuid is spelt"
        );
        let stored = tilo_content::content_address(reference).unwrap();
        assert_ne!(
            host.content(WORKSPACE, &stored).as_deref(),
            Some(&b"a picture's bytes"[..]),
            "the store holds ciphertext alone"
        );
        // Kept under the state directory: a host started again reads it.
        let again = agent_service(state.path());
        assert!(again.content(WORKSPACE, &address).is_some());
    }

    #[test]
    fn one_workspaces_content_is_not_anothers() {
        let state = tempfile::tempdir().unwrap();
        let host = agent_service(state.path());
        let address = host
            .put_content(WORKSPACE, b"kept in one workspace")
            .unwrap();
        assert!(host.content(OTHER_WORKSPACE, &address).is_none());
        assert!(host.put_content("../escape", b"anything").is_err());
        assert!(host.content("../escape", &address).is_none());
    }

    /// The content addresses kept under `dir`, each with what it holds: a
    /// store names an item's file after its hash, in a shard directory of
    /// its own, beside the lock and generation files it keeps to write
    /// safely.
    fn contents(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut found = BTreeMap::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    pending.push(path);
                } else if let (Some(name), Ok(bytes)) = (
                    entry.file_name().to_str().map(str::to_owned),
                    std::fs::read(&path),
                ) && tilo_content::valid_hash(&name)
                {
                    found.insert(name, bytes);
                }
            }
        }
        found
    }

    /// Every file of `dir` that is not content, relative to it: what a
    /// person would read as a note, a log or a record.
    fn documents(dir: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    pending.push(path);
                    continue;
                }
                let named = path.strip_prefix(dir).unwrap_or(&path).to_string_lossy();
                if !named.contains("content/")
                    && !entry
                        .file_name()
                        .to_str()
                        .is_some_and(tilo_content::valid_hash)
                {
                    found.push(named.into_owned());
                }
            }
        }
        found.sort();
        found
    }

    /// An agent whose work is done leaves nothing in the host's state
    /// directory but its credentials and the workspace's content: no notes, no
    /// facts, nothing read off a record. What it learned, it wrote where a
    /// person would, in the workspace's records, which the state directory does
    /// not hold.
    #[test]
    fn the_host_keeps_credentials_and_content_and_nothing_learned() {
        let state = tempfile::tempdir().unwrap();
        let host = agent_service(state.path());
        let reference = host.put_content(WORKSPACE, b"a picture's bytes").unwrap();
        // A grant survives a restart, which is the state file's whole job.
        let root = SigningKey::from_bytes(&[7; 32]);
        let expires_at = tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000);
        host.supervisor()
            .add_grant(crate::control::tests::identity_until(&root, expires_at));
        host.flush();

        // The credential file, and the one content address the workspace
        // holds: no file of any other kind anywhere under it.
        let kept = contents(&state.path().join(CONTENT_DIR));
        let reference = tilo_actions::content_hash(&reference).unwrap();
        let address = tilo_content::content_address(reference).unwrap();
        assert_eq!(
            kept.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![address.as_str()],
        );
        assert_ne!(
            kept[&address].as_slice(),
            b"a picture's bytes",
            "kept sealed"
        );
        assert_eq!(documents(state.path()), vec!["agents.json"]);
        // The state file is the credentials and the grants, nothing about
        // the person or the work.
        let text = std::fs::read(state.path().join("agents.json")).unwrap();
        let written: serde_json::Value = serde_json::from_slice(&text).unwrap();
        assert_eq!(written["grants"].as_array().map(Vec::len), Some(1));
        let mut fields: Vec<&str> = written
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect())
            .unwrap_or_default();
        fields.sort_unstable();
        assert_eq!(fields, vec!["agents", "grants"]);
    }
}
