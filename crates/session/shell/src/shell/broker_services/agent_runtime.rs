//! Owning workers for the broker's authenticated native Agent start door.
use super::NativeOwnerCommitService;
use crate::shell::managed_agent::{
    NativeAgentCancellation, NativeAgentInstall, NativeAgentServiceEndpoints, NativeAgentStart,
    NativeManagedSession,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tilo_action_broker::collab_broker::{AppActionBroker, WeakAppActionBroker};
use tilo_user_identity::DeviceCert;

type AgentKey = (String, String);

struct Worker {
    stop: mpsc::SyncSender<()>,
    cancellation: Arc<Mutex<Option<NativeAgentCancellation>>>,
    join: JoinHandle<Result<(), String>>,
}

impl Worker {
    fn request_stop(&self) -> Result<(), String> {
        let _ = self.stop.try_send(());
        if let Some(control) = self
            .cancellation
            .lock()
            .map_err(|_| "agent cancellation state poisoned")?
            .as_ref()
        {
            control.cancel()?;
        }
        Ok(())
    }

    fn finish(self) -> Result<(), String> {
        self.join
            .join()
            .map_err(|_| "native Agent worker panicked")?
    }
}

#[derive(Clone)]
struct StartRequest {
    room: String,
    agent: String,
    caller: DeviceCert,
}

/// Owns every process started by this native broker. Dropping the service
/// stops those exact launcher-owned children; no recovered PID is trusted.
pub(crate) struct NativeAgentRuntime {
    owner: Arc<NativeOwnerCommitService>,
    broker: OnceLock<WeakAppActionBroker>,
    workers: Arc<Mutex<HashMap<AgentKey, Worker>>>,
}

impl NativeAgentRuntime {
    pub(super) fn new(owner: Arc<NativeOwnerCommitService>) -> Self {
        Self {
            owner,
            broker: OnceLock::new(),
            workers: Arc::default(),
        }
    }

    pub(crate) fn attach(&self, broker: &AppActionBroker) -> Result<(), String> {
        self.broker
            .set(broker.downgrade())
            .map_err(|_| "native Agent broker already attached".into())
    }

    pub(super) fn start(&self, room: &str, agent: &str, caller: &DeviceCert) -> Result<(), String> {
        let broker = self
            .broker
            .get()
            .cloned()
            .ok_or("native Agent broker unavailable")?;
        let request = StartRequest {
            room: room.into(),
            agent: agent.into(),
            caller: caller.clone(),
        };
        let key = (request.room.clone(), request.agent.clone());
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| "native Agent workers unavailable")?;
        if workers
            .get(&key)
            .is_some_and(|worker| worker.join.is_finished())
            && let Some(finished) = workers.remove(&key)
        {
            finished.finish()?;
        }
        if workers.contains_key(&key) {
            return Err("Agent is already starting or running".into());
        }
        if workers.len() >= launcher::managed::MAX_LIVE_MANAGED_LAUNCHES {
            return Err("native Agent capacity exhausted".into());
        }
        let owner = self.owner.clone();
        let (stop, stopping) = mpsc::sync_channel(1);
        let cancellation = Arc::new(Mutex::new(None));
        let control = cancellation.clone();
        let join = std::thread::Builder::new()
            .name("tilo-native-agent".into())
            .spawn(move || run_session(&owner, &broker, &request, &stopping, &control))
            .map_err(|error| error.to_string())?;
        workers.insert(
            key,
            Worker {
                stop,
                cancellation,
                join,
            },
        );
        drop(workers);
        Ok(())
    }

    fn shutdown(&self) {
        let Ok(mut retained) = self.workers.lock() else {
            return;
        };
        let workers = std::mem::take(&mut *retained);
        drop(retained);
        for worker in workers.values() {
            let _ = worker.request_stop();
        }
        for worker in workers.into_values() {
            let _ = worker.finish();
        }
    }
}

impl Drop for NativeAgentRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn prepare(
    owner: &NativeOwnerCommitService,
    request: &StartRequest,
) -> Result<NativeManagedSession, String> {
    let native = owner
        .owner
        .as_ref()
        .ok_or("native Agent owner unavailable")?;
    let room =
        crate::shell::state::RoomId::parse(&request.room).ok_or("invalid native Agent room")?;
    if let Some(network) = &owner.network {
        network.check_native_custody(native, room)?;
    }
    let catalog = package_store::Catalog::from_env().map_err(|error| error.to_string())?;
    let install = NativeAgentInstall::from_host_catalog(
        &catalog,
        &launcher::run::RunOptions::default(),
        crate::shell::launcher::resolve_launcher_bin().map_err(|error| error.to_string())?,
        "/usr/bin/bwrap".into(),
    )?;
    let attestor =
        tilo_action_broker::services::WorkspaceTasks::host_certificate(owner, &request.room)?;
    NativeManagedSession::new(
        native.clone(),
        attestor,
        NativeAgentStart {
            room,
            caller: request.caller.clone(),
            agent: request.agent.clone(),
        },
        install,
    )
}

fn endpoints() -> Result<NativeAgentServiceEndpoints, String> {
    let config = agent_provider_config::ProviderConfig::load();
    let profile = config
        .active_profile()
        .ok_or("select an enabled Agent provider in Settings")?;
    let model = profile
        .default_model
        .as_ref()
        .ok_or("select an Agent model in Settings")?;
    if !profile
        .models
        .iter()
        .any(|candidate| candidate.id == *model)
    {
        return Err("selected Agent model is not in its provider profile".into());
    }
    Ok(NativeAgentServiceEndpoints {
        identity: launcher::services::identity_socket(),
        model_gateway: launcher::services::socket_for(launcher::services::MODEL_GATEWAY_SERVICE),
        data_home: xdg_paths::data_dir().ok_or("Agent data home unavailable")?,
        provider: launcher::services::ManagedProviderSelection {
            profile_id: profile.id.clone(),
            model_id: model.clone(),
            protocol: match profile.protocol {
                agent_provider_config::ProviderProtocol::Anthropic => "anthropic",
                agent_provider_config::ProviderProtocol::OpenAiCompatible => "open_ai_compatible",
            }
            .into(),
        },
    })
}

fn run_session(
    owner: &NativeOwnerCommitService,
    broker: &WeakAppActionBroker,
    request: &StartRequest,
    stopping: &mpsc::Receiver<()>,
    control: &Mutex<Option<NativeAgentCancellation>>,
) -> Result<(), String> {
    if stopping.try_recv().is_ok() {
        return Ok(());
    }
    let services = endpoints()?;
    let mut session = prepare(owner, request)?;
    let cancellation = session.cancellation_handle();
    match control.lock() {
        Ok(mut slot) => *slot = Some(cancellation),
        Err(_) => {
            return retire_after_error(
                &mut session,
                broker,
                request,
                "Agent cancellation state poisoned".into(),
            );
        }
    }
    if let Err(error) = session.start_with_host_services(services) {
        return retire_after_error(&mut session, broker, request, error);
    }
    tracing::info!(
        managed_context_present = true,
        "native Agent services started"
    );
    let Some(deadline) = Instant::now().checked_add(Duration::from_secs(30)) else {
        return retire_after_error(
            &mut session,
            broker,
            request,
            "native Agent startup deadline exhausted".into(),
        );
    };
    let mut ready = false;
    loop {
        if stopping.try_recv().is_ok() {
            return retire_session(&mut session, broker, request);
        }
        let facade = match broker.upgrade() {
            Ok(facade) => facade,
            Err(error) => return retire_after_error(&mut session, broker, request, error),
        };
        match session.poll_broker_attachment(&facade) {
            Ok(true) => {
                tracing::info!(
                    exact_adoption_verified = true,
                    "native Agent connection adopted"
                );
            }
            Ok(false) => {}
            Err(error) => {
                return retire_after_error(&mut session, broker, request, error);
            }
        }
        if !ready
            && facade.managed_agent_session_is_ready(
                &request.room,
                &request.agent,
                session.launch_id(),
            )
        {
            tracing::info!(
                enrolled_subscription_active = true,
                "native Agent enrolled task subscription active"
            );
            ready = true;
        }
        if !ready && Instant::now() >= deadline {
            return retire_after_error(
                &mut session,
                broker,
                request,
                "Agent did not negotiate enrolled task service".into(),
            );
        }
        drop(facade);
        let delay = if ready {
            Duration::from_secs(1)
        } else {
            Duration::from_millis(100)
        };
        match stopping.recv_timeout(delay) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return retire_session(&mut session, broker, request);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn retire_after_error(
    session: &mut NativeManagedSession,
    broker: &WeakAppActionBroker,
    request: &StartRequest,
    error: String,
) -> Result<(), String> {
    match retire_session(session, broker, request) {
        Ok(()) => Err(error),
        Err(retirement) => Err(format!("{error}; Agent retirement failed: {retirement}")),
    }
}

fn retire_session(
    session: &mut NativeManagedSession,
    broker: &WeakAppActionBroker,
    request: &StartRequest,
) -> Result<(), String> {
    let abandoned = abandon_active_tasks(broker, request, session.launch_id());
    let stopped = session.stop();
    match (abandoned, stopped) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(abandon), Err(stop)) => Err(format!(
            "task retirement failed: {abandon}; process retirement failed: {stop}"
        )),
    }
}

fn abandon_active_tasks(
    broker: &WeakAppActionBroker,
    request: &StartRequest,
    launch: &str,
) -> Result<(), String> {
    let facade = broker.upgrade()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(facade.abandon_managed_agent_tasks(&request.room, &request.agent, launch))
}
