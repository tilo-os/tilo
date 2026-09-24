//! One live runtime owner. Durable authority remains in Store; this thread retains
//! only nonserializable launch/dispatch custody and the exact service descriptors.
use crate::{
    Store,
    agent_runtime_adapter::{RuntimeAdmission, RuntimeOwner},
    agent_runtime_config::ManagedConfig,
    agent_runtime_owner::{AgentControlId, AgentRuntimeObservation, AgentRuntimePhase},
    agent_workers::AgentWorkers,
};
use qos_action_broker::agent_control::{AgentControlProvider, AgentProviderState};
use qos_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use qos_agent_control_protocol::{AgentOperation, AgentOperationState, GrantOffer};
use qos_peer_state::{
    NeighbourMsg,
    owner_commit::{PrivateAgentState, VerifiedAgentCaller},
};
use qos_relay::{client::CertifiedAction, peer_actions::ActionMessage};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
#[cfg(target_os = "linux")]
#[path = "agent_runtime_linux.rs"]
mod execution;

pub(crate) mod read_attachment;
mod work_queue;
pub use read_attachment::CloudAgentCommands;
use work_queue::AgentWorkQueue;
#[cfg(target_os = "linux")]
use work_queue::RECOVERY_BATCH;

struct Command {
    policy_time: qos_action_broker::peer_actions::PolicyTime,
    evidence: CertifiedAction,
    kind: read_attachment::CommandKind,
    deadline: tokio::time::Instant,
    custody: Option<qos_action_broker::agent_reads::AgentReadCustody>,
    reply: oneshot::Sender<
        qos_action_broker::agent_reads::AgentReadReply<
            Result<read_attachment::CommandReply, read_attachment::CommandError>,
        >,
    >,
}
pub(crate) struct CloudAgentRuntime {
    owner: Arc<RuntimeOwner>,
    commands: mpsc::Sender<Command>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
    pub(crate) outbound: mpsc::Receiver<NeighbourMsg>,
    reads_supported: bool,
    closed: Arc<std::sync::atomic::AtomicBool>,
}
impl CloudAgentRuntime {
    pub(crate) async fn start(
        root: &Path,
        machine: &str,
        reads_supported: bool,
    ) -> Result<Self, String> {
        let owner = Arc::new(RuntimeOwner::open(root, machine)?);
        Self::start_owner(root, owner, reads_supported).await
    }
    pub(crate) async fn start_with_policy_time(
        root: &Path,
        machine: &str,
        reads_supported: bool,
        clock: qos_action_broker::peer_actions::OwnerPolicyClock,
        time: qos_action_broker::peer_actions::PolicyTime,
        source: crate::https_owner::CloudStoreSource,
    ) -> Result<Self, String> {
        let owner = Arc::new(RuntimeOwner::open_with_source(
            root, machine, clock, time, source,
        )?);
        Self::start_owner(root, owner, reads_supported).await
    }
    async fn start_owner(
        root: &Path,
        owner: Arc<RuntimeOwner>,
        reads_supported: bool,
    ) -> Result<Self, String> {
        owner.policy_context().mark_started();
        // This is the one runtime-start boundary. Ordinary Store opens never imply restart.
        owner.retain_restart_uncertainty()?;
        let (commands, rx) = mpsc::channel(32);
        let (outbound, outbound_rx) = mpsc::channel(32);
        let (stop, stop_rx) = oneshot::channel();
        let (ready, ready_rx) = oneshot::channel();
        let owned_root = root.to_path_buf();
        let thread_owner = owner.clone();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_closed = closed.clone();
        let thread = std::thread::Builder::new()
            .name("cloud-agent-owner".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                let _closed =
                    read_attachment::CloseOnDrop(thread_closed, thread_owner.policy_context());
                tokio::task::LocalSet::new().block_on(
                    &runtime,
                    run(
                        owned_root,
                        thread_owner,
                        rx,
                        outbound,
                        stop_rx,
                        ready,
                        reads_supported,
                    ),
                )
            })
            .map_err(|e| e.to_string())?;
        match ready_rx
            .await
            .map_err(|_| "agent owner stopped during startup")?
        {
            Ok(()) => Ok(Self {
                owner,
                commands,
                stop: Some(stop),
                thread: Some(thread),
                outbound: outbound_rx,
                reads_supported,
                closed,
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }
    pub(crate) async fn close(&mut self) -> Result<(), String> {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.owner.stop_readiness();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || {
            thread
                .join()
                .map_err(|_| "agent owner panicked".to_string())?
        })
        .await
        .map_err(|e| e.to_string())?
    }
    pub(crate) fn certified_commands(&self) -> CloudAgentCommands {
        CloudAgentCommands::from_runtime(self)
    }
    pub(crate) async fn handle_certified(
        &self,
        evidence: &CertifiedAction,
    ) -> Result<Option<ActionMessage>, String> {
        self.certified_commands()
            .handle_certified(
                evidence.clone(),
                tokio::time::Instant::now() + Duration::from_secs(5),
            )
            .await
    }
}
impl Drop for CloudAgentRuntime {
    fn drop(&mut self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.owner.stop_readiness();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
async fn handle_certified_command(
    root: &Path,
    owner: &RuntimeOwner,
    provider: Option<&Arc<AgentControlProvider>>,
    offer: Option<&GrantOffer>,
    reads_supported: bool,
    evidence: &CertifiedAction,
    queued: &mut AgentWorkQueue,
    policy_time: &qos_action_broker::peer_actions::PolicyTime,
) -> Result<Option<ActionMessage>, String> {
    let action = evidence.action();
    if !matches!(
        &action.message,
        ActionMessage::Status { .. }
            | ActionMessage::Cancel { .. }
            | ActionMessage::CatalogueRefresh { .. }
            | ActionMessage::AgentReadV1 { .. }
    ) && !matches!(&action.message,ActionMessage::InvokeBound { app_id,.. } if app_id==qos_action_broker::agent_control::APP_ID)
    {
        return Ok(None);
    }
    if matches!(&action.message, ActionMessage::AgentReadV1 { .. }) && !reads_supported {
        return Ok(None);
    }
    let store = owner.store()?;
    if let ActionMessage::Status { invocation } | ActionMessage::Cancel { invocation } =
        &action.message
    {
        if !store.private_agent_invocation_reserved(action.source, invocation.0)? {
            return Ok(None);
        }
    }
    let caller = match authenticate(&store, evidence) {
        Ok(caller) => caller,
        Err(_) if matches!(&action.message, ActionMessage::CatalogueRefresh { .. }) => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if let ActionMessage::Status { invocation } | ActionMessage::Cancel { invocation } =
        &action.message
    {
        return retained_status(&store, &caller, action.room, *invocation).map(Some);
    }
    drop(store);
    handle_command(
        root,
        owner,
        provider,
        offer,
        reads_supported,
        evidence,
        queued,
        policy_time,
    )
    .await
    .map(Some)
}
/// Fresh retained status for the invocation the certified evidence itself
/// names (its Invoke or Status). Read-only; the caller is re-authenticated
/// against the current Store. `None` for invocations this owner never reserved.
fn observe_certified(
    owner: &RuntimeOwner,
    evidence: &CertifiedAction,
    invocation: InvocationId,
) -> Result<Option<ActionMessage>, String> {
    let action = evidence.action();
    if action.message.invocation() != Some(invocation) {
        return Err("observed invocation differs from its evidence".into());
    }
    let store = owner.store()?;
    if !store.private_agent_invocation_reserved(action.source, invocation.0)? {
        return Ok(None);
    }
    let caller = authenticate(&store, evidence)?;
    retained_status(&store, &caller, action.room, invocation).map(Some)
}
fn authenticate(store: &Store, evidence: &CertifiedAction) -> Result<VerifiedAgentCaller, String> {
    let action = evidence.action();
    let peer = store.peer().map_err(|e| e.to_string())?;
    if action.room.to_string() != peer.room()
        || action.source != evidence.certificate().device_public_key
        || action.target != peer.authority().device_public_key
        || !peer.certificates().contains(evidence.certificate())
    {
        return Err("certified agent caller/route mismatch".into());
    }
    let caller = store.agent_caller(action.source)?;
    if caller.certificate() != evidence.certificate() {
        return Err("certified agent caller changed".into());
    }
    Ok(caller)
}
fn retained_status(
    store: &Store,
    caller: &VerifiedAgentCaller,
    host: Uuid,
    invocation: InvocationId,
) -> Result<ActionMessage, String> {
    let id = AgentControlId::new(invocation.0).map_err(|e| e.to_string())?;
    if let Some(PrivateAgentState::V1(ledger)) = &store.state.private_agent_ledger {
        let current = store.refresh_agent_caller(caller)?;
        let record = ledger
            .record_for(&current, &invocation.0.to_string())?
            .ok_or("retained private operation missing")?;
        use qos_peer_state::owner_commit::AgentControlPhase;
        let update = match record.phase {
            AgentControlPhase::Failed => Some(InvocationUpdate::Failed {
                invocation,
                error: ActionFailure {
                    code: "agent_control_failed".into(),
                    message: record
                        .error
                        .as_ref()
                        .map(|e| e.message.clone())
                        .unwrap_or_else(|| "Agent control failed".into()),
                    details: None,
                    retryable: false,
                },
            }),
            AgentControlPhase::Completed => Some(InvocationUpdate::Accepted { invocation }),
            _ => None,
        };
        return Ok(ActionMessage::Observation {
            host,
            invocation,
            uncertain: update.is_none(),
            update,
            cancellation_requested: false,
        });
    }
    store
        .observe_agent_runtime(caller, &id)
        .map(|value| observation(host, value))
        .map_err(|e| e.to_string())
}
fn observation(host: Uuid, value: AgentRuntimeObservation) -> ActionMessage {
    let invocation = InvocationId(value.invocation.parse().expect("validated invocation"));
    let uncertain = value.phase == AgentRuntimePhase::Unknown;
    let update = match value.phase {
        AgentRuntimePhase::Completed if value.qualified_completed => {
            Some(InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(
                        serde_json::to_value(AgentOperation {
                            invocation_id: value.invocation,
                            state: AgentOperationState::Accepted,
                            agent_id: Some(value.agent_id),
                            task_id: Some(value.task_id),
                            error: None,
                        })
                        .expect("operation serialization"),
                    ),
                    resources: vec![],
                },
            })
        }
        AgentRuntimePhase::Failed => Some(InvocationUpdate::Failed {
            invocation,
            error: ActionFailure {
                code: "agent_control_failed".into(),
                message: value
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "Agent control failed".into()),
                details: None,
                retryable: false,
            },
        }),
        AgentRuntimePhase::Unknown => None,
        _ => Some(InvocationUpdate::Accepted { invocation }),
    };
    ActionMessage::Observation {
        host,
        invocation,
        update,
        uncertain,
        cancellation_requested: false,
    }
}
fn retained(
    store: &Store,
    caller: &VerifiedAgentCaller,
    evidence: &CertifiedAction,
) -> Result<Option<AgentRuntimeObservation>, String> {
    let ActionMessage::InvokeBound {
        host,
        invocation,
        app_peer,
        app_id,
        record_id,
        resource,
        action,
        argument,
        confirmation,
        revision,
    } = &evidence.action().message
    else {
        return Ok(None);
    };
    if !store.private_agent_invocation_reserved(evidence.action().source, invocation.0)? {
        return Ok(None);
    }
    let id = AgentControlId::new(invocation.0).map_err(|e| e.to_string())?;
    let original = store
        .retained_agent_intent(caller, &id)
        .map_err(|e| e.to_string())?
        .ok_or("retained invocation missing")?;
    let request = qos_agent_control_protocol::decode_request(
        &serde_json::to_vec(argument.as_ref().ok_or("agent argument missing")?)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if *host != evidence.action().room
        || confirmation.is_some()
        || original.route.app_peer != *app_peer
        || original.route.app_id != *app_id
        || original.route.record_id != *record_id
        || original.route.resource != *resource
        || original.route.action != *action
        || revision.as_deref() != Some(original.descriptor_revision.as_str())
        || qos_agent_control_protocol::canonical_request(&request).map_err(|e| e.to_string())?
            != original.request.as_bytes()
    {
        return Err("retained agent invocation intent conflict".into());
    }
    store
        .observe_agent_runtime(caller, &id)
        .map(Some)
        .map_err(|e| e.to_string())
}
/// Longest interval between provider state republications without a change.
const RUNTIME_STATE_INTERVAL: Duration = Duration::from_millis(250);
async fn run(
    root: PathBuf,
    owner: Arc<RuntimeOwner>,
    mut commands: mpsc::Receiver<Command>,
    outbound: mpsc::Sender<NeighbourMsg>,
    mut stop: oneshot::Receiver<()>,
    ready: oneshot::Sender<Result<(), String>>,
    reads_supported: bool,
) -> Result<(), String> {
    // Writer selection is explicit. An older Store still gets status/read service;
    // mounting mutating workers must not silently migrate it or stop ordinary hosting.
    let selected = owner
        .store()?
        .peer()
        .map_err(|e| e.to_string())?
        .admission_v5_selected();
    let _outbound_guard = outbound.clone();
    let (mut workers, mut documents) = if selected {
        match AgentWorkers::start(&root, owner.clone(), outbound) {
            Ok((mut workers, documents)) => {
                workers.set_transport_ready(true);
                (Some(workers), Some(documents))
            }
            Err(error) => {
                let _ = ready.send(Err(error.clone()));
                return Err(error);
            }
        }
    } else {
        (None, None)
    };
    let configured = ManagedConfig::load(&root);
    let mut configuration = None;
    let mut launcher = None;
    let mut provider = Some(AgentControlProvider::new_read_only(owner.clone()));
    match configured {
        Ok(Some(config)) => match config.launcher(owner.clone()) {
            Ok(value) => {
                if let Err(error) = owner.install_runtime(value.runtime_lease()) {
                    let _ = ready.send(Err(error.clone()));
                    let closed = workers.as_mut().map_or(Ok(()), |w| w.close());
                    return Err(closed.err().unwrap_or(error));
                }
                provider = Some(AgentControlProvider::new(
                    value.runtime_lease(),
                    owner.clone(),
                ));
                launcher = Some(value);
                configuration = Some(config);
            }
            Err(error) => {
                #[cfg(test)]
                test_readiness_diagnostic("launcher", &error);
                tracing::warn!(%error,"managed runtime prerequisites unavailable");
            }
        },
        Err(error) => {
            #[cfg(test)]
            test_readiness_diagnostic("configuration", &error);
            tracing::warn!(%error,"managed runtime configuration rejected");
        }
        Ok(None) => {}
    }
    #[cfg(target_os = "linux")]
    let mut execution = match workers.as_ref().map(|w| w.app_broker()).transpose() {
        Ok(broker) => broker.map(|broker| execution::Execution::new(owner.clone(), broker)),
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            let closed = workers.as_mut().map_or(Ok(()), |w| w.close());
            return Err(closed.err().unwrap_or(error));
        }
    };
    let mut queued = AgentWorkQueue::default();
    #[cfg(target_os = "linux")]
    let mut last_recovery = Instant::now() - Duration::from_secs(2);
    let mut offer: Option<GrantOffer> = None;
    let mut last_configuration = Instant::now() - Duration::from_secs(10);
    let mut plan_configured = false;
    let (probe_tx, mut probe_rx) = mpsc::channel(1);
    let mut probe: Option<tokio::task::JoinHandle<()>> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    // Provider state is republished after document commits, plan changes and
    // at most every RUNTIME_STATE_INTERVAL otherwise. Each publication opens
    // the Store; doing so on every 10 ms tick starved other Store users.
    let mut published_at = Instant::now();
    let mut publish_due = false;
    if let Err(error) = publish_runtime_state(
        &owner,
        workers.as_ref(),
        &mut offer,
        false,
        launcher.is_some(),
    ) {
        let _ = ready.send(Err(error.clone()));
        if let Some(launcher) = &mut launcher {
            let _ = launcher.shutdown();
        }
        let closed = workers.as_mut().map_or(Ok(()), |w| w.close());
        return Err(closed.err().unwrap_or(error));
    }
    let _ = ready.send(Ok(()));
    let outcome:Result<(),String>=async {
        loop {tokio::select!{
            _=&mut stop=>break,
            command=commands.recv()=>match command {
                None=>break,
                Some(command)=>{
                    read_attachment::dispatch(command,&root,&owner,provider.as_ref(),offer.as_ref(),reads_supported,&mut queued).await;
                }
            },
            _=tick.tick()=>{
                if let Some(workers)=&mut workers {workers.pump()?;}
                // apply opens Store only in its callback; refresh and release the deferred signer
                // after the exact committed bytes have entered the worker cache.
                while let Some(pending)=documents.as_mut().and_then(|requests|requests.try_recv().ok()){
                    let(complete,result)=crate::agent_documents::apply_worker_document(&owner,pending,Instant::now()+Duration::from_secs(5));
                    publish_due = true;
                    match result {
                        Ok(())=>{workers.as_mut().ok_or("document owner unavailable")?.committed()?;complete();}
                        Err(error)=>{
                            // The handoff owns its refusal reply. Only a latched
                            // (uncertain) signer retires the runtime loop.
                            complete();
                            workers.as_ref().ok_or("document owner unavailable")?.document_health()?;
                            tracing::warn!(%error,"worker document refused without owner change");
                        }
                    }
                }
                if let Ok(configured)=probe_rx.try_recv(){publish_due |= configured != plan_configured; plan_configured=configured;}
                if probe.as_ref().is_some_and(|t|t.is_finished()){let _=probe.take().expect("present probe").await;}
                if last_configuration.elapsed()>=Duration::from_secs(5)&&probe.is_none(){
                    last_configuration=Instant::now();
                    publish_due |= plan_configured;
                    plan_configured=false; // No stale positive capability while the next bounded query is unresolved.
                    if let Some(config)=configuration.clone(){
                        let sender=probe_tx.clone();
                        probe=Some(tokio::task::spawn_local(async move{
                            let result = config.configured_for_plan().await;
                            #[cfg(test)]
                            if let Err(error) = &result { test_readiness_diagnostic("gateway-plan", error); }
                            let _=sender.try_send(result.is_ok());
                        }));
                    }
                }
                if publish_due || published_at.elapsed() >= RUNTIME_STATE_INTERVAL {
                    publish_runtime_state(&owner,workers.as_ref(),&mut offer,plan_configured,launcher.is_some())?;
                    published_at = Instant::now();
                    publish_due = false;
                }
                #[cfg(target_os="linux")]
                if let (Some(launcher),Some(config),Some(execution))=(&mut launcher,&configuration,&mut execution){
                    execution.pump(launcher,config).await?;
                    queued.synchronize(execution.running_ids());
                    if last_recovery.elapsed() >= Duration::from_secs(1) {
                        last_recovery = Instant::now();
                        // Readiness/authority/capacity can return later. Store remains the obligation.
                        if let Err(error) = queued.rescan(&owner) { tracing::debug!(%error,"Spawn recovery deferred"); }
                    }
                    for _ in 0..RECOVERY_BATCH {
                        let Some(id)=queued.pop_front() else { break; };
                        match execution.begin(id,config) {
                            Ok(()) => queued.started(id),
                            Err(error) => { owner.uncertain(id,&error)?; }
                        }
                    }
                }
                #[cfg(not(target_os="linux"))]
                while let Some(id)=queued.pop_front(){owner.uncertain(id,"managed execution requires Linux")?;}
            }
        }}
        Ok(())
    }.await;
    #[cfg(test)]
    if let Err(error) = &outcome {
        test_readiness_diagnostic("runtime-loop", error);
    }
    owner.stop_readiness();
    if let Some(probe) = probe.take() {
        probe.abort();
        let _ = probe.await;
    }
    let mut queued_cleanup = Ok(());
    while let Some(id) = queued.pop_front() {
        if let Err(error) = owner.uncertain(id, "runtime stopped before launch or dispatch") {
            queued_cleanup = Err(error);
        }
    }
    if let Some(workers) = &mut workers {
        workers.set_transport_ready(false);
    }
    #[cfg(target_os = "linux")]
    let execution_close = match execution.as_mut() {
        Some(execution) => execution.close(launcher.as_mut()).await,
        None => Ok(()),
    };
    #[cfg(not(target_os = "linux"))]
    let execution_close = launcher.as_mut().map_or(Ok(()), |l| l.shutdown());
    let worker_close = workers.as_mut().map_or(Ok(()), |w| w.close());
    outcome
        .and(queued_cleanup)
        .and(execution_close)
        .and(worker_close)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn agent_runtime_older_store_starts_read_owner_without_migration_or_workers() {
        let (dir, store, _) = crate::agent_owner_tests::enrolled();
        let before = std::fs::read(dir.path().join("state.json")).unwrap();
        drop(store);
        let mut runtime = CloudAgentRuntime::start(dir.path(), "agent-admission", true)
            .await
            .unwrap();
        runtime.close().await.unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            before
        );
        assert!(!dir.path().join("actions/journal").exists());
    }
    #[test]
    fn agent_runtime_legacy_status_requires_no_writer_upgrade() {
        let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
        store
            .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
            .unwrap();
        let invocation = InvocationId(Uuid::new_v4());
        let (caller, request) =
            crate::agent_owner_tests::prepare(&store, device, &invocation.0.to_string());
        store.commit_agent_request(&request, &caller).unwrap();
        let caller = store.agent_caller(device).unwrap();
        let before = std::fs::read(dir.path().join("state.json")).unwrap();
        let response = retained_status(
            &store,
            &caller,
            store.state.room.parse().unwrap(),
            invocation,
        )
        .unwrap();
        assert!(matches!(
            response,
            ActionMessage::Observation {
                uncertain: true,
                update: None,
                ..
            }
        ));
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            before
        );
    }
}

async fn handle_command(
    root: &Path,
    owner: &RuntimeOwner,
    provider: Option<&Arc<AgentControlProvider>>,
    offer: Option<&GrantOffer>,
    reads_supported: bool,
    evidence: &CertifiedAction,
    queued: &mut AgentWorkQueue,
    policy_time: &qos_action_broker::peer_actions::PolicyTime,
) -> Result<ActionMessage, String> {
    let store = owner.store()?;
    let caller = authenticate(&store, &evidence)?;
    let action = evidence.action();
    match &action.message {
        ActionMessage::CatalogueRefresh { scope } => {
            let machine = store.state.machine_id.clone();
            drop(store);
            let mut value = crate::broker::handle_with_policy_time(
                root,
                &machine,
                evidence.certificate(),
                action,
                policy_time,
                owner.policy_context(),
            )
            .await
            .map_err(|e| e.to_string())?;
            if let (Some(provider), ActionMessage::BrokerCatalogue { catalogue, .. }) =
                (provider, &mut value)
            {
                if let Ok(mut surface) = provider.catalogue_surface() {
                    let matches = scope.as_ref().is_none_or(|s| {
                        s.app_id == qos_action_broker::agent_control::APP_ID
                            && s.record_id.is_empty()
                            && surface["actions"].as_array().is_some_and(|a| {
                                a.iter()
                                    .any(|d| d["resource"] == s.resource && d["action"] == s.action)
                            })
                    });
                    if matches {
                        if let Some(scope) = scope {
                            surface["actions"]
                                .as_array_mut()
                                .ok_or("invalid provider actions")?
                                .retain(|d| {
                                    d["resource"] == scope.resource && d["action"] == scope.action
                                });
                        }
                        catalogue["surfaces"]
                            .as_array_mut()
                            .ok_or("invalid native catalogue")?
                            .push(surface);
                    }
                }
                if reads_supported {
                    if let Ok(binding) = provider.read_binding(evidence.certificate()) {
                        catalogue["agentReadsV1"] =
                            serde_json::to_value(binding).map_err(|e| e.to_string())?;
                    }
                }
            }
            Ok(value)
        }
        ActionMessage::AgentReadV1 { .. } => {
            drop(store);
            read_attachment::read_current(owner, provider, offer, evidence)
                .map_err(|error| error.to_string())
        }
        ActionMessage::InvokeBound { .. } => {
            if let Some(observed) = retained(&store, &caller, &evidence)? {
                let id = observed
                    .invocation
                    .parse::<Uuid>()
                    .map_err(|e| e.to_string())?;
                let control = AgentControlId::new(id).map_err(|e| e.to_string())?;
                if store
                    .sole_admitted_spawn(&control)
                    .map_err(|e| e.to_string())?
                    .is_some()
                {
                    // Exact retained intent was checked before scheduling; the same closed
                    // installed-owner recovery door still runs immediately before begin.
                    let _ = queued.enqueue(id);
                }
                return Ok(observation(action.room, observed));
            }
            drop(store);
            queued.reserve_new_slot()?;
            let verified = provider
                .ok_or("native managed provider unavailable")?
                .verify_invocation(&evidence)?;
            let observed = match owner.admit(&caller, &verified, policy_time)? {
                RuntimeAdmission::Existing(value) => value,
                RuntimeAdmission::Fresh(value) => {
                    // Queue saturation cannot erase a committed Spawn; bounded scans retry it.
                    queued.enqueue(verified.invocation())?;
                    value
                }
            };
            Ok(observation(action.room, observed))
        }
        _ => Err("unsupported private runtime command".into()),
    }
}

pub(crate) fn publish_runtime_state(
    owner: &RuntimeOwner,
    workers: Option<&AgentWorkers>,
    offer: &mut Option<GrantOffer>,
    plan_configured: bool,
    launcher_available: bool,
) -> Result<(), String> {
    let publications = if let Some(workers) = workers {
        workers.publications()?
    } else {
        let store = owner.store()?;
        let peer = store.peer().map_err(|e| e.to_string())?;
        crate::agent_workers::WorkerPublications {
            room: store.state.room.clone(),
            host_device: peer.authority().device_public_key,
            governance_revision: qos_peer_state::owner_commit::governance_revision(&peer)?,
            publication_revision: "unavailable".into(),
            grants: vec![],
            ready: false,
            native_v5_ready: false,
        }
    };
    let store = owner.store()?;
    let machine = store.state.machine_id.clone();
    let private_v2_ready = matches!(
        store.state.private_agent_ledger,
        Some(PrivateAgentState::V2(_))
    );
    drop(store);
    let revision = blake3::hash(
        &serde_json::to_vec(&(
            &publications.governance_revision,
            &publications.publication_revision,
        ))
        .map_err(|e| e.to_string())?,
    )
    .to_hex()
    .to_string();
    if offer.as_ref().is_none_or(|o| {
        o.revision != revision
            || chrono::DateTime::parse_from_rfc3339(&o.expires_at)
                .map_or(true, |t| t.timestamp() <= chrono::Utc::now().timestamp())
    }) {
        *offer = (!publications.grants.is_empty() && publications.grants.len() <= 64).then(|| {
            GrantOffer {
                id: Uuid::new_v4().to_string(),
                revision: revision.clone(),
                expires_at: (chrono::Utc::now() + chrono::Duration::minutes(15))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                label: "Current workspace documents".into(),
                actions: publications.grants.clone(),
            }
        });
    }
    let room = publications
        .room
        .parse::<Uuid>()
        .map_err(|e| e.to_string())?;
    #[cfg(test)]
    {
        type Readiness = (bool, bool, bool, bool, bool, usize);
        std::thread_local! {
            static LAST: std::cell::Cell<Option<Readiness>> = const { std::cell::Cell::new(None) };
        }
        let current = (
            publications.ready,
            publications.native_v5_ready,
            private_v2_ready,
            plan_configured,
            launcher_available,
            publications.grants.len(),
        );
        if LAST.with(|last| last.replace(Some(current)) != Some(current)) {
            test_readiness_diagnostic(
                "publication",
                &format!(
                    "workers={} native_v5={} private_v2={} plan={} launcher={} grants={}",
                    current.0, current.1, current.2, current.3, current.4, current.5
                ),
            );
        }
    }
    owner.publish_provider(
        AgentProviderState {
            machine,
            room,
            host: room,
            host_device: publications.host_device,
            app_peer: qos_relay::room_workspace_id(&publications.room)
                .map_err(|e| e.to_string())?,
            governance_revision: publications.governance_revision,
            publication_revision: publications.publication_revision,
            grant_offers: offer.clone().into_iter().collect(),
            private_v2_ready,
            required_services_ready: cfg!(target_os = "linux")
                && publications.ready
                && publications.native_v5_ready
                && plan_configured
                && launcher_available,
        },
        publications.grants,
    )?;

    Ok(())
}

// Observation only: bound diagnostic output even if readiness oscillates.
#[cfg(test)]
fn test_readiness_diagnostic(phase: &str, reason: &str) {
    std::thread_local! {
        static REMAINING: std::cell::Cell<u8> = const { std::cell::Cell::new(32) };
    }
    if REMAINING.with(|remaining| {
        let current = remaining.get();
        remaining.set(current.saturating_sub(1));
        current != 0
    }) {
        eprintln!(
            "agent readiness {phase}: {}",
            reason.chars().take(512).collect::<String>()
        );
    }
}
