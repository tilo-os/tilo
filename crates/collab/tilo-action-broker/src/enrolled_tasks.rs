//! Private enrolled transport. Live kernel custody is never reconstructed from JSON.
use super::{
    AdmittedJoinIdentity, Arc, AsyncMutex, CapRole, CapnpBrokerState, CapnpChannelHandle,
    CrdtRoute, HashMap, LocalHostWorkerScope, Ordering, PendingWorker, Promise, Rc, WorkerBinding,
    mpsc,
};
use launcher::managed::{AdmittedManagedConnection, ManagedLaunchScope, VerifiedManagedProcess};
use runtime_collab::{EnrolledTaskAcceptance, EnrolledTaskEnvelope, TaskRunUpdate};
use tilo_peer_state::{
    owner_commit::{CommitOperation, OwnerCommitRequest},
    workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand, WorkspaceTaskFinish},
};

const MAX_CONNECTIONS: usize = 32;
const MAX_RUNS: usize = 256;
const MAX_UPDATE_BYTES: usize = 8192;

#[derive(Clone)]
struct Connection {
    process: VerifiedManagedProcess,
    certificate: tilo_user_identity::DeviceCert,
    route: CrdtRoute,
    peer: Option<u64>,
    subscribed: bool,
    sender: Option<mpsc::Sender<String>>,
}
#[derive(Clone)]
struct Run {
    scope: uuid::Uuid,
    envelope: EnrolledTaskEnvelope,
    acceptance: Option<EnrolledTaskAcceptance>,
    epoch: Option<u64>,
    start_epoch: Option<u64>,
    sequence: u64,
    last_update: Option<TaskRunUpdate>,
    terminal: bool,
    fenced: bool,
}
#[derive(Default)]
pub(super) struct Registry {
    connections: HashMap<uuid::Uuid, Connection>,
    runs: HashMap<String, Run>,
    retired: Vec<RetiredRun>,
}
struct RetiredRun {
    room: String,
    task: String,
    agent_id: String,
    epoch: u64,
    attestor: tilo_user_identity::DeviceCert,
}
impl Registry {
    pub(super) fn close(&mut self) {
        self.connections.clear();
        self.runs.clear();
        self.retired.clear();
    }
    pub(super) fn disconnect(&mut self, peer: u64) {
        let scopes = self
            .connections
            .iter()
            .filter_map(|(id, c)| (c.peer == Some(peer)).then_some(*id))
            .collect::<Vec<_>>();
        for scope in scopes {
            self.retire_scope(scope);
        }
    }
    fn retire_scope(&mut self, scope: uuid::Uuid) {
        let Some(connection) = self.connections.remove(&scope) else {
            return;
        };
        for run in self.runs.values_mut().filter(|r| r.scope == scope) {
            run.fenced = true;
            if !run.terminal
                && let Some(epoch) = run.start_epoch
            {
                self.retired.push(RetiredRun {
                    room: connection.route.cluster_id.clone(),
                    task: run.envelope.task_id.clone(),
                    agent_id: run.envelope.recipient_agent.clone(),
                    epoch,
                    attestor: connection.certificate.clone(),
                });
            }
        }
    }
}

pub(super) fn disconnect(state: &CapnpBrokerState, peer: u64) {
    if let Ok(mut registry) = state.enrolled_tasks.lock() {
        registry.disconnect(peer);
    }
}

/// Owner cleanup runs on the existing broker turn, including its orderly shutdown.
pub(super) async fn retire(state: &CapnpBrokerState, all: bool) -> Result<(), String> {
    if !retirement_needed(state, all)? {
        return Ok(());
    }
    let _order = state.owner_commit_order.lock().await;
    let scopes = {
        let registry = state
            .enrolled_tasks
            .lock()
            .map_err(|_| "enrolled registry unavailable")?;
        let identities = registry.connections.keys().copied().collect::<Vec<_>>();
        // Revalidation locks the same registry; release its guard before checking custody.
        drop(registry);
        identities
    };
    let obsolete = scopes
        .into_iter()
        .filter(|scope| all || current(state, *scope).is_err())
        .collect::<Vec<_>>();
    let retired = {
        let mut registry = state
            .enrolled_tasks
            .lock()
            .map_err(|_| "enrolled registry unavailable")?;
        for scope in obsolete {
            registry.retire_scope(scope);
        }
        std::mem::take(&mut registry.retired)
    };
    for run in retired {
        let snapshot = state
            .services
            .workspace_memberships
            .as_ref()
            .ok_or("native owner absent")?
            .task_snapshot(&run.room)?;
        let active = snapshot
            .controls
            .get(&run.task)
            .and_then(|c| c.active_run.as_ref());
        // A completed or superseded run needs no mutation; never abandon a newer epoch.
        if !active
            .is_some_and(|active| active.epoch == run.epoch && active.agent_id == run.agent_id)
        {
            continue;
        }
        commit(
            state,
            &run.room,
            WorkspaceTaskAuthority::Host {
                attestor: run.attestor,
            },
            WorkspaceTaskCommand::Abandon {
                task_id: run.task,
                run_epoch: run.epoch,
                reason: "Managed recipient connection retired".into(),
            },
        )
        .await?;
    }
    Ok(())
}

// Ordinary owner operations share the order lock, but an empty enrolled
// registry has no owner cleanup to serialize. On shutdown first drain every
// in-flight lifecycle admission; accepting=false then prevents new entries.
fn retirement_needed(state: &CapnpBrokerState, all: bool) -> Result<bool, String> {
    if all {
        let guard = state
            .lifecycle
            .write()
            .map_err(|_| "broker lifecycle unavailable")?;
        if state.is_accepting() {
            return Err("enrolled shutdown requires retired admission".into());
        }
        drop(guard);
    }
    let registry = state
        .enrolled_tasks
        .lock()
        .map_err(|_| "enrolled registry unavailable")?;
    let needed = !registry.connections.is_empty() || !registry.retired.is_empty();
    drop(registry);
    Ok(needed)
}

impl CapnpBrokerState {
    /// Consume the exact kernel-admitted descriptor. Labels alone cannot enter this path.
    /// # Errors
    /// Refuses stale process custody, unavailable native authority, or bounded capacity.
    pub fn connect_enrolled_worker(
        &self,
        connection: AdmittedManagedConnection,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        self.while_live(|state| state.admit_enrolled_worker(connection, certificate))
    }
    fn admit_enrolled_worker(
        &self,
        connection: AdmittedManagedConnection,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        self.owner_health()?;
        let (process, stream) = connection.into_parts();
        let facts = process.facts()?;
        let route = CrdtRoute {
            app_id: facts.binding.app.clone(),
            cluster_id: facts.binding.room.clone(),
            record_id: String::new(),
        };
        let native = self
            .services
            .workspace_memberships
            .as_ref()
            .ok_or("native task owner unavailable")?;
        if !native.requires_room(&route.cluster_id) {
            return Err("enrolled descriptor requires a native workspace".into());
        }
        native.health()?;
        let authority = self
            .certificate_authority
            .get()
            .ok_or("certificate authority unavailable")?;
        let host_scope = if certificate.capabilities.can_act_as_host {
            authority.authorize_host_worker(&route, certificate)?;
            Some(LocalHostWorkerScope {
                route: route.clone(),
                certificate: certificate.clone(),
            })
        } else {
            authority.authorize_join(
                uuid::Uuid::parse_str(&route.cluster_id).map_err(|_| "invalid room")?,
                certificate,
            )?;
            None
        };
        tilo_user_identity::verify_device_cert_at(certificate, self.policy_now_ns()?)
            .map_err(|_| "enrolled certificate rejected")?;
        // Native owner rechecks the host attestor itself at every command.
        let agent = crate::services::WorkspaceTaskAgentInvocation {
            room: route.cluster_id.clone(),
            agent_id: facts.binding.agent.clone(),
        };
        match native.task_agent_authority(&agent)? {
            WorkspaceTaskAuthority::Agent { attestor, agent_id }
                if &attestor == certificate && agent_id == facts.binding.agent => {}
            _ => {
                return Err(
                    "enrolled descriptor does not name the actual native host attestor".into(),
                );
            }
        }
        if !matches!(facts.binding.scope, ManagedLaunchScope::Task { .. }) {
            return Err("enrolled task backend requires a task-bound process".into());
        }
        stream.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| "worker registry unavailable")?;
        let mut registry = self
            .enrolled_tasks
            .lock()
            .map_err(|_| "enrolled registry unavailable")?;
        if workers.len() >= MAX_CONNECTIONS || registry.connections.len() >= MAX_CONNECTIONS {
            return Err("enrolled descriptor capacity exhausted".into());
        }
        let scope = uuid::Uuid::new_v4();
        registry.connections.insert(
            scope,
            Connection {
                process,
                certificate: certificate.clone(),
                route: route.clone(),
                peer: None,
                subscribed: false,
                sender: None,
            },
        );
        workers.push(PendingWorker {
            stream,
            binding: WorkerBinding {
                managed_scope: Some(scope),
                host_scope,
                registration: None,
                route,
                root: certificate.user_public_key,
                device: certificate.device_public_key,
            },
        });
        drop(registry);
        drop(workers);
        self.worker_admitted.notify_one();
        Ok(())
    }
}

fn current(state: &CapnpBrokerState, scope: uuid::Uuid) -> Result<Connection, String> {
    if !state.is_accepting() {
        return Err("enrolled broker retired".into());
    }
    state.owner_health()?;
    let connection = state
        .enrolled_tasks
        .lock()
        .map_err(|_| "enrolled registry unavailable")?
        .connections
        .get(&scope)
        .cloned()
        .ok_or("enrolled process custody retired")?;
    let facts = connection.process.facts()?;
    if facts.binding.room != connection.route.cluster_id
        || facts.binding.app != connection.route.app_id
    {
        return Err("enrolled process route changed".into());
    }
    let native = state
        .services
        .workspace_memberships
        .as_ref()
        .ok_or("native task owner unavailable")?;
    let invocation = crate::services::WorkspaceTaskAgentInvocation {
        room: facts.binding.room.clone(),
        agent_id: facts.binding.agent.clone(),
    };
    match native.task_agent_authority(&invocation)? {
        WorkspaceTaskAuthority::Agent { attestor, agent_id }
            if attestor == connection.certificate && agent_id == facts.binding.agent => {}
        _ => return Err("enrolled host attestation changed".into()),
    }
    let cert = &connection.certificate;
    tilo_user_identity::verify_device_cert_at(cert, state.policy_now_ns()?)
        .map_err(|_| "enrolled certificate expired")?;
    let authority = state
        .certificate_authority
        .get()
        .ok_or("certificate authority unavailable")?;
    if cert.capabilities.can_act_as_host {
        authority.authorize_host_worker(&connection.route, cert)?;
    } else {
        authority.authorize_join(
            uuid::Uuid::parse_str(&connection.route.cluster_id).map_err(|_| "invalid room")?,
            cert,
        )?;
    }
    let tree = state
        .membership
        .as_ref()
        .ok_or("membership unavailable")?
        .snapshot()?;
    let workspace = tree
        .workspaces
        .iter()
        .find(|w| w.room.to_string() == facts.binding.room)
        .ok_or("enrolled workspace absent")?;
    let now = state.policy_now_ns()? / 1_000_000_000;
    if workspace
        .role_of_agent_in_room_at(&facts.binding.agent, &connection.route.cluster_id, now)
        .is_none_or(|r| r == CapRole::Denied)
    {
        return Err("enrolled Agent membership revoked".into());
    }
    drop(tree);
    Ok(connection)
}

pub(super) fn bind_transport(
    state: &CapnpBrokerState,
    scope: uuid::Uuid,
    peer: u64,
) -> Result<(), String> {
    current(state, scope)?;
    let mut registry = state
        .enrolled_tasks
        .lock()
        .map_err(|_| "enrolled registry unavailable")?;
    let connection = registry
        .connections
        .get_mut(&scope)
        .ok_or("enrolled custody retired")?;
    if connection.peer.is_some() {
        return Err("enrolled transport already consumed".into());
    }
    connection.peer = Some(peer);
    drop(registry);
    Ok(())
}

pub(super) fn revalidate(
    state: &CapnpBrokerState,
    route: &CrdtRoute,
    admitted: &AdmittedJoinIdentity,
) -> Result<AdmittedJoinIdentity, String> {
    let connection = current(state, admitted.managed_scope.ok_or("no enrolled custody")?)?;
    let facts = connection.process.facts()?;
    if route != &connection.route
        || admitted.certificate.as_ref() != Some(&connection.certificate)
        || admitted.agent_id.as_deref() != Some(&facts.binding.agent)
    {
        return Err("enrolled identity binding changed".into());
    }
    let tree = state
        .membership
        .as_ref()
        .ok_or("membership unavailable")?
        .snapshot()?;
    let workspace = tree
        .workspaces
        .iter()
        .find(|w| w.room.to_string() == route.cluster_id)
        .ok_or("workspace absent")?;
    let mut identity = admitted.clone();
    identity.role = workspace
        .role_of_agent_in_room_at(
            &facts.binding.agent,
            &route.cluster_id,
            state.policy_now_ns()? / 1_000_000_000,
        )
        .ok_or("Agent absent")?;
    Ok(identity)
}

pub(super) fn join(
    state: &CapnpBrokerState,
    scope: uuid::Uuid,
    peer: u64,
    route: &CrdtRoute,
    admitted: &mut AdmittedJoinIdentity,
    kind: &str,
    handle: &str,
) -> Result<(), String> {
    let connection = current(state, scope)?;
    let facts = connection.process.facts()?;
    if kind != "agent"
        || handle != facts.binding.agent
        || route != &connection.route
        || admitted.certificate.as_ref() != Some(&connection.certificate)
    {
        return Err("enrolled join differs from the actual process".into());
    }
    let mut registry = state
        .enrolled_tasks
        .lock()
        .map_err(|_| "enrolled registry unavailable")?;
    let entry = registry
        .connections
        .get_mut(&scope)
        .ok_or("enrolled custody retired")?;
    if entry.peer.is_some_and(|id| id != peer) {
        return Err("enrolled socket already joined".into());
    }
    entry.peer = Some(peer);
    drop(registry);
    admitted.managed_scope = Some(scope);
    admitted.agent_id = Some(facts.binding.agent.clone());
    admitted.display_handle = Some(facts.binding.agent);
    *admitted = revalidate(state, route, admitted)?;
    Ok(())
}

fn channel(handle: &CapnpChannelHandle) -> Result<(uuid::Uuid, Connection), String> {
    handle
        .state
        .revalidate_admission(&handle.route(), &handle.verified_identity)
        .map_err(|e| e.to_string())?;
    let scope = handle
        .verified_identity
        .managed_scope
        .ok_or("ordinary channels cannot use enrolled tasks")?;
    let connection = current(&handle.state, scope)?;
    if connection.peer != Some(handle.peer_id) {
        return Err("enrolled channel peer changed".into());
    }
    Ok((scope, connection))
}

fn offer(
    handle: &CapnpChannelHandle,
    connection: &Connection,
) -> Result<EnrolledTaskEnvelope, String> {
    let facts = connection.process.facts()?;
    let ManagedLaunchScope::Task { task, run } = &facts.binding.scope else {
        return Err("process is not task-bound".into());
    };
    let snapshot = handle
        .state
        .services
        .workspace_memberships
        .as_ref()
        .ok_or("native owner absent")?
        .task_snapshot(&facts.binding.room)?;
    let task_state = snapshot
        .tasks
        .iter()
        .find(|t| &t.id == task)
        .ok_or("native task absent")?;
    let control = snapshot
        .controls
        .get(task)
        .ok_or("native task control absent")?;
    if task_state.route != tilo_desktop_model::WorkspaceTaskRoute::Model
        || task_state.status != tilo_desktop_model::WorkspaceTaskStatus::Queued
        || task_state.assigned_agent.as_deref() != Some(&facts.binding.agent)
        || control.active_run.is_some()
    {
        return Err("native task is not queued for this enrolled Agent".into());
    }
    let numeric = handle
        .state
        .task_run_id
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| "task run identity exhausted")?;
    let envelope = EnrolledTaskEnvelope {
        version: 1,
        message_id: uuid::Uuid::new_v4().to_string(),
        task_id: task.clone(),
        run_id: run.clone(),
        launch_id: facts.binding.launch.clone(),
        runtime_epoch: facts.runtime.runtime_epoch.clone(),
        recipient_agent: facts.binding.agent.clone(),
        channel_lease: facts.channel_id.clone(),
        task: runtime_collab::TaskRunEnvelope {
            version: 1,
            task_id: task.clone(),
            title: task_state
                .resource
                .as_ref()
                .and_then(|r| r.agent_request.as_ref())
                .map_or_else(|| task_state.title.clone(), |r| r.instruction.clone()),
            run_id: numeric,
            lease: uuid::Uuid::new_v4().to_string(),
            planning: runtime_collab::TaskRunPlanning::Model,
            execution_budget: control.execution_budget,
            action_context: serde_json::json!({"group":{"id":task,"label":task,"kind":"task"}}),
        },
        history: None,
    };
    envelope.validate()?;
    Ok(envelope)
}

pub(super) fn subscribe(
    handle: &CapnpChannelHandle,
) -> Result<runtime_collab::collab_capnp::enrolled_task_stream::Client, String> {
    let (scope, connection) = channel(handle)?;
    let envelope = offer(handle, &connection)?;
    let run = envelope.run_id.clone();
    let bytes = serde_json::to_string(&envelope).map_err(|e| e.to_string())?;
    let mut registry = handle
        .state
        .enrolled_tasks
        .lock()
        .map_err(|_| "enrolled registry unavailable")?;
    if registry.runs.len() >= MAX_RUNS
        || registry
            .connections
            .get(&scope)
            .is_none_or(|c| c.subscribed)
        || registry.runs.contains_key(&run)
    {
        return Err("enrolled stream cannot be replaced or redelivered".into());
    }
    let retained = registry
        .runs
        .values()
        .try_fold(bytes.len(), |total, r| {
            serde_json::to_vec(&r.envelope).map(|v| total.saturating_add(v.len()))
        })
        .map_err(|e| e.to_string())?;
    if retained > 8 * 1024 * 1024 {
        return Err("enrolled retained-byte capacity exhausted".into());
    }
    let (sender, receiver) = mpsc::channel(1);
    sender
        .try_send(bytes)
        .map_err(|_| "enrolled delivery unavailable")?;
    let retained_connection = registry
        .connections
        .get_mut(&scope)
        .ok_or("enrolled custody retired")?;
    retained_connection.subscribed = true;
    retained_connection.sender = Some(sender);
    registry.runs.insert(
        run,
        Run {
            scope,
            envelope,
            acceptance: None,
            epoch: None,
            start_epoch: None,
            sequence: 0,
            last_update: None,
            terminal: false,
            fenced: false,
        },
    );
    drop(registry);
    // One bounded task, with the stream retained until actual connection retirement.
    Ok(capnp_rpc::new_client(Stream {
        state: handle.state.clone(),
        scope,
        peer: handle.peer_id,
        receiver: AsyncMutex::new(receiver),
    }))
}

struct Stream {
    state: Arc<CapnpBrokerState>,
    scope: uuid::Uuid,
    peer: u64,
    receiver: AsyncMutex<mpsc::Receiver<String>>,
}
impl runtime_collab::collab_capnp::enrolled_task_stream::Server for Stream {
    fn read(
        self: Rc<Self>,
        _params: runtime_collab::collab_capnp::enrolled_task_stream::ReadParams,
        mut results: runtime_collab::collab_capnp::enrolled_task_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let connection = current(&self.state, self.scope).map_err(capnp::Error::failed)?;
            if connection.peer != Some(self.peer) {
                return Err(capnp::Error::failed("enrolled stream peer changed".into()));
            }
            drop(connection);
            let payload = self
                .receiver
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| capnp::Error::disconnected("enrolled process retired".into()))?;
            let connection = current(&self.state, self.scope).map_err(capnp::Error::failed)?;
            if connection.peer != Some(self.peer) {
                return Err(capnp::Error::failed("enrolled stream peer changed".into()));
            }
            results.get().set_task_json(&payload);
            Ok(())
        })
    }
}

pub(super) struct Context {
    state: Arc<CapnpBrokerState>,
    scope: uuid::Uuid,
    peer: u64,
}
impl Context {
    pub(super) fn from_channel(handle: &CapnpChannelHandle) -> Result<Self, String> {
        let (scope, _) = channel(handle)?;
        Ok(Self {
            state: handle.state.clone(),
            scope,
            peer: handle.peer_id,
        })
    }
    fn connection(&self) -> Result<(uuid::Uuid, Connection), String> {
        let connection = current(&self.state, self.scope)?;
        if connection.peer != Some(self.peer) {
            return Err("enrolled channel peer changed".into());
        }
        Ok((self.scope, connection))
    }
}

fn acceptance(
    handle: &Context,
    json: &str,
) -> Result<(EnrolledTaskAcceptance, Run, Connection), String> {
    if json.len() > 4096 {
        return Err("enrolled acceptance exceeds bound".into());
    }
    let accepted: EnrolledTaskAcceptance =
        serde_json::from_str(json).map_err(|_| "invalid enrolled acceptance")?;
    let (scope, connection) = handle.connection()?;
    let registry = handle
        .state
        .enrolled_tasks
        .lock()
        .map_err(|_| "enrolled registry unavailable")?;
    let run = registry
        .runs
        .get(&accepted.run_id)
        .cloned()
        .ok_or("enrolled offer not retained")?;
    drop(registry);
    if run.scope != scope || run.fenced || !accepted.matches(&run.envelope) {
        return Err("acceptance does not own this exact live enrolled offer".into());
    }
    Ok((accepted, run, connection))
}

fn authority(handle: &Context, connection: &Connection) -> Result<WorkspaceTaskAuthority, String> {
    let facts = connection.process.facts()?;
    let invocation = crate::services::WorkspaceTaskAgentInvocation {
        room: facts.binding.room,
        agent_id: facts.binding.agent,
    };
    let authority = handle
        .state
        .services
        .workspace_memberships
        .as_ref()
        .ok_or("native owner absent")?
        .task_agent_authority(&invocation)?;
    match &authority {
        WorkspaceTaskAuthority::Agent { attestor, agent_id }
            if attestor == &connection.certificate && agent_id == invocation.agent_id() =>
        {
            Ok(authority)
        }
        _ => Err("native Agent authority changed".into()),
    }
}

fn fence(state: &CapnpBrokerState, id: &str) -> Result<(), String> {
    state
        .enrolled_tasks
        .lock()
        .map_err(|_| "enrolled registry unavailable")?
        .runs
        .get_mut(id)
        .ok_or("enrolled offer retired")?
        .fenced = true;
    Ok(())
}

async fn commit(
    state: &CapnpBrokerState,
    room: &str,
    authority: WorkspaceTaskAuthority,
    command: WorkspaceTaskCommand,
) -> Result<(), String> {
    let request = OwnerCommitRequest {
        room: room.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::new_v4(),
            authority,
            command,
        },
    };
    let receipt = state
        .services
        .workspace_memberships
        .as_ref()
        .ok_or("native owner absent")?
        .execute(request.clone())
        .await?;
    receipt.validate_for(&request)?;
    Ok(())
}

fn observed_started_epoch(
    handle: &Context,
    connection: &Connection,
    accepted: &EnrolledTaskAcceptance,
    run: &Run,
    start_epoch: u64,
) -> Result<u64, String> {
    let snapshot = handle
        .state
        .services
        .workspace_memberships
        .as_ref()
        .ok_or("native owner absent")?
        .task_snapshot(&connection.route.cluster_id)?;
    let active = snapshot
        .controls
        .get(&accepted.task_id)
        .and_then(|c| c.active_run.as_ref())
        .ok_or("accepted native run is not observable")?;
    if snapshot
        .controls
        .get(&accepted.task_id)
        .is_none_or(|control| control.execution_budget != run.envelope.task.execution_budget)
    {
        return Err("native task execution budget changed after delivery".into());
    }
    if active.agent_id != accepted.recipient_agent || active.epoch != start_epoch {
        return Err("native accepted Agent changed".into());
    }
    Ok(active.epoch)
}

pub(super) async fn accept(handle: &Context, json: &str) -> Result<(), String> {
    let _ordered = handle.state.owner_commit_order.lock().await;
    let (accepted, run, connection) = acceptance(handle, json)?;
    if let Some(previous) = &run.acceptance {
        return if previous == &accepted {
            Ok(())
        } else {
            Err("changed enrolled acceptance retry".into())
        };
    }
    let agent_authority = authority(handle, &connection)?;
    let WorkspaceTaskAuthority::Agent { attestor, .. } = &agent_authority else {
        return Err("native Agent authority absent".into());
    };
    let snapshot = handle
        .state
        .services
        .workspace_memberships
        .as_ref()
        .ok_or("native owner absent")?
        .task_snapshot(&connection.route.cluster_id)?;
    let control = snapshot
        .controls
        .get(&accepted.task_id)
        .ok_or("queued task control absent")?;
    if control.active_run.is_some() {
        return Err("queued task already has a native run".into());
    }
    let start_epoch = control
        .run_epoch
        .checked_add(1)
        .ok_or("native run epoch exhausted")?;
    handle
        .state
        .enrolled_tasks
        .lock()
        .map_err(|_| "enrolled registry unavailable")?
        .runs
        .get_mut(&accepted.run_id)
        .ok_or("enrolled offer retired")?
        .start_epoch = Some(start_epoch);
    fence(&handle.state, &accepted.run_id)?;
    commit(
        &handle.state,
        &connection.route.cluster_id,
        WorkspaceTaskAuthority::Host {
            attestor: attestor.clone(),
        },
        WorkspaceTaskCommand::StartRun {
            task_id: accepted.task_id.clone(),
            agent_id: accepted.recipient_agent.clone(),
        },
    )
    .await?;
    // Only the durable owner snapshot establishes the accepted epoch and budget.
    let epoch = observed_started_epoch(handle, &connection, &accepted, &run, start_epoch)?;
    handle.connection()?;
    commit(
        &handle.state,
        &connection.route.cluster_id,
        agent_authority,
        WorkspaceTaskCommand::ExecutionPhase {
            task_id: accepted.task_id.clone(),
            run_epoch: epoch,
            sequence: 0,
            phase: tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted,
        },
    )
    .await?;
    let mut registry = handle
        .state
        .enrolled_tasks
        .lock()
        .map_err(|_| "enrolled registry unavailable")?;
    let retained = registry
        .runs
        .get_mut(&accepted.run_id)
        .ok_or("accepted run retired")?;
    retained.acceptance = Some(accepted);
    retained.epoch = Some(epoch);
    retained.fenced = false;
    drop(registry);
    Ok(())
}

fn report_command(
    update: &TaskRunUpdate,
    accepted: &EnrolledTaskAcceptance,
    epoch: u64,
    sequence: u64,
    phase: tilo_desktop_model::WorkspaceTaskExecutionPhase,
) -> (WorkspaceTaskCommand, bool) {
    match update {
        TaskRunUpdate::DirectionsApplied { revision } => (
            WorkspaceTaskCommand::AppliedAck {
                task_id: accepted.task_id.clone(),
                run_epoch: epoch,
                revision: *revision,
            },
            false,
        ),
        TaskRunUpdate::Accepted => (
            WorkspaceTaskCommand::ExecutionPhase {
                task_id: accepted.task_id.clone(),
                run_epoch: epoch,
                sequence,
                phase: tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted,
            },
            false,
        ),
        TaskRunUpdate::Progress { .. } => (
            WorkspaceTaskCommand::ExecutionPhase {
                task_id: accepted.task_id.clone(),
                run_epoch: epoch,
                sequence,
                phase,
            },
            false,
        ),
        TaskRunUpdate::Succeeded { message }
        | TaskRunUpdate::Failed { message }
        | TaskRunUpdate::Cancelled { message } => {
            let outcome = match update {
                TaskRunUpdate::Succeeded { .. } => WorkspaceTaskFinish::Done,
                TaskRunUpdate::Failed { .. } => WorkspaceTaskFinish::Waiting,
                _ => WorkspaceTaskFinish::Cancelled,
            };
            (
                WorkspaceTaskCommand::Finish {
                    task_id: accepted.task_id.clone(),
                    run_epoch: epoch,
                    outcome,
                    summary: message.clone(),
                },
                true,
            )
        }
    }
}

pub(super) async fn report(
    handle: &Context,
    json: &str,
    sequence: u64,
    update_json: &str,
) -> Result<(), String> {
    if update_json.len() > MAX_UPDATE_BYTES {
        return Err("enrolled update exceeds bound".into());
    }
    let update: TaskRunUpdate =
        serde_json::from_str(update_json).map_err(|_| "invalid enrolled task update")?;
    let _ordered = handle.state.owner_commit_order.lock().await;
    let (accepted, run, connection) = acceptance(handle, json)?;
    if run.acceptance.as_ref() != Some(&accepted) {
        return Err("enrolled run has not been accepted".into());
    }
    if sequence == run.sequence && run.last_update.as_ref() == Some(&update) {
        return Ok(());
    }
    if run.terminal || sequence == 0 || sequence <= run.sequence {
        return Err("stale or terminal enrolled report".into());
    }
    let epoch = run.epoch.ok_or("native run epoch absent")?;
    // Generic progress is not evidence of planning or tool execution. Preserve
    // the owner's observed phase instead of inferring one from progress prose.
    let native = handle
        .state
        .services
        .workspace_memberships
        .as_ref()
        .ok_or("native owner absent")?
        .task_snapshot(&connection.route.cluster_id)?;
    let phase = native
        .controls
        .get(&accepted.task_id)
        .and_then(|c| c.execution.as_ref())
        .map(|e| e.phase)
        .ok_or("accepted native phase is not observable")?;
    let (command, terminal) = report_command(&update, &accepted, epoch, sequence, phase);
    let agent_authority = authority(handle, &connection)?;
    fence(&handle.state, &accepted.run_id)?;
    commit(
        &handle.state,
        &connection.route.cluster_id,
        agent_authority,
        command,
    )
    .await?;
    handle.connection()?;
    let mut registry = handle
        .state
        .enrolled_tasks
        .lock()
        .map_err(|_| "enrolled registry unavailable")?;
    let retained = registry
        .runs
        .get_mut(&accepted.run_id)
        .ok_or("reported run retired")?;
    retained.sequence = sequence;
    retained.last_update = Some(update);
    retained.terminal = terminal;
    retained.fenced = false;
    drop(registry);
    Ok(())
}
