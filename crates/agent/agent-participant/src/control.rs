//! Steering agents from any participant: spawn, message, pause, resume,
//! stop and status, as `tilo_actions` remote actions the host answers.
//!
//! [`AgentControl`] is one agent's shared state: its task queue, its
//! conversation, and the pause and stop switches its participant obeys
//! before every step. [`AgentSupervisor`] holds every approved delegation
//! (Cloud's grant offers) and every running agent, and answers the
//! vocabulary in `tilo_relay_protocol::agents` through [`ActionHost`], so a
//! browser steers agents over the same remote-action frames it uses for
//! apps. Only devices of the delegating user may steer; an agent may not.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CallerPrincipal, CataloguePage, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvocationLedger, InvocationReceipt, InvocationState,
    InvokeArgument, InvokeRequest, Pending,
};
use tilo_actions::{
    ActionContract, ActionOutput, Collects, ExecutionLocus, InvocationId, InvocationUpdate,
    OperationKind,
};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::agents::{
    AGENT_KIND, AgentGrant, AgentInfo, AgentMessage, AgentStatus, AgentStep, AgentTask,
    AgentsStatus, CANCEL_TASK_ACTION, GrantRequest, GrantState, MESSAGE_ACTION, MessageRole,
    PAUSE_ACTION, REQUEST_GRANT_ACTION, RESUME_ACTION, REVOKE_ACTION, SPAWN_ACTION, STATUS_ACTION,
    STOP_ACTION, SUPERVISOR_KIND, SUPERVISOR_RESOURCE, TaskState, agent_resource, color_hex,
    parse_color_hex,
};
use tilo_relay_protocol::delegation::{
    Delegate, DelegateProposal, DelegateScope, Delegation, MAX_DELEGATE_TEXT_BYTES,
    MAX_DELEGATED_APPS, MAX_DELEGATED_ROOMS,
};
use tilo_user_identity::DeviceCert;

/// Conversation entries kept per agent.
const MAX_CONVERSATION: usize = 200;
/// Tasks kept per agent, finished ones first to go.
const MAX_TASKS: usize = 100;
/// Grants kept, settled (denied, expired, revoked) ones first to go.
const MAX_GRANTS: usize = 32;
/// Shortest and longest grant a person may ask for.
const GRANT_SECS: std::ops::RangeInclusive<u64> = 60..=30 * 24 * 60 * 60;
/// Cursor colours for grants that name none.
const PALETTE: [[u8; 3]; 6] = [
    [0x7c, 0x3a, 0xed],
    [0xdb, 0x27, 0x77],
    [0x05, 0x96, 0x69],
    [0xd9, 0x77, 0x06],
    [0x25, 0x63, 0xeb],
    [0x0e, 0x74, 0x90],
];

/// An agent's certified device: key, capability-less certificate and
/// root-signed delegation.
#[derive(Clone)]
pub struct AgentIdentity {
    /// The agent's device key.
    pub key: SigningKey,
    /// Its root-signed certificate.
    pub cert: DeviceCert,
    /// Its root-signed delegation.
    pub delegation: Delegation,
}

/// The agent id `delegation` names. The supervisor only ever holds agent
/// delegations: a grant's proposal names an agent, the approver's answer is
/// held to the proposal, and restoring refuses any other kind.
#[must_use]
pub fn agent_id(delegation: &Delegation) -> &str {
    delegation.delegate.agent_id().unwrap_or_default()
}

/// Why a control request was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlError {
    /// The agent was stopped; it takes nothing further.
    Stopped,
    /// No such agent, delegation or task.
    Unknown,
    /// The delegation already runs an agent.
    InUse,
    /// The agent has no task with that id.
    UnknownTask,
    /// The task already completed or failed.
    Finished,
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stopped => "the agent was stopped",
            Self::Unknown => "no such agent",
            Self::InUse => "an agent already runs under this delegation",
            Self::UnknownTask => "the agent has no such task",
            Self::Finished => "the task already ended",
        })
    }
}

struct ControlState {
    status: AgentStatus,
    paused: bool,
    tasks: Vec<AgentTask>,
    conversation: Vec<AgentMessage>,
    activity: Option<String>,
    room: Option<String>,
}

/// Raised whenever something the agent's loop acts on changed — its
/// steering, its session or its host — so the loop sleeps until then
/// instead of on an interval.
#[derive(Default)]
struct Changed {
    raised: Mutex<bool>,
    condvar: Condvar,
}

/// Raised whenever something [`AgentSupervisor::snapshot`] returns changed,
/// so the host writes its state when it changes rather than on an interval.
#[derive(Default)]
pub(crate) struct SnapshotChanges {
    generation: Mutex<u64>,
    condvar: Condvar,
}

impl SnapshotChanges {
    fn generation_guard(&self) -> MutexGuard<'_, u64> {
        self.generation.lock_recover()
    }

    /// Something the snapshot holds changed, or the host is stopping.
    pub(crate) fn raise(&self) {
        let mut generation = self.generation_guard();
        *generation = generation.wrapping_add(1);
        drop(generation);
        self.condvar.notify_all();
    }

    /// The current generation: read it before taking a snapshot, then
    /// [`Self::wait_past`] it.
    pub(crate) fn generation(&self) -> u64 {
        *self.generation_guard()
    }

    /// Block until the generation moves past `seen` or `timeout` passes
    /// (`None`: no limit). A change raised after `seen` was read returns at
    /// once, so none is lost between a write and this wait.
    pub(crate) fn wait_past(&self, seen: u64, timeout: Option<Duration>) {
        let generation = self.generation_guard();
        let woken = match timeout {
            Some(timeout) => {
                self.condvar
                    .wait_timeout_while(generation, timeout, |now| *now == seen)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => self
                .condvar
                .wait_while(generation, |now| *now == seen)
                .unwrap_or_else(PoisonError::into_inner),
        };
        drop(woken);
    }
}

/// One agent's steering state, shared by its participant and the
/// supervisor.
pub struct AgentControl {
    delegation: Delegation,
    state: Mutex<ControlState>,
    changed: Changed,
    /// Who confirms an action the agent's grants leave to the person: the
    /// person's own host, never the agent.
    witness: Mutex<Option<Witness>>,
    /// The supervisor's signal that its snapshot changed, once the agent
    /// is one of its agents.
    reports_to: OnceLock<Arc<SnapshotChanges>>,
}

/// The person's host an agent asks before an action that asks first
/// ([`tilo_actions::remote::ActionHost::confirm`]): it puts the question to
/// the person on their own device and answers with their
/// [`Confirmation::Witnessed`](tilo_actions::remote::Confirmation::Witnessed),
/// or refuses.
pub type Witness = Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>;

impl AgentControl {
    /// A starting agent under `delegation`.
    #[must_use]
    pub fn new(delegation: Delegation) -> Arc<Self> {
        Arc::new(Self {
            delegation,
            state: Mutex::new(ControlState {
                status: AgentStatus::Starting,
                paused: false,
                tasks: Vec::new(),
                conversation: Vec::new(),
                activity: None,
                room: None,
            }),
            changed: Changed::default(),
            witness: Mutex::new(None),
            reports_to: OnceLock::new(),
        })
    }

    /// An agent under `delegation` as a host persisted it (`info`), after a
    /// restart: queued tasks stay queued, a task that was running when the
    /// host went down failed, a paused agent stays paused, and a stopped or
    /// failed one stays so. Anything else starts again.
    #[must_use]
    pub fn restore(delegation: Delegation, info: &AgentInfo) -> Arc<Self> {
        let mut tasks = info.tasks.clone();
        for task in &mut tasks {
            if task.state == TaskState::Running {
                task.state = TaskState::Failed;
                task.error = Some("the host restarted while this task ran".into());
            }
        }
        let (status, paused) = match info.status {
            AgentStatus::Stopped | AgentStatus::Failed => (info.status, false),
            AgentStatus::Paused => (AgentStatus::Paused, true),
            _ => (AgentStatus::Starting, false),
        };
        Arc::new(Self {
            delegation,
            state: Mutex::new(ControlState {
                status,
                paused,
                tasks,
                conversation: info.conversation.clone(),
                activity: None,
                room: info.room.clone(),
            }),
            changed: Changed::default(),
            witness: Mutex::new(None),
            reports_to: OnceLock::new(),
        })
    }

    /// The room the agent works in.
    #[must_use]
    pub fn room(&self) -> Option<String> {
        self.state().room.clone()
    }

    /// Name the room the agent works in.
    pub fn set_room(&self, room: &str) {
        self.state().room = Some(room.to_owned());
        self.persisted_changed();
    }

    /// Whether the agent is done for good: stopped, or its session failed.
    #[must_use]
    pub fn ended(&self) -> bool {
        matches!(
            self.state().status,
            AgentStatus::Stopped | AgentStatus::Failed
        )
    }

    /// The agent could not run: record why and take nothing further.
    pub fn fail(&self, reason: &str) {
        let mut state = self.state();
        for task in &mut state.tasks {
            if matches!(task.state, TaskState::Queued | TaskState::Running) {
                task.state = TaskState::Failed;
                task.error = Some(reason.to_owned());
            }
        }
        push_bounded(
            &mut state.conversation,
            AgentMessage {
                role: MessageRole::Agent,
                text: reason.to_owned(),
            },
            MAX_CONVERSATION,
        );
        state.status = AgentStatus::Failed;
        state.paused = false;
        state.activity = None;
        drop(state);
        self.persisted_changed();
        self.wake();
    }

    fn state(&self) -> MutexGuard<'_, ControlState> {
        self.state.lock_recover()
    }

    /// Tell `changes` whenever what this agent persists changes.
    pub(crate) fn report_to(&self, changes: &Arc<SnapshotChanges>) {
        let _ = self.reports_to.set(Arc::clone(changes));
    }

    /// What this agent persists changed: its status, tasks, conversation,
    /// activity or room.
    fn persisted_changed(&self) {
        if let Some(changes) = self.reports_to.get() {
            changes.raise();
        }
    }

    /// Wake the agent's loop: its steering, its session or its host
    /// changed.
    pub(crate) fn wake(&self) {
        *self.changed.raised.lock_recover() = true;
        self.changed.condvar.notify_all();
    }

    /// Block until [`Self::wake`] or `timeout` (`None`: no limit), then
    /// lower the signal. A wake that came before the call returns at once,
    /// so none is lost between a pump and this wait.
    pub(crate) fn wait_for_change(&self, timeout: Option<Duration>) {
        let raised = self.changed.raised.lock_recover();
        let mut raised = match timeout {
            Some(timeout) => {
                self.changed
                    .condvar
                    .wait_timeout_while(raised, timeout, |raised| !*raised)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => self
                .changed
                .condvar
                .wait_while(raised, |raised| !*raised)
                .unwrap_or_else(PoisonError::into_inner),
        };
        *raised = false;
    }

    /// The person's host that confirms what the agent's grants leave to
    /// them. Without one, such an action is refused.
    pub fn set_witness(&self, witness: Option<Witness>) {
        *self.witness.lock_recover() = witness;
    }

    pub(crate) fn witness(&self) -> Option<Witness> {
        self.witness.lock_recover().clone()
    }

    /// The agent's delegation.
    #[must_use]
    pub const fn delegation(&self) -> &Delegation {
        &self.delegation
    }

    /// Queue a task and return its id.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Stopped`] once the agent was stopped.
    pub fn message(&self, text: &str) -> Result<String, ControlError> {
        let mut state = self.state();
        if matches!(state.status, AgentStatus::Stopped | AgentStatus::Failed) {
            return Err(ControlError::Stopped);
        }
        // A grant may spawn a replacement participant. Task addresses must
        // never refer to a new task when a delayed cancellation arrives.
        let id = InvocationId::new().0.to_string();
        push_bounded(
            &mut state.conversation,
            AgentMessage {
                role: MessageRole::User,
                text: text.to_owned(),
            },
            MAX_CONVERSATION,
        );
        if state.tasks.len() >= MAX_TASKS
            && let Some(index) = state
                .tasks
                .iter()
                .position(|task| !matches!(task.state, TaskState::Queued | TaskState::Running))
        {
            state.tasks.remove(index);
        }
        state.tasks.push(AgentTask {
            id: id.clone(),
            text: text.to_owned(),
            state: TaskState::Queued,
            steps: Vec::new(),
            error: None,
        });
        drop(state);
        self.persisted_changed();
        self.wake();
        Ok(id)
    }

    /// Hold the agent before its next step.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Stopped`] once the agent was stopped.
    pub fn pause(&self) -> Result<(), ControlError> {
        let mut state = self.state();
        if state.status == AgentStatus::Stopped {
            return Err(ControlError::Stopped);
        }
        state.paused = true;
        state.status = AgentStatus::Paused;
        drop(state);
        self.persisted_changed();
        self.wake();
        Ok(())
    }

    /// Let a paused agent continue.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Stopped`] once the agent was stopped.
    pub fn resume(&self) -> Result<(), ControlError> {
        let mut state = self.state();
        if state.status == AgentStatus::Stopped {
            return Err(ControlError::Stopped);
        }
        state.paused = false;
        state.status = if state
            .tasks
            .iter()
            .any(|task| task.state == TaskState::Running)
        {
            AgentStatus::Running
        } else {
            AgentStatus::Waiting
        };
        drop(state);
        self.persisted_changed();
        self.wake();
        Ok(())
    }

    /// Cancel the running and every queued task; the agent takes nothing
    /// further.
    pub fn stop(&self) {
        let mut state = self.state();
        for task in &mut state.tasks {
            if matches!(task.state, TaskState::Queued | TaskState::Running) {
                task.state = TaskState::Cancelled;
            }
        }
        state.status = AgentStatus::Stopped;
        state.paused = false;
        state.activity = None;
        drop(state);
        self.persisted_changed();
        self.wake();
    }

    /// Cancel task `id` while it is queued or running; the agent goes on
    /// with the next queued one. Cancelling a cancelled task changes
    /// nothing. Running tasks stop at the participant's next task-state
    /// check. A step already past that check may complete and publish;
    /// planning and accepted app operations are not interrupted. This
    /// acknowledges cancellation state, not execution quiescence.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::UnknownTask`] for a task the agent does not
    /// have and [`ControlError::Finished`] for one that already ended.
    pub fn cancel_task(&self, id: &str) -> Result<(), ControlError> {
        let mut state = self.state();
        let task = state
            .tasks
            .iter_mut()
            .find(|task| task.id == id)
            .ok_or(ControlError::UnknownTask)?;
        let was = task.state;
        match was {
            TaskState::Queued | TaskState::Running => task.state = TaskState::Cancelled,
            TaskState::Cancelled => {}
            TaskState::Completed | TaskState::Failed => return Err(ControlError::Finished),
        }
        if was == TaskState::Running {
            state.activity = None;
            if state.status == AgentStatus::Running {
                state.status = AgentStatus::Waiting;
            }
        }
        drop(state);
        self.persisted_changed();
        self.wake();
        Ok(())
    }

    /// Where the agent stands.
    #[must_use]
    pub fn status(&self) -> AgentStatus {
        self.state().status
    }

    /// Whether the participant may take its next step now.
    #[must_use]
    pub fn may_act(&self) -> bool {
        let state = self.state();
        !state.paused && state.status != AgentStatus::Stopped
    }

    /// Whether task `id` is still wanted.
    #[must_use]
    pub fn is_running(&self, id: &str) -> bool {
        self.state()
            .tasks
            .iter()
            .any(|task| task.id == id && task.state == TaskState::Running)
    }

    /// The participant joined (`Waiting`) or its session failed.
    pub fn set_status(&self, status: AgentStatus) {
        let mut state = self.state();
        if state.status != AgentStatus::Stopped && !state.paused && state.status != status {
            state.status = status;
            drop(state);
            self.persisted_changed();
        }
    }

    /// Start the oldest queued task, unless paused or stopped.
    pub fn begin_next(&self) -> Option<(String, String)> {
        let mut state = self.state();
        if state.paused || state.status == AgentStatus::Stopped {
            return None;
        }
        let task = state
            .tasks
            .iter_mut()
            .find(|task| task.state == TaskState::Queued)?;
        task.state = TaskState::Running;
        let begun = (task.id.clone(), task.text.clone());
        state.status = AgentStatus::Running;
        drop(state);
        self.persisted_changed();
        Some(begun)
    }

    /// Whether a task waits for [`Self::begin_next`].
    pub(crate) fn has_queued(&self) -> bool {
        self.state()
            .tasks
            .iter()
            .any(|task| task.state == TaskState::Queued)
    }

    /// Record one action task `id` performed.
    pub fn record_step(&self, id: &str, step: AgentStep) {
        let mut state = self.state();
        if let Some(task) = state.tasks.iter_mut().find(|task| task.id == id) {
            task.steps.push(step);
            drop(state);
            self.persisted_changed();
        }
    }

    /// Finish task `id` with a reply or an error. A cancelled task stays
    /// cancelled.
    pub fn finish(&self, id: &str, outcome: Result<String, String>) {
        let mut state = self.state();
        let Some(task) = state.tasks.iter_mut().find(|task| task.id == id) else {
            return;
        };
        if task.state != TaskState::Running {
            return;
        }
        let reply = match outcome {
            Ok(reply) => {
                task.state = TaskState::Completed;
                reply
            }
            Err(error) => {
                task.state = TaskState::Failed;
                task.error = Some(error.clone());
                error
            }
        };
        push_bounded(
            &mut state.conversation,
            AgentMessage {
                role: MessageRole::Agent,
                text: reply,
            },
            MAX_CONVERSATION,
        );
        state.activity = None;
        if !state.paused && state.status != AgentStatus::Stopped {
            state.status = AgentStatus::Waiting;
        }
        drop(state);
        self.persisted_changed();
    }

    /// What the agent is doing now, as its presence shows it.
    pub fn set_activity(&self, activity: Option<String>) {
        let mut state = self.state();
        if state.activity != activity {
            state.activity = activity;
            drop(state);
            self.persisted_changed();
        }
    }

    /// The agent's current activity line.
    #[must_use]
    pub fn activity(&self) -> Option<String> {
        self.state().activity.clone()
    }

    /// The agent as a browser shows it.
    #[must_use]
    pub fn info(&self) -> AgentInfo {
        let state = self.state();
        AgentInfo {
            id: agent_id(&self.delegation).to_owned(),
            label: self.delegation.label.clone(),
            color: color_hex(self.delegation.color),
            device: hex::encode(self.delegation.device_public_key),
            status: state.status,
            activity: state.activity.clone(),
            room: state.room.clone(),
            tasks: state.tasks.clone(),
            conversation: state.conversation.clone(),
            delegation: hex::encode(self.delegation.encode()),
        }
    }
}

/// A grant id from its label: a short slug and a few key-derived hex digits.
fn grant_id(label: &str, seed: &[u8; 32]) -> String {
    let mut slug: String = label
        .chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c.is_whitespace() || c == '-' || c == '_' {
                Some('-')
            } else {
                None
            }
        })
        .take(40)
        .collect();
    if slug.trim_matches('-').is_empty() {
        slug = "agent".into();
    }
    format!("{}-{}", slug.trim_matches('-'), hex::encode(&seed[..3]))
}

fn trim_grants(grants: &mut Vec<Grant>) {
    while grants.len() > MAX_GRANTS {
        match grants
            .iter()
            .position(|grant| !matches!(grant.state, GrantState::Pending | GrantState::Approved))
        {
            Some(index) => {
                grants.remove(index);
            }
            None => break,
        }
    }
}

fn restore_identity(
    key: Option<String>,
    cert: Option<DeviceCert>,
    delegation: Option<String>,
) -> Option<AgentIdentity> {
    let seed = <[u8; 32]>::try_from(hex::decode(key?).ok()?).ok()?;
    let delegation = Delegation::decode(&hex::decode(delegation?).ok()?).ok()?;
    let key = SigningKey::from_bytes(&seed);
    let cert = cert?;
    (cert.device_public_key == key.verifying_key().to_bytes()
        && delegation.device_public_key == cert.device_public_key
        && delegation.delegate.agent_id().is_some())
    .then_some(AgentIdentity {
        key,
        cert,
        delegation,
    })
}

fn push_bounded<T>(list: &mut Vec<T>, item: T, max: usize) {
    if list.len() >= max {
        list.remove(0);
    }
    list.push(item);
}

/// Starts an agent's participant for an identity and its control state:
/// on its own thread in a host, on the test's thread in a test.
pub type Spawner =
    Arc<dyn Fn(AgentIdentity, Arc<AgentControl>) -> Result<(), String> + Send + Sync>;

/// What a host gives its supervisor: the approver for new grants, and the
/// relay's register of agent devices.
pub trait AgentHostLink: Send + Sync {
    /// Ask the user's approving device to certify `key` as the agent
    /// `proposal` describes, in one consent step. Returns the pairing code
    /// the approver shows; `done` later receives the agent's certificate
    /// and root-signed delegation, or why there are none.
    ///
    /// # Errors
    ///
    /// Returns why the request could not be queued.
    fn request_approval(
        &self,
        key: &SigningKey,
        proposal: &DelegateProposal,
        done: ApprovalDone,
    ) -> Result<String, String>;

    /// Admit the agent device `cert` of `delegation` to its rooms.
    ///
    /// # Errors
    ///
    /// Returns why the host refused the delegation.
    fn register(&self, cert: &DeviceCert, delegation: &Delegation) -> Result<(), String>;

    /// Refuse the agent device from now on.
    fn withdraw(&self, device_public_key: [u8; 32]);

    /// The host of the person `delegation` names, which puts an action the
    /// agent's grants leave to them to them on their own device. `None`: no
    /// such host here, and the agent's actions that ask first are refused.
    fn witness(&self, delegation: &Delegation) -> Option<Witness> {
        let _ = delegation;
        None
    }
}

/// Receives the outcome of [`AgentHostLink::request_approval`].
pub type ApprovalDone = Box<dyn FnOnce(Result<(DeviceCert, Delegation), String>) + Send>;

/// One grant: what was asked, where it stands, and once approved the
/// agent identity it runs under.
#[derive(Clone)]
struct Grant {
    proposal: DelegateProposal,
    state: GrantState,
    /// The agent's key while the approver has not answered.
    key: Option<SigningKey>,
    identity: Option<AgentIdentity>,
    pairing_code: Option<String>,
    reason: Option<String>,
}

impl Grant {
    fn approved(identity: AgentIdentity) -> Self {
        let delegation = &identity.delegation;
        Self {
            proposal: DelegateProposal {
                delegate: delegation.delegate.clone(),
                label: delegation.label.clone(),
                color: delegation.color,
                scope: delegation.scope.clone(),
                expires_at_ns: delegation.expires_at_ns,
            },
            state: GrantState::Approved,
            key: None,
            identity: Some(identity),
            pairing_code: None,
            reason: None,
        }
    }

    fn id(&self) -> &str {
        self.proposal.delegate.agent_id().unwrap_or_default()
    }

    /// Settle the grant: drop its key material and say why.
    fn settle(&mut self, state: GrantState, reason: &str) {
        self.state = state;
        self.key = None;
        self.identity = None;
        self.pairing_code = None;
        self.reason = Some(reason.to_owned());
    }
}

/// A grant as a host keeps it across restarts. The key is the agent
/// device's secret: the file holding it is the host's to protect.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantRecord {
    /// What was asked.
    pub proposal: DelegateProposal,
    /// Where it stands.
    pub state: GrantState,
    /// The agent device's key seed (hex), while approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Its certificate, while approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<DeviceCert>,
    /// Its delegation ([`Delegation::encode`], hex), while approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation: Option<String>,
    /// Why it was settled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Everything a supervisor needs to pick up where it was.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SupervisorSnapshot {
    /// Every grant.
    pub grants: Vec<GrantRecord>,
    /// Every agent, as a browser sees it.
    pub agents: Vec<AgentInfo>,
}

struct SupervisorState {
    grants: Vec<Grant>,
    agents: BTreeMap<String, Arc<AgentControl>>,
}

/// Every grant and running agent of one host.
pub struct AgentSupervisor {
    user_public_key: [u8; 32],
    spawner: Spawner,
    link: Mutex<Option<Arc<dyn AgentHostLink>>>,
    state: Mutex<SupervisorState>,
    ledger: Mutex<InvocationLedger<InvocationState>>,
    /// Raised whenever [`Self::snapshot`] would return something new.
    changes: Arc<SnapshotChanges>,
    me: Weak<Self>,
}

impl AgentSupervisor {
    /// A supervisor for the agents of `user_public_key`.
    #[must_use]
    pub fn new(user_public_key: [u8; 32], spawner: Spawner) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            user_public_key,
            spawner,
            link: Mutex::new(None),
            state: Mutex::new(SupervisorState {
                grants: Vec::new(),
                agents: BTreeMap::new(),
            }),
            ledger: Mutex::new(InvocationLedger::default()),
            changes: Arc::default(),
            me: me.clone(),
        })
    }

    fn state(&self) -> MutexGuard<'_, SupervisorState> {
        self.state.lock_recover()
    }

    /// The signal raised whenever [`Self::snapshot`] would return something
    /// new: a grant or an agent changed.
    pub(crate) fn changes(&self) -> &Arc<SnapshotChanges> {
        &self.changes
    }

    /// When the next approved grant expires (ns since the epoch), if any.
    pub(crate) fn next_expiry_ns(&self) -> Option<u64> {
        self.state()
            .grants
            .iter()
            .filter(|grant| grant.state == GrantState::Approved)
            .map(|grant| grant.proposal.expires_at_ns)
            .min()
    }

    fn ledger(&self) -> MutexGuard<'_, InvocationLedger<InvocationState>> {
        self.ledger.lock_recover()
    }

    fn link(&self) -> Option<Arc<dyn AgentHostLink>> {
        self.link.lock_recover().clone()
    }

    /// Connect the host: grants can then be requested, and approved ones
    /// are registered and withdrawn with it.
    pub fn set_link(&self, link: Arc<dyn AgentHostLink>) {
        // Agents already running ask the person through this host too.
        for control in self.state().agents.values() {
            control.set_witness(link.witness(control.delegation()));
        }
        *self.link.lock_recover() = Some(link);
    }

    /// Offer an approved agent identity for spawning. The host has already
    /// registered its delegation.
    pub fn add_grant(&self, identity: AgentIdentity) {
        let mut state = self.state();
        state
            .grants
            .retain(|grant| grant.id() != agent_id(&identity.delegation));
        state.grants.push(Grant::approved(identity));
        drop(state);
        self.changes.raise();
    }

    /// Ask the user's approver for a new grant as `request` describes.
    /// Returns the grant id and the pairing code the approver shows.
    ///
    /// # Errors
    ///
    /// Returns why the request is malformed or could not be queued.
    pub fn request_grant(
        &self,
        request: &GrantRequest,
        now_ns: u64,
    ) -> Result<(String, String), String> {
        let link = self.link().ok_or("this host cannot ask for agent grants")?;
        let label = request.label.trim();
        if label.is_empty() || label.len() > MAX_DELEGATE_TEXT_BYTES {
            return Err(format!(
                "a grant's label is 1 to {MAX_DELEGATE_TEXT_BYTES} bytes"
            ));
        }
        if request.rooms.is_empty() || request.rooms.len() > MAX_DELEGATED_ROOMS {
            return Err(format!("a grant names 1 to {MAX_DELEGATED_ROOMS} rooms"));
        }
        if request.apps.len() > MAX_DELEGATED_APPS {
            return Err(format!("a grant names at most {MAX_DELEGATED_APPS} apps"));
        }
        if !GRANT_SECS.contains(&request.expires_in_secs) {
            return Err("a grant lasts from a minute to 30 days".into());
        }
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|error| error.to_string())?;
        let key = SigningKey::from_bytes(&secret);
        let seed = key.verifying_key().to_bytes();
        let color = match &request.color {
            Some(text) => parse_color_hex(text).ok_or("a grant's colour is #rrggbb")?,
            None => seed
                .first()
                .and_then(|byte| usize::from(*byte).checked_rem(PALETTE.len()))
                .and_then(|index| PALETTE.get(index))
                .copied()
                .unwrap_or([0x7c, 0x3a, 0xed]),
        };
        let proposal = DelegateProposal {
            delegate: Delegate::Agent {
                agent_id: grant_id(label, &seed),
            },
            label: label.to_owned(),
            color,
            scope: DelegateScope {
                rooms: request.rooms.clone(),
                apps: request.apps.clone(),
            },
            expires_at_ns: now_ns
                .saturating_add(request.expires_in_secs.saturating_mul(1_000_000_000)),
        };
        let id = proposal.delegate.agent_id().unwrap_or_default().to_owned();
        let me = self.me.clone();
        let answered = id.clone();
        let pairing_code = link.request_approval(
            &key,
            &proposal,
            Box::new(move |outcome| {
                if let Some(supervisor) = me.upgrade() {
                    supervisor.approval_answered(&answered, outcome);
                }
            }),
        )?;
        let mut state = self.state();
        state.grants.push(Grant {
            proposal,
            state: GrantState::Pending,
            key: Some(key),
            identity: None,
            pairing_code: Some(pairing_code.clone()),
            reason: None,
        });
        trim_grants(&mut state.grants);
        drop(state);
        self.changes.raise();
        Ok((id, pairing_code))
    }

    fn approval_answered(&self, id: &str, outcome: Result<(DeviceCert, Delegation), String>) {
        let link = self.link();
        let mut state = self.state();
        Self::answer_grant(&mut state, link.as_deref(), id, outcome);
        drop(state);
        self.changes.raise();
    }

    fn answer_grant(
        state: &mut SupervisorState,
        link: Option<&dyn AgentHostLink>,
        id: &str,
        outcome: Result<(DeviceCert, Delegation), String>,
    ) {
        let Some(grant) = state
            .grants
            .iter_mut()
            .find(|grant| grant.id() == id && grant.state == GrantState::Pending)
        else {
            return;
        };
        let Some(key) = grant.key.take() else {
            return;
        };
        match outcome {
            Ok((cert, delegation)) => {
                let registered = link.map_or(Ok(()), |link| link.register(&cert, &delegation));
                match registered {
                    Ok(()) => {
                        grant.proposal.scope = delegation.scope.clone();
                        grant.proposal.expires_at_ns = delegation.expires_at_ns;
                        grant.state = GrantState::Approved;
                        grant.pairing_code = None;
                        grant.identity = Some(AgentIdentity {
                            key,
                            cert,
                            delegation,
                        });
                    }
                    Err(reason) => grant.settle(GrantState::Denied, &reason),
                }
            }
            Err(reason) => grant.settle(GrantState::Denied, &reason),
        }
    }

    /// Withdraw grant `id`: its agent stops, the host refuses its device
    /// and the agent's key is forgotten.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Unknown`] for an unknown grant.
    pub fn revoke(&self, id: &str) -> Result<(), ControlError> {
        self.settle(id, GrantState::Revoked, "revoked by a person")
    }

    fn settle(&self, id: &str, to: GrantState, reason: &str) -> Result<(), ControlError> {
        let mut state = self.state();
        let grant = state
            .grants
            .iter_mut()
            .find(|grant| grant.id() == id)
            .ok_or(ControlError::Unknown)?;
        let device = grant
            .identity
            .as_ref()
            .map(|identity| identity.delegation.device_public_key);
        grant.settle(to, reason);
        let agent = state.agents.get(id).cloned();
        drop(state);
        self.changes.raise();
        if let Some(agent) = agent {
            agent.stop();
        }
        if let (Some(device), Some(link)) = (device, self.link()) {
            link.withdraw(device);
        }
        Ok(())
    }

    /// Settle every approved grant that expired by `now_ns`: its agent
    /// stops and the host refuses its device. Returns the grants expired.
    pub fn expire(&self, now_ns: u64) -> Vec<String> {
        let due: Vec<String> = self
            .state()
            .grants
            .iter()
            .filter(|grant| {
                grant.state == GrantState::Approved && grant.proposal.expires_at_ns <= now_ns
            })
            .map(|grant| grant.id().to_owned())
            .collect();
        for id in &due {
            let _ = self.settle(id, GrantState::Expired, "the grant expired");
        }
        due
    }

    /// Start the agent of grant `grant` in `room` (the grant's first room
    /// when `None`) on `task`. Returns the agent and task ids.
    ///
    /// # Errors
    ///
    /// Returns why: an unknown or unapproved grant, a room outside it, or
    /// an agent already running under it.
    pub fn spawn(
        &self,
        grant: &str,
        task: &str,
        room: Option<&str>,
    ) -> Result<(String, String), String> {
        let (identity, control) = {
            let mut state = self.state();
            let entry = state
                .grants
                .iter()
                .find(|entry| entry.id() == grant)
                .ok_or_else(|| ControlError::Unknown.to_string())?;
            let identity = match (&entry.identity, entry.state) {
                (Some(identity), GrantState::Approved) => identity.clone(),
                (_, GrantState::Pending) => {
                    return Err("the grant is waiting for the approving device".into());
                }
                _ => {
                    return Err(format!(
                        "the grant is {}",
                        entry.reason.as_deref().unwrap_or("not approved")
                    ));
                }
            };
            let scope = &identity.delegation.scope;
            let room = match room {
                Some(room) if scope.rooms.iter().any(|own| own == room) => room.to_owned(),
                Some(_) => return Err("the grant does not cover this room".into()),
                None => scope
                    .rooms
                    .first()
                    .cloned()
                    .ok_or("the grant names no room")?,
            };
            if state.agents.get(grant).is_some_and(|agent| !agent.ended()) {
                return Err(ControlError::InUse.to_string());
            }
            let control = AgentControl::new(identity.delegation.clone());
            control.report_to(&self.changes);
            control.set_room(&room);
            control.set_witness(
                self.link()
                    .and_then(|link| link.witness(&identity.delegation)),
            );
            state.agents.insert(grant.to_owned(), control.clone());
            drop(state);
            (identity, control)
        };
        let task_id = control.message(task).map_err(|error| error.to_string())?;
        (self.spawner)(identity, control.clone()).inspect_err(|error| {
            control.fail(error);
        })?;
        Ok((grant.to_owned(), task_id))
    }

    /// The control state of agent `id`.
    #[must_use]
    pub fn control(&self, id: &str) -> Option<Arc<AgentControl>> {
        self.state().agents.get(id).cloned()
    }

    /// Every agent and grant.
    #[must_use]
    pub fn status(&self) -> AgentsStatus {
        let state = self.state();
        AgentsStatus {
            version: 1,
            cancel_task: true,
            agents: state.agents.values().map(|agent| agent.info()).collect(),
            grants: state
                .grants
                .iter()
                .map(|grant| AgentGrant {
                    id: grant.id().to_owned(),
                    label: grant.proposal.label.clone(),
                    rooms: grant.proposal.scope.rooms.clone(),
                    apps: grant.proposal.scope.apps.clone(),
                    in_use: state
                        .agents
                        .get(grant.id())
                        .is_some_and(|agent| !agent.ended()),
                    state: grant.state,
                    color: color_hex(grant.proposal.color),
                    expires_at_ms: grant.proposal.expires_at_ns / 1_000_000,
                    pairing_code: grant.pairing_code.clone(),
                    reason: grant.reason.clone(),
                })
                .collect(),
        }
    }

    /// Every grant and agent, for the host to keep across restarts.
    #[must_use]
    pub fn snapshot(&self) -> SupervisorSnapshot {
        let state = self.state();
        SupervisorSnapshot {
            grants: state
                .grants
                .iter()
                .map(|grant| GrantRecord {
                    proposal: grant.proposal.clone(),
                    state: grant.state,
                    key: grant
                        .identity
                        .as_ref()
                        .map(|identity| hex::encode(identity.key.to_bytes())),
                    cert: grant
                        .identity
                        .as_ref()
                        .map(|identity| identity.cert.clone()),
                    delegation: grant
                        .identity
                        .as_ref()
                        .map(|identity| hex::encode(identity.delegation.encode())),
                    reason: grant.reason.clone(),
                })
                .collect(),
            agents: state.agents.values().map(|agent| agent.info()).collect(),
        }
    }

    /// Pick up a [`Self::snapshot`] after a restart. Approved grants are
    /// registered with the host again, unless they expired meanwhile; a
    /// grant still waiting for its approver lost its request and is denied.
    /// Returns the agents to start again: those that were neither stopped
    /// nor failed, under a grant still approved. The others are listed as
    /// they ended.
    pub fn restore(
        &self,
        snapshot: SupervisorSnapshot,
        now_ns: u64,
    ) -> Vec<(AgentIdentity, Arc<AgentControl>)> {
        let link = self.link();
        let mut grants = Vec::new();
        for record in snapshot.grants {
            let mut grant = Grant {
                proposal: record.proposal,
                state: record.state,
                key: None,
                identity: None,
                pairing_code: None,
                reason: record.reason,
            };
            match grant.state {
                GrantState::Pending => {
                    grant.settle(GrantState::Denied, "the host restarted before the approval");
                }
                GrantState::Approved => {
                    match restore_identity(record.key, record.cert, record.delegation) {
                        Some(identity) if identity.delegation.expires_at_ns <= now_ns => {
                            if let Some(link) = &link {
                                link.withdraw(identity.delegation.device_public_key);
                            }
                            grant.settle(GrantState::Expired, "the grant expired");
                        }
                        Some(identity) => {
                            match link.as_ref().map_or(Ok(()), |link| {
                                link.register(&identity.cert, &identity.delegation)
                            }) {
                                Ok(()) => grant.identity = Some(identity),
                                Err(reason) => grant.settle(GrantState::Denied, &reason),
                            }
                        }
                        None => {
                            grant.settle(GrantState::Denied, "its identity could not be read back");
                        }
                    }
                }
                GrantState::Denied | GrantState::Expired | GrantState::Revoked => {}
            }
            grants.push(grant);
        }
        let mut agents = BTreeMap::new();
        let mut resume = Vec::new();
        for info in snapshot.agents {
            let identity = grants
                .iter()
                .find(|grant| grant.id() == info.id)
                .and_then(|grant| grant.identity.clone());
            let delegation = match (&identity, hex::decode(&info.delegation)) {
                (Some(identity), _) => identity.delegation.clone(),
                (None, Ok(bytes)) => match Delegation::decode(&bytes) {
                    Ok(delegation) => delegation,
                    Err(_) => continue,
                },
                (None, Err(_)) => continue,
            };
            let control = AgentControl::restore(delegation, &info);
            control.set_witness(
                self.link()
                    .and_then(|link| link.witness(control.delegation())),
            );
            control.report_to(&self.changes);
            match identity {
                Some(identity) if !control.ended() => resume.push((identity, control.clone())),
                Some(_) => {}
                None => control.stop(),
            }
            agents.insert(info.id.clone(), control);
        }
        let mut state = self.state();
        state.grants = grants;
        state.agents = agents;
        drop(state);
        self.changes.raise();
        resume
    }

    /// Whether `caller` may steer agents here: a device of the delegating
    /// user, never an agent.
    fn authorized(&self, caller: &Caller) -> bool {
        matches!(caller.principal(), CallerPrincipal::Person(_))
            && caller.user_public_key() == Some(&self.user_public_key)
    }

    fn run(&self, request: &InvokeRequest) -> Result<ActionOutput, ActionHostError> {
        let address = &request.target.address;
        let value = if address.resource == SUPERVISOR_RESOURCE {
            match address.action.as_str() {
                STATUS_ACTION => serde_json::to_value(self.status())
                    .map_err(|error| refused(error.to_string()))?,
                SPAWN_ACTION => {
                    let InvokeArgument::Json(arguments) = &request.argument else {
                        return Err(invalid("spawn takes {grant, task}"));
                    };
                    let text = |key: &str| {
                        arguments
                            .get(key)
                            .and_then(serde_json::Value::as_str)
                            .filter(|value| !value.is_empty())
                            .ok_or_else(|| invalid(&format!("spawn needs {key}")))
                    };
                    let room = arguments
                        .get("room")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty());
                    let (agent, task) = self
                        .spawn(text("grant")?, text("task")?, room)
                        .map_err(refused)?;
                    serde_json::json!({"agentId": agent, "taskId": task})
                }
                REQUEST_GRANT_ACTION => {
                    let InvokeArgument::Json(arguments) = &request.argument else {
                        return Err(invalid(
                            "request-grant takes {label, rooms, apps, expiresInSecs}",
                        ));
                    };
                    let grant: GrantRequest =
                        serde_json::from_value(serde_json::Value::Object(arguments.clone()))
                            .map_err(|error| invalid(&error.to_string()))?;
                    let (id, pairing_code) = self
                        .request_grant(&grant, tilo_clock::unix_now_ns())
                        .map_err(refused)?;
                    serde_json::json!({"grant": id, "pairingCode": pairing_code})
                }
                _ => return Err(ActionHostError::UnknownAction),
            }
        } else {
            let id = address
                .resource
                .strip_prefix(tilo_relay_protocol::agents::AGENT_RESOURCE_PREFIX)
                .ok_or(ActionHostError::UnknownAction)?;
            if address.action == REVOKE_ACTION {
                self.revoke(id)
                    .map_err(|error| refused(error.to_string()))?;
                return Ok(ActionOutput {
                    value: Some(serde_json::json!({})),
                    resources: Vec::new(),
                });
            }
            let control = self.control(id).ok_or(ActionHostError::UnknownAction)?;
            match address.action.as_str() {
                MESSAGE_ACTION => {
                    let InvokeArgument::Text(text) = &request.argument else {
                        return Err(invalid("message takes text"));
                    };
                    let task = control
                        .message(text)
                        .map_err(|error| refused(error.to_string()))?;
                    serde_json::json!({"taskId": task})
                }
                PAUSE_ACTION => {
                    control
                        .pause()
                        .map_err(|error| refused(error.to_string()))?;
                    serde_json::json!({})
                }
                RESUME_ACTION => {
                    control
                        .resume()
                        .map_err(|error| refused(error.to_string()))?;
                    serde_json::json!({})
                }
                STOP_ACTION => {
                    control.stop();
                    serde_json::json!({})
                }
                CANCEL_TASK_ACTION => {
                    let InvokeArgument::Text(task) = &request.argument else {
                        return Err(invalid("cancel-task takes the task id"));
                    };
                    control
                        .cancel_task(task)
                        .map_err(|error| refused(error.to_string()))?;
                    serde_json::json!({})
                }
                _ => return Err(ActionHostError::UnknownAction),
            }
        };
        Ok(ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
    }

    fn rows(&self) -> Vec<CatalogueRow> {
        let supervisor = |action: &str, label: &str, kind: OperationKind, input: InputSpec| {
            row(
                SUPERVISOR_RESOURCE,
                SUPERVISOR_KIND,
                "Agents",
                action,
                label,
                kind,
                input,
            )
        };
        let mut rows = vec![
            supervisor(
                STATUS_ACTION,
                "Agent status",
                OperationKind::View,
                InputSpec::None,
            ),
            supervisor(
                SPAWN_ACTION,
                "Start an agent",
                OperationKind::Effect,
                InputSpec::None,
            ),
            supervisor(
                REQUEST_GRANT_ACTION,
                "Ask for an agent grant",
                OperationKind::Effect,
                InputSpec::None,
            ),
        ];
        let grants: Vec<(String, String)> = self
            .state()
            .grants
            .iter()
            .filter(|grant| matches!(grant.state, GrantState::Pending | GrantState::Approved))
            .map(|grant| (grant.id().to_owned(), grant.proposal.label.clone()))
            .collect();
        for (id, label) in grants {
            rows.push(row(
                &agent_resource(&id),
                AGENT_KIND,
                &label,
                REVOKE_ACTION,
                "Revoke",
                OperationKind::Effect,
                InputSpec::None,
            ));
        }
        for agent in self.state().agents.values() {
            let resource = agent_resource(agent_id(&agent.delegation));
            let label = agent.delegation.label.clone();
            let effect = |action: &str, action_label: &str, input: InputSpec| {
                row(
                    &resource,
                    AGENT_KIND,
                    &label,
                    action,
                    action_label,
                    OperationKind::Effect,
                    input,
                )
            };
            rows.push(effect(
                MESSAGE_ACTION,
                "Message",
                InputSpec::Text {
                    prompt: "Task".into(),
                    placeholder: "What should the agent do?".into(),
                    collects: Collects::Words,
                },
            ));
            rows.push(effect(PAUSE_ACTION, "Pause", InputSpec::None));
            rows.push(effect(RESUME_ACTION, "Resume", InputSpec::None));
            rows.push(effect(STOP_ACTION, "Stop", InputSpec::None));
            rows.push(effect(
                CANCEL_TASK_ACTION,
                "Cancel task",
                InputSpec::Text {
                    prompt: "Task".into(),
                    placeholder: "Task id".into(),
                    collects: Collects::Words,
                },
            ));
        }
        rows
    }
}

fn row(
    resource: &str,
    kind: &str,
    resource_label: &str,
    action: &str,
    action_label: &str,
    operation: OperationKind,
    input: InputSpec,
) -> CatalogueRow {
    CatalogueRow {
        provider: tilo_relay_protocol::agents::AGENTS_PROVIDER.into(),
        app_id: tilo_relay_protocol::agents::AGENTS_PROVIDER.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.into(),
            kind: kind.into(),
            label: resource_label.into(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: kind.into(),
            resource_label: resource_label.into(),
            action_label: action_label.into(),
            description: None,
            locus: ExecutionLocus::ServerPreferred,
            kind: operation,
            contract: ActionContract::default(),
            input,
            params: Vec::new(),
            view: None,
        }),
        has_children: false,
        visibility: None,
    }
}

fn invalid(reason: &str) -> ActionHostError {
    ActionHostError::InvalidRequest {
        reason: reason.into(),
    }
}

fn refused(reason: String) -> ActionHostError {
    ActionHostError::Refused { reason }
}

/// [`ActionHost`] over a shared [`AgentSupervisor`]; wrap it in
/// `tilo_relay::ActionHostPlane` to answer browsers through the host.
#[derive(Clone)]
pub struct SupervisorHost(pub Arc<AgentSupervisor>);

impl ActionHost for SupervisorHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        if !self.0.authorized(caller) {
            return Pending::ready(Ok(CataloguePage::from_rows("0".into(), Vec::new(), &query)));
        }
        Pending::ready(Ok(CataloguePage::from_rows(
            "1".into(),
            self.0.rows(),
            &query,
        )))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        if !self.0.authorized(caller) {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        let invocation = request.invocation;
        let supervisor = &self.0;
        let receipt = supervisor
            .ledger()
            .admit(caller, &request, || {
                let output = supervisor.run(&request)?;
                Ok(InvocationState::new(
                    invocation,
                    InvocationUpdate::Succeeded { invocation, output },
                ))
            })
            .map(|(state, _)| state.receipt());
        Pending::ready(receipt)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0.ledger().get(caller, id).cloned()
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
        if self.0.ledger().get(caller, id).is_none() {
            return Err(ActionHostError::UnknownInvocation);
        }
        Err(ActionHostError::NotCancellable {
            reason: "agent control completes at once".into(),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tilo_actions::remote::{PendingPoll, ReceiptStatus, RemoteActionTarget};

    const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

    fn identity(root: &SigningKey) -> AgentIdentity {
        identity_until(root, u64::MAX)
    }

    /// The `writer` agent of `root`, delegated until `expires_at_ns`.
    pub(crate) fn identity_until(root: &SigningKey, expires_at_ns: u64) -> AgentIdentity {
        let key = SigningKey::from_bytes(&[9; 32]);
        let device = key.verifying_key().to_bytes();
        let delegation = Delegation::sign(
            root,
            device,
            tilo_relay_protocol::delegation::Delegate::Agent {
                agent_id: "writer".into(),
            },
            "Writer helper",
            [0x7c, 0x3a, 0xed],
            DelegateScope {
                rooms: vec![ROOM.into()],
                apps: Vec::new(),
            },
            1,
            expires_at_ns,
        )
        .unwrap();
        let cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: "Agent: Writer helper".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        AgentIdentity {
            key,
            cert,
            delegation,
        }
    }

    /// A supervisor with one approved grant, `writer`, whose spawns count.
    fn supervisor() -> (SupervisorHost, Caller, Arc<AtomicUsize>) {
        let root = SigningKey::from_bytes(&[1; 32]);
        let spawned = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&spawned);
        let supervisor = AgentSupervisor::new(
            root.verifying_key().to_bytes(),
            Arc::new(move |_, _| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        );
        supervisor.add_grant(identity(&root));
        let caller = Caller::authenticated(root.verifying_key().to_bytes(), [2; 32]);
        (SupervisorHost(supervisor), caller, spawned)
    }

    fn request(
        invocation: InvocationId,
        resource: &str,
        action: &str,
        argument: InvokeArgument,
    ) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: tilo_relay_protocol::agents::AGENTS_PROVIDER.into(),
                address: ActionAddress::new(resource, action),
            },
            argument,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        }
    }

    fn invoke(
        host: &SupervisorHost,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Result<serde_json::Value, ActionHostError> {
        let receipt = match host.invoke(caller, request).poll_ready() {
            PendingPoll::Ready(receipt) => Some(receipt),
            _ => None,
        }
        .expect("agent control answers at once")?;
        let output = match receipt.status {
            ReceiptStatus::Completed { output } => Some(output.value.unwrap_or_default()),
            _ => None,
        }
        .expect("agent control returns a completed receipt");
        Ok(output)
    }

    fn spawn_argument(task: &str) -> InvokeArgument {
        InvokeArgument::Json(serde_json::Map::from_iter([
            ("grant".into(), serde_json::Value::String("writer".into())),
            ("task".into(), serde_json::Value::String(task.into())),
        ]))
    }

    #[test]
    fn one_task_is_cancelled_and_the_agent_goes_on_with_the_next() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let control = AgentControl::new(identity(&root).delegation);
        control.set_status(AgentStatus::Waiting);
        let first = control.message("tidy the notes").unwrap();
        let second = control.message("sum the sheet").unwrap();
        let third = control.message("draft a reply").unwrap();
        assert_eq!(control.begin_next().unwrap().0, first);
        control.set_activity(Some("Tidying".into()));

        control.cancel_task(&second).unwrap();
        assert!(
            control.is_running(&first),
            "a queued task leaves the running one"
        );
        control.cancel_task(&first).unwrap();
        assert!(!control.is_running(&first));
        assert_eq!(control.status(), AgentStatus::Waiting);
        assert_eq!(control.activity(), None);
        assert_eq!(
            control.cancel_task(&first),
            Ok(()),
            "cancelling again changes nothing"
        );
        assert_eq!(
            control.begin_next().unwrap().0,
            third,
            "the cancelled one is skipped"
        );
        assert!(control.may_act(), "the agent is not stopped");

        control.finish(&third, Ok("Done.".into()));
        assert_eq!(control.cancel_task(&third), Err(ControlError::Finished));
        assert_eq!(control.cancel_task("t99"), Err(ControlError::UnknownTask));
        let states: Vec<TaskState> = control.info().tasks.iter().map(|t| t.state).collect();
        assert_eq!(
            states,
            [
                TaskState::Cancelled,
                TaskState::Cancelled,
                TaskState::Completed
            ]
        );
    }

    #[test]
    fn a_delayed_cancel_cannot_address_a_replacement_participant() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let previous = AgentControl::new(identity(&root).delegation);
        let old_task = previous.message("old task").unwrap();
        previous.stop();
        let replacement = AgentControl::new(identity(&root).delegation);
        let new_task = replacement.message("new task").unwrap();
        assert_ne!(old_task, new_task);
        assert_eq!(
            replacement.cancel_task(&old_task),
            Err(ControlError::UnknownTask)
        );
        assert_eq!(replacement.info().tasks[0].state, TaskState::Queued);
        let restored = AgentControl::restore(identity(&root).delegation, &replacement.info());
        let next_task = restored.message("after restore").unwrap();
        assert_ne!(next_task, new_task);
        assert_ne!(next_task, old_task);
    }

    #[test]
    fn a_resent_spawn_or_message_is_answered_once() {
        let (host, caller, spawned) = supervisor();
        let spawn_id = InvocationId::new();
        let spawn = || {
            request(
                spawn_id,
                SUPERVISOR_RESOURCE,
                SPAWN_ACTION,
                spawn_argument("tidy the notes"),
            )
        };
        let first = invoke(&host, &caller, spawn()).unwrap();
        assert_eq!(first["agentId"], "writer");
        assert!(!first["taskId"].as_str().unwrap().is_empty());
        assert_eq!(invoke(&host, &caller, spawn()).unwrap(), first);
        assert_eq!(spawned.load(Ordering::SeqCst), 1, "one agent");

        let agent = agent_resource("writer");
        let message_id = InvocationId::new();
        let message = |text: &str| {
            request(
                message_id,
                &agent,
                MESSAGE_ACTION,
                InvokeArgument::Text(text.into()),
            )
        };
        let task = invoke(&host, &caller, message("sum the sheet")).unwrap();
        let task_id = task["taskId"].as_str().unwrap();
        assert_ne!(task_id, first["taskId"].as_str().unwrap());
        assert_eq!(
            invoke(&host, &caller, message("sum the sheet")).unwrap(),
            task
        );
        assert_eq!(
            invoke(&host, &caller, message("something else")),
            Err(ActionHostError::InvocationConflict),
            "an id names one request"
        );
        let other = Caller::authenticated(
            SigningKey::from_bytes(&[1; 32]).verifying_key().to_bytes(),
            [3; 32],
        );
        assert_eq!(
            invoke(&host, &other, message("sum the sheet")),
            Err(ActionHostError::InvocationConflict),
            "and one caller"
        );
        let control = host.0.control("writer").unwrap();
        assert_eq!(control.info().tasks.len(), 2, "one task per message");

        invoke(
            &host,
            &caller,
            request(
                InvocationId::new(),
                &agent,
                CANCEL_TASK_ACTION,
                InvokeArgument::Text(task_id.into()),
            ),
        )
        .unwrap();
        assert_eq!(control.info().tasks[1].state, TaskState::Cancelled);
        assert_ne!(control.status(), AgentStatus::Stopped);
        let refused = invoke(
            &host,
            &caller,
            request(
                InvocationId::new(),
                &agent,
                CANCEL_TASK_ACTION,
                InvokeArgument::Text("t9".into()),
            ),
        );
        assert!(
            matches!(refused, Err(ActionHostError::Refused { ref reason }) if reason == "the agent has no such task"),
            "{refused:?}"
        );
    }
}
