//! Installed runtime callbacks. Store remains the only durable state owner.
use crate::agent_owner::RuntimePolicyClock;
use crate::{Store, agent_owner::CloudAgentAuthority, agent_runtime_owner::*};
use launcher::managed::{
    ManagedBinding, ManagedLaunchAuthority, VerifiedManagedProcess, VerifiedManagedRuntime,
    channel::ManagedProcessAuthority,
};
use qos_action_broker::peer_actions::{OwnerPolicyClock, PolicyTime};
use qos_action_broker::{
    agent_control::{AgentProviderAuthority, AgentProviderState, VerifiedAgentAdmission},
    enrolled_dispatch::{
        ChildCommit, ChildTicket, DispatchCommit, EnrolledAgentOwner, EnrolledProcess,
        ParentFailure, ParentOwner, ParentReceipt, ParentRecord, ParentStep, TaskRunUpdate,
        VerifiedEnrolledDispatch, VerifiedRecipientAcceptance, VerifiedTaskUpdate,
    },
};
use qos_agent_control_protocol::{AgentError, AgentErrorCode, AgentRequest, GrantAction};
use qos_peer_state::owner_commit::{
    AgentExecutionIdsV2, AgentIntentV2, AgentRuntimeFact, MAX_PRIVATE_AGENT_ADMISSIONS,
    OwnerCommitReceipt, PrivateAgentState, VerifiedAgentCaller,
};
use qos_user_identity::DeviceCert;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
use uuid::Uuid;

/// Process-local custody only. No Clone/Serde or reconstruction from the ledger.
pub(crate) struct LiveAttempt {
    policy_time: PolicyTime,
    pub(crate) ids: AgentExecutionIdsV2,
    pub(crate) caller: DeviceCert,
    pub(crate) intent: AgentIntentV2,
    custody: AgentAttemptCustody,
    launch: Mutex<launch_retry::LiveLaunchSlot>,
}
impl LiveAttempt {
    pub(crate) fn launch_custody(&self) -> &AgentAttemptCustody {
        &self.custody
    }
}
#[derive(Clone)]
pub(crate) struct PrivateProgress {
    pub(crate) sequence: u64,
    pub(crate) update: TaskRunUpdate,
}
pub(crate) enum RuntimeAdmission {
    Fresh(AgentRuntimeObservation),
    Existing(AgentRuntimeObservation),
}
#[derive(Default)]
struct LiveState {
    attempts: BTreeMap<Uuid, Arc<LiveAttempt>>,
    processes: BTreeMap<String, VerifiedManagedProcess>,
    progress: BTreeMap<Uuid, PrivateProgress>,
    provider: Option<AgentProviderState>,
    published_grants: Vec<GrantAction>,
}
pub(crate) struct RuntimeOwner {
    policy: Arc<RuntimePolicyClock>,
    root: PathBuf,
    machine: String,
    ordinary: CloudAgentAuthority,
    runtime: OnceLock<VerifiedManagedRuntime>,
    live: Mutex<LiveState>,
}
impl RuntimeOwner {
    pub(crate) fn open(root: &Path, machine: &str) -> Result<Self, String> {
        Self::open_with_clock(
            root,
            machine,
            OwnerPolicyClock::new().map_err(|e| e.to_string())?,
        )
    }
    pub(crate) fn open_with_clock(
        root: &Path,
        machine: &str,
        clock: OwnerPolicyClock,
    ) -> Result<Self, String> {
        let ordinary = CloudAgentAuthority::open_with_clock(root, machine, clock)?;
        Self::from_ordinary(root, machine, ordinary)
    }
    pub(crate) fn open_with_source(
        root: &Path,
        machine: &str,
        clock: OwnerPolicyClock,
        time: PolicyTime,
        source: crate::https_owner::CloudStoreSource,
    ) -> Result<Self, String> {
        let policy = Arc::new(RuntimePolicyClock::new(clock));
        policy.bind_source_owner(time, source)?;
        let ordinary = CloudAgentAuthority::open_with_policy(root, machine, policy)?;
        Self::from_ordinary(root, machine, ordinary)
    }
    fn from_ordinary(
        root: &Path,
        machine: &str,
        ordinary: CloudAgentAuthority,
    ) -> Result<Self, String> {
        Ok(Self {
            policy: ordinary.policy_context(),
            root: root.canonicalize().map_err(|e| e.to_string())?,
            machine: machine.into(),
            ordinary,
            runtime: OnceLock::new(),
            live: Mutex::new(LiveState::default()),
        })
    }
    pub(crate) fn policy_context(&self) -> Arc<RuntimePolicyClock> {
        self.policy.clone()
    }
    pub(crate) fn policy_time(&self) -> Result<PolicyTime, String> {
        self.policy.capture()
    }
    pub(crate) fn validate_policy_time(&self, time: &PolicyTime) -> Result<(), String> {
        self.policy.validate(time)
    }
    pub(crate) fn store(&self) -> Result<Store, String> {
        self.store_for(&self.policy_time()?)
    }
    pub(crate) fn store_for(&self, time: &PolicyTime) -> Result<Store, String> {
        self.policy.open_store(&self.root, &self.machine, time)
    }
    pub(crate) fn ordinary(&self) -> CloudAgentAuthority {
        self.ordinary.clone()
    }
    pub(crate) fn install_runtime(&self, runtime: VerifiedManagedRuntime) -> Result<(), String> {
        runtime.revalidate()?;
        self.runtime
            .set(runtime)
            .map_err(|_| "runtime already installed".into())
    }
    pub(crate) fn runtime(&self) -> Result<&VerifiedManagedRuntime, String> {
        self.runtime
            .get()
            .ok_or_else(|| "managed runtime unavailable".into())
    }
    // No Store lock is taken here: this is called by witness revalidation inside Store.
    pub(crate) fn publish_provider(
        &self,
        snapshot: AgentProviderState,
        published_grants: Vec<GrantAction>,
    ) -> Result<(), String> {
        let mut live = self.live.lock().map_err(|_| "live registry poisoned")?;
        live.provider = Some(snapshot);
        live.published_grants = published_grants;
        Ok(())
    }
    pub(crate) fn stop_readiness(&self) {
        if let Ok(mut live) = self.live.lock() {
            if let Some(state) = live.provider.as_mut() {
                state.required_services_ready = false;
            }
        }
    }
    /// Store for one task's step ledger: the live attempt's original policy
    /// time when present, else the installed registered owner time. Opening
    /// Store never implies authority; each step method rechecks custody.
    fn step_store(&self, id: Uuid) -> Result<Store, ParentFailure> {
        let time = match self.attempt(id) {
            Ok(attempt) => attempt.policy_time.clone(),
            Err(_) => self.policy_time().map_err(ParentFailure::Unavailable)?,
        };
        self.store_for(&time).map_err(ParentFailure::Unavailable)
    }
    /// Host recovery parent over the step ledger (see [`HostRecoveryParent`]).
    pub(crate) fn recovery_parent(self: &Arc<Self>) -> HostRecoveryParent {
        HostRecoveryParent {
            owner: self.clone(),
        }
    }
    pub(crate) fn attempt(&self, id: Uuid) -> Result<Arc<LiveAttempt>, String> {
        let attempt = self
            .live
            .lock()
            .map_err(|_| "live registry poisoned")?
            .attempts
            .get(&id)
            .cloned()
            .ok_or("fresh admission custody absent; status only")?;
        self.validate_policy_time(&attempt.policy_time)?;
        Ok(attempt)
    }
    fn launch_attempt(&self, binding: &ManagedBinding) -> Result<Arc<LiveAttempt>, String> {
        let attempt = self
            .live
            .lock()
            .map_err(|_| "live registry poisoned")?
            .attempts
            .values()
            .find(|a| a.ids.launch_id == binding.launch)
            .cloned()
            .ok_or("fresh launch permit absent; status only")?;
        self.validate_policy_time(&attempt.policy_time)?;
        Ok(attempt)
    }
    /// Called only by the serial runtime command owner after reserving its bounded queue slot.
    pub(crate) fn admit(
        &self,
        caller: &VerifiedAgentCaller,
        admission: &VerifiedAgentAdmission,
        policy_time: &PolicyTime,
    ) -> Result<RuntimeAdmission, String> {
        let id = AgentControlId::new(admission.invocation()).map_err(|e| e.to_string())?;
        {
            let live = self.live.lock().map_err(|_| "live registry poisoned")?;
            if live.attempts.len() >= MAX_PRIVATE_AGENT_ADMISSIONS
                && !live.attempts.contains_key(&id.invocation())
            {
                return Err("live attempt capacity reached".into());
            }
        }
        let mut store = self.store_for(policy_time)?;
        // Replays are observations; a busy check must never hide their original result.
        if let Some(existing) = store
            .observe_agent_intent(caller, &id, admission.intent())
            .map_err(|e| e.to_string())?
        {
            return Ok(RuntimeAdmission::Existing(existing));
        }
        if let AgentRequest::Message { agent_id, .. } =
            admission.intent().validate(&store.state.room)?
        {
            let Some(PrivateAgentState::V2(ledger)) = &store.state.private_agent_ledger else {
                return Err("private runtime schema unavailable".into());
            };
            let busy = ledger.facts.values().any(|receipt| {
                matches!(&receipt.change.fact, AgentRuntimeFact::Admitted { ids, intent, .. }
                    if ids.agent_id == agent_id && intent.scope.caller == *caller.certificate())
                    && !ledger
                        .facts
                        .contains_key(&format!("{}:failed", receipt.change.invocation))
                    && !ledger
                        .facts
                        .contains_key(&format!("{}:task_finished", receipt.change.invocation))
            });
            if busy {
                return Err("agent already has an unfinished or uncertain task".into());
            }
        }
        match store
            .admit_agent_runtime(caller, admission)
            .map_err(|e| e.to_string())?
        {
            AgentAdmissionResult::Existing(observation) => {
                Ok(RuntimeAdmission::Existing(observation))
            }
            AgentAdmissionResult::Fresh(admitted) => {
                let fresh = Arc::new(LiveAttempt {
                    policy_time: policy_time.clone(),
                    ids: admitted.ids.clone(),
                    caller: caller.certificate().clone(),
                    intent: admission.intent().clone(),
                    custody: admitted.into_custody(),
                    launch: Mutex::new(launch_retry::LiveLaunchSlot::Empty),
                });
                let observation = store
                    .observe_agent_runtime(caller, &id)
                    .map_err(|e| e.to_string())?;
                drop(store);
                let mut live = self
                    .live
                    .lock()
                    .map_err(|_| "live registry poisoned after durable admission")?;
                if live.attempts.contains_key(&id.invocation()) {
                    return Err("live admission identity conflict; status only".into());
                }
                live.attempts.insert(id.invocation(), fresh);
                Ok(RuntimeAdmission::Fresh(observation))
            }
        }
    }
    /// Called under the actual Store lock. It never opens Store or invokes authorize().
    pub(crate) fn validate_spawn_current(
        &self,
        store: &Store,
        id: &AgentControlId,
        time: &PolicyTime,
    ) -> Result<(), String> {
        let installed = self.policy.capture_installed()?;
        if !installed.same_origin(time)
            || !store
                .policy_time
                .as_ref()
                .is_some_and(|t| t.same_origin(time))
            || store.root.canonicalize().map_err(|e| e.to_string())? != self.root
            || store.state.machine_id != self.machine
        {
            return Err("spawn recovery differs from installed Store Source".into());
        }
        self.validate_policy_time(time)?;
        self.runtime()?.revalidate()?;
        let intent = store.current_spawn_intent(id).map_err(|e| e.to_string())?;
        let caller = store.agent_caller(intent.scope.caller.device_public_key)?;
        let peer = store.peer().map_err(|e| e.to_string())?;
        let live = self.live.lock().map_err(|_| "live registry poisoned")?;
        let state = live
            .provider
            .as_ref()
            .ok_or("current provider unavailable")?;
        validate_retained_publication(
            state,
            &live.published_grants,
            intent,
            caller.governance_revision(),
            peer.authority().device_public_key,
        )
    }
    /// Same closed door for newly admitted, retried and scanned Spawn work.
    pub(crate) fn recover_before_launch(&self, invocation: Uuid) -> Result<bool, String> {
        let time = self.policy.capture_installed()?;
        let store = self.store_for(&time)?;
        let id = AgentControlId::new(invocation).map_err(|e| e.to_string())?;
        let Some(admission) = store.sole_admitted_spawn(&id).map_err(|e| e.to_string())? else {
            return Ok(false);
        };
        self.validate_spawn_current(&store, &id, &time)?;
        let AgentRuntimeFact::Admitted { ids, intent, .. } = &admission.change.fact else {
            unreachable!()
        };
        // Store -> short live lock, never the reverse. Existing stage reservations survive retries.
        {
            let live = self.live.lock().map_err(|_| "live registry poisoned")?;
            if let Some(attempt) = live.attempts.get(&invocation) {
                if attempt.ids != *ids
                    || attempt.intent != *intent
                    || attempt.caller != intent.scope.caller
                    || !attempt.policy_time.same_origin(&time)
                {
                    return Err("retained live attempt differs from original admission".into());
                }
                return Ok(true);
            }
            if live.attempts.len() >= MAX_PRIVATE_AGENT_ADMISSIONS {
                return Err("live attempt capacity reached".into());
            }
        }
        let admitted = store
            .recover_admitted_spawn(&id, self, &time)
            .map_err(|e| e.to_string())?
            .ok_or("admission changed")?;
        let attempt = Arc::new(LiveAttempt {
            policy_time: time,
            ids: admitted.ids.clone(),
            caller: intent.scope.caller.clone(),
            intent: intent.clone(),
            custody: admitted.into_custody(),
            launch: Mutex::new(launch_retry::LiveLaunchSlot::Empty),
        });
        let mut live = self.live.lock().map_err(|_| "live registry poisoned")?;
        // Serial runtime owns reconstruction; never replace an existing permit or stage state.
        live.attempts.entry(invocation).or_insert(attempt);
        Ok(true)
    }
    /// Test-only: the live attempt of a fixture-launched enrolled task.
    #[cfg(test)]
    pub(crate) fn install_fixture_attempt(&self, task: EnrolledFixtureTask) -> Result<(), String> {
        let time = self.policy_time()?;
        let id = task.custody.id().invocation();
        let attempt = Arc::new(LiveAttempt {
            policy_time: time,
            ids: task.ids,
            caller: task.intent.scope.caller.clone(),
            intent: task.intent,
            custody: task.custody,
            launch: Mutex::new(launch_retry::LiveLaunchSlot::Empty),
        });
        let mut live = self.live.lock().map_err(|_| "live registry poisoned")?;
        if live.attempts.insert(id, attempt).is_some() {
            return Err("fixture attempt replaced a live attempt".into());
        }
        Ok(())
    }
    pub(crate) fn pending_spawns(
        &self,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<Uuid>, String> {
        let time = self.policy.capture_installed()?;
        self.store_for(&time)?
            .pending_spawn_ids(after, limit)
            .map_err(|e| e.to_string())
    }
    pub(crate) fn observe(
        &self,
        caller: &VerifiedAgentCaller,
        id: Uuid,
    ) -> Result<AgentRuntimeObservation, String> {
        self.store()?
            .observe_agent_runtime(caller, &AgentControlId::new(id).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    }
    pub(crate) fn process(&self, agent: &str) -> Result<VerifiedManagedProcess, String> {
        let proof = self
            .live
            .lock()
            .map_err(|_| "live registry poisoned")?
            .processes
            .get(agent)
            .cloned()
            .ok_or("live parent process unavailable")?;
        self.launch_attempt(proof.binding())?;
        proof.facts()?;
        Ok(proof)
    }
    /// Startup is a runtime lifecycle event, never an ordinary Store::open side effect.
    /// Retained work has no corresponding live attempt in this new runtime.
    pub(crate) fn retain_restart_uncertainty(&self) -> Result<(), String> {
        let mut store = self.store()?;
        let Some(PrivateAgentState::V2(ledger)) = &store.state.private_agent_ledger else {
            return Ok(());
        };
        let pending: std::collections::BTreeSet<_> = store
            .pending_spawn_ids(None, MAX_PRIVATE_AGENT_ADMISSIONS)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|id| id.to_string())
            .collect();
        let work: Vec<_> = ledger
            .facts
            .values()
            .filter_map(|receipt| {
                if !matches!(receipt.change.fact, AgentRuntimeFact::Admitted { .. }) {
                    return None;
                }
                let id = receipt.change.invocation.clone();
                // No runtime authority is minted by this structural startup preservation.
                if pending.contains(&id) {
                    return None;
                }
                let has = |kind: &str| ledger.facts.contains_key(&format!("{id}:{kind}"));
                Some((
                    id.clone(),
                    has("completed"),
                    has("failed") || has("unknown"),
                    has("task_finished") || has("task_unknown"),
                ))
            })
            .collect();
        for (id, completed, terminal, task_terminal) in work {
            let id = AgentControlId::new(id.parse().map_err(|_| "invalid retained invocation")?)
                .map_err(|e| e.to_string())?;
            if completed && !task_terminal {
                store
                    .mark_agent_task_unknown(
                        &id,
                        uncertain("Host runtime restarted; original task outcome unknown"),
                    )
                    .map_err(|e| e.to_string())?;
            } else if !completed && !terminal {
                store
                    .mark_agent_unknown(
                        &id,
                        uncertain("Host runtime restarted; original control outcome unknown"),
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    pub(crate) fn uncertain(&self, id: Uuid, reason: &str) -> Result<(), String> {
        let mut store = self.store()?;
        let id = AgentControlId::new(id).map_err(|e| e.to_string())?;
        let Some(PrivateAgentState::V2(ledger)) = &store.state.private_agent_ledger else {
            return Err("private v2 unavailable".into());
        };
        if store
            .sole_admitted_spawn(&id)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            #[cfg(test)]
            if let Ok(attempt) = self.attempt(id.invocation()) {
                launch_retry::test_audit(self, &attempt, "sole_after_error");
            }
            return Ok(());
        }
        let key = id.invocation().to_string();
        if ledger.facts.contains_key(&format!("{key}:completed")) {
            if !ledger.facts.contains_key(&format!("{key}:task_finished"))
                && !ledger.facts.contains_key(&format!("{key}:task_unknown"))
            {
                store
                    .mark_agent_task_unknown(&id, uncertain(reason))
                    .map_err(|e| e.to_string())?;
            }
        } else if !ledger.facts.contains_key(&format!("{key}:failed"))
            && !ledger.facts.contains_key(&format!("{key}:unknown"))
        {
            store
                .mark_agent_unknown(&id, uncertain(reason))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub(crate) fn progress_for(
        &self,
        caller: &VerifiedAgentCaller,
        id: Uuid,
    ) -> Result<Option<PrivateProgress>, String> {
        // Refresh the original caller and operation scope before reading any private progress.
        self.observe(caller, id)?;
        Ok(self
            .live
            .lock()
            .map_err(|_| "live registry poisoned")?
            .progress
            .get(&id)
            .cloned())
    }
}
impl ManagedLaunchAuthority for RuntimeOwner {
    fn claim_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        // Resolve only the live map. Persisted launch labels never replenish a permit.
        let attempt = self.launch_attempt(binding)?;
        let permit = launch_retry::take_permit(&attempt)?;
        let store = self.store_for(&attempt.policy_time)?;
        self.validate_spawn_current(&store, &attempt.custody.id(), &attempt.policy_time)?;
        permit.consume(&store, binding).map_err(|e| e.to_string())?;
        Ok(())
    }
    fn revalidate_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        let attempt = self.launch_attempt(binding)?;
        let store = self.store_for(&attempt.policy_time)?;
        self.validate_spawn_current(&store, &attempt.custody.id(), &attempt.policy_time)?;
        store
            .revalidate_agent_launch(binding, self.runtime()?)
            .map_err(|e| e.to_string())
    }
}
impl ManagedProcessAuthority for RuntimeOwner {
    fn bind_process(&self, process: &VerifiedManagedProcess) -> Result<(), String> {
        let attempt = self.launch_attempt(process.binding())?;
        let mut store = self.store_for(&attempt.policy_time)?;
        let id = store
            .control_for_launch(process.binding())
            .map_err(|e| e.to_string())?;
        // Require retained live admission as well as the actual provisional process witness.
        let attempt = self.attempt(id.invocation())?;
        if attempt.ids.agent_id != process.binding().agent {
            return Err("process agent differs from live admission".into());
        }
        store
            .bind_agent_process(&id, process)
            .map_err(|e| e.to_string())?;
        drop(store);
        let mut live = self
            .live
            .lock()
            .map_err(|_| "live registry poisoned after process barrier")?;
        if let Some(old) = live.processes.get(&process.binding().agent) {
            if old.facts()? != process.facts()? {
                return Err("agent live process conflict".into());
            }
        } else {
            live.processes
                .insert(process.binding().agent.clone(), process.clone());
        }
        Ok(())
    }
}
/// The Cloud's recovery parent for its own installed sender after restart,
/// when the original enrolled connection is gone. It reads and settles only
/// already durably bound steps; `bind` always refuses, so recovery can never
/// create a new child or a second invocation for an existing step.
pub(crate) struct HostRecoveryParent {
    owner: Arc<RuntimeOwner>,
}
impl ParentOwner for HostRecoveryParent {
    fn lookup(&self, step: ParentStep) -> Result<Option<ParentRecord>, ParentFailure> {
        self.owner
            .step_store(Uuid::from_bytes(step.parent()))?
            .recovery_task_step(step)
    }
    fn find_child(&self, child: ChildTicket) -> Result<Option<ParentRecord>, ParentFailure> {
        self.owner
            .store()
            .map_err(ParentFailure::Unavailable)?
            .recovery_task_step_child(child)
    }
    fn bind(
        &mut self,
        _: ParentStep,
        _: ChildTicket,
        _: [u8; 32],
    ) -> Result<ParentRecord, ParentFailure> {
        Err(ParentFailure::Unavailable(
            "recovery parent cannot bind a new step".into(),
        ))
    }
    fn commit(&mut self, commit: ChildCommit<'_>) -> Result<ParentReceipt, ParentFailure> {
        self.owner
            .step_store(Uuid::from_bytes(commit.step.parent()))?
            .recovery_commit_task_step(commit)
    }
}
impl EnrolledAgentOwner for RuntimeOwner {
    fn authorize_process(
        &self,
        process: &EnrolledProcess,
        caller: &DeviceCert,
    ) -> Result<(), String> {
        let attempt = self.launch_attempt(process.binding())?;
        self.store_for(&attempt.policy_time)?
            .authorize_agent_process(process.facts(), caller)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn dispatch(&self, dispatch: &VerifiedEnrolledDispatch) -> Result<DispatchCommit, String> {
        let attempt = self.attempt(dispatch.invocation())?;
        let mut store = self.store_for(&attempt.policy_time)?;
        match store
            .claim_agent_dispatch(&attempt.custody, dispatch)
            .map_err(|e| e.to_string())?
        {
            EffectClaim::Fresh { permit, .. } => Ok(DispatchCommit::Fresh(
                permit.consume(&store).map_err(|e| e.to_string())?,
            )),
            EffectClaim::Existing(_) => Ok(DispatchCommit::Existing),
        }
    }
    fn accept(
        &self,
        acceptance: &VerifiedRecipientAcceptance,
    ) -> Result<OwnerCommitReceipt, String> {
        let attempt = self.attempt(acceptance.invocation())?;
        self.store_for(&attempt.policy_time)?
            .complete_agent_control(acceptance)
            .map_err(|e| e.to_string())
    }
    fn update(&self, update: &VerifiedTaskUpdate) -> Result<OwnerCommitReceipt, String> {
        let attempt = self.attempt(update.invocation())?;
        let receipt = self
            .store_for(&attempt.policy_time)?
            .record_agent_task_update(update)
            .map_err(|e| e.to_string())?;
        self.live
            .lock()
            .map_err(|_| "live registry poisoned")?
            .progress
            .remove(&update.invocation());
        Ok(receipt)
    }
    fn progress(&self, update: &VerifiedTaskUpdate) -> Result<(), String> {
        let attempt = self.attempt(update.invocation())?;
        self.store_for(&attempt.policy_time)?
            .validate_agent_progress(update)
            .map_err(|e| e.to_string())?;
        let mut live = self.live.lock().map_err(|_| "live registry poisoned")?;
        if live.progress.len() >= 32 && !live.progress.contains_key(&update.invocation()) {
            return Err("private progress capacity reached".into());
        }
        if let Some(prior) = live.progress.get(&update.invocation()) {
            if update.sequence() < prior.sequence
                || (update.sequence() == prior.sequence && update.update() != &prior.update)
            {
                return Err("private progress sequence conflict".into());
            }
        }
        live.progress.insert(
            update.invocation(),
            PrivateProgress {
                sequence: update.sequence(),
                update: update.update().clone(),
            },
        );
        Ok(())
    }
    fn authorize_task_action(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        app: &str,
        record: &str,
        resource: &str,
        action: &str,
    ) -> Result<(), String> {
        let attempt = self.attempt(dispatch.invocation())?;
        self.store_for(&attempt.policy_time)?
            .authorize_agent_task_action(dispatch, app, record, resource, action)
            .map_err(|e| e.to_string())
    }
    fn authorize_task_action_with_policy_time(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        app: &str,
        record: &str,
        resource: &str,
        action: &str,
        time: &PolicyTime,
    ) -> Result<(), String> {
        // The original invocation's registered clock and origin, never a
        // fresh sample: the Store opened for it rechecks the task ledger.
        self.validate_policy_time(time)?;
        self.store_for(time)?
            .authorize_agent_task_action(dispatch, app, record, resource, action)
            .map_err(|e| e.to_string())
    }
    fn task_step(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        step: ParentStep,
    ) -> Result<Option<ParentRecord>, ParentFailure> {
        self.step_store(dispatch.invocation())?
            .agent_task_step(dispatch, step)
    }
    fn task_step_child(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        child: ChildTicket,
    ) -> Result<Option<ParentRecord>, ParentFailure> {
        self.step_store(dispatch.invocation())?
            .agent_task_step_child(dispatch, child)
    }
    fn bind_task_step(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        step: ParentStep,
        child: ChildTicket,
        intent: [u8; 32],
    ) -> Result<ParentRecord, ParentFailure> {
        self.step_store(dispatch.invocation())?
            .bind_agent_task_step(dispatch, step, child, intent)
    }
    fn commit_task_step(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        commit: ChildCommit<'_>,
    ) -> Result<ParentReceipt, ParentFailure> {
        self.step_store(dispatch.invocation())?
            .commit_agent_task_step(dispatch, commit)
    }
    fn grants(
        &self,
        process: &EnrolledProcess,
        caller: &DeviceCert,
    ) -> Result<Vec<GrantAction>, String> {
        let attempt = self.launch_attempt(process.binding())?;
        self.store_for(&attempt.policy_time)?
            .agent_process_grants(process.facts(), caller)
            .map(|(_, g)| g)
            .map_err(|e| e.to_string())
    }
    fn disconnected(&self, process: &EnrolledProcess) {
        // The witness may already have been retired. Only labels locate uncertain facts;
        // no positive effect or permission is granted from these labels.
        let ids: Vec<_> = match self.live.lock() {
            Ok(mut live) => {
                live.processes.remove(&process.binding().agent);
                live.attempts
                    .iter()
                    .filter(|(_, a)| a.ids.agent_id == process.binding().agent)
                    .map(|(id, _)| *id)
                    .collect()
            }
            Err(_) => return,
        };
        for id in ids {
            if let Err(error) = self.uncertain(
                id,
                "Managed recipient disconnected; original outcome unknown",
            ) {
                tracing::error!(%error,"retaining agent uncertainty failed");
            }
        }
    }
}
impl AgentProviderAuthority for RuntimeOwner {
    fn snapshot(&self) -> Result<AgentProviderState, String> {
        self.live
            .lock()
            .map_err(|_| "live registry poisoned")?
            .provider
            .clone()
            .ok_or_else(|| "agent provider unavailable".into())
    }
    fn parent_process(
        &self,
        caller: &DeviceCert,
        agent: &str,
    ) -> Result<VerifiedManagedProcess, String> {
        let process = self.process(agent)?;
        self.store()?
            .agent_process_grants(process.facts(), caller)
            .map_err(|e| e.to_string())?;
        Ok(process)
    }
    fn parent_grants(&self, caller: &DeviceCert, agent: &str) -> Result<Vec<GrantAction>, String> {
        let process = self.process(agent)?;
        self.store()?
            .agent_process_grants(process.facts(), caller)
            .map(|(_, g)| g)
            .map_err(|e| e.to_string())
    }
    fn authorize(
        &self,
        caller: &DeviceCert,
        request: &AgentRequest,
    ) -> Result<Vec<GrantAction>, String> {
        let store = self.store()?;
        let current = store.agent_caller(caller.device_public_key)?;
        if current.certificate() != caller {
            return Err("agent caller certificate changed".into());
        }
        let snapshot = self.snapshot()?;
        if snapshot.governance_revision != current.governance_revision() {
            return Err("agent provider governance refresh pending".into());
        }
        if let AgentRequest::Message { agent_id, .. } = request {
            let process = self.process(agent_id)?;
            let (_, grants) = store
                .agent_process_grants(process.facts(), caller)
                .map_err(|e| e.to_string())?;
            // Exact current worker grant availability is checked by the installed publication snapshot.
            let published = self
                .live
                .lock()
                .map_err(|_| "live registry poisoned")?
                .published_grants
                .clone();
            if !grants.iter().all(|grant| published.contains(grant)) {
                return Err("inherited task grant is no longer published".into());
            }
            Ok(grants)
        } else if matches!(request, AgentRequest::Spawn { .. }) {
            if !snapshot.private_v2_ready
                || !snapshot.required_services_ready
                || snapshot.grant_offers.len() != 1
            {
                return Err("agent mutation services unavailable".into());
            }
            Ok(snapshot.grant_offers[0].actions.clone())
        } else {
            Ok(vec![])
        }
    }
}
impl qos_action_broker::peer_actions::CertificateAuthority for RuntimeOwner {
    fn policy_time(&self) -> Result<Option<PolicyTime>, String> {
        RuntimeOwner::policy_time(self).map(Some)
    }
    fn authorize_with_policy_time(
        &self,
        room: Uuid,
        certificate: &DeviceCert,
        time: &PolicyTime,
    ) -> Result<(), String> {
        self.validate_policy_time(time)?;
        qos_action_broker::peer_actions::CertificateAuthority::authorize(self, room, certificate)
    }
    fn authorize_host_worker(
        &self,
        route: &qos_action_broker::route::CrdtRoute,
        certificate: &DeviceCert,
    ) -> Result<(), String> {
        let store = self.store()?;
        let peer = store.peer().map_err(|e| e.to_string())?;
        let bound = store.state.bound.as_ref().ok_or("workspace not enrolled")?;
        if certificate != peer.authority()
            || certificate != &bound.authority
            || !certificate.capabilities.can_act_as_host
            || certificate.device_public_key != store.key().verifying_key().to_bytes()
            || !peer.certificates().contains(certificate)
            || peer.incomplete_reason().is_some()
            || route.cluster_id != peer.room()
            || !collab_types::first_party_app_ids::is_any_first_party_app(
                &route.app_id,
                &["so.tilo.word-processor", "so.tilo.chat"],
            )
            || peer.document(&route.app_id, &route.record_id).is_none()
        {
            return Err("local worker route or exact pinned host is unavailable".into());
        }
        peer.permissions(
            certificate.device_public_key,
            store.policy_now().map_err(|e| e.to_string())?,
        )
        .map(|_| ())
    }
    fn authorize(&self, room: Uuid, certificate: &DeviceCert) -> Result<(), String> {
        let store = self.store()?;
        let peer = store.peer().map_err(|e| e.to_string())?;
        if peer.room() != room.to_string() || !peer.certificates().contains(certificate) {
            return Err("worker certificate no longer admitted".into());
        }
        peer.permissions(
            certificate.device_public_key,
            store.policy_now().map_err(|e| e.to_string())?,
        )
        .map(|_| ())
    }
}
impl qos_action_broker::services::WorkspaceAuthority for RuntimeOwner {
    fn snapshot(&self) -> Result<qos_desktop::WorkspaceTree, String> {
        qos_action_broker::services::WorkspaceAuthority::snapshot(&self.ordinary)
    }
}
fn uncertain(message: &str) -> AgentError {
    AgentError {
        code: AgentErrorCode::OutcomeUncertain,
        message: message.chars().take(4096).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn agent_runtime_adapter_reopen_labels_do_not_create_launch_custody() {
        let (dir, store, _) = crate::agent_owner_tests::enrolled();
        let binding = ManagedBinding {
            room: store.state.room.clone(),
            app: "so.tilo.agent".into(),
            agent: "agent-a".into(),
            task: "task-a".into(),
            run: "run-a".into(),
            launch: "launch-a".into(),
            generation: store.state.generation,
        };
        let before = std::fs::read(dir.path().join("state.json")).unwrap();
        drop(store);
        for _ in 0..2 {
            let owner = RuntimeOwner::open(dir.path(), "agent-admission").unwrap();
            assert_eq!(
                owner.claim_launch(&binding).unwrap_err(),
                "fresh launch permit absent; status only"
            );
        }
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            before
        );
    }
}

#[cfg(test)]
mod source_tests;

// Current publication availability, not a new offer or descriptor revision. Exact
// retained grants may be a subset of a larger current publication, never substituted.
fn validate_retained_publication(
    state: &AgentProviderState,
    published: &[GrantAction],
    intent: &AgentIntentV2,
    governance: &str,
    host_device: [u8; 32],
) -> Result<(), String> {
    if !state.private_v2_ready
        || !state.required_services_ready
        || state.machine != intent.scope.machine
        || state.room.to_string() != intent.scope.room
        || state.host.to_string() != intent.scope.host
        || state.host_device != host_device
        || state.app_peer.to_string() != intent.route.app_peer
        || state.governance_revision != governance
        || !intent.grants.iter().all(|g| published.contains(g))
    {
        return Err("retained Spawn current authority/publication unavailable".into());
    }
    Ok(())
}

#[cfg(test)]
mod recovery_tests;

pub(crate) mod launch_retry;
