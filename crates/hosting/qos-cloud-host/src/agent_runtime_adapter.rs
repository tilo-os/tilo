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
    /// Broker certificate verdicts proved under the Store lock, with the
    /// instant each was last asked for. Reused only while the Store files are
    /// unchanged, before the next roster/revocation time boundary and for at
    /// most `store_hint::STORE_RECHECK`; see [`RuntimeOwner::authorize_key`].
    authorized: Mutex<Vec<(AuthorizationKey, store_hint::Verified, std::time::Instant)>>,
    /// Serializes locked re-proofs so concurrent misses share one Store open.
    proving: Mutex<()>,
}
#[derive(Clone, PartialEq)]
enum AuthorizationKey {
    HostWorker {
        route: qos_action_broker::route::CrdtRoute,
        certificate: DeviceCert,
    },
    Room {
        room: Uuid,
        certificate: DeviceCert,
    },
}
/// Content-free Store-open accounting: every 10 s, one line naming the code
/// sites (file:line) that opened the Store through the runtime owner and how
/// often. Attributes idle and per-step Store work.
pub(crate) fn count_open(site: &'static std::panic::Location<'static>) {
    use std::collections::BTreeMap;
    type Counts = (
        Option<std::time::Instant>,
        BTreeMap<(&'static str, u32), u64>,
    );
    static OPENS: Mutex<Counts> = Mutex::new((None, BTreeMap::new()));
    if !qos_action_broker::agent_timing::store_spans_enabled() {
        return;
    }
    let Ok(mut opens) = OPENS.lock() else { return };
    *opens.1.entry((site.file(), site.line())).or_default() += 1;
    let started = *opens.0.get_or_insert_with(std::time::Instant::now);
    if started.elapsed() >= std::time::Duration::from_secs(10) {
        let mut top: Vec<_> = opens.1.iter().map(|(k, v)| (*v, *k)).collect();
        top.sort_unstable_by(|a, b| b.cmp(a));
        let total: u64 = top.iter().map(|(n, _)| n).sum();
        let sites = top
            .iter()
            .take(8)
            .map(|(n, (file, line))| {
                format!("{}:{line}={n}", file.rsplit('/').next().unwrap_or(file))
            })
            .collect::<Vec<_>>()
            .join(" ");
        tracing::info!(
            target: "qos_cloud_host::agent_timing",
            event = "store_opens",
            total,
            window_ms = started.elapsed().as_millis() as u64,
            sites = %sites,
            "agent timing"
        );
        opens.1.clear();
        opens.0 = Some(std::time::Instant::now());
    }
}
/// Bound on retained broker verdicts; the least recently asked is dropped
/// first. Above the live worker count (one host-worker key per mounted record
/// plus room keys), so a round-robin revalidation never evicts itself.
const AUTHORIZATION_CACHE: usize = 512;
/// A retained verdict nobody asked for within this interval is not re-proved
/// in the next batch (its worker is gone); it is simply dropped.
const AUTHORIZATION_IDLE: std::time::Duration = std::time::Duration::from_secs(10);
/// Token-to-action timing: one info line per runtime event, carrying only the
/// opaque invocation id, the event name and a wall-clock microsecond stamp.
/// Filter with target `qos_cloud_host::agent_timing`.
pub(crate) fn agent_timing(event: &'static str, invocation: Uuid) {
    qos_action_broker::agent_timing::mark(event, Some(invocation));
}
/// A finished owner stage: total duration plus the Store open share.
fn owner_span(event: &'static str, invocation: Uuid, began: std::time::Instant, open_us: u64) {
    qos_action_broker::agent_timing::span(
        event,
        Some(invocation),
        began.elapsed(),
        &[("open_us", open_us)],
    );
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
        crate::peer_memo::configure(root);
        Ok(Self {
            policy: ordinary.policy_context(),
            root: root.canonicalize().map_err(|e| e.to_string())?,
            machine: machine.into(),
            ordinary,
            runtime: OnceLock::new(),
            live: Mutex::new(LiveState::default()),
            authorized: Mutex::new(vec![]),
            proving: Mutex::new(()),
        })
    }
    /// The canonical Store directory (for unlocked change hints only).
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    fn authorization_current(&self, key: &AuthorizationKey) -> Result<bool, String> {
        let now = self.policy_time()?.now_ns().map_err(|e| e.to_string())?;
        let mut cache = self
            .authorized
            .lock()
            .map_err(|_| "authorization cache poisoned")?;
        Ok(cache
            .iter_mut()
            .find(|(retained, _, _)| retained == key)
            .is_some_and(|(_, verified, used)| {
                *used = std::time::Instant::now();
                verified.current(&self.root, now)
            }))
    }
    /// A broker certificate verdict (host worker or room admission).
    ///
    /// Every live worker session revalidates on each RPC, so an idle workspace
    /// asks this for every mounted record several times a second. A verdict is
    /// reused only under the exact rule of `store_hint::Verified::current`
    /// (identical Store files, policy time inside the roster/authority/
    /// revocation interval it was proved in, locked proof < `STORE_RECHECK`).
    /// On a miss the Store is opened once and, under that one lock and policy
    /// instant, the full check is run again for the requested key *and* for
    /// every other recently asked key. Each re-proved verdict is therefore a
    /// fresh locked proof, identical to what that key's own call would have
    /// computed at that instant; failures are never retained. So N workers
    /// cost one Store open per Store change or per `STORE_RECHECK`, not N.
    fn authorize_key(&self, key: AuthorizationKey) -> Result<(), String> {
        if self.authorization_current(&key)? {
            return Ok(());
        }
        let _proving = self
            .proving
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A concurrent miss may have re-proved this key while we waited.
        if self.authorization_current(&key)? {
            return Ok(());
        }
        let others: Vec<_> = self
            .authorized
            .lock()
            .map_err(|_| "authorization cache poisoned")?
            .iter()
            .filter(|(retained, _, used)| retained != &key && used.elapsed() < AUTHORIZATION_IDLE)
            .map(|(retained, _, used)| (retained.clone(), *used))
            .collect();
        let store = self.store()?;
        let now_ns = store.policy_now_ns().map_err(|e| e.to_string())?;
        let peers = store.peer_shared().map_err(|e| e.to_string())?;
        let verdict = self.authorize_locked(&store, &peers.1, now_ns, &key);
        let mut retained: Vec<_> = others
            .into_iter()
            .filter(|(other, _)| {
                self.authorize_locked(&store, &peers.1, now_ns, other)
                    .is_ok()
            })
            .collect();
        if verdict.is_ok() {
            retained.push((key, std::time::Instant::now()));
        }
        let verified = store_hint::Verified::under_lock(&self.root, &store.state, now_ns);
        drop(peers);
        drop(store);
        let mut cache = self
            .authorized
            .lock()
            .map_err(|_| "authorization cache poisoned")?;
        cache.clear();
        if let Some(verified) = verified {
            retained.sort_by_key(|(_, used)| *used);
            let excess = retained.len().saturating_sub(AUTHORIZATION_CACHE);
            cache.extend(
                retained
                    .into_iter()
                    .skip(excess)
                    .map(|(key, used)| (key, verified, used)),
            );
        }
        verdict
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
    #[track_caller]
    pub(crate) fn store(&self) -> Result<Store, String> {
        self.store_for(&self.policy_time()?)
    }
    #[track_caller]
    pub(crate) fn store_for(&self, time: &PolicyTime) -> Result<Store, String> {
        let site = std::panic::Location::caller();
        count_open(site);
        #[cfg(test)]
        authorization_tests::OPENS.with(|opens| opens.set(opens.get() + 1));
        let mut store = self.policy.open_store(&self.root, &self.machine, time)?;
        store.mark_site(site);
        Ok(store)
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
    #[track_caller]
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
    /// Live agent tasks (a live process, no terminal progress) with their
    /// host-known agent id, display name and retained grants: the worker
    /// participant views and presence labels follow exactly these.
    /// In-memory only; grants no authority.
    /// Whether any agent task is live (a live process whose task has no
    /// terminal progress) — the same filter as [`Self::active_agent_tasks`],
    /// without building its list. In-memory only.
    pub(crate) fn has_active_agent_tasks(&self) -> bool {
        let Ok(live) = self.live.lock() else {
            return true;
        };
        live.attempts.iter().any(|(id, attempt)| {
            live.processes.contains_key(&attempt.ids.agent_id)
                && !live.progress.get(id).is_some_and(|p| {
                    matches!(
                        p.update,
                        TaskRunUpdate::Succeeded { .. }
                            | TaskRunUpdate::Failed { .. }
                            | TaskRunUpdate::Cancelled { .. }
                    )
                })
        })
    }
    pub(crate) fn active_agent_tasks(&self) -> Vec<crate::agent_workers::ActiveAgentTask> {
        let Ok(live) = self.live.lock() else {
            return vec![];
        };
        live.attempts
            .iter()
            .filter(|(id, attempt)| {
                live.processes.contains_key(&attempt.ids.agent_id)
                    && !live.progress.get(id).is_some_and(|p| {
                        matches!(
                            p.update,
                            TaskRunUpdate::Succeeded { .. }
                                | TaskRunUpdate::Failed { .. }
                                | TaskRunUpdate::Cancelled { .. }
                        )
                    })
            })
            .map(|(_, attempt)| crate::agent_workers::ActiveAgentTask {
                task: attempt.ids.task_id.clone(),
                agent: attempt.ids.agent_id.clone(),
                name: match qos_agent_control_protocol::decode_request(
                    attempt.intent.request.as_bytes(),
                ) {
                    Ok(AgentRequest::Spawn { name, .. }) => name,
                    _ => "Agent".into(),
                },
                grants: attempt.intent.grants.clone(),
            })
            .collect()
    }
    /// Owner revocation of an agent (security review MUST-10): commit a
    /// terminal fact for each of its live tasks (only the tasks' own owner
    /// may), then close its enrolled connection so no queued step leaves.
    /// Wire from the owner's agents panel (HTTPS owner route).
    pub(crate) fn revoke_agent(
        &self,
        caller: &DeviceCert,
        agent: &str,
        broker: &qos_action_broker::collab_broker::AppActionBroker,
    ) -> Result<usize, String> {
        let attempts: Vec<Uuid> = self
            .live
            .lock()
            .map_err(|_| "live registry poisoned")?
            .attempts
            .iter()
            .filter(|(_, attempt)| attempt.ids.agent_id == agent)
            .map(|(id, _)| *id)
            .collect();
        if attempts.is_empty() {
            return Err("no live task for this agent".into());
        }
        {
            let mut store = self.store()?;
            for id in &attempts {
                let id = crate::agent_runtime_owner::AgentControlId::new(*id)
                    .map_err(|e| e.to_string())?;
                store
                    .revoke_agent_task(caller, &id)
                    .map_err(|e| e.to_string())?;
            }
        }
        if let Ok(mut live) = self.live.lock() {
            live.processes.remove(agent);
        }
        Ok(broker.close_enrolled_agent(agent))
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
        let request = admission.intent().validate(&store.state.room)?;
        let kind = match &request {
            AgentRequest::Spawn { .. } => "spawn_admitted",
            AgentRequest::Message { .. } => "message_admitted",
            _ => "command_admitted",
        };
        if let AgentRequest::Message { agent_id, .. } = &request {
            let Some(PrivateAgentState::V2(ledger)) = &store.state.private_agent_ledger else {
                return Err("private runtime schema unavailable".into());
            };
            let busy = ledger.facts.values().any(|receipt| {
                matches!(&receipt.change.fact, AgentRuntimeFact::Admitted { ids, intent, .. }
                    if ids.agent_id == *agent_id && intent.scope.caller == *caller.certificate())
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
                drop(live);
                agent_timing(kind, id.invocation());
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
        let peers = store.peer_shared().map_err(|e| e.to_string())?;
        let peer = &peers.1;
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
        // Operator visibility: the reason otherwise only reaches the private ledger.
        tracing::warn!(target: "qos_cloud_host::agent_timing", invocation = %id, reason, "agent control uncertain");
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
        agent_timing("process_bound", id.invocation());
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
        let began = std::time::Instant::now();
        let attempt = self.attempt(dispatch.invocation())?;
        let mut store = self.store_for(&attempt.policy_time)?;
        let open_us = qos_action_broker::agent_timing::us(began);
        match store
            .claim_agent_dispatch(&attempt.custody, dispatch)
            .map_err(|e| e.to_string())?
        {
            EffectClaim::Fresh { permit, .. } => {
                let commit = permit.consume(&store).map_err(|e| e.to_string())?;
                owner_span("dispatch_owner", dispatch.invocation(), began, open_us);
                agent_timing("dispatch_committed", dispatch.invocation());
                Ok(DispatchCommit::Fresh(commit))
            }
            EffectClaim::Existing(_) => Ok(DispatchCommit::Existing),
        }
    }
    fn accept(
        &self,
        acceptance: &VerifiedRecipientAcceptance,
    ) -> Result<OwnerCommitReceipt, String> {
        let began = std::time::Instant::now();
        let attempt = self.attempt(acceptance.invocation())?;
        let mut store = self.store_for(&attempt.policy_time)?;
        let open_us = qos_action_broker::agent_timing::us(began);
        let receipt = store
            .complete_agent_control(acceptance)
            .map_err(|e| e.to_string())?;
        owner_span("accept_owner", acceptance.invocation(), began, open_us);
        agent_timing("control_accepted", acceptance.invocation());
        Ok(receipt)
    }
    fn update(&self, update: &VerifiedTaskUpdate) -> Result<OwnerCommitReceipt, String> {
        let began = std::time::Instant::now();
        let attempt = self.attempt(update.invocation())?;
        let mut store = self.store_for(&attempt.policy_time)?;
        let open_us = qos_action_broker::agent_timing::us(began);
        let receipt = store
            .record_agent_task_update(update)
            .map_err(|e| e.to_string())?;
        drop(store);
        owner_span("update_owner", update.invocation(), began, open_us);
        self.live
            .lock()
            .map_err(|_| "live registry poisoned")?
            .progress
            .remove(&update.invocation());
        crate::agent_live::on_update(&attempt, update.invocation(), update.update());
        agent_timing("task_update_committed", update.invocation());
        Ok(receipt)
    }
    fn progress(&self, update: &VerifiedTaskUpdate) -> Result<(), String> {
        let began = std::time::Instant::now();
        let attempt = self.attempt(update.invocation())?;
        let store = self.store_for(&attempt.policy_time)?;
        let open_us = qos_action_broker::agent_timing::us(began);
        store
            .validate_agent_progress(update)
            .map_err(|e| e.to_string())?;
        drop(store);
        owner_span("progress_owner", update.invocation(), began, open_us);
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
        drop(live);
        crate::agent_live::on_progress(self, &attempt, update.invocation(), update.update());
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
        self.authorize_task_action_at(
            dispatch,
            app,
            record,
            resource,
            action,
            &attempt.policy_time,
        )
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
        self.authorize_task_action_at(dispatch, app, record, resource, action, time)
    }
    fn task_step(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        step: ParentStep,
    ) -> Result<Option<ParentRecord>, ParentFailure> {
        let began = std::time::Instant::now();
        let record = self
            .step_store(dispatch.invocation())?
            .agent_task_step(dispatch, step);
        qos_action_broker::agent_timing::slow(
            "lookup_step_owner",
            Some(dispatch.invocation()),
            began,
            &[],
        );
        record
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
        let began = std::time::Instant::now();
        let mut store = self.step_store(dispatch.invocation())?;
        let open_us = qos_action_broker::agent_timing::us(began);
        let record = store.bind_agent_task_step(dispatch, step, child, intent)?;
        drop(store);
        owner_span("bind_step_owner", dispatch.invocation(), began, open_us);
        agent_timing("action_ticket_bound", dispatch.invocation());
        Ok(record)
    }
    fn commit_task_step(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        commit: ChildCommit<'_>,
    ) -> Result<ParentReceipt, ParentFailure> {
        let began = std::time::Instant::now();
        let mut store = self.step_store(dispatch.invocation())?;
        let open_us = qos_action_broker::agent_timing::us(began);
        let receipt = store.commit_agent_task_step(dispatch, commit)?;
        drop(store);
        owner_span("commit_step_owner", dispatch.invocation(), began, open_us);
        agent_timing("action_committed", dispatch.invocation());
        Ok(receipt)
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
            .agent_process_grants_for_new_control(process.facts(), caller)
            .map_err(|e| e.to_string())?;
        Ok(process)
    }
    fn parent_grants(&self, caller: &DeviceCert, agent: &str) -> Result<Vec<GrantAction>, String> {
        let process = self.process(agent)?;
        self.store()?
            .agent_process_grants_for_new_control(process.facts(), caller)
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
                .agent_process_grants_for_new_control(process.facts(), caller)
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
        self.authorize_key(AuthorizationKey::HostWorker {
            route: route.clone(),
            certificate: certificate.clone(),
        })
    }
    fn authorize(&self, room: Uuid, certificate: &DeviceCert) -> Result<(), String> {
        self.authorize_key(AuthorizationKey::Room {
            room,
            certificate: certificate.clone(),
        })
    }
}
impl RuntimeOwner {
    /// Task-action authorization at `time`: the full Store-side task check on
    /// every call (security review MUST-10: no caching of grant, scope or
    /// task verdicts); its inputs (peer, caller) come from exact memos.
    fn authorize_task_action_at(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        app: &str,
        record: &str,
        resource: &str,
        action: &str,
        time: &PolicyTime,
    ) -> Result<(), String> {
        let began = std::time::Instant::now();
        let store = self.store_for(time)?;
        let open_us = qos_action_broker::agent_timing::us(began);
        store
            .authorize_agent_task_action(dispatch, app, record, resource, action)
            .map_err(|e| e.to_string())?;
        drop(store);
        owner_span("authorize_owner", dispatch.invocation(), began, open_us);
        agent_timing("action_authorized", dispatch.invocation());
        Ok(())
    }
    /// The full broker check for one key against the open (locked) Store.
    fn authorize_locked(
        &self,
        store: &Store,
        peer: &crate::StatePeer,
        now_ns: u64,
        key: &AuthorizationKey,
    ) -> Result<(), String> {
        match key {
            AuthorizationKey::HostWorker { route, certificate } => {
                let bound = store.state.bound.as_ref().ok_or("workspace not enrolled")?;
                if certificate != peer.authority()
                    || certificate != &bound.authority
                    || !certificate.capabilities.can_act_as_host
                    || certificate.device_public_key != store.key().verifying_key().to_bytes()
                    || !peer.certificates().contains(certificate)
                    || peer.incomplete_reason().is_some()
                    || route.cluster_id != peer.room()
                    || qos_session_peer::canonical_records::resolve(&route.app_id).is_none()
                    // Existence only: the heads read the same entry without
                    // serializing the whole document.
                    || peer.document_heads(&route.app_id, &route.record_id).is_none()
                {
                    return Err("local worker route or exact pinned host is unavailable".into());
                }
                peer.permissions(certificate.device_public_key, crate::now_time(now_ns))?;
            }
            AuthorizationKey::Room { room, certificate } => {
                if peer.room() != room.to_string() || !peer.certificates().contains(certificate) {
                    return Err("worker certificate no longer admitted".into());
                }
                peer.permissions(certificate.device_public_key, crate::now_time(now_ns))?;
            }
        }
        Ok(())
    }
}
impl qos_action_broker::services::WorkspaceAuthority for RuntimeOwner {
    fn snapshot(&self) -> Result<qos_desktop::WorkspaceTree, String> {
        qos_action_broker::services::WorkspaceAuthority::snapshot(&self.ordinary)
    }
    fn task_opened_records(
        &self,
        task: &str,
    ) -> Result<Vec<qos_agent_control_protocol::OpenedRecord>, String> {
        Ok(self.store()?.task_opened_records(task))
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

#[cfg(test)]
mod authorization_tests;

pub(crate) mod launch_retry;
pub(crate) mod store_hint;
