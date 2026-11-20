//! Steering agents from any member: spawn, message, pause, resume,
//! stop and status, as `tilo_actions` remote actions the host answers.
//!
//! [`AgentControl`] is one agent's shared state: its task queue, its
//! conversation, and the pause and stop switches its member obeys
//! before every step. [`AgentSupervisor`] holds every approved delegation
//! (Cloud's grant offers) and every running agent, and answers the
//! vocabulary in `tilo_server_protocol::agents` through [`ActionProvider`], so a
//! browser steers agents over the same remote-action frames it uses for
//! apps. Only devices of the delegating user may steer; an agent may not.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_actions::api::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationLog, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest,
    Pending,
};
use tilo_actions::{
    ActionContract, ActionOutput, Collects, ExecutionLocus, InvocationId, InvocationUpdate,
    OperationKind,
};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::agents::{
    AGENT_KIND, AgentGrant, AgentInfo, AgentMessage, AgentStatus, AgentStep, AgentTask,
    AgentsStatus, CANCEL_TASK_ACTION, GrantRequest, GrantState, MESSAGE_ACTION, MessageRole,
    PAUSE_ACTION, REQUEST_GRANT_ACTION, RESUME_ACTION, REVOKE_ACTION, SPAWN_ACTION, STATUS_ACTION,
    STOP_ACTION, SUPERVISOR_KIND, SUPERVISOR_RESOURCE, TaskState, agent_resource, color_hex,
    parse_color_hex,
};
use tilo_server_protocol::delegation::{
    Delegate, DelegateProposal, DelegateScope, Delegation, MAX_DELEGATE_TEXT_BYTES,
    MAX_DELEGATED_APPS, MAX_DELEGATED_WORKSPACES,
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

/// An agent's certified device: key, capability-less certificate and
/// root-signed delegation.
#[derive(Clone)]
pub struct AgentIdentity {
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

/// The person `delegation`'s agent acts for.
#[must_use]
pub fn person(delegation: &Delegation) -> tilo_actions::MemberId {
    tilo_actions::MemberId::person(ui::permissions::policy::PersonId::from_root(
        delegation.user_public_key,
    ))
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
    workspace: Option<String>,
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

/// One agent's steering state, shared by its member and the
/// supervisor.
pub struct AgentControl {
    delegation: Delegation,
    state: Mutex<ControlState>,
    changed: Changed,
    /// The supervisor's signal that its snapshot changed, once the agent
    /// is one of its agents.
    reports_to: OnceLock<Arc<SnapshotChanges>>,
}

impl AgentControl {
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
                workspace: None,
            }),
            changed: Changed::default(),
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
                workspace: info.workspace.clone(),
            }),
            changed: Changed::default(),
            reports_to: OnceLock::new(),
        })
    }

    #[must_use]
    pub fn workspace(&self) -> Option<String> {
        self.state().workspace.clone()
    }

    /// Name the workspace the agent works in.
    pub fn set_workspace(&self, workspace: &str) {
        self.state().workspace = Some(workspace.to_owned());
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
    /// activity or workspace.
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
        // A grant may spawn a replacement member. Task addresses must
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
    /// nothing. Running tasks stop at the member's next task-state
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

    /// Whether the member may take its next step now.
    #[must_use]
    pub fn may_act(&self) -> bool {
        let state = self.state();
        !state.paused && state.status != AgentStatus::Stopped
    }

    /// Whether the agent is paused while task `id` runs.
    pub(crate) fn paused_on(&self, id: &str) -> bool {
        let state = self.state();
        state.paused
            && state
                .tasks
                .iter()
                .any(|task| task.id == id && task.state == TaskState::Running)
    }

    /// Block while the agent is paused and task `id` still runs: a turn under
    /// way takes no step until its person resumes the agent, stops it or
    /// cancels the task. Every one of those wakes the agent, and this wait
    /// with it; the signal stays raised for the agent's loop.
    pub(crate) fn hold_while_paused(&self, id: &str) {
        let raised = self.changed.raised.lock_recover();
        drop(
            self.changed
                .condvar
                .wait_while(raised, |_| self.paused_on(id))
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    /// Whether task `id` is still wanted.
    #[must_use]
    pub fn is_running(&self, id: &str) -> bool {
        self.state()
            .tasks
            .iter()
            .any(|task| task.id == id && task.state == TaskState::Running)
    }

    /// The member joined (`Waiting`) or its session failed.
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
            workspace: state.workspace.clone(),
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

/// The sessions of the agents working beside a device, by the device each
/// signs as: a change an action made for one of them is that agent's, signed
/// with its own key.
#[derive(Clone, Default)]
pub struct AgentSessions {
    links: Arc<Mutex<BTreeMap<[u8; 32], AgentSession>>>,
}

struct AgentSession {
    link: tilo_session_client::Link,
    roster: Option<(RosterCreatedAt, Delegation)>,
}

impl AgentSessions {
    /// The session of agent `device`, while it is open here.
    #[must_use]
    pub fn link(&self, device: &[u8; 32]) -> Option<tilo_session_client::Link> {
        self.links
            .lock_recover()
            .get(device)
            .map(|session| session.link.clone())
    }

    /// Whether this peer was opened solely to sign for a hosted roster entry.
    #[must_use]
    pub fn is_roster_signer(&self, device: &[u8; 32]) -> bool {
        self.links
            .lock_recover()
            .get(device)
            .is_some_and(|session| session.roster.is_some())
    }

    /// Whether this signing peer was opened for the exact roster assignment.
    #[must_use]
    pub fn matches_roster(
        &self,
        device: &[u8; 32],
        workspace: WorkspaceId,
        agent: &str,
        keeper: &[u8; 32],
        created_at: RosterCreatedAt,
    ) -> bool {
        self.links
            .lock_recover()
            .get(device)
            .and_then(|session| session.roster.as_ref())
            .is_some_and(|(created, delegation)| {
                *created == created_at
                    && delegation.scope.workspaces.as_slice() == [workspace]
                    && delegation.delegate.agent_id() == Some(agent)
                    && &delegation.user_public_key == keeper
            })
    }

    /// The live, joined signing session and its exact approved delegation for
    /// a caller bound by the hosted roster runner.
    #[must_use]
    pub fn live_roster(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
    ) -> Option<(tilo_session_client::Link, Delegation)> {
        let device = caller.device_public_key()?;
        let sessions = self.links.lock_recover();
        let session = sessions.get(device)?;
        let (created_at, delegation) = session.roster.as_ref()?;
        if caller.roster_created_at() != Some(*created_at)
            || caller.workspace().and_then(WorkspaceId::parse) != Some(workspace)
            || delegation.scope.workspaces.as_slice() != [workspace]
            || delegation.delegate.agent_id() != caller.acting().agent_id()
            || caller.acting().app().is_some()
            || caller
                .acting_for_person()
                .and_then(|person| tilo_actions::PersonId::parse(person).ok())
                .and_then(|person| person.root())
                != Some(delegation.user_public_key)
            || Some(&delegation.user_public_key) != caller.user_public_key()
            || &delegation.device_public_key != device
            || tilo_clock::unix_now_ns() >= delegation.expires_at_ns
        {
            return None;
        }
        let status = session.link.status();
        (status.authenticated
            && status.phase == tilo_session_client::TransportPhase::Connected
            && &status.device == device
            && status.workspaces.get(&workspace) == Some(&true)
            && status.serving.contains(&workspace))
        .then(|| (session.link.clone(), delegation.clone()))
    }

    /// `link` now serves agent `device`'s changes.
    pub(crate) fn joined(
        &self,
        device: [u8; 32],
        link: tilo_session_client::Link,
        roster: Option<(RosterCreatedAt, Delegation)>,
    ) {
        self.links
            .lock_recover()
            .insert(device, AgentSession { link, roster });
    }

    /// Agent `device`'s session ended.
    pub(crate) fn left(&self, device: &[u8; 32]) {
        self.links.lock_recover().remove(device);
    }
}

/// Starts an agent's member for an identity and its control state:
/// on its own thread in a host, on the test's thread in a test.
pub type Spawner =
    Arc<dyn Fn(AgentIdentity, Arc<AgentControl>) -> Result<(), String> + Send + Sync>;

/// The incarnation of an existing hosted roster entry. The signed proposal supplies
/// its id, keeper and single workspace.
pub type RosterCreatedAt = i64;

pub(crate) type RosterSpawner =
    Arc<dyn Fn(AgentIdentity, RosterCreatedAt) -> Result<(), String> + Send + Sync>;

/// What a host gives its supervisor: the approver for new grants, and the
/// server's register of agent devices.
pub trait AgentHostLink: Send + Sync {
    /// Ask the user's approving device to certify `key` as the agent
    /// `proposal` describes, in one consent step. Returns the pairing code
    /// the approver shows; `done` later receives the agent's certificate
    /// and root-signed delegation, or why there are none.
    fn request_approval(
        &self,
        key: &SigningKey,
        proposal: &DelegateProposal,
        done: ApprovalDone,
    ) -> Result<String, String>;

    /// Request the same approval for an existing hosted roster entry.
    fn request_roster_approval(
        &self,
        key: &SigningKey,
        proposal: &DelegateProposal,
        created_at: RosterCreatedAt,
        requester: &DeviceCert,
        done: ApprovalDone,
    ) -> Result<String, String> {
        let _ = (key, proposal, created_at, requester, done);
        Err("this supervisor does not provision roster agents".into())
    }

    /// Recheck the roster entry before admitting its passive signing session.
    fn admit_roster_session(
        &self,
        identity: &AgentIdentity,
        created_at: RosterCreatedAt,
        lifetime: tokio::sync::oneshot::Receiver<()>,
        done: RosterAdmissionDone,
    ) {
        let _ = (identity, created_at, lifetime);
        done(Err(
            "this supervisor does not provision roster agents".into()
        ));
    }

    /// Admit the agent device `cert` of `delegation` to its workspaces.
    fn register(&self, cert: &DeviceCert, delegation: &Delegation) -> Result<(), String>;

    /// Call `done` once the workspaces `delegation` names hold the agent
    /// `cert` as a member of their own, so its session is admitted when it
    /// joins. A host that admits its agents at once calls it at once.
    fn when_admitted(&self, cert: &DeviceCert, delegation: &Delegation, done: AdmissionDone) {
        let _ = (cert, delegation);
        done();
    }

    /// Refuse the agent device from now on.
    fn withdraw(&self, device_public_key: [u8; 32]);
}

/// Receives the outcome of admitting a roster agent's signing session.
pub type RosterAdmissionDone = Box<dyn FnOnce(Result<(), String>) + Send>;

/// Receives the outcome of [`AgentHostLink::request_approval`].
pub type ApprovalDone = Box<dyn FnOnce(Result<(DeviceCert, Delegation), String>) + Send>;

/// Called once the agent's workspaces admit it ([`AgentHostLink::when_admitted`]).
pub type AdmissionDone = Box<dyn FnOnce() + Send>;

/// One grant: what was asked, where it stands, and once approved the
/// agent identity it runs under.
#[derive(Clone)]
struct Grant {
    proposal: DelegateProposal,
    roster_created_at: Option<RosterCreatedAt>,
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
            roster_created_at: None,
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

    fn is_roster_in(&self, workspace: WorkspaceId) -> bool {
        self.roster_created_at.is_some() && self.proposal.scope.workspaces.as_slice() == [workspace]
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
    /// Which existing roster entry this grant provisions, where it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_created_at: Option<RosterCreatedAt>,
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
    roster_spawner: OnceLock<RosterSpawner>,
    link: Mutex<Option<Arc<dyn AgentHostLink>>>,
    state: Mutex<SupervisorState>,
    log: Mutex<InvocationLog<InvocationState>>,
    /// Raised whenever [`Self::snapshot`] would return something new.
    changes: Arc<SnapshotChanges>,
    me: Weak<Self>,
}

impl AgentSupervisor {
    #[must_use]
    pub fn new(user_public_key: [u8; 32], spawner: Spawner) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            user_public_key,
            spawner,
            roster_spawner: OnceLock::new(),
            link: Mutex::new(None),
            state: Mutex::new(SupervisorState {
                grants: Vec::new(),
                agents: BTreeMap::new(),
            }),
            log: Mutex::new(InvocationLog::default()),
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

    fn log(&self) -> MutexGuard<'_, InvocationLog<InvocationState>> {
        self.log.lock_recover()
    }

    fn link(&self) -> Option<Arc<dyn AgentHostLink>> {
        self.link.lock_recover().clone()
    }

    /// Connect the host: grants can then be requested, and approved ones
    /// are registered and withdrawn with it.
    pub fn set_link(&self, link: Arc<dyn AgentHostLink>) {
        *self.link.lock_recover() = Some(link);
    }

    pub(crate) fn set_roster_spawner(&self, spawner: RosterSpawner) {
        let _ = self.roster_spawner.set(spawner);
    }

    /// Ask for the certified signing session of a verified existing roster entry.
    pub fn invoke_roster_grant(
        &self,
        caller: &Caller,
        invocation: &InvokeRequest,
        request: &GrantRequest,
        agent: &str,
        created_at: RosterCreatedAt,
        requester: &DeviceCert,
    ) -> Result<InvocationReceipt, ActionCallError> {
        tilo_actions::appearance::refuse_here(invocation)?;
        if !self.authorized(caller)
            || caller.device_public_key() != Some(&requester.device_public_key)
        {
            return Err(ActionCallError::UnknownAction);
        }
        self.invoke_once(caller, invocation, || {
            let (id, pairing_code) = self
                .request_grant_for(
                    request,
                    tilo_clock::unix_now_ns(),
                    Some((agent, created_at, requester)),
                )
                .map_err(refused)?;
            Ok(grant_output(&id, &pairing_code))
        })
    }

    fn invoke_once(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        dispatch: impl FnOnce() -> Result<ActionOutput, ActionCallError>,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let invocation = request.invocation;
        self.log()
            .admit(caller, request, || {
                let output = dispatch()?;
                Ok(InvocationState::new(
                    invocation,
                    InvocationUpdate::Succeeded { invocation, output },
                ))
            })
            .map(|(state, _)| state.receipt())
    }

    /// Offer an approved agent identity for spawning. The host has already
    /// registered its delegation.
    pub fn add_grant(&self, identity: AgentIdentity) {
        let mut state = self.state();
        state.grants.retain(|grant| {
            grant.roster_created_at.is_some() || grant.id() != agent_id(&identity.delegation)
        });
        state.grants.push(Grant::approved(identity));
        drop(state);
        self.changes.raise();
    }

    /// Ask the user's approver for a new grant as `request` describes.
    /// Returns the grant id and the pairing code the approver shows.
    pub fn request_grant(
        &self,
        request: &GrantRequest,
        now_ns: u64,
    ) -> Result<(String, String), String> {
        self.request_grant_for(request, now_ns, None)
    }

    fn request_grant_for(
        &self,
        request: &GrantRequest,
        now_ns: u64,
        roster: Option<(&str, RosterCreatedAt, &DeviceCert)>,
    ) -> Result<(String, String), String> {
        let link = self.link().ok_or("this host cannot ask for agent grants")?;
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|error| error.to_string())?;
        let key = SigningKey::from_bytes(&secret);
        let seed = key.verifying_key().to_bytes();
        let proposal = grant_proposal(request, now_ns, &seed, roster.map(|(agent, _, _)| agent))?;
        let id = proposal.delegate.agent_id().unwrap_or_default().to_owned();
        let roster_created_at = roster.map(|(_, created_at, _)| created_at);
        let mut state = self.state();
        if state
            .grants
            .iter()
            .any(|grant| grant.id() == id && grant.roster_created_at.is_some() != roster.is_some())
        {
            return Err("that agent id already belongs to another grant".into());
        }
        let prior = state.grants.iter().position(|grant| {
            grant.id() == id
                && match roster {
                    None => grant.roster_created_at.is_none(),
                    Some(_) => {
                        grant.roster_created_at.is_some()
                            && grant.proposal.scope.workspaces == proposal.scope.workspaces
                    }
                }
        });
        if let Some(grant) = prior.and_then(|index| state.grants.get(index)) {
            if roster.is_none() || grant.roster_created_at != roster_created_at {
                return Err("that agent id already belongs to another grant".into());
            }
            match grant.state {
                GrantState::Pending
                    if grant.proposal.label == proposal.label
                        && grant.proposal.color == proposal.color
                        && grant.proposal.scope == proposal.scope =>
                {
                    return Ok((id, grant.pairing_code.clone().unwrap_or_default()));
                }
                GrantState::Approved if grant.proposal.scope == proposal.scope => {
                    return Ok((id, String::new()));
                }
                GrantState::Denied | GrantState::Expired => {}
                _ => return Err("that agent already has a different or withdrawn grant".into()),
            }
        }
        let me = self.me.clone();
        let answered = id.clone();
        let done: ApprovalDone = Box::new(move |outcome| {
            if let Some(supervisor) = me.upgrade() {
                supervisor.approval_answered(&answered, seed, outcome);
            }
        });
        // Hold the state across enqueue so a fast answer sees its pending grant,
        // and simultaneous requests for this roster id share only one proposal.
        let pairing_code = match roster {
            Some((_, created_at, requester)) => {
                link.request_roster_approval(&key, &proposal, created_at, requester, done)?
            }
            None => link.request_approval(&key, &proposal, done)?,
        };
        if let Some(index) = prior {
            state.grants.remove(index);
        }
        state.grants.push(Grant {
            proposal,
            roster_created_at,
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

    fn approval_answered(
        &self,
        id: &str,
        device: [u8; 32],
        outcome: Result<(DeviceCert, Delegation), String>,
    ) {
        let link = self.link();
        let mut state = self.state();
        let Some(grant) = state.grants.iter_mut().find(|grant| {
            grant.id() == id
                && grant.state == GrantState::Pending
                && grant
                    .key
                    .as_ref()
                    .is_some_and(|key| key.verifying_key().to_bytes() == device)
        }) else {
            return;
        };
        let Some(key) = grant.key.take() else {
            return;
        };
        let approved = outcome.and_then(|(cert, delegation)| {
            if cert.device_public_key != device
                || delegation.device_public_key != device
                || delegation.user_public_key != self.user_public_key
            {
                return Err("the approval names a different identity".into());
            }
            grant.proposal.admits(&delegation).map_err(str::to_owned)?;
            if let Some(created_at) = grant.roster_created_at {
                if delegation.scope.workspaces != grant.proposal.scope.workspaces {
                    return Err("the approval does not cover the roster workspace".into());
                }
                let spawn = self
                    .roster_spawner
                    .get()
                    .ok_or("this supervisor cannot join roster sessions")?;
                let identity = AgentIdentity {
                    key,
                    cert,
                    delegation,
                };
                spawn(identity.clone(), created_at)?;
                Ok(identity)
            } else {
                if let Some(link) = &link {
                    link.register(&cert, &delegation)?;
                }
                Ok(AgentIdentity {
                    key,
                    cert,
                    delegation,
                })
            }
        });
        match approved {
            Ok(identity) => {
                grant.proposal.scope = identity.delegation.scope.clone();
                grant.proposal.expires_at_ns = identity.delegation.expires_at_ns;
                grant.state = GrantState::Approved;
                grant.pairing_code = None;
                grant.identity = Some(identity);
            }
            Err(reason) => grant.settle(GrantState::Denied, &reason),
        }
        drop(state);
        self.changes.raise();
    }

    pub(crate) fn roster_session_failed(&self, device: [u8; 32], reason: &str) {
        let mut state = self.state();
        let Some(grant) = state.grants.iter_mut().find(|grant| {
            grant.roster_created_at.is_some()
                && grant
                    .identity
                    .as_ref()
                    .is_some_and(|identity| identity.cert.device_public_key == device)
        }) else {
            return;
        };
        if let Some(link) = self.link() {
            link.withdraw(device);
        }
        grant.settle(GrantState::Denied, reason);
        drop(state);
        self.changes.raise();
    }

    /// Withdraw grant `id`: its agent stops, the host refuses its device
    /// and the agent's key is forgotten.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Unknown`] for an unknown grant.
    pub fn revoke(&self, id: &str) -> Result<(), ControlError> {
        self.revoke_in(id, None)
    }

    /// Whether this id has a signing grant in this exact workspace.
    #[must_use]
    pub fn has_roster_grant(&self, workspace: WorkspaceId, id: &str) -> bool {
        self.state()
            .grants
            .iter()
            .any(|grant| grant.id() == id && grant.is_roster_in(workspace))
    }

    /// Withdraw the signing grant in the workspace of this certified request.
    pub fn invoke_roster_revoke(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        workspace: WorkspaceId,
        id: &str,
    ) -> Result<InvocationReceipt, ActionCallError> {
        tilo_actions::appearance::refuse_here(request)?;
        if !self.authorized(caller) {
            return Err(ActionCallError::UnknownAction);
        }
        self.invoke_once(
            &caller.clone().in_workspace(workspace.to_string()),
            request,
            || {
                self.revoke_in(id, Some(workspace))
                    .map_err(|error| refused(error.to_string()))?;
                Ok(ActionOutput {
                    value: Some(serde_json::json!({})),
                    resources: Vec::new(),
                })
            },
        )
    }

    fn revoke_in(&self, id: &str, workspace: Option<WorkspaceId>) -> Result<(), ControlError> {
        let mut state = self.state();
        let grant = state
            .grants
            .iter_mut()
            .find(|grant| {
                grant.id() == id
                    && workspace.map_or_else(
                        || grant.roster_created_at.is_none(),
                        |workspace| grant.is_roster_in(workspace),
                    )
            })
            .ok_or(ControlError::Unknown)?;
        if let (Some(identity), Some(link)) = (&grant.identity, self.link()) {
            link.withdraw(identity.cert.device_public_key);
        }
        grant.settle(GrantState::Revoked, "revoked by a person");
        if workspace.is_none()
            && let Some(agent) = state.agents.get(id)
        {
            agent.stop();
        }
        drop(state);
        self.changes.raise();
        Ok(())
    }

    /// Settle every approved grant that expired by `now_ns`: its agent
    /// stops and the host refuses its device. Returns the grants expired.
    pub fn expire(&self, now_ns: u64) -> Vec<String> {
        let mut state = self.state();
        let link = self.link();
        let SupervisorState { grants, agents } = &mut *state;
        let due = grants
            .iter_mut()
            .filter_map(|grant| {
                if grant.state != GrantState::Approved || grant.proposal.expires_at_ns > now_ns {
                    return None;
                }
                if let (Some(identity), Some(link)) = (&grant.identity, &link) {
                    link.withdraw(identity.cert.device_public_key);
                }
                if grant.roster_created_at.is_none()
                    && let Some(agent) = agents.get(grant.id())
                {
                    agent.stop();
                }
                grant.settle(GrantState::Expired, "the grant expired");
                Some(grant.id().to_owned())
            })
            .collect::<Vec<_>>();
        drop(state);
        if !due.is_empty() {
            self.changes.raise();
        }
        due
    }

    /// Start the agent of grant `grant` in `workspace` (the grant's first workspace
    /// when `None`) on `task`. Returns the agent and task ids.
    ///
    /// # Errors
    ///
    /// Returns why: an unknown or unapproved grant, a workspace outside it, or
    /// an agent already running under it.
    pub fn spawn(
        &self,
        grant: &str,
        task: &str,
        workspace: Option<&str>,
    ) -> Result<(String, String), String> {
        let (identity, control) = {
            let mut state = self.state();
            let entry = state
                .grants
                .iter()
                .find(|entry| entry.id() == grant && entry.roster_created_at.is_none())
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
            let workspace = match workspace {
                Some(workspace)
                    if scope
                        .workspaces
                        .iter()
                        .any(|own| own.to_string() == workspace) =>
                {
                    workspace.to_owned()
                }
                Some(_) => return Err("the grant does not cover this workspace".into()),
                None => scope
                    .workspaces
                    .first()
                    .map(ToString::to_string)
                    .ok_or("the grant names no workspace")?,
            };
            if state.agents.get(grant).is_some_and(|agent| !agent.ended()) {
                return Err(ControlError::InUse.to_string());
            }
            let control = AgentControl::new(identity.delegation.clone());
            control.report_to(&self.changes);
            control.set_workspace(&workspace);
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
                    workspaces: grant
                        .proposal
                        .scope
                        .workspaces
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    apps: grant.proposal.scope.apps.clone(),
                    in_use: grant.roster_created_at.is_none()
                        && state
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
                    roster_created_at: grant.roster_created_at,
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
        for saved in snapshot.grants {
            let mut grant = Grant {
                proposal: saved.proposal,
                roster_created_at: saved.roster_created_at,
                state: saved.state,
                key: None,
                identity: None,
                pairing_code: None,
                reason: saved.reason,
            };
            match grant.state {
                GrantState::Pending => {
                    grant.settle(GrantState::Denied, "the host restarted before the approval");
                }
                GrantState::Approved => {
                    match restore_identity(saved.key, saved.cert, saved.delegation) {
                        Some(identity) if identity.delegation.expires_at_ns <= now_ns => {
                            if let Some(link) = &link {
                                link.withdraw(identity.delegation.device_public_key);
                            }
                            grant.settle(GrantState::Expired, "the grant expired");
                        }
                        Some(identity) if grant.roster_created_at.is_some() => {
                            if grant.proposal.scope.workspaces
                                == identity.delegation.scope.workspaces
                                && grant.proposal.admits(&identity.delegation).is_ok()
                            {
                                grant.identity = Some(identity);
                            } else {
                                grant.settle(
                                    GrantState::Denied,
                                    "its roster grant could not be read back",
                                );
                            }
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
                .find(|grant| grant.id() == info.id && grant.roster_created_at.is_none())
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
        for grant in &mut state.grants {
            if let (Some(created_at), Some(identity)) = (grant.roster_created_at, &grant.identity) {
                let spawned = self
                    .roster_spawner
                    .get()
                    .ok_or_else(|| "this supervisor cannot join roster sessions".to_owned())
                    .and_then(|spawn| spawn(identity.clone(), created_at));
                if let Err(reason) = spawned {
                    grant.settle(GrantState::Denied, &reason);
                }
            }
        }
        drop(state);
        self.changes.raise();
        resume
    }

    /// Whether `caller` may steer agents here: a device of the delegating
    /// user, never an agent.
    fn authorized(&self, caller: &Caller) -> bool {
        caller.acting().is_person() && caller.user_public_key() == Some(&self.user_public_key)
    }

    fn dispatch(&self, request: &InvokeRequest) -> Result<ActionOutput, ActionCallError> {
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
                    let workspace = arguments
                        .get("workspace")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty());
                    let (agent, task) = self
                        .spawn(text("grant")?, text("task")?, workspace)
                        .map_err(refused)?;
                    serde_json::json!({"agentId": agent, "taskId": task})
                }
                REQUEST_GRANT_ACTION => return self.request_new_grant(&request.argument),
                _ => return Err(ActionCallError::UnknownAction),
            }
        } else {
            let id = address
                .resource
                .strip_prefix(tilo_server_protocol::agents::AGENT_RESOURCE_PREFIX)
                .ok_or(ActionCallError::UnknownAction)?;
            if address.action == REVOKE_ACTION {
                self.revoke(id)
                    .map_err(|error| refused(error.to_string()))?;
                return Ok(ActionOutput {
                    value: Some(serde_json::json!({})),
                    resources: Vec::new(),
                });
            }
            let control = self.control(id).ok_or(ActionCallError::UnknownAction)?;
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
                _ => return Err(ActionCallError::UnknownAction),
            }
        };
        Ok(ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
    }

    fn request_new_grant(
        &self,
        argument: &InvokeArgument,
    ) -> Result<ActionOutput, ActionCallError> {
        let InvokeArgument::Json(arguments) = argument else {
            return Err(invalid(
                "request-grant takes {label, workspaces, apps, expiresInSecs}",
            ));
        };
        if arguments.contains_key("agentId") {
            return Err(refused(
                "a roster grant needs a certified workspace session".into(),
            ));
        }
        let grant: GrantRequest =
            serde_json::from_value(serde_json::Value::Object(arguments.clone()))
                .map_err(|error| invalid(&error.to_string()))?;
        let (id, pairing_code) = self
            .request_grant(&grant, tilo_clock::unix_now_ns())
            .map_err(refused)?;
        Ok(grant_output(&id, &pairing_code))
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
        offered: None,
        provider: tilo_server_protocol::agents::AGENTS_PROVIDER.into(),
        app_id: tilo_server_protocol::agents::AGENTS_PROVIDER.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.into(),
            kind: kind.into(),
            label: resource_label.into(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: kind.into(),
            resource_label: resource_label.into(),
            action_label: action_label.into(),
            keywords: Vec::new(),
            icon: None,
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
        surface: None,
    }
}

fn grant_proposal(
    request: &GrantRequest,
    now_ns: u64,
    seed: &[u8; 32],
    agent: Option<&str>,
) -> Result<DelegateProposal, String> {
    let label = request.label.trim();
    if label.is_empty() || label.len() > MAX_DELEGATE_TEXT_BYTES {
        return Err(format!(
            "a grant's label is 1 to {MAX_DELEGATE_TEXT_BYTES} bytes"
        ));
    }
    if request.workspaces.is_empty() || request.workspaces.len() > MAX_DELEGATED_WORKSPACES {
        return Err(format!(
            "a grant names 1 to {MAX_DELEGATED_WORKSPACES} workspaces"
        ));
    }
    if request.apps.len() > MAX_DELEGATED_APPS {
        return Err(format!("a grant names at most {MAX_DELEGATED_APPS} apps"));
    }
    if !GRANT_SECS.contains(&request.expires_in_secs) {
        return Err("a grant lasts from a minute to 30 days".into());
    }
    let agent_id = agent.map_or_else(|| grant_id(label, seed), str::to_owned);
    // A grant that names no colour wears the one the agent member it is wears everywhere.
    let color = if let Some(text) = &request.color {
        parse_color_hex(text).ok_or("a grant's colour is #rrggbb")?
    } else {
        let (r, g, b) =
            collab_types::presence::identity_rgb(collab_types::presence::PresenceHue::of_member(
                &collab_types::MemberId::agent(&agent_id),
            ));
        [r, g, b]
    };
    Ok(DelegateProposal {
        delegate: Delegate::Agent { agent_id },
        label: label.to_owned(),
        color,
        scope: DelegateScope {
            workspaces: request
                .workspaces
                .iter()
                .filter_map(|workspace| WorkspaceId::parse(workspace))
                .collect(),
            apps: request.apps.clone(),
        },
        expires_at_ns: now_ns.saturating_add(request.expires_in_secs.saturating_mul(1_000_000_000)),
    })
}

fn grant_output(id: &str, pairing_code: &str) -> ActionOutput {
    ActionOutput {
        value: Some(serde_json::json!({"grant": id, "pairingCode": pairing_code})),
        resources: Vec::new(),
    }
}

fn invalid(reason: &str) -> ActionCallError {
    ActionCallError::InvalidRequest {
        reason: reason.into(),
    }
}

fn refused(reason: String) -> ActionCallError {
    ActionCallError::Refused { reason }
}

/// [`ActionProvider`] over a shared [`AgentSupervisor`]; wrap it in
/// `tilo_server::ActionProviderServer` to answer browsers through the host.
#[derive(Clone)]
pub struct SupervisorHost(pub Arc<AgentSupervisor>);

impl ActionProvider for SupervisorHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
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
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        if !self.0.authorized(caller) {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let supervisor = &self.0;
        Pending::ready(supervisor.invoke_once(caller, &request, || supervisor.dispatch(&request)))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0.log().get(caller, id).cloned()
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(if self.0.log().get(caller, id).is_none() {
            Err(ActionCallError::UnknownInvocation)
        } else {
            Err(ActionCallError::NotCancellable {
                reason: "agent control completes at once".into(),
            })
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tilo_actions::remote::{PendingPoll, ReceiptStatus, RemoteActionTarget};

    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

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
            tilo_server_protocol::delegation::Delegate::Agent {
                agent_id: "writer".into(),
            },
            "Writer helper",
            [0x7c, 0x3a, 0xed],
            DelegateScope {
                workspaces: vec![WorkspaceId::parse(WORKSPACE).unwrap()],
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
            issuer: None,
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
                provider: tilo_server_protocol::agents::AGENTS_PROVIDER.into(),
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
    ) -> Result<serde_json::Value, ActionCallError> {
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
    fn a_delayed_cancel_cannot_address_a_replacement_member() {
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
            Err(ActionCallError::InvocationConflict),
            "an id names one request"
        );
        let other = Caller::authenticated(
            SigningKey::from_bytes(&[1; 32]).verifying_key().to_bytes(),
            [3; 32],
        );
        assert_eq!(
            invoke(&host, &other, message("sum the sheet")),
            Err(ActionCallError::InvocationConflict),
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
            matches!(refused, Err(ActionCallError::Refused { ref reason }) if reason == "the agent has no such task"),
            "{refused:?}"
        );
    }
}
