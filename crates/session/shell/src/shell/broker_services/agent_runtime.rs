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
    join: JoinHandle<Result<WorkerOutcome, String>>,
}

enum WorkerSlot {
    Active(Worker),
    Fenced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerOutcome {
    Stopped,
    FailedBeforeSession,
    FailedAndRetired,
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

    fn finish(self) -> Result<WorkerOutcome, String> {
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
    workers: Arc<Mutex<HashMap<AgentKey, WorkerSlot>>>,
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
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| "native Agent workers unavailable")?;
        let owner = self.owner.clone();
        start_worker(&mut workers, request, move |request| {
            let (stop, stopping) = mpsc::sync_channel(1);
            let cancellation = Arc::new(Mutex::new(None));
            let control = cancellation.clone();
            let join = std::thread::Builder::new()
                .name("tilo-native-agent".into())
                .spawn(move || run_session(&owner, &broker, &request, &stopping, &control))
                .map_err(|error| error.to_string())?;
            Ok(Worker {
                stop,
                cancellation,
                join,
            })
        })
    }

    fn shutdown(&self) {
        let Ok(mut retained) = self.workers.lock() else {
            return;
        };
        let workers = std::mem::take(&mut *retained);
        drop(retained);
        for slot in workers.values() {
            if let WorkerSlot::Active(worker) = slot {
                let _ = worker.request_stop();
            }
        }
        for slot in workers.into_values() {
            if let WorkerSlot::Active(worker) = slot {
                let _ = worker.finish();
            }
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
) -> Result<WorkerOutcome, String> {
    if stopping.try_recv().is_ok() {
        return Ok(WorkerOutcome::Stopped);
    }
    let Ok(services) = endpoints() else {
        return Ok(WorkerOutcome::FailedBeforeSession);
    };
    // `prepare` includes durable session reservation. Without a returned
    // session there is no handle with which to prove cleanup, so this key
    // remains fenced rather than treating an error as pre-session failure.
    let mut session = prepare(owner, request)
        .map_err(|_| "native Agent session preparation outcome is uncertain".to_string())?;
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
            return retire_session(&mut session, broker, request).map(|()| WorkerOutcome::Stopped);
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
                return retire_session(&mut session, broker, request)
                    .map(|()| WorkerOutcome::Stopped);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn retire_after_error(
    session: &mut NativeManagedSession,
    broker: &WeakAppActionBroker,
    request: &StartRequest,
    _error: String,
) -> Result<WorkerOutcome, String> {
    match retire_session(session, broker, request) {
        Ok(()) => Ok(WorkerOutcome::FailedAndRetired),
        Err(_) => Err("native Agent retirement outcome is uncertain".into()),
    }
}

fn start_worker(
    workers: &mut HashMap<AgentKey, WorkerSlot>,
    request: StartRequest,
    spawn: impl FnOnce(StartRequest) -> Result<Worker, String>,
) -> Result<(), String> {
    let key = (request.room.clone(), request.agent.clone());
    match workers.get(&key) {
        Some(WorkerSlot::Fenced) => {
            return Err("native Agent retirement outcome is uncertain".into());
        }
        Some(WorkerSlot::Active(worker)) if !worker.join.is_finished() => {
            return Err("Agent is already starting or running".into());
        }
        Some(WorkerSlot::Active(_)) | None => {}
    }
    if let Some(WorkerSlot::Active(finished)) = workers.remove(&key) {
        match finished.finish() {
            Ok(WorkerOutcome::Stopped) => {}
            Ok(WorkerOutcome::FailedBeforeSession) => {
                tracing::warn!("native Agent worker ended before session reservation");
            }
            Ok(WorkerOutcome::FailedAndRetired) => {
                tracing::warn!("native Agent worker failure was safely retired");
            }
            Err(_) => {
                workers.insert(key, WorkerSlot::Fenced);
                return Err("native Agent retirement outcome is uncertain".into());
            }
        }
    }
    if workers.len() >= launcher::managed::MAX_LIVE_MANAGED_LAUNCHES {
        return Err("native Agent capacity exhausted".into());
    }
    let worker = spawn(request)?;
    workers.insert(key, WorkerSlot::Active(worker));
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn caller(seed: u8) -> DeviceCert {
        let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[seed.saturating_add(1); 32]);
        tilo_joined_room::testing::certificate(&root, &device, "runtime registry fixture")
    }

    fn request(caller: DeviceCert) -> StartRequest {
        StartRequest {
            room: "room-a".into(),
            agent: "scribe".into(),
            caller,
        }
    }

    fn worker_with_join(join: JoinHandle<Result<WorkerOutcome, String>>) -> Worker {
        let (stop, _stopping) = mpsc::sync_channel(1);
        Worker {
            stop,
            cancellation: Arc::new(Mutex::new(None)),
            join,
        }
    }

    fn finished_worker(outcome: Result<WorkerOutcome, String>) -> Worker {
        let (finished, observed) = mpsc::sync_channel(1);
        let join = std::thread::spawn(move || {
            let result = outcome;
            let _ = finished.send(());
            result
        });
        observed.recv().unwrap();
        while !join.is_finished() {
            std::thread::yield_now();
        }
        worker_with_join(join)
    }

    fn remove_active(workers: &mut HashMap<AgentKey, WorkerSlot>, key: &AgentKey) -> Worker {
        let Some(WorkerSlot::Active(worker)) = workers.remove(key) else {
            panic!("active worker expected")
        };
        worker
    }

    #[test]
    fn safely_retired_failure_starts_one_fresh_worker_with_the_new_caller() {
        let old = caller(11);
        let fresh = caller(21);
        let key = ("room-a".to_string(), "scribe".to_string());
        let mut workers = HashMap::from([(
            key.clone(),
            WorkerSlot::Active(finished_worker(Ok(WorkerOutcome::FailedAndRetired))),
        )]);
        let spawns = AtomicUsize::new(0);

        start_worker(&mut workers, request(fresh.clone()), |request| {
            assert_eq!(request.caller, fresh);
            spawns.fetch_add(1, Ordering::Relaxed);
            Ok(finished_worker(Ok(WorkerOutcome::Stopped)))
        })
        .unwrap();

        assert_eq!(spawns.load(Ordering::Relaxed), 1);
        assert_eq!(workers.len(), 1);
        remove_active(&mut workers, &key).finish().unwrap();
        assert_ne!(old, fresh);
    }

    #[test]
    fn uncertain_cleanup_and_panic_fence_every_later_start() {
        for panicked in [false, true] {
            let key = ("room-a".to_string(), "scribe".to_string());
            let old = if panicked {
                let join = std::thread::spawn(|| -> Result<WorkerOutcome, String> {
                    panic!("fixture worker panic")
                });
                while !join.is_finished() {
                    std::thread::yield_now();
                }
                worker_with_join(join)
            } else {
                finished_worker(Err("fixture retirement failure".into()))
            };
            let mut workers = HashMap::from([(key.clone(), WorkerSlot::Active(old))]);
            let spawns = AtomicUsize::new(0);
            for seed in [31, 41] {
                assert!(
                    start_worker(&mut workers, request(caller(seed)), |_| {
                        spawns.fetch_add(1, Ordering::Relaxed);
                        Ok(finished_worker(Ok(WorkerOutcome::Stopped)))
                    })
                    .is_err()
                );
                assert!(matches!(workers.get(&key), Some(WorkerSlot::Fenced)));
            }
            assert_eq!(spawns.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn unfinished_worker_refuses_a_parallel_start_without_losing_ownership() {
        let key = ("room-a".to_string(), "scribe".to_string());
        let (release, held) = mpsc::sync_channel(1);
        let old = worker_with_join(std::thread::spawn(move || {
            held.recv().unwrap();
            Ok(WorkerOutcome::Stopped)
        }));
        let mut workers = HashMap::from([(key.clone(), WorkerSlot::Active(old))]);
        let spawns = AtomicUsize::new(0);

        assert!(
            start_worker(&mut workers, request(caller(51)), |_| {
                spawns.fetch_add(1, Ordering::Relaxed);
                Ok(finished_worker(Ok(WorkerOutcome::Stopped)))
            })
            .is_err()
        );
        assert_eq!(spawns.load(Ordering::Relaxed), 0);
        assert!(matches!(workers.get(&key), Some(WorkerSlot::Active(_))));
        release.send(()).unwrap();
        remove_active(&mut workers, &key).finish().unwrap();
    }
}
