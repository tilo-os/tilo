//! Installed owner-thread sender runtime with parent custody.
//!
//! The runtime is the only public door to the managed sender. It keeps three
//! orderings that the private phases cannot express on their own:
//!
//! 1. A child row is durably committed *and* its parent step mapping is
//!    durably recorded before any attempt can leave this process.
//! 2. The exact committed terminal observation and the parent's next-step
//!    continuation are durably committed by the parent before the child row
//!    is consumed. A sender terminal receipt alone never releases a child.
//! 3. Recovery reuses the original row and UUID. A parent step never obtains a
//!    second invocation while its original is retained, uncertain or lost.
use super::{
    ManagedAttempt, ManagedAttemptResult,
    attempt::{AttemptFailure, Operation, Ticket},
    codec::{CallerView, TargetView},
    lifecycle::{Completion, Failure, Intent, ManagedSender, RECOVERY_SLOTS, ReplyDisposition},
};
use crate::time::Instant;
use crate::{
    collaboration::RouteSpec,
    host_worker::private_actions::{CurrentPrivateState, SenderStorageCode},
};
use runtime_collab::PublishedInvocationBinding;

/// Digest of one original immutable semantic request, including None versus
/// Some("") arguments. Confirmation evidence is deliberately excluded.
pub(super) fn intent_digest(
    target: &TargetView<'_>,
    revision: &str,
    resource: &str,
    action: &str,
    argument: Option<&str>,
) -> [u8; 32] {
    fn field(hash: &mut blake3::Hasher, text: &str) {
        hash.update(&(text.len() as u64).to_le_bytes());
        hash.update(text.as_bytes());
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"native-managed-intent-v1\0");
    hash.update(&target.host);
    hash.update(&target.peer.to_le_bytes());
    field(&mut hash, target.app);
    field(&mut hash, target.record);
    hash.update(&target.root);
    hash.update(&target.device);
    hash.update(&target.binding);
    for text in [revision, resource, action] {
        field(&mut hash, text);
    }
    match argument {
        None => {
            hash.update(&[0]);
        }
        Some(text) => {
            hash.update(&[1]);
            field(&mut hash, text);
        }
    }
    *hash.finalize().as_bytes()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SenderError {
    Capacity,
    /// A physical barrier or an in-flight attempt is still owned. Retry after
    /// [`SenderRuntime::drive`] or after the attempt's result is accepted.
    Busy,
    Invalid,
    /// The sender no longer retains this child. Nothing was sent or begun.
    NotRetained,
    Scope,
    Storage(SenderStorageCode),
    /// The parent step already names another child or another intent.
    IdentityConflict,
    /// The parent has no durable mapping for this child; nothing may be sent.
    Unmapped,
    /// No certain terminal observation is committed for this child yet.
    NotTerminal,
    /// The parent could not derive its continuation from the exact result.
    Continuation(String),
    Parent(ParentFailure),
}
impl From<Failure> for SenderError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Capacity => Self::Capacity,
            Failure::Busy => Self::Busy,
            Failure::Invalid => Self::Invalid,
            Failure::NotRetained => Self::NotRetained,
            Failure::Scope => Self::Scope,
            Failure::Storage(code) => Self::Storage(code),
        }
    }
}
impl From<ParentFailure> for SenderError {
    fn from(failure: ParentFailure) -> Self {
        Self::Parent(failure)
    }
}

/// Parent-owner refusal. `Uncertain` means the owner's durable outcome is
/// unknown; the runtime treats it exactly like a refusal and retains the child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParentFailure {
    Conflict,
    Unavailable(String),
    Uncertain(String),
}

/// Exact caller scope derived by the host from its authenticated channel.
/// Labels here are not authority; the receiver still checks the live caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SenderCaller {
    root: [u8; 32],
    device: [u8; 32],
    app: String,
    record: String,
    agent: Option<String>,
}
impl SenderCaller {
    pub fn new(
        root: [u8; 32],
        device: [u8; 32],
        app: &str,
        record: &str,
        agent: Option<&str>,
    ) -> Result<Self, SenderError> {
        if root == [0; 32]
            || device == [0; 32]
            || app.is_empty()
            || agent.is_some_and(str::is_empty)
        {
            return Err(SenderError::Invalid);
        }
        Ok(Self {
            root,
            device,
            app: app.into(),
            record: record.into(),
            agent: agent.map(str::to_owned),
        })
    }
    fn view(&self) -> CallerView<'_> {
        CallerView {
            root: self.root,
            device: self.device,
            app: &self.app,
            record: &self.record,
            agent: self.agent.as_deref(),
        }
    }
}

/// One parent-owned step slot: the parent operation's identity and the index
/// of the step within it. The parent chooses and persists both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParentStep {
    parent: [u8; 16],
    index: u32,
}
impl ParentStep {
    pub fn new(parent: [u8; 16], index: u32) -> Result<Self, SenderError> {
        if parent == [0; 16] {
            return Err(SenderError::Invalid);
        }
        Ok(Self { parent, index })
    }
    pub fn parent(&self) -> [u8; 16] {
        self.parent
    }
    pub fn index(&self) -> u32 {
        self.index
    }
    pub fn next(&self) -> Option<Self> {
        Some(Self {
            parent: self.parent,
            index: self.index.checked_add(1)?,
        })
    }
}

/// Inert correlation to one original child row. It confers no authority;
/// every use is checked against the installed outbox instance and caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildTicket(Ticket);
impl ChildTicket {
    pub fn invocation(&self) -> [u8; 16] {
        self.0.invocation
    }
    pub fn host(&self) -> [u8; 16] {
        self.0.host
    }
    /// Stable encoding for the parent's own durable record.
    pub fn to_bytes(&self) -> [u8; 48] {
        let mut bytes = [0; 48];
        bytes[..16].copy_from_slice(&self.0.instance);
        bytes[16..32].copy_from_slice(&self.0.host);
        bytes[32..].copy_from_slice(&self.0.invocation);
        bytes
    }
    pub fn from_bytes(bytes: &[u8; 48]) -> Option<Self> {
        let part = |range: std::ops::Range<usize>| -> Option<[u8; 16]> {
            let value: [u8; 16] = bytes[range].try_into().ok()?;
            (value != [0; 16]).then_some(value)
        };
        Some(Self(Ticket {
            instance: part(0..16)?,
            host: part(16..32)?,
            invocation: part(32..48)?,
        }))
    }
}

/// The original immutable semantic request of one child. Current confirmation
/// evidence is supplied per attempt and is never part of this identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildIntent {
    pub binding: PublishedInvocationBinding,
    pub revision: String,
    pub resource: String,
    pub action: String,
    pub argument: Option<String>,
}
impl ChildIntent {
    pub fn digest(&self) -> [u8; 32] {
        intent_digest(
            &TargetView {
                host: self.binding.host,
                peer: self.binding.peer_id,
                app: &self.binding.app_id,
                record: &self.binding.record_id,
                root: self.binding.root,
                device: self.binding.device,
                binding: self.binding.target_binding,
            },
            &self.revision,
            &self.resource,
            &self.action,
            self.argument.as_deref(),
        )
    }
    fn view(&self) -> Intent<'_> {
        Intent {
            binding: &self.binding,
            revision: &self.revision,
            resource: &self.resource,
            action: &self.action,
            argument: self.argument.as_deref(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParentCommitted {
    /// Digest of the exact committed child observation.
    pub result: [u8; 32],
    /// The parent's next-step continuation derived from that exact result.
    pub continuation: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParentRecord {
    pub step: ParentStep,
    pub child: ChildTicket,
    pub intent: [u8; 32],
    pub committed: Option<ParentCommitted>,
}
/// Borrowed exact commit input. `result_json` is lent from the committed
/// sender row; it is not a caller copy and must be stored byte-for-byte.
#[derive(Clone, Copy, Debug)]
pub struct ChildCommit<'a> {
    pub step: ParentStep,
    pub child: ChildTicket,
    pub intent: [u8; 32],
    pub result: [u8; 32],
    pub result_json: &'a str,
    pub continuation: &'a [u8],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParentReceipt {
    pub step: ParentStep,
    pub child: ChildTicket,
    pub result: [u8; 32],
}

/// The real durable owner of the parent operation.
///
/// `bind` and `commit` must return only after their record is durable in the
/// owner's committed state. An equal repeat returns the retained record or
/// receipt without advancing the parent again; any different child, intent,
/// result or continuation for the same step is a `Conflict`.
pub trait ParentOwner {
    fn lookup(&self, step: ParentStep) -> Result<Option<ParentRecord>, ParentFailure>;
    fn find_child(&self, child: ChildTicket) -> Result<Option<ParentRecord>, ParentFailure>;
    fn bind(
        &mut self,
        step: ParentStep,
        child: ChildTicket,
        intent: [u8; 32],
    ) -> Result<ParentRecord, ParentFailure>;
    fn commit(&mut self, commit: ChildCommit<'_>) -> Result<ParentReceipt, ParentFailure>;
}

/// Continuation derivation from the exact committed child result.
pub type Continuation<'a> = dyn FnMut(ParentStep, &str) -> Result<Vec<u8>, String> + 'a;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepStart {
    /// A new original row and its mapping are both durable. Nothing was sent.
    Fresh(ChildTicket),
    /// The step's original child is retained; drive it with the same UUID.
    Recovered(ChildTicket),
    /// The parent already committed this step's child result.
    Committed(ChildTicket),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptKind {
    Invoke,
    Observe,
    Cancel,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The exact observation is durable in the sender row.
    Retained,
    /// The receiver has no retained row for this UUID.
    Absent,
    /// Neither variant proves that execution did not occur.
    Unavailable,
    Deadline,
    /// The observation write consumed the reply but its barrier did not
    /// finish. [`SenderRuntime::drive`] resumes that same write.
    Pending,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovered {
    /// The parent committed the exact result and the child row is released.
    Settled(ParentStep),
    /// Mapped but not certain-terminal: observe or resubmit the same UUID.
    Drive(ParentStep, ChildTicket),
    /// Never sent: attempts require a mapping. Only exact-intent adoption by
    /// [`SenderRuntime::begin_step`] may use it; it is never replaced.
    Unmapped(ChildTicket),
}
pub struct Recovery {
    children: [Option<Recovered>; RECOVERY_SLOTS],
}
impl Recovery {
    pub fn iter(&self) -> impl Iterator<Item = Recovered> + '_ {
        self.children.iter().flatten().copied()
    }
}

/// Refusal retains the original committed private state.
pub struct InstallRefusal {
    state: CurrentPrivateState,
    error: SenderError,
}
impl std::fmt::Debug for InstallRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("InstallRefusal").field(&self.error).finish()
    }
}
impl InstallRefusal {
    pub fn error(&self) -> &SenderError {
        &self.error
    }
    pub fn into_state(self) -> CurrentPrivateState {
        self.state
    }
}
/// Refusal retains the original result custody for a later retry.
#[derive(Debug)]
pub struct AcceptRefusal {
    result: ManagedAttemptResult,
    error: SenderError,
}
impl AcceptRefusal {
    pub fn error(&self) -> &SenderError {
        &self.error
    }
    pub fn into_result(self) -> ManagedAttemptResult {
        self.result
    }
}

pub struct SenderRuntime {
    sender: ManagedSender,
    caller: SenderCaller,
}
impl SenderRuntime {
    /// Install over the host's selected, hydrated private state. The state's
    /// instance is preserved; no empty or replacement outbox is created here.
    pub fn install(
        state: CurrentPrivateState,
        caller: SenderCaller,
    ) -> Result<Self, InstallRefusal> {
        match ManagedSender::open(state) {
            Ok(sender) => Ok(Self { sender, caller }),
            Err(refusal) => {
                let error = SenderError::from(refusal.failure());
                Err(InstallRefusal {
                    state: refusal.into_state(),
                    error,
                })
            }
        }
    }
    /// The only route on which this runtime's attempts may be dispatched.
    pub fn route(&self) -> RouteSpec {
        RouteSpec {
            app_id: self.caller.app.clone(),
            cluster_id: self.sender.room(),
            record: (!self.caller.record.is_empty()).then(|| self.caller.record.clone()),
        }
    }
    /// Resume one retained physical barrier after a storage failure.
    pub fn drive(&mut self) -> Result<(), SenderError> {
        if self.sender.has_pending() {
            self.sender.complete()?;
        }
        Ok(())
    }
    fn complete(&mut self, expected: Completion) -> Result<(), SenderError> {
        if self.sender.complete()? != expected {
            return Err(SenderError::Invalid);
        }
        Ok(())
    }
    fn facts(&self, ticket: Ticket) -> Result<Option<super::lifecycle::RowFacts>, SenderError> {
        Ok(self
            .sender
            .inspect(self.caller.view())?
            .into_iter()
            .flatten()
            .find(|facts| facts.ticket == ticket))
    }
    /// Obtain the step's one child. Existing mappings return their original
    /// ticket; a different intent for the same step is refused. A fresh child
    /// is committed to the outbox and mapped by the parent before return.
    pub fn begin_step(
        &mut self,
        parent: &mut dyn ParentOwner,
        step: ParentStep,
        intent: &ChildIntent,
    ) -> Result<StepStart, SenderError> {
        self.drive()?;
        let wanted = intent.digest();
        if let Some(record) = parent.lookup(step)? {
            if record.intent != wanted || record.step != step {
                return Err(SenderError::IdentityConflict);
            }
            if record.committed.is_some() {
                return Ok(StepStart::Committed(record.child));
            }
            // Never replace an uncertain or lost original with a new UUID.
            let facts = self
                .facts(record.child.0)?
                .ok_or(SenderError::NotRetained)?;
            if facts.intent != wanted {
                return Err(SenderError::IdentityConflict);
            }
            return Ok(StepStart::Recovered(record.child));
        }
        // A row committed before its mapping was never sent. Adopt the exact
        // original instead of adding a second row for the same request.
        let unmapped = self
            .sender
            .inspect(self.caller.view())?
            .into_iter()
            .flatten()
            .filter(|facts| facts.intent == wanted && !facts.observed);
        for facts in unmapped {
            let child = ChildTicket(facts.ticket);
            if parent.find_child(child)?.is_none() {
                Self::bound(parent.bind(step, child, wanted)?, step, child, wanted)?;
                return Ok(StepStart::Recovered(child));
            }
        }
        let ticket = self.sender.begin_new(self.caller.view(), intent.view())?;
        self.complete(Completion::Began(ticket))?;
        let child = ChildTicket(ticket);
        Self::bound(parent.bind(step, child, wanted)?, step, child, wanted)?;
        Ok(StepStart::Fresh(child))
    }
    fn bound(
        record: ParentRecord,
        step: ParentStep,
        child: ChildTicket,
        intent: [u8; 32],
    ) -> Result<(), SenderError> {
        if record.step != step || record.child != child || record.intent != intent {
            return Err(SenderError::IdentityConflict);
        }
        Ok(())
    }
    /// One attempt for the step's mapped original child. Invoke resubmits the
    /// original UUID and request; the receiver's durable dedup decides effect.
    pub fn attempt(
        &mut self,
        parent: &dyn ParentOwner,
        step: ParentStep,
        kind: AttemptKind,
        confirmation: &str,
        deadline: Instant,
    ) -> Result<ManagedAttempt, SenderError> {
        let record = parent.lookup(step)?.ok_or(SenderError::Unmapped)?;
        if record.step != step {
            return Err(SenderError::IdentityConflict);
        }
        if record.committed.is_some() {
            return Err(SenderError::NotRetained);
        }
        let operation = match kind {
            AttemptKind::Invoke => Operation::Invoke,
            AttemptKind::Observe => Operation::Observe,
            AttemptKind::Cancel => Operation::Cancel,
        };
        Ok(self.sender.request(
            record.child.0,
            self.caller.view(),
            operation,
            confirmation,
            deadline,
        )?)
    }
    /// Retain one correlated result. Only `Retained` changed durable state.
    pub fn accept(&mut self, result: ManagedAttemptResult) -> Result<Reply, AcceptRefusal> {
        let disposition = match self.sender.observe_result(result, self.caller.view()) {
            Ok(disposition) => disposition,
            Err(refusal) => {
                let error = SenderError::from(refusal.failure());
                return Err(AcceptRefusal {
                    result: refusal.into_result(),
                    error,
                });
            }
        };
        Ok(match disposition {
            ReplyDisposition::Absent => Reply::Absent,
            ReplyDisposition::Transport(AttemptFailure::Deadline) => Reply::Deadline,
            ReplyDisposition::Transport(AttemptFailure::Unavailable) => Reply::Unavailable,
            ReplyDisposition::Queued => {
                // The row write is queued; the reply custody was already
                // consumed into it. A barrier failure is resumed by drive().
                match self.sender.complete() {
                    Ok(Completion::Observed(_)) => Reply::Retained,
                    Ok(_) | Err(_) => Reply::Pending,
                }
            }
        })
    }
    /// Commit the exact terminal result and continuation in the parent, then
    /// release the child row. Repeats after any crash reach the same receipt.
    pub fn settle(
        &mut self,
        parent: &mut dyn ParentOwner,
        step: ParentStep,
        continuation: &mut Continuation<'_>,
    ) -> Result<ParentReceipt, SenderError> {
        self.drive()?;
        let record = parent.lookup(step)?.ok_or(SenderError::Unmapped)?;
        if record.step != step {
            return Err(SenderError::IdentityConflict);
        }
        let ticket = record.child.0;
        let Some(facts) = self.facts(ticket)? else {
            // Consumption committed but its acknowledgement was lost.
            return match record.committed {
                Some(committed) => Ok(ParentReceipt {
                    step,
                    child: record.child,
                    result: committed.result,
                }),
                None => Err(SenderError::NotRetained),
            };
        };
        let result = facts.terminal.ok_or(SenderError::NotTerminal)?;
        let receipt = self.sender.terminal_receipt(ticket, self.caller.view())?;
        if receipt.digest() != result || receipt.ticket() != ticket {
            return Err(SenderError::Invalid);
        }
        match &record.committed {
            Some(committed) if committed.result == result => {}
            Some(_) => return Err(SenderError::IdentityConflict),
            None => {
                let read = self.sender.read_result(ticket, self.caller.view())?;
                let next = continuation(step, read.json()).map_err(SenderError::Continuation)?;
                let committed = parent.commit(ChildCommit {
                    step,
                    child: record.child,
                    intent: record.intent,
                    result,
                    result_json: read.json(),
                    continuation: &next,
                })?;
                if committed
                    != (ParentReceipt {
                        step,
                        child: record.child,
                        result,
                    })
                {
                    return Err(SenderError::IdentityConflict);
                }
            }
        }
        self.sender.consume(receipt, self.caller.view())?;
        self.complete(Completion::Consumed(ticket))?;
        Ok(ParentReceipt {
            step,
            child: record.child,
            result,
        })
    }
    /// Reconcile every retained row of this caller with its parent owner.
    /// Certain terminal children are settled; everything else stays retained.
    pub fn recover(
        &mut self,
        parent: &mut dyn ParentOwner,
        continuation: &mut Continuation<'_>,
    ) -> Result<Recovery, SenderError> {
        self.drive()?;
        let rows = self.sender.inspect(self.caller.view())?;
        let mut recovery = Recovery {
            children: [None; RECOVERY_SLOTS],
        };
        for (slot, facts) in recovery.children.iter_mut().zip(rows.into_iter().flatten()) {
            let child = ChildTicket(facts.ticket);
            *slot = Some(match parent.find_child(child)? {
                None => Recovered::Unmapped(child),
                Some(record) if record.child != child => {
                    return Err(SenderError::IdentityConflict);
                }
                Some(record) if facts.terminal.is_some() => {
                    self.settle(parent, record.step, continuation)?;
                    Recovered::Settled(record.step)
                }
                Some(record) => Recovered::Drive(record.step, child),
            });
        }
        Ok(recovery)
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests;
