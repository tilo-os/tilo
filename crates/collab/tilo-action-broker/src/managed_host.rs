//! Native managed launch controller. Retained disk observations never restore process custody.
//! The installed host supplies private service connections; public launch args are task IDs.
mod connections;
pub use connections::InstalledSessionConnections;
mod journal;
#[cfg(test)]
mod journal_tests;
use crate::{
    collab_broker_capnp::CapnpBrokerState,
    peer_actions::CertificateAuthority,
    services::{WorkspaceAuthority, WorkspaceMemberships, WorkspaceTaskAgentInvocation},
};
use journal::Journal;
use launcher::{
    managed::{
        InstalledManagedPackage, ManagedBinding, ManagedLaunchAuthority, ManagedLaunchEvent,
        ManagedLaunchScope, ManagedLauncher, ManagedRuntime, PackageTrust, VerifiedManagedProcess,
        channel::{ManagedProcessAuthority, PendingManagedConnection},
    },
    services::{EnrolledServiceContext, ManagedProviderSelection},
};
use std::{
    collections::HashMap,
    os::unix::{fs::MetadataExt as _, net::UnixStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tilo_desktop_model::{WorkspaceTaskRoute, WorkspaceTaskStatus};
use tilo_peer_state::workspace_task::WorkspaceTaskAuthority;

/// Actual committed native authority; none of these capabilities is supplied by an app.
pub struct ManagedNativeAuthority {
    pub membership: Arc<dyn WorkspaceAuthority>,
    pub tasks: Arc<dyn WorkspaceMemberships>,
    pub certificates: Arc<dyn CertificateAuthority>,
    pub host_custody: Arc<dyn ManagedHostCustody>,
}

/// Installed native owner and network custody, independent of descriptive certificate labels.
pub trait ManagedHostCustody: Send + Sync {
    /// Require current native owner/network custody and this host's actual private attestor.
    /// Check the native owner's exact pinned host (or governor when absent), then its reducer's
    /// exact workspace authority device OR admitted `can_act_as_host` rule.
    /// # Errors
    /// Refuses unavailable/foreign governance, lost key custody or a non-host member certificate.
    fn authorize_host(
        &self,
        room: &str,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String>;
}

/// Trusted installation inputs, separate from any collaborator's launch arguments.
#[derive(Clone)]
pub struct ManagedHostInstallation {
    pub package: InstalledManagedPackage,
    pub trust: PackageTrust,
    pub runtime: ManagedRuntime,
    pub journal_root: PathBuf,
    pub data_home: PathBuf,
    pub provider: ManagedProviderSelection,
}

/// The existing installed identity and model gateway provide connected, context-bound streams.
/// Implementations must use host-held custody and must not discover services inside the app.
pub trait ManagedServiceConnections: Send + Sync {
    /// Open this exact launch's identity and model handoffs. Credentials remain service-owned.
    /// # Errors
    /// Refuses unavailable services or a changed private provider/room selection.
    fn connect(
        &self,
        binding: &ManagedBinding,
        provider: &ManagedProviderSelection,
    ) -> Result<(UnixStream, UnixStream), String>;
}

#[derive(Clone)]
struct Policy {
    binding: ManagedBinding,
    slot: String,
    prior_epoch: u64,
    budget: Option<collab_types::task_budget::TaskExecutionBudget>,
    certificate: tilo_user_identity::DeviceCert,
    original: Option<Arc<OriginalGate>>,
}
struct OriginalGate {
    check: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    consumed: AtomicBool,
}
impl OriginalGate {
    fn recheck(&self) -> Result<(), String> {
        if self.consumed.load(Ordering::Acquire) {
            return Ok(());
        }
        (self.check)()
    }
}
struct Authority {
    native: ManagedNativeAuthority,
    journal: Journal,
    runtime: Mutex<Option<launcher::managed::ManagedRuntimeFacts>>,
    provider: ManagedProviderSelection,
    policies: Mutex<HashMap<String, Policy>>,
}
impl Authority {
    fn attestor(&self, room: &str, agent: &str) -> Result<tilo_user_identity::DeviceCert, String> {
        self.native.tasks.health()?;
        if !self.native.tasks.requires_room(room) {
            return Err("managed launch requires an admitted native workspace".into());
        }
        let invocation = WorkspaceTaskAgentInvocation {
            room: room.into(),
            agent_id: agent.into(),
        };
        let WorkspaceTaskAuthority::Agent { attestor, agent_id } =
            self.native.tasks.task_agent_authority(&invocation)?
        else {
            return Err("managed launch has no native host attestation".into());
        };
        if agent_id != agent {
            return Err("managed launch Agent attestation changed".into());
        }
        let room_id = uuid::Uuid::parse_str(room).map_err(|_| "invalid native launch room")?;
        let clock = self
            .native
            .certificates
            .policy_time()?
            .ok_or("native policy clock unavailable")?;
        let now = clock.now_ns().map_err(|error| error.to_string())?;
        tilo_user_identity::verify_device_cert_at(&attestor, now)
            .map_err(|error| error.to_string())?;
        self.native.host_custody.authorize_host(room, &attestor)?;
        let route = crate::route::CrdtRoute::app("so.tilo.agent", room.to_owned());
        if attestor.capabilities.can_act_as_host {
            self.native
                .certificates
                .authorize_host_worker(&route, &attestor)?;
        } else {
            self.native
                .certificates
                .authorize_join(room_id, &attestor)?;
        }
        let tree = self.native.membership.snapshot()?;
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)
            .ok_or("managed launch workspace absent")?;
        if workspace
            .role_of_agent_in_room_at(agent, room, now / 1_000_000_000)
            .is_none_or(|role| role == ui::permissions::Role::Denied)
        {
            return Err("managed launch Agent is not a current room member".into());
        }
        Ok(attestor)
    }

    fn retained_policy(&self, binding: &ManagedBinding) -> Result<Policy, String> {
        let policies = self
            .policies
            .lock()
            .map_err(|_| "managed launch policies unavailable")?;
        let policy = policies
            .get(&binding.launch)
            .cloned()
            .ok_or("managed launch permit absent")?;
        drop(policies);
        if policy.binding != *binding {
            return Err("retained managed binding changed".into());
        }
        Ok(policy)
    }

    fn policy(&self, binding: &ManagedBinding) -> Result<Policy, String> {
        let policy = self.retained_policy(binding)?;
        if let Some(original) = &policy.original {
            original.recheck()?;
        }
        if policy.binding != *binding
            || self.attestor(&binding.room, &binding.agent)? != policy.certificate
        {
            return Err("managed launch binding or governing host changed".into());
        }
        let snapshot = self.native.tasks.task_snapshot(&binding.room)?;
        let ManagedLaunchScope::Task { task, .. } = &binding.scope else {
            return Err("managed native controller supports task scope only".into());
        };
        let definition = snapshot
            .tasks
            .iter()
            .find(|definition| &definition.id == task)
            .ok_or("managed task removed")?;
        let control = snapshot
            .controls
            .get(task)
            .ok_or("managed task control removed")?;
        if definition.assigned_agent.as_deref() != Some(binding.agent.as_str())
            || definition.route != WorkspaceTaskRoute::Model
            || control.execution_budget != policy.budget
            || !(control.run_epoch == policy.prior_epoch || control.run_epoch == binding.generation)
            || control.active_run.as_ref().is_some_and(|active| {
                active.agent_id != binding.agent || active.epoch != binding.generation
            })
        {
            return Err("managed task assignment, epoch or immutable policy changed".into());
        }
        Ok(policy)
    }
}
impl ManagedLaunchAuthority for Authority {
    fn claim_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        let policy = self.policy(binding)?;
        let ManagedLaunchScope::Task { task, run } = &binding.scope else {
            return Err("managed task scope absent".into());
        };
        let snapshot = self.native.tasks.task_snapshot(&binding.room)?;
        let definition = snapshot
            .tasks
            .iter()
            .find(|definition| &definition.id == task)
            .ok_or("managed task removed")?;
        let control = snapshot
            .controls
            .get(task)
            .ok_or("managed task control removed")?;
        if definition.status != WorkspaceTaskStatus::Queued
            || control.active_run.is_some()
            || control.run_epoch != policy.prior_epoch
        {
            return Err("managed launch requires the exact idle queued task".into());
        }
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| "managed runtime identity unavailable")?;
        let pinned_runtime = runtime
            .as_ref()
            .ok_or("managed runtime identity not installed")?;
        let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"room":binding.room,
            "task":task,"run":run,"agent":binding.agent,"launch":binding.launch,
            "generation":binding.generation,"prior_epoch":policy.prior_epoch,
            "budget":policy.budget,"provider":self.provider,"host_device":hex::encode(policy.certificate.device_public_key),
            "runtime_epoch":pinned_runtime.runtime_epoch,"package_hash":pinned_runtime.package_hash,
            "executable_hash":pinned_runtime.executable_hash,"runtime_hash":pinned_runtime.runtime_hash}))
        .map_err(|error| error.to_string())?;
        drop(runtime);
        self.journal.claim(&policy.slot, &bytes)
    }
    fn revalidate_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        self.policy(binding).map(|_| ())
    }
}
impl ManagedProcessAuthority for Authority {
    fn bind_process(&self, process: &VerifiedManagedProcess) -> Result<(), String> {
        let facts = process.facts()?;
        let policy = self.policy(&facts.binding)?;
        let bytes = serde_json::to_vec(&serde_json::json!({"version":1,
            "launch":facts.binding.launch,"channel":facts.channel_id,
            "agent_pid":facts.process.pid,"agent_start":facts.process.start,
            "supervisor_pid":facts.supervisor.pid,"supervisor_start":facts.supervisor.start,
            "runtime_epoch":facts.runtime.runtime_epoch,"package_hash":facts.runtime.package_hash,
            "executable_hash":facts.runtime.executable_hash,"runtime_hash":facts.runtime.runtime_hash}))
            .map_err(|error| error.to_string())?;
        self.journal.process(&policy.slot, &bytes)
    }
}

/// Owns existing signed-package supervision across native rooms and tasks.
/// The host must drive `poll` off the compositor paint thread and retain this owner until shutdown.
pub struct ManagedHost {
    launcher: ManagedLauncher,
    authority: Arc<Authority>,
    connections: Arc<dyn ManagedServiceConnections>,
    broker: Arc<CapnpBrokerState>,
    pending: HashMap<String, PendingManagedConnection>,
    data_home: PathBuf,
    provider: ManagedProviderSelection,
}
impl ManagedHost {
    /// Verify the configured signed installation and exclusively own its private launch journal.
    /// # Errors
    /// Refuses unsafe installation, unavailable canonical authority or another journal owner.
    pub fn new(
        installation: ManagedHostInstallation,
        native: ManagedNativeAuthority,
        connections: Arc<dyn ManagedServiceConnections>,
        broker: Arc<CapnpBrokerState>,
    ) -> Result<Self, String> {
        native.tasks.health()?;
        let data_home = installation
            .data_home
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let metadata = std::fs::metadata(&data_home).map_err(|error| error.to_string())?;
        if !metadata.is_dir()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err("managed data home is not private to this host".into());
        }
        let authority = Arc::new(Authority {
            native,
            journal: Journal::open(&installation.journal_root)?,
            runtime: Mutex::default(),
            provider: installation.provider.clone(),
            policies: Mutex::default(),
        });
        let launcher = ManagedLauncher::new(
            installation.package,
            installation.trust,
            installation.runtime,
            authority.clone(),
        )?;
        *authority
            .runtime
            .lock()
            .map_err(|_| "managed runtime identity unavailable")? =
            Some(launcher.runtime_lease().revalidate()?);
        Ok(Self {
            launcher,
            authority,
            connections,
            broker,
            pending: HashMap::new(),
            data_home,
            provider: installation.provider,
        })
    }

    /// Reserve and start an already owner-committed queued task. This is a host capability,
    /// not public admission: the published action must authenticate the original Person first.
    /// # Errors
    /// Refuses absent/changed tasks, repeated/ambiguous durable claims or service failures.
    pub fn launch_task(&mut self, room: &str, task: &str) -> Result<String, String> {
        self.start_task(room, task, None)
    }

    /// Retain an original public invocation's current authority until actual process admission.
    /// The callback is host-held custody, never an application or serialized authority claim.
    /// # Errors
    /// Refuses revoked original authority, changed native policy or uncertain launch barriers.
    pub fn launch_guarded_task(
        &mut self,
        room: &str,
        task: &str,
        check: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    ) -> Result<String, String> {
        check()?;
        self.start_task(
            room,
            task,
            Some(Arc::new(OriginalGate {
                check,
                consumed: AtomicBool::new(false),
            })),
        )
    }

    fn start_task(
        &mut self,
        room: &str,
        task: &str,
        original: Option<Arc<OriginalGate>>,
    ) -> Result<String, String> {
        let snapshot = self.authority.native.tasks.task_snapshot(room)?;
        let definition = snapshot
            .tasks
            .iter()
            .find(|definition| definition.id == task)
            .ok_or("native queued task absent")?;
        let agent = definition
            .assigned_agent
            .as_deref()
            .ok_or("native task Agent absent")?;
        let control = snapshot
            .controls
            .get(task)
            .ok_or("native task control absent")?;
        let certificate = self.authority.attestor(room, agent)?;
        let generation = control
            .run_epoch
            .checked_add(1)
            .ok_or("native launch epoch exhausted")?;
        let binding = ManagedBinding {
            room: room.into(),
            app: "so.tilo.agent".into(),
            agent: agent.into(),
            scope: ManagedLaunchScope::Task {
                task: task.into(),
                run: uuid::Uuid::new_v4().to_string(),
            },
            launch: uuid::Uuid::new_v4().to_string(),
            generation,
        };
        let slot = blake3::hash(
            &serde_json::to_vec(&(room, task, generation)).map_err(|error| error.to_string())?,
        )
        .to_hex()
        .to_string();
        let policy = Policy {
            binding: binding.clone(),
            slot,
            prior_epoch: control.run_epoch,
            budget: control.execution_budget,
            certificate,
            original,
        };
        let mut policies = self
            .authority
            .policies
            .lock()
            .map_err(|_| "managed policies unavailable")?;
        if policies.len() >= 1024 {
            return Err("managed launch policy capacity exhausted".into());
        }
        policies.insert(binding.launch.clone(), policy);
        drop(policies);
        let mut permit = self.launcher.authorize(binding.clone())?;
        let (pending, collab) = PendingManagedConnection::pair(&mut permit)?;
        let (identity, model) = self.connections.connect(&binding, &self.provider)?;
        let facts = self.launcher.runtime_lease().revalidate()?;
        let services = EnrolledServiceContext::new(
            binding.clone(),
            self.data_home.clone(),
            facts.runtime_epoch,
            pending.channel_id().into(),
            self.provider.clone(),
            identity,
            collab,
            model,
        )?;
        self.pending.insert(binding.launch.clone(), pending);
        self.launcher.launch(permit, services)?;
        Ok(binding.launch)
    }

    /// Drain the existing supervisor and consume actual kernel-admitted descriptors into the broker.
    /// # Errors
    /// Refuses uncertain process binding/admission; callers must stop and retain the original claim.
    pub fn poll(&mut self) -> Result<Vec<ManagedLaunchEvent>, String> {
        let events = self.launcher.drain_events()?;
        for event in &events {
            match event {
                ManagedLaunchEvent::Started(supervisor) => {
                    if let Err(error) = self.admit_started(supervisor) {
                        let stopped = self.launcher.stop(supervisor.binding());
                        return Err(format!(
                            "managed admission uncertain; claim retained: {error}; stopped: {stopped:?}"
                        ));
                    }
                }
                ManagedLaunchEvent::Finished { binding, .. }
                | ManagedLaunchEvent::Failed {
                    binding,
                    terminal: true,
                    ..
                } => {
                    self.pending.remove(&binding.launch);
                    let policy = self.authority.retained_policy(binding)?;
                    let observation = match event {
                        ManagedLaunchEvent::Finished { exit, .. } => serde_json::json!({
                            "version":1,"launch":binding.launch,"observed":"exact_process_exit", "code":exit.status().code()}),
                        _ => {
                            serde_json::json!({"version":1,"launch":binding.launch,"observed":"supervisor_failure"})
                        }
                    };
                    let bytes =
                        serde_json::to_vec(&observation).map_err(|error| error.to_string())?;
                    self.authority.journal.terminal(&policy.slot, &bytes)?;
                }
                ManagedLaunchEvent::Failed {
                    terminal: false, ..
                } => {}
            }
        }
        Ok(events)
    }

    fn admit_started(
        &mut self,
        supervisor: &launcher::managed::VerifiedManagedSupervisor,
    ) -> Result<(), String> {
        let binding = supervisor.binding();
        let pending = self
            .pending
            .remove(&binding.launch)
            .ok_or("managed startup descriptor absent")?;
        let connection = pending.admit_connection(supervisor, self.authority.as_ref())?;
        let policy = self.authority.policy(binding)?;
        self.broker
            .connect_enrolled_worker(connection, &policy.certificate)?;
        // Only this actual consumed kernel descriptor ends the original startup gate.
        // A terminal action reply alone cannot establish an admitted process/run.
        if let Some(original) = &policy.original {
            original.consumed.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// Stop one exact owned launch through its retained supervisor binding, never a supplied PID.
    /// # Errors
    /// Refuses an unknown launch or unavailable exact supervisor custody.
    pub fn stop_launch(&mut self, launch: &str) -> Result<(), String> {
        let policies = self
            .authority
            .policies
            .lock()
            .map_err(|_| "managed policies unavailable")?;
        let binding = policies
            .get(launch)
            .ok_or("owned managed launch absent")?
            .binding
            .clone();
        drop(policies);
        self.launcher.stop(&binding)
    }

    /// Stop exactly this owner's children using retained supervisor custody, never journal PIDs.
    /// # Errors
    /// Reports uncertain shutdown; retained launch claims remain fenced.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.pending.clear();
        self.launcher.shutdown()
    }
}
