//! Live private enrolled channel custody; durable facts belong to the injected owner.
use launcher::managed::ManagedBinding;
use launcher::managed::channel::{ChannelProcessFacts, VerifiedManagedProcess};
use qos_peer_state::owner_commit::OwnerCommitReceipt;
pub use runtime_collab::{
    EnrolledTaskAcceptance, EnrolledTaskEnvelope, TaskRunPlanning, TaskRunUpdate,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tilo_user_identity::DeviceCert;
use tokio::sync::mpsc;
pub use ui::host_worker::managed_sender::{
    ChildCommit, ChildTicket, ParentFailure, ParentOwner, ParentReceipt, ParentRecord, ParentStep,
};
use uuid::Uuid;

/// Live process custody behind one enrolled connection.
///
/// Production custody is only [`VerifiedManagedProcess`], created from actual
/// kernel sender evidence (Linux). A fixture process exists only in test or
/// `test-support` builds so the enrolled Agent/broker/worker/Store path can be
/// exercised in-process on macOS; production builds cannot name it.
#[derive(Clone)]
pub struct EnrolledProcess(ProcessKind);
#[derive(Clone)]
enum ProcessKind {
    Managed(VerifiedManagedProcess),
    #[cfg(any(test, feature = "test-support"))]
    Fixture(Arc<FixtureProcess>),
}
impl EnrolledProcess {
    pub fn managed(process: VerifiedManagedProcess) -> Self {
        Self(ProcessKind::Managed(process))
    }
    /// Current exact process facts; fails once the process is gone.
    pub fn facts(&self) -> Result<ChannelProcessFacts, String> {
        match &self.0 {
            ProcessKind::Managed(process) => process.facts(),
            #[cfg(any(test, feature = "test-support"))]
            ProcessKind::Fixture(fixture) => fixture.facts(),
        }
    }
    pub fn binding(&self) -> &ManagedBinding {
        match &self.0 {
            ProcessKind::Managed(process) => process.binding(),
            #[cfg(any(test, feature = "test-support"))]
            ProcessKind::Fixture(fixture) => &fixture.facts.binding,
        }
    }
    /// Kernel-verified custody, when this is a real managed process.
    pub fn managed_process(&self) -> Option<&VerifiedManagedProcess> {
        match &self.0 {
            ProcessKind::Managed(process) => Some(process),
            #[cfg(any(test, feature = "test-support"))]
            ProcessKind::Fixture(_) => None,
        }
    }
    /// Same live custody instance (not merely equal descriptive facts).
    pub fn same_custody(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (ProcessKind::Managed(a), ProcessKind::Managed(b)) => {
                a.binding() == b.binding() && a.process() == b.process()
            }
            #[cfg(any(test, feature = "test-support"))]
            (ProcessKind::Fixture(a), ProcessKind::Fixture(b)) => Arc::ptr_eq(a, b),
            #[cfg(any(test, feature = "test-support"))]
            _ => false,
        }
    }
}
/// Test-only stand-in for kernel process evidence. It reports fixed facts
/// until [`FixtureProcess::exit`], modelling a process that disappeared.
#[cfg(any(test, feature = "test-support"))]
pub struct FixtureProcess {
    facts: ChannelProcessFacts,
    alive: AtomicBool,
}
#[cfg(any(test, feature = "test-support"))]
impl FixtureProcess {
    pub fn new(facts: ChannelProcessFacts) -> (EnrolledProcess, Arc<Self>) {
        let fixture = Arc::new(Self {
            facts,
            alive: AtomicBool::new(true),
        });
        (
            EnrolledProcess(ProcessKind::Fixture(fixture.clone())),
            fixture,
        )
    }
    fn facts(&self) -> Result<ChannelProcessFacts, String> {
        if !self.alive.load(Ordering::Acquire) {
            return Err("fixture enrolled process exited".into());
        }
        Ok(self.facts.clone())
    }
    pub fn exit(&self) {
        self.alive.store(false, Ordering::Release);
    }
}

/// Only Fresh may follow a newly committed Store dispatch intent and consumption
/// of its one-use permit. Retained lookup, replay and uncertain writes return Existing.
pub enum DispatchCommit {
    Fresh(OwnerCommitReceipt),
    Existing,
}
pub enum EnrolledDispatchResult {
    Sent(OwnerCommitReceipt),
    Existing,
}

/// Host adapter to the existing private owner. No private data enters room replay.
/// Each callback resolves the retained operation's original caller, then checks
/// current authority under its own Store lock. No broker lock spans callbacks.
pub trait EnrolledAgentOwner: Send + Sync {
    fn authorize_process(
        &self,
        process: &EnrolledProcess,
        caller: &DeviceCert,
    ) -> Result<(), String>;
    fn dispatch(&self, dispatch: &VerifiedEnrolledDispatch) -> Result<DispatchCommit, String>;
    fn accept(
        &self,
        acceptance: &VerifiedRecipientAcceptance,
    ) -> Result<OwnerCommitReceipt, String>;
    fn update(&self, update: &VerifiedTaskUpdate) -> Result<OwnerCommitReceipt, String>;
    fn progress(&self, update: &VerifiedTaskUpdate) -> Result<(), String>;
    fn authorize_task_action(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        app: &str,
        record: &str,
        resource: &str,
        action: &str,
    ) -> Result<(), String>;
    /// Required original-context lifecycle check. Legacy callbacks cannot
    /// substitute a fresh clock for an already admitted invocation.
    fn authorize_task_action_with_policy_time(
        &self,
        _dispatch: &VerifiedEnrolledDispatch,
        _app: &str,
        _record: &str,
        _resource: &str,
        _action: &str,
        _time: &crate::peer_actions::PolicyTime,
    ) -> Result<(), String> {
        Err("original enrolled action policy context unavailable".into())
    }
    fn grants(
        &self,
        process: &EnrolledProcess,
        caller: &DeviceCert,
    ) -> Result<Vec<qos_agent_control_protocol::GrantAction>, String>;
    fn disconnected(&self, process: &EnrolledProcess);
    /// Durable parent custody of one accepted task's steps, under the same
    /// private owner as its dispatch. `bind` and `commit` return only after
    /// the owner's commit. Defaults refuse: without the step ledger no managed
    /// child can be mapped, driven or released for an enrolled task.
    fn task_step(
        &self,
        _dispatch: &VerifiedEnrolledDispatch,
        _step: ParentStep,
    ) -> Result<Option<ParentRecord>, ParentFailure> {
        Err(step_ledger_unavailable())
    }
    fn task_step_child(
        &self,
        _dispatch: &VerifiedEnrolledDispatch,
        _child: ChildTicket,
    ) -> Result<Option<ParentRecord>, ParentFailure> {
        Err(step_ledger_unavailable())
    }
    fn bind_task_step(
        &self,
        _dispatch: &VerifiedEnrolledDispatch,
        _step: ParentStep,
        _child: ChildTicket,
        _intent: [u8; 32],
    ) -> Result<ParentRecord, ParentFailure> {
        Err(step_ledger_unavailable())
    }
    fn commit_task_step(
        &self,
        _dispatch: &VerifiedEnrolledDispatch,
        _commit: ChildCommit<'_>,
    ) -> Result<ParentReceipt, ParentFailure> {
        Err(step_ledger_unavailable())
    }
}
/// One enrolled task step request from the Agent's `invokeTaskAction` with a
/// step identity. The host maps the step to exactly one durable child.
pub struct EnrolledStepRequest {
    pub dispatch: VerifiedEnrolledDispatch,
    pub step: u32,
    pub peer: u64,
    pub resource: String,
    pub action: String,
    pub argument: Option<String>,
}
/// The step's durable outcome: the committed child observation and the
/// parent's committed next-step continuation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrolledStepReply {
    pub update_json: String,
    pub continuation: Vec<u8>,
}
pub type EnrolledStepResult = Result<EnrolledStepReply, String>;
/// Host service that owns the managed sender for enrolled task children.
pub trait EnrolledStepService: Send + Sync {
    /// Queue one step. The reply arrives once the parent committed the step's
    /// exact result; a dropped reply means "still pending, retry the step".
    fn submit(
        &self,
        request: EnrolledStepRequest,
    ) -> Result<tokio::sync::oneshot::Receiver<EnrolledStepResult>, String>;
}
fn step_ledger_unavailable() -> ParentFailure {
    ParentFailure::Unavailable("enrolled task step ledger unavailable".into())
}

/// The accepted task's own owner as the sender runtime's parent. Every step
/// is scoped to this task's original invocation; the owner still revalidates
/// the live custody and current authority inside each Store transaction.
pub struct EnrolledTaskParent {
    connection: Arc<EnrolledConnection>,
    dispatch: VerifiedEnrolledDispatch,
}
impl EnrolledTaskParent {
    pub fn new(dispatch: &VerifiedEnrolledDispatch) -> Result<Self, String> {
        let connection = dispatch.live_connection()?;
        {
            let state = connection
                .state
                .lock()
                .map_err(|_| "enrolled registry unavailable")?;
            let retained = state
                .retained
                .get(&dispatch.data.envelope.message_id)
                .ok_or("original enrolled task disappeared")?;
            if !Arc::ptr_eq(&retained.data, &dispatch.data)
                || retained.accepted.is_none()
                || retained.terminal
            {
                return Err("original enrolled task is no longer active".into());
            }
        }
        Ok(Self {
            connection,
            dispatch: dispatch.clone(),
        })
    }
    fn scoped(&self, step: ParentStep) -> Result<(), ParentFailure> {
        if step.parent() != *self.dispatch.invocation().as_bytes() {
            return Err(ParentFailure::Conflict);
        }
        self.dispatch
            .revalidate()
            .map(|_| ())
            .map_err(ParentFailure::Unavailable)
    }
    fn exact(&self, record: Option<ParentRecord>) -> Result<Option<ParentRecord>, ParentFailure> {
        match record {
            Some(record) if record.step.parent() != *self.dispatch.invocation().as_bytes() => {
                Err(ParentFailure::Conflict)
            }
            other => Ok(other),
        }
    }
}
impl ParentOwner for EnrolledTaskParent {
    fn lookup(&self, step: ParentStep) -> Result<Option<ParentRecord>, ParentFailure> {
        self.scoped(step)?;
        let record = self.connection.owner.task_step(&self.dispatch, step)?;
        self.exact(record)
    }
    fn find_child(&self, child: ChildTicket) -> Result<Option<ParentRecord>, ParentFailure> {
        self.dispatch
            .revalidate()
            .map_err(ParentFailure::Unavailable)?;
        let record = self
            .connection
            .owner
            .task_step_child(&self.dispatch, child)?;
        self.exact(record)
    }
    fn bind(
        &mut self,
        step: ParentStep,
        child: ChildTicket,
        intent: [u8; 32],
    ) -> Result<ParentRecord, ParentFailure> {
        self.scoped(step)?;
        let record = self
            .connection
            .owner
            .bind_task_step(&self.dispatch, step, child, intent)?;
        if record.step != step || record.child != child || record.intent != intent {
            return Err(ParentFailure::Conflict);
        }
        Ok(record)
    }
    fn commit(&mut self, commit: ChildCommit<'_>) -> Result<ParentReceipt, ParentFailure> {
        self.scoped(commit.step)?;
        let receipt = self
            .connection
            .owner
            .commit_task_step(&self.dispatch, commit)?;
        if receipt.step != commit.step
            || receipt.child != commit.child
            || receipt.result != commit.result
        {
            return Err(ParentFailure::Conflict);
        }
        Ok(receipt)
    }
}

struct DispatchData {
    invocation: Uuid,
    caller: DeviceCert,
    envelope: EnrolledTaskEnvelope,
    canonical: String,
}
/// Minted only while an exact managed descriptor has a live enrolled subscription.
#[derive(Clone)]
pub struct VerifiedEnrolledDispatch {
    connection: Weak<EnrolledConnection>,
    data: Arc<DispatchData>,
}
impl VerifiedEnrolledDispatch {
    pub fn invocation(&self) -> Uuid {
        self.data.invocation
    }
    pub fn caller(&self) -> &DeviceCert {
        &self.data.caller
    }
    pub fn envelope(&self) -> &EnrolledTaskEnvelope {
        &self.data.envelope
    }
    pub fn canonical_envelope(&self) -> &str {
        &self.data.canonical
    }
    pub fn process(&self) -> Result<EnrolledProcess, String> {
        let connection = self.live_connection()?;
        Ok(connection.process.clone())
    }
    pub(crate) fn live_connection(&self) -> Result<Arc<EnrolledConnection>, String> {
        let connection = self
            .connection
            .upgrade()
            .ok_or("enrolled connection lost")?;
        let facts = connection.revalidate()?;
        validate_process_envelope(&facts, &self.data.envelope)?;
        if connection.caller != self.data.caller {
            return Err("enrolled caller mismatch".into());
        }
        Ok(connection)
    }
    pub fn revalidate(&self) -> Result<ChannelProcessFacts, String> {
        self.live_connection()?.revalidate()
    }
    /// Original accepted-task custody only. This never opens the owner Store.
    pub fn check_action_custody_at(&self, now_ns: u64) -> Result<ChannelProcessFacts, String> {
        let connection = self
            .connection
            .upgrade()
            .ok_or("enrolled connection lost")?;
        let facts = connection.revalidate_at(now_ns)?;
        validate_process_envelope(&facts, &self.data.envelope)?;
        if connection.caller != self.data.caller {
            return Err("enrolled caller mismatch".into());
        }
        let state = connection
            .state
            .lock()
            .map_err(|_| "enrolled registry unavailable")?;
        let retained = state
            .retained
            .get(&self.data.envelope.message_id)
            .ok_or("original enrolled task disappeared")?;
        if !Arc::ptr_eq(&retained.data, &self.data)
            || retained.accepted.is_none()
            || retained.terminal
        {
            return Err("original enrolled task is no longer active".into());
        }
        Ok(facts)
    }
    pub(crate) fn authorize_original_action(
        &self,
        app: &str,
        record: &str,
        resource: &str,
        action: &str,
        time: &crate::peer_actions::PolicyTime,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .upgrade()
            .ok_or("enrolled connection lost")?;
        connection
            .owner
            .authorize_task_action_with_policy_time(self, app, record, resource, action, time)
    }
}
/// Exact acceptance from the authenticated recipient RPC, never deserialized authority.
#[derive(Clone)]
pub struct VerifiedRecipientAcceptance {
    dispatch: VerifiedEnrolledDispatch,
    acceptance: EnrolledTaskAcceptance,
}
impl VerifiedRecipientAcceptance {
    pub fn invocation(&self) -> Uuid {
        self.dispatch.invocation()
    }
    pub fn caller(&self) -> &DeviceCert {
        self.dispatch.caller()
    }
    pub fn envelope(&self) -> &EnrolledTaskEnvelope {
        self.dispatch.envelope()
    }
    pub fn canonical_envelope(&self) -> &str {
        self.dispatch.canonical_envelope()
    }
    pub fn acceptance(&self) -> &EnrolledTaskAcceptance {
        &self.acceptance
    }
    pub fn revalidate(&self) -> Result<ChannelProcessFacts, String> {
        if !self.acceptance.matches(self.envelope()) {
            return Err("enrolled acceptance changed".into());
        }
        self.dispatch.revalidate()
    }
}
/// Constructed by the live RPC only after an acknowledged durable acceptance.
pub struct VerifiedTaskUpdate {
    accepted: VerifiedRecipientAcceptance,
    sequence: u64,
    update: TaskRunUpdate,
}
impl VerifiedTaskUpdate {
    pub fn invocation(&self) -> Uuid {
        self.accepted.invocation()
    }
    pub fn caller(&self) -> &DeviceCert {
        self.accepted.caller()
    }
    pub fn envelope(&self) -> &EnrolledTaskEnvelope {
        self.accepted.envelope()
    }
    pub fn canonical_envelope(&self) -> &str {
        self.accepted.canonical_envelope()
    }
    pub fn acceptance(&self) -> &EnrolledTaskAcceptance {
        self.accepted.acceptance()
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn update(&self) -> &TaskRunUpdate {
        &self.update
    }
    pub fn revalidate(&self) -> Result<ChannelProcessFacts, String> {
        self.accepted.revalidate()
    }
}

struct RetainedDispatch {
    data: Arc<DispatchData>,
    accepted: Option<EnrolledTaskAcceptance>,
    last_update: Option<(u64, TaskRunUpdate)>,
    terminal: bool,
    callback_pending: bool,
    sent: bool,
}
#[derive(Default)]
struct RecipientState {
    sender: Option<mpsc::Sender<String>>,
    subscribed: bool,
    retained: BTreeMap<String, RetainedDispatch>,
    retained_bytes: usize,
    last_acceptance: u64,
    action_slots: usize,
    invocations: BTreeMap<u64, (String, qos_agent_control_protocol::GrantAction)>,
}
impl RecipientState {
    fn begin_acceptance(
        &mut self,
        acceptance: &EnrolledTaskAcceptance,
    ) -> Result<Arc<DispatchData>, String> {
        let retained = self
            .retained
            .get_mut(&acceptance.message_id)
            .ok_or("unknown enrolled dispatch")?;
        if !retained.sent
            || !acceptance.matches(&retained.data.envelope)
            || acceptance.acceptance_sequence > 1024
        {
            return Err("enrolled acceptance does not name exact delivered envelope".into());
        }
        if let Some(previous) = &retained.accepted {
            if previous != acceptance {
                return Err("enrolled acceptance sequence changed".into());
            }
        } else if acceptance.acceptance_sequence
            != self
                .last_acceptance
                .checked_add(1)
                .ok_or("acceptance sequence overflow")?
        {
            return Err("enrolled acceptance sequence is not next".into());
        }
        if retained.callback_pending {
            return Err("enrolled acceptance commit pending".into());
        }
        retained.callback_pending = true;
        Ok(retained.data.clone())
    }
    fn finish_acceptance(
        &mut self,
        acceptance: &EnrolledTaskAcceptance,
        room: &str,
        result: Result<OwnerCommitReceipt, String>,
    ) -> Result<OwnerCommitReceipt, String> {
        let retained = self
            .retained
            .get_mut(&acceptance.message_id)
            .ok_or("enrolled dispatch lost")?;
        retained.callback_pending = false;
        let receipt = result?;
        validate_enrolled_receipt(&receipt, room, retained.data.invocation, "completed")?;
        retained.accepted = Some(acceptance.clone());
        self.last_acceptance = self.last_acceptance.max(acceptance.acceptance_sequence);
        Ok(receipt)
    }
}

pub(crate) struct EnrolledConnection {
    process: EnrolledProcess,
    facts: ChannelProcessFacts,
    caller: DeviceCert,
    owner: Arc<dyn EnrolledAgentOwner>,
    alive: AtomicBool,
    state: Mutex<RecipientState>,
}
impl EnrolledConnection {
    pub(crate) fn new(
        process: EnrolledProcess,
        caller: DeviceCert,
        owner: Arc<dyn EnrolledAgentOwner>,
    ) -> Result<Arc<Self>, String> {
        let facts = process.facts()?;
        if facts.binding.app != crate::agent_control::APP_ID
            || facts.runtime.package_id != crate::agent_control::APP_ID
        {
            return Err("enrolled connection is not the installed agent package".into());
        }
        owner.authorize_process(&process, &caller)?;
        Ok(Arc::new(Self {
            process,
            facts,
            caller,
            owner,
            alive: AtomicBool::new(true),
            state: Mutex::new(RecipientState::default()),
        }))
    }
    pub(crate) fn facts(&self) -> &ChannelProcessFacts {
        &self.facts
    }
    pub(crate) fn caller(&self) -> &DeviceCert {
        &self.caller
    }
    pub(crate) fn revalidate(&self) -> Result<ChannelProcessFacts, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        self.revalidate_at(now)
    }
    fn revalidate_at(&self, now_ns: u64) -> Result<ChannelProcessFacts, String> {
        if !self.alive.load(Ordering::Acquire) {
            return Err("enrolled connection disconnected".into());
        }
        let facts = self.process.facts()?;
        if facts != self.facts {
            return Err("managed process channel or epoch changed".into());
        }
        tilo_user_identity::verify_device_cert_at(&self.caller, now_ns)
            .map_err(|_| "enrolled caller certificate expired")?;
        Ok(facts)
    }
    pub(crate) fn authorize(&self) -> Result<(), String> {
        self.revalidate()?;
        self.owner.authorize_process(&self.process, &self.caller)
    }
    pub(crate) fn grants(&self) -> Result<Vec<qos_agent_control_protocol::GrantAction>, String> {
        self.authorize()?;
        let grants = self.owner.grants(&self.process, &self.caller)?;
        if grants.is_empty() || grants.len() > 64 {
            return Err("enrolled grants unavailable".into());
        }
        Ok(grants)
    }
    pub(crate) fn subscribe(&self) -> Result<mpsc::Receiver<String>, String> {
        self.authorize()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "enrolled recipient registry unavailable")?;
        if state.subscribed {
            return Err("enrolled recipient already subscribed".into());
        }
        let (sender, receiver) = mpsc::channel(32);
        state.subscribed = true;
        state.sender = Some(sender);
        Ok(receiver)
    }
    pub(crate) fn close(&self) {
        if self.alive.swap(false, Ordering::AcqRel) {
            if let Ok(mut state) = self.state.lock() {
                state.sender.take();
            }
            self.owner.disconnected(&self.process);
        }
    }
    pub(crate) fn dispatch(
        self: &Arc<Self>,
        invocation: Uuid,
        caller: &DeviceCert,
        envelope: EnrolledTaskEnvelope,
    ) -> Result<EnrolledDispatchResult, String> {
        self.authorize()?;
        if invocation.is_nil() || caller != &self.caller {
            return Err("enrolled dispatch original caller mismatch".into());
        }
        validate_process_envelope(&self.facts, &envelope)?;
        validate_enrolled_envelope(&envelope)?;
        let canonical = serde_json::to_string(&envelope).map_err(|e| e.to_string())?;
        let data = Arc::new(DispatchData {
            invocation,
            caller: caller.clone(),
            envelope,
            canonical,
        });
        let sender = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "enrolled registry unavailable")?;
            if let Some(existing) = state.retained.get(&data.envelope.message_id) {
                if existing.data.invocation != invocation
                    || existing.data.canonical != data.canonical
                    || existing.data.caller != *caller
                {
                    return Err("enrolled message identity conflict".into());
                }
                return Ok(EnrolledDispatchResult::Existing);
            }
            check_capacity(&state, &data)?;
            let sender = state
                .sender
                .as_ref()
                .filter(|s| !s.is_closed())
                .cloned()
                .ok_or("enrolled recipient not subscribed")?;
            // The exact envelope is charged before the durable effect boundary,
            // including ambiguous sends. There is no eviction or replay permit.
            state.retained_bytes += data.canonical.len();
            state.retained.insert(
                data.envelope.message_id.clone(),
                RetainedDispatch {
                    data: data.clone(),
                    accepted: None,
                    last_update: None,
                    terminal: false,
                    callback_pending: false,
                    sent: false,
                },
            );
            sender
        };
        let witness = VerifiedEnrolledDispatch {
            connection: Arc::downgrade(self),
            data: data.clone(),
        };
        let commit = self.owner.dispatch(&witness)?;
        let DispatchCommit::Fresh(receipt) = commit else {
            return Ok(EnrolledDispatchResult::Existing);
        };
        witness.revalidate()?;
        validate_enrolled_receipt(
            &receipt,
            &self.facts.binding.room,
            invocation,
            "dispatch_intended",
        )?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "enrolled registry unavailable")?;
            let retained = state
                .retained
                .get_mut(&data.envelope.message_id)
                .ok_or("dispatch reservation lost")?;
            if retained.sent {
                return Err("enrolled dispatch permit already consumed".into());
            }
            retained.sent = true;
        }
        // Consumed even when the receiver disappears. Status is the only retry.
        sender
            .try_send(data.canonical.clone())
            .map_err(|_| "enrolled dispatch uncertain after durable intent")?;
        Ok(EnrolledDispatchResult::Sent(receipt))
    }
    pub(crate) fn accept(
        self: &Arc<Self>,
        acceptance: EnrolledTaskAcceptance,
    ) -> Result<OwnerCommitReceipt, String> {
        self.authorize()?;
        let data = self
            .state
            .lock()
            .map_err(|_| "enrolled registry unavailable")?
            .begin_acceptance(&acceptance)?;
        let witness = VerifiedRecipientAcceptance {
            dispatch: VerifiedEnrolledDispatch {
                connection: Arc::downgrade(self),
                data,
            },
            acceptance: acceptance.clone(),
        };
        let result = self.owner.accept(&witness);
        self.state
            .lock()
            .map_err(|_| "enrolled registry unavailable")?
            .finish_acceptance(&acceptance, &self.facts.binding.room, result)
    }

    pub(crate) fn report(
        self: &Arc<Self>,
        acceptance: EnrolledTaskAcceptance,
        sequence: u64,
        update: TaskRunUpdate,
    ) -> Result<(), String> {
        self.authorize()?;
        validate_update(sequence, &update)?;
        let data = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "enrolled registry unavailable")?;
            let retained = state
                .retained
                .get_mut(&acceptance.message_id)
                .ok_or("unknown enrolled dispatch")?;
            if retained.accepted.as_ref() != Some(&acceptance)
                || !acceptance.matches(&retained.data.envelope)
            {
                return Err("enrolled update requires committed exact acceptance".into());
            }
            if let Some((last, prior)) = &retained.last_update {
                if sequence == *last && *prior == update {
                    return Ok(());
                }
                if sequence <= *last || retained.terminal {
                    return Err("enrolled task sequence or terminal conflict".into());
                }
            }
            if retained.callback_pending {
                return Err("enrolled task commit pending".into());
            }
            retained.callback_pending = true;
            retained.data.clone()
        };
        let witness = VerifiedTaskUpdate {
            accepted: VerifiedRecipientAcceptance {
                dispatch: VerifiedEnrolledDispatch {
                    connection: Arc::downgrade(self),
                    data,
                },
                acceptance: acceptance.clone(),
            },
            sequence,
            update: update.clone(),
        };
        let terminal = !matches!(update, TaskRunUpdate::Progress { .. });
        let result = if terminal {
            self.owner.update(&witness).and_then(|receipt| {
                validate_enrolled_receipt(
                    &receipt,
                    &self.facts.binding.room,
                    witness.invocation(),
                    "task_finished",
                )
            })
        } else {
            self.owner.progress(&witness)
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| "enrolled registry unavailable")?;
        let retained = state
            .retained
            .get_mut(&acceptance.message_id)
            .ok_or("enrolled dispatch lost")?;
        retained.callback_pending = false;
        result?;
        retained.last_update = Some((sequence, update));
        retained.terminal = terminal;
        Ok(())
    }
    pub(crate) fn authorize_task_action(
        self: &Arc<Self>,
        run: u64,
        lease: &str,
        app: &str,
        record: &str,
        resource: &str,
        action: &str,
    ) -> Result<(), String> {
        self.task(run, lease)?;
        let data = self
            .state
            .lock()
            .map_err(|_| "enrolled registry unavailable")?
            .retained
            .values()
            .find(|entry| {
                entry.data.envelope.task.run_id == run && entry.data.envelope.task.lease == lease
            })
            .map(|entry| entry.data.clone())
            .ok_or("enrolled task disappeared")?;
        let witness = VerifiedEnrolledDispatch {
            connection: Arc::downgrade(self),
            data,
        };
        self.owner
            .authorize_task_action(&witness, app, record, resource, action)
    }
    pub(crate) fn capture_task_action(
        self: &Arc<Self>,
        run: u64,
        lease: &str,
        app: &str,
        record: &str,
        resource: &str,
        action: &str,
    ) -> Result<VerifiedEnrolledDispatch, String> {
        self.authorize_task_action(run, lease, app, record, resource, action)?;
        let state = self
            .state
            .lock()
            .map_err(|_| "enrolled registry unavailable")?;
        let data = state
            .retained
            .values()
            .find(|entry| {
                entry.accepted.is_some()
                    && !entry.terminal
                    && entry.data.envelope.task.run_id == run
                    && entry.data.envelope.task.lease == lease
            })
            .map(|entry| entry.data.clone())
            .ok_or("original enrolled task disappeared")?;
        Ok(VerifiedEnrolledDispatch {
            connection: Arc::downgrade(self),
            data,
        })
    }
    pub(crate) fn reserve_action_slot(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "enrolled registry unavailable")?;
        if state.action_slots >= 8192 {
            return Err("enrolled action retention full".into());
        }
        state.action_slots += 1;
        Ok(())
    }
    pub(crate) fn remember_action(
        &self,
        id: u64,
        message: String,
        grant: qos_agent_control_protocol::GrantAction,
    ) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "enrolled registry unavailable")?;
        if state.invocations.contains_key(&id) {
            return Err("enrolled action handle conflict".into());
        }
        state.invocations.insert(id, (message, grant));
        Ok(())
    }
    pub(crate) fn authorize_action_observation(
        self: &Arc<Self>,
        id: u64,
    ) -> Result<EnrolledTaskEnvelope, String> {
        self.authorize()?;
        let (data, grant) = {
            let state = self
                .state
                .lock()
                .map_err(|_| "enrolled registry unavailable")?;
            let (message, grant) = state
                .invocations
                .get(&id)
                .ok_or("task does not own invocation")?;
            let data = state
                .retained
                .get(message)
                .ok_or("enrolled action task lost")?
                .data
                .clone();
            (data, grant.clone())
        };
        let witness = VerifiedEnrolledDispatch {
            connection: Arc::downgrade(self),
            data,
        };
        self.owner.authorize_task_action(
            &witness,
            &grant.app_id,
            &grant.record_id,
            &grant.resource,
            &grant.action,
        )?;
        Ok(witness.envelope().clone())
    }
    pub(crate) fn task(&self, run: u64, lease: &str) -> Result<EnrolledTaskEnvelope, String> {
        self.authorize()?;
        let state = self
            .state
            .lock()
            .map_err(|_| "enrolled registry unavailable")?;
        state
            .retained
            .values()
            .find(|entry| {
                entry.accepted.is_some()
                    && !entry.terminal
                    && entry.data.envelope.task.run_id == run
                    && entry.data.envelope.task.lease == lease
            })
            .map(|entry| entry.data.envelope.clone())
            .ok_or_else(|| "unknown enrolled task lease".into())
    }
}
impl Drop for EnrolledConnection {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Default)]
pub(crate) struct EnrolledRegistry {
    connections: Mutex<BTreeMap<u64, Arc<EnrolledConnection>>>,
}
impl EnrolledRegistry {
    pub(crate) fn register(
        &self,
        peer: u64,
        connection: Arc<EnrolledConnection>,
    ) -> Result<(), String> {
        connection.authorize()?;
        let mut registry = self
            .connections
            .lock()
            .map_err(|_| "enrolled registry unavailable")?;
        if registry.len() >= 32
            || registry.contains_key(&peer)
            || registry
                .values()
                .any(|c| c.facts.channel_id == connection.facts.channel_id)
        {
            return Err("enrolled connection already installed".into());
        }
        registry.insert(peer, connection);
        Ok(())
    }
    pub(crate) fn connection(&self, peer: u64) -> Result<Arc<EnrolledConnection>, String> {
        self.connections
            .lock()
            .map_err(|_| "enrolled registry unavailable")?
            .get(&peer)
            .cloned()
            .ok_or_else(|| "enrolled task RPC requires exact managed descriptor".into())
    }
    pub(crate) fn dispatch(
        &self,
        invocation: Uuid,
        caller: &DeviceCert,
        envelope: EnrolledTaskEnvelope,
    ) -> Result<EnrolledDispatchResult, String> {
        let connection = self
            .connections
            .lock()
            .map_err(|_| "enrolled registry unavailable")?
            .values()
            .find(|c| c.facts.channel_id == envelope.channel_lease)
            .cloned()
            .ok_or("enrolled recipient channel unavailable")?;
        connection.dispatch(invocation, caller, envelope)
    }
    /// Witness for an accepted, nonterminal task retained on `channel`.
    pub(crate) fn task_dispatch(
        &self,
        channel: &str,
        invocation: Uuid,
    ) -> Result<VerifiedEnrolledDispatch, String> {
        let connection = self
            .connections
            .lock()
            .map_err(|_| "enrolled registry unavailable")?
            .values()
            .find(|c| c.facts.channel_id == channel)
            .cloned()
            .ok_or("enrolled recipient channel unavailable")?;
        connection.revalidate()?;
        let data = connection
            .state
            .lock()
            .map_err(|_| "enrolled registry unavailable")?
            .retained
            .values()
            .find(|entry| {
                entry.data.invocation == invocation && entry.accepted.is_some() && !entry.terminal
            })
            .map(|entry| entry.data.clone())
            .ok_or("accepted enrolled task unavailable")?;
        Ok(VerifiedEnrolledDispatch {
            connection: Arc::downgrade(&connection),
            data,
        })
    }
    pub(crate) fn ready(&self, channel: &str) -> bool {
        let connection = self.connections.lock().ok().and_then(|registry| {
            registry
                .values()
                .find(|connection| connection.facts.channel_id == channel)
                .cloned()
        });
        connection.is_some_and(|connection| {
            connection.revalidate().is_ok()
                && connection.state.lock().is_ok_and(|state| {
                    state
                        .sender
                        .as_ref()
                        .is_some_and(|sender| !sender.is_closed())
                })
        })
    }
    pub(crate) fn remove(&self, peer: u64) {
        let connection = self
            .connections
            .lock()
            .ok()
            .and_then(|mut r| r.remove(&peer));
        if let Some(connection) = connection {
            connection.close();
        }
    }
    pub(crate) fn clear(&self) {
        let connections = self
            .connections
            .lock()
            .map(|mut r| std::mem::take(&mut *r))
            .unwrap_or_default();
        for connection in connections.values() {
            connection.close();
        }
    }
}

fn validate_enrolled_receipt(
    receipt: &OwnerCommitReceipt,
    room: &str,
    invocation: Uuid,
    kind: &str,
) -> Result<(), String> {
    if receipt.binding.room != room
        || receipt.generation == 0
        || receipt.binding.target
            != (qos_peer_state::owner_commit::CommitTarget::AgentRuntime {
                mutation: format!("{invocation}:{kind}"),
            })
    {
        return Err("enrolled receipt does not commit the exact invocation fact".into());
    }
    Ok(())
}

fn check_capacity(state: &RecipientState, data: &DispatchData) -> Result<(), String> {
    if state.retained.len() >= 1024
        || state
            .retained_bytes
            .checked_add(data.canonical.len())
            .is_none_or(|n| n > 8 * 1024 * 1024)
    {
        return Err("enrolled recipient retention full".into());
    }
    if state.retained.values().any(|entry| !entry.terminal) {
        return Err("enrolled agent busy or uncertain".into());
    }
    if state.retained.values().any(|entry| {
        entry.data.invocation == data.invocation
            || entry.data.envelope.task_id == data.envelope.task_id
            || entry.data.envelope.run_id == data.envelope.run_id
            || entry.data.envelope.task.run_id == data.envelope.task.run_id
    }) {
        return Err("enrolled task identity already retained".into());
    }
    Ok(())
}
fn validate_process_envelope(
    facts: &ChannelProcessFacts,
    envelope: &EnrolledTaskEnvelope,
) -> Result<(), String> {
    if facts.binding.app != crate::agent_control::APP_ID
        || facts.channel_id != envelope.channel_lease
        || facts.runtime.runtime_epoch != envelope.runtime_epoch
        || facts.binding.launch != envelope.launch_id
        || facts.binding.agent != envelope.recipient_agent
    {
        return Err("enrolled envelope process, channel, epoch or recipient mismatch".into());
    }
    Ok(())
}
fn validate_update(sequence: u64, update: &TaskRunUpdate) -> Result<(), String> {
    let text = match update {
        TaskRunUpdate::Accepted => return Err("use enrolled acceptance RPC before work".into()),
        TaskRunUpdate::Progress { message }
        | TaskRunUpdate::Succeeded { message }
        | TaskRunUpdate::Failed { message }
        | TaskRunUpdate::Cancelled { message } => message,
    };
    if sequence == 0 || text.is_empty() || text.chars().count() > 8000 || text.len() > 32000 {
        return Err("enrolled task output exceeds retained bounds".into());
    }
    Ok(())
}
fn validate_enrolled_envelope(envelope: &EnrolledTaskEnvelope) -> Result<(), String> {
    envelope.validate()?;
    for id in [
        &envelope.message_id,
        &envelope.task_id,
        &envelope.run_id,
        &envelope.launch_id,
        &envelope.runtime_epoch,
        &envelope.recipient_agent,
        &envelope.channel_lease,
        &envelope.task.lease,
    ] {
        let parsed = Uuid::parse_str(id).map_err(|_| "enrolled IDs must be canonical UUIDs")?;
        if parsed.is_nil() || parsed.to_string() != *id {
            return Err("enrolled IDs must be canonical UUIDs".into());
        }
    }

    let context: tilo_actions::ActionContext =
        serde_json::from_value(envelope.task.action_context.clone()).map_err(|e| e.to_string())?;
    if context.purpose.is_some()
        || context.group.as_ref().is_none_or(|group| {
            group.kind != "task" || group.id != envelope.task_id || group.label != envelope.task_id
        })
    {
        return Err("private task text cannot enter public action attribution".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn envelope() -> EnrolledTaskEnvelope {
        let id = "11111111-1111-4111-8111-111111111111".to_string();
        EnrolledTaskEnvelope {
            version: 1,
            message_id: id.clone(),
            task_id: id.clone(),
            run_id: id.clone(),
            launch_id: id.clone(),
            runtime_epoch: id.clone(),
            recipient_agent: id.clone(),
            channel_lease: id.clone(),
            task: runtime_collab::TaskRunEnvelope {
                version: 1,
                task_id: id.clone(),
                title: "Private task".into(),
                run_id: 1,
                lease: id.clone(),
                planning: runtime_collab::TaskRunPlanning::Model,
                action_context: serde_json::json!({"purpose":null,"group":{"kind":"task","id":id,"label":id}}),
            },
        }
    }
    fn data(index: u128) -> Arc<DispatchData> {
        let mut envelope = envelope();
        envelope.message_id = Uuid::from_u128(index * 3 + 1).to_string();
        envelope.task_id = Uuid::from_u128(index * 3 + 2).to_string();
        envelope.task.task_id = envelope.task_id.clone();
        envelope.run_id = Uuid::from_u128(index * 3 + 3).to_string();
        envelope.task.run_id = index as u64 + 1;
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let mut caller = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [2; 32],
            device_label: "unit caller".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut caller, &root).unwrap();
        Arc::new(DispatchData {
            invocation: Uuid::from_u128(index + 1),
            caller,
            canonical: serde_json::to_string(&envelope).unwrap(),
            envelope,
        })
    }
    fn retained(data: Arc<DispatchData>, sent: bool) -> RetainedDispatch {
        RetainedDispatch {
            data,
            accepted: None,
            last_update: None,
            terminal: false,
            callback_pending: false,
            sent,
        }
    }
    fn acceptance(envelope: &EnrolledTaskEnvelope, sequence: u64) -> EnrolledTaskAcceptance {
        EnrolledTaskAcceptance {
            version: 1,
            message_id: envelope.message_id.clone(),
            task_id: envelope.task_id.clone(),
            run_id: envelope.run_id.clone(),
            launch_id: envelope.launch_id.clone(),
            runtime_epoch: envelope.runtime_epoch.clone(),
            recipient_agent: envelope.recipient_agent.clone(),
            channel_lease: envelope.channel_lease.clone(),
            acceptance_sequence: sequence,
        }
    }
    #[test]
    fn enrolled_failed_barrier_never_becomes_recipient_acceptance() {
        let data = data(1);
        let accepted = acceptance(&data.envelope, 1);
        let mut state = RecipientState::default();
        state
            .retained
            .insert(accepted.message_id.clone(), retained(data.clone(), true));
        state.begin_acceptance(&accepted).unwrap();
        assert!(state.retained[&accepted.message_id].accepted.is_none());
        assert!(
            state.begin_acceptance(&accepted).is_err(),
            "concurrent commit must not race"
        );
        assert!(
            state
                .finish_acceptance(&accepted, "room", Err("disk barrier failed".into()))
                .is_err()
        );
        assert!(state.retained[&accepted.message_id].accepted.is_none());
        assert_eq!(state.last_acceptance, 0);
        let mut mismatch = accepted.clone();
        mismatch.channel_lease = Uuid::from_u128(9).to_string();
        assert!(state.begin_acceptance(&mismatch).is_err());
        mismatch = accepted.clone();
        mismatch.acceptance_sequence = 2;
        assert!(state.begin_acceptance(&mismatch).is_err());
        state.retained.get_mut(&accepted.message_id).unwrap().sent = false;
        assert!(
            state.begin_acceptance(&accepted).is_err(),
            "reservation alone is not delivered"
        );
    }
    #[test]
    fn enrolled_ambiguous_dispatch_stays_busy_and_charged() {
        let first = data(1);
        let second = data(2);
        let mut state = RecipientState::default();
        state.retained_bytes = first.canonical.len();
        state.retained.insert(
            first.envelope.message_id.clone(),
            retained(first.clone(), false),
        );
        assert!(
            check_capacity(&state, &second).is_err(),
            "ambiguous dispatch cannot permit another task"
        );
        state
            .retained
            .get_mut(&first.envelope.message_id)
            .unwrap()
            .terminal = true;
        assert!(
            check_capacity(&state, &first).is_err(),
            "terminal identity cannot be reused"
        );
        assert!(check_capacity(&state, &second).is_ok());
        state.retained_bytes = 8 * 1024 * 1024 - second.canonical.len() + 1;
        assert!(check_capacity(&state, &second).is_err());
        assert!(
            state.retained.contains_key(&first.envelope.message_id),
            "capacity cannot evict identity"
        );
    }
    #[test]
    fn enrolled_output_keeps_full_unicode_and_refuses_overflow() {
        assert!(
            validate_update(
                1,
                &TaskRunUpdate::Succeeded {
                    message: "🚀".repeat(8000)
                }
            )
            .is_ok()
        );
        assert!(
            validate_update(
                1,
                &TaskRunUpdate::Failed {
                    message: "x".repeat(8001)
                }
            )
            .is_err()
        );
        assert!(
            validate_update(
                0,
                &TaskRunUpdate::Succeeded {
                    message: "ok".into()
                }
            )
            .is_err()
        );
        assert!(validate_update(1, &TaskRunUpdate::Accepted).is_err());
    }

    #[test]
    fn enrolled_receipt_cannot_acknowledge_another_fact_or_invocation() {
        use qos_peer_state::owner_commit::{CommitBinding, CommitTarget};
        let invocation = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let room = Uuid::parse_str("22222222-2222-4222-8222-222222222222")
            .unwrap()
            .to_string();
        let mut receipt = OwnerCommitReceipt {
            generation: 9,
            aggregate_hash: [1; 32],
            binding: CommitBinding {
                room: room.clone(),
                target: CommitTarget::AgentRuntime {
                    mutation: format!("{invocation}:completed"),
                },
                request_digest: [2; 32],
                basis: [3; 32],
            },
        };
        assert!(validate_enrolled_receipt(&receipt, &room, invocation, "completed").is_ok());
        assert!(
            validate_enrolled_receipt(&receipt, &room, invocation, "dispatch_intended").is_err()
        );
        receipt.binding.target = CommitTarget::AgentRuntime {
            mutation: format!("{}:completed", Uuid::nil()),
        };
        assert!(validate_enrolled_receipt(&receipt, &room, invocation, "completed").is_err());
    }

    #[test]
    fn enrolled_rejects_private_text_attribution() {
        let mut value = envelope();
        value.task.action_context["purpose"] = "Private task".into();
        assert!(validate_enrolled_envelope(&value).is_err());
        value.task.action_context["purpose"] = serde_json::Value::Null;
        value.task.action_context["group"]["label"] = "Private task".into();
        assert!(validate_enrolled_envelope(&value).is_err());
    }
}
