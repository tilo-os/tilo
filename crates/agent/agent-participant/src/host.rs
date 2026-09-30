//! Agents on a host: each agent's participant on its own thread, every grant
//! and agent kept under a state directory, picked up again when the host
//! restarts.
//!
//! [`AgentHost`] owns an [`AgentSupervisor`] whose spawner starts one thread
//! per agent. The thread mounts its apps, plans with the host's
//! [`ModelChoice`] and pumps the participant until the agent stops or the
//! host shuts down. A ticker settles expired grants (the agent stops and the
//! host refuses its device) and writes the supervisor's snapshot to
//! `agents.json` whenever it changed. The file holds the agents' device keys
//! and is written owner-only.
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
/// How often an agent's loop takes in its session and steps.
const PUMP_INTERVAL: Duration = Duration::from_millis(20);
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
    /// Agents use the host user's profile and secret store instead of their
    /// own (granted explicitly with [`AgentHost::share_host_profile`]).
    host_profile: AtomicBool,
    model: ModelChoice,
    catalogue: CatalogueFactory,
    endpoint: OnceLock<HostEndpoint>,
    /// The runtime agents' sessions run their I/O on.
    runtime: OnceLock<tokio::runtime::Handle>,
    stopping: AtomicBool,
    threads: Mutex<Vec<JoinHandle<()>>>,
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
            host_profile: AtomicBool::new(false),
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

    /// Let agents' apps use the host user's own files and secret store
    /// (the Keychain, the Secret Service), as a person at this machine
    /// would. By default each agent has a profile of its own below the
    /// state directory and secrets kept in memory. Call before
    /// [`Self::start`].
    pub fn share_host_profile(&self) {
        self.shared.host_profile.store(true, Ordering::Release);
    }

    /// Where an agent's apps keep their files unless the host profile is
    /// shared.
    #[must_use]
    pub fn profile_dir(&self, agent_id: &str) -> PathBuf {
        profile_dir(&self.shared.state_dir, agent_id)
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
        let resume = self.supervisor.restore(snapshot, now_ns());
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
                        for expired in supervisor.expire(now_ns()) {
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
        for thread in threads {
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
    let handle = std::thread::Builder::new()
        .name(name)
        .spawn(move || run_agent(&thread_shared, endpoint, room, identity, &control))
        .map_err(|error| error.to_string())?;
    let mut threads = shared
        .threads
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    threads.retain(|thread| !thread.is_finished());
    threads.push(handle);
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
    // The model and the apps' theme and fonts are the host's; what the
    // apps keep (files, secrets) is the agent's own.
    let planner = Box::new(HostSettings(shared.model.planner()));
    let catalogue = (shared.catalogue)();
    let _profile = if shared.host_profile.load(Ordering::Acquire) {
        None
    } else {
        let directory = profile_dir(&shared.state_dir, &agent_id);
        if let Err(error) = std::fs::create_dir_all(&directory) {
            control.fail(&format!("create {}: {error}", directory.display()));
            return;
        }
        Some((
            tilo_paths::scope_profile(directory),
            tilo_secrets::scope_thread(tilo_secrets::memory_backend()),
        ))
    };
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
    while !shared.stopping.load(Ordering::Acquire) {
        participant.pump();
        if control.status() == AgentStatus::Stopped {
            // One more pump publishes the cleared presence.
            participant.pump();
            break;
        }
        std::thread::sleep(PUMP_INTERVAL);
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

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        })
}

/// `<state>/<agent id>/profile`: an agent's home, with its id reduced to
/// characters safe in one path component.
fn profile_dir(state_dir: &std::path::Path, agent_id: &str) -> PathBuf {
    let component: String = agent_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    state_dir.join(component).join("profile")
}

/// Plans with the host user's settings and model gateway while the agent's
/// thread otherwise sees its own profile.
struct HostSettings(crate::ModelPlanner);

impl crate::Planner for HostSettings {
    fn plan(
        &mut self,
        task: &str,
        context: &crate::PlanContext,
    ) -> Result<Vec<crate::Step>, String> {
        tilo_paths::unscoped(|| tilo_secrets::unscoped(|| self.0.plan(task, context)))
    }
}
