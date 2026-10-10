//! Agents on a host: each agent's participant on its own thread, every grant
//! and agent kept under a state directory, picked up again when the host
//! restarts.
//!
//! [`AgentHost`] owns an [`AgentSupervisor`] whose spawner starts one thread
//! per agent. The thread mounts its apps, plans with the host's
//! [`ModelChoice`] and pumps the participant until the agent stops or the
//! host shuts down, sleeping between pumps until its session or steering
//! changes or its presence keep-alive is due. A ticker settles expired
//! grants (the agent stops and the host refuses its device) and writes the
//! supervisor's snapshot to `agents.json` whenever it changed. The file
//! holds the agents' device keys and is written owner-only.
//!
//! On start the snapshot is restored ([`AgentSupervisor::restore`]):
//! approved grants are registered with the host again, agents that were
//! neither stopped nor failed start again with their queued tasks (a task
//! that was running when the host went down is marked failed), and paused
//! agents come back paused. A host shutting down leaves its agents'
//! statuses as they were, so the next start resumes them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use tilo_browser_peer::Host;
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
/// How often grants are checked for expiry and the state written.
const TICK_INTERVAL: Duration = Duration::from_millis(500);

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
        let supervisor = self.supervisor.clone();
        let shared = self.shared.clone();
        let written = self.written.clone();
        let file = self.state_file();
        self.ticker = Some(
            std::thread::Builder::new()
                .name("agent-host".into())
                .spawn(move || {
                    while !shared.stopping.load(Ordering::Acquire) {
                        for expired in supervisor.expire(tilo_clock::unix_now_ns()) {
                            tracing::info!(grant = %expired, "agent grant expired");
                        }
                        persist(&supervisor, &file, &written);
                        std::thread::sleep(TICK_INTERVAL);
                    }
                })
                .map_err(|error| error.to_string())?,
        );
        Ok(())
    }

    /// Write the state now.
    pub fn flush(&self) {
        persist(&self.supervisor, &self.state_file(), &self.written);
    }

    /// Stop every agent thread, leaving each agent's status as it was so
    /// the next start resumes it, and write the state a last time.
    pub fn shutdown(mut self) {
        self.shared.stopping.store(true, Ordering::Release);
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
    let mut participant = match AgentParticipant::start(
        ParticipantConfig {
            host: endpoint.host,
            host_cert: endpoint.host_cert,
            room,
        },
        identity,
        control.clone(),
        catalogue,
        planner,
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
