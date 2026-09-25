//! Owner-thread consumer of the real private storage phases. No host installer.
use super::{
    ManagedAttempt, ManagedAttemptResult,
    attempt::{AttemptOwner, Operation, Ticket},
    codec::{CallerView, RowView},
};
use crate::host_worker::private_actions::{
    CommittedPrivateWrite, CurrentPrivateBorrow, CurrentPrivateState, FailedPrivateWrite,
    FinishRefusal, PreparedPrivateWrite, PrivateActionsCapture, PrivateActionsFile,
    PrivateQueueOutcome, PrivateWriteOutcome, PrivateWriteRefusal, SenderStorageCode,
};
use crate::time::Instant;
use runtime_collab::PublishedInvocationBinding;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Capacity,
    Busy,
    Invalid,
    NotRetained,
    Scope,
    Storage(SenderStorageCode),
}
#[derive(Clone, Copy)]
pub(super) struct Intent<'a> {
    pub binding: &'a PublishedInvocationBinding,
    pub revision: &'a str,
    pub resource: &'a str,
    pub action: &'a str,
    pub argument: Option<&'a str>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Completion {
    Began(Ticket),
    Observed(Ticket),
    Consumed(Ticket),
}
#[derive(Debug)]
pub(super) struct ObserveRefusal {
    result: ManagedAttemptResult,
    failure: Failure,
}
impl ObserveRefusal {
    pub(super) fn failure(&self) -> Failure {
        self.failure
    }
    pub(super) fn into_result(self) -> ManagedAttemptResult {
        self.result
    }
}
pub(super) struct OpenRefusal {
    state: CurrentPrivateState,
    failure: Failure,
}
impl std::fmt::Debug for OpenRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OpenRefusal").field(&self.failure).finish()
    }
}
impl OpenRefusal {
    pub(super) fn failure(&self) -> Failure {
        self.failure
    }
    pub(super) fn into_state(self) -> CurrentPrivateState {
        self.state
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReplyDisposition {
    Queued,
    Absent,
    Transport(super::AttemptFailure),
}
pub(super) const RECOVERY_SLOTS: usize =
    super::codec::MAX_BYTES / (super::codec::MAX_OBSERVATION + 14);
pub(super) struct ObservationRead<'a> {
    current: CurrentPrivateBorrow<'a>,
    range: std::ops::Range<usize>,
}
impl ObservationRead<'_> {
    #[expect(
        clippy::expect_used,
        reason = "an ObservationRead is only built for a range that validated as UTF-8 JSON"
    )]
    pub(super) fn json(&self) -> &str {
        std::str::from_utf8(&self.current.payload()[self.range.clone()])
            .expect("validated original JSON slice")
    }
}
pub(super) struct TerminalReceipt {
    ticket: Ticket,
    digest: [u8; 32],
}
impl TerminalReceipt {
    pub(super) fn ticket(&self) -> Ticket {
        self.ticket
    }
    /// Digest of the exact committed observation, never of a caller copy.
    pub(super) fn digest(&self) -> [u8; 32] {
        self.digest
    }
}
/// Inert facts about one retained row of this caller. Not a receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RowFacts {
    pub ticket: Ticket,
    /// Digest of the original immutable semantic request.
    pub intent: [u8; 32],
    pub observed: bool,
    /// Present only for a committed certain terminal observation.
    pub terminal: Option<[u8; 32]>,
}
pub(super) struct ManagedSender {
    phase: Option<Phase>,
    pending: Option<Completion>,
    active: [Option<AttemptOwner>; super::codec::MAX_ROWS],
    room: uuid::Uuid,
}
enum Phase {
    Current(CurrentPrivateState),
    Capture(PrivateActionsFile),
    Prepare(PrivateActionsFile, PrivateActionsCapture),
    Prepared(PreparedPrivateWrite),
    Failed(FailedPrivateWrite),
    Refused(PrivateWriteRefusal),
    Committed(CommittedPrivateWrite),
    Finish(FinishRefusal),
}
fn storage(code: SenderStorageCode) -> Failure {
    Failure::Storage(code)
}
fn invalid(error: super::OutboxFailure) -> Failure {
    match error {
        super::OutboxFailure::Capacity => Failure::Capacity,
        super::OutboxFailure::Invalid | super::OutboxFailure::Conflict => Failure::Invalid,
    }
}
fn digest(observation: super::codec::ObservationView<'_>) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"native-managed-terminal-v1\0");
    hash.update(&observation.id.to_le_bytes());
    hash.update(&[
        u8::from(observation.uncertain),
        u8::from(observation.cancellation_requested),
    ]);
    hash.update(observation.json.as_bytes());
    *hash.finalize().as_bytes()
}
impl ManagedSender {
    pub(super) fn room(&self) -> uuid::Uuid {
        self.room
    }
    /// Complete the one queued physical mutation. A storage failure retains
    /// its phase; a later call resumes that same barrier, never a new write.
    pub(super) fn complete(&mut self) -> Result<Completion, Failure> {
        for _ in 0..8 {
            if let Some(done) = self.advance()? {
                return Ok(done);
            }
        }
        Err(Failure::Busy)
    }
    pub(super) fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn inspect(
        &self,
        caller: CallerView<'_>,
    ) -> Result<[Option<RowFacts>; RECOVERY_SLOTS], Failure> {
        let current = self.current()?.borrow_current().map_err(storage)?;
        let view = super::NativeSenderOutboxV1::decode(current.payload()).map_err(invalid)?;
        let mut output = [None; RECOVERY_SLOTS];
        let mut count = 0;
        for row in view.rows() {
            let row = row.map_err(invalid)?;
            if row.caller != caller {
                continue;
            }
            let slot = output.get_mut(count).ok_or(Failure::Capacity)?;
            *slot = Some(RowFacts {
                ticket: Ticket {
                    instance: *current.instance(),
                    host: row.target.host,
                    invocation: row.invocation,
                },
                intent: super::runtime::intent_digest(
                    &row.target,
                    row.revision,
                    row.resource,
                    row.action,
                    row.argument,
                ),
                observed: row.observation.is_some(),
                terminal: row
                    .observation
                    .filter(|value| value.facts.consumable)
                    .map(digest),
            });
            count += 1;
        }
        Ok(output)
    }
    pub(super) fn read_result(
        &self,
        ticket: Ticket,
        caller: CallerView<'_>,
    ) -> Result<ObservationRead<'_>, Failure> {
        let current = self.current()?.borrow_current().map_err(storage)?;
        let range = {
            let view = super::NativeSenderOutboxV1::decode(current.payload()).map_err(invalid)?;
            let row = Self::row(&view, current.instance(), ticket, caller)?;
            let observation = row.observation.ok_or(Failure::NotRetained)?;
            let start = (observation.json.as_ptr() as usize)
                .checked_sub(current.payload().as_ptr() as usize)
                .ok_or(Failure::Invalid)?;
            let end = start
                .checked_add(observation.json.len())
                .filter(|end| *end <= current.payload().len())
                .ok_or(Failure::Invalid)?;
            start..end
        };
        Ok(ObservationRead { current, range })
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn recover_pending(
        &self,
        caller: CallerView<'_>,
    ) -> Result<[Option<Ticket>; RECOVERY_SLOTS], Failure> {
        let current = self.current()?.borrow_current().map_err(storage)?;
        let view = super::NativeSenderOutboxV1::decode(current.payload()).map_err(invalid)?;
        let mut output = [None; RECOVERY_SLOTS];
        let mut count = 0;
        for row in view.rows() {
            let row = row.map_err(invalid)?;
            if row.caller == caller {
                let slot = output.get_mut(count).ok_or(Failure::Capacity)?;
                *slot = Some(Ticket {
                    instance: *current.instance(),
                    host: row.target.host,
                    invocation: row.invocation,
                });
                count += 1;
            }
        }
        Ok(output)
    }
    pub(super) fn open(state: CurrentPrivateState) -> Result<Self, OpenRefusal> {
        let checked = (|| {
            let room = uuid::Uuid::parse_str(state.workspace()).map_err(|_| Failure::Invalid)?;
            if room.is_nil() {
                return Err(Failure::Invalid);
            }
            let current = state.borrow_current().map_err(storage)?;
            super::NativeSenderOutboxV1::decode(current.payload()).map_err(invalid)?;
            Ok(room)
        })();
        match checked {
            Ok(room) => Ok(Self {
                phase: Some(Phase::Current(state)),
                pending: None,
                active: [const { None }; super::codec::MAX_ROWS],
                room,
            }),
            Err(failure) => Err(OpenRefusal { state, failure }),
        }
    }
    /// An occupied slot whose attempt still has copies in flight.
    fn slot_busy(&self, index: usize) -> bool {
        self.active
            .get(index)
            .and_then(Option::as_ref)
            .is_some_and(|owner| !owner.quiescent())
    }
    fn current(&self) -> Result<&CurrentPrivateState, Failure> {
        if self.pending.is_some() {
            return Err(Failure::Busy);
        }
        match self.phase.as_ref() {
            Some(Phase::Current(value)) => Ok(value),
            _ => Err(Failure::Busy),
        }
    }
    fn row<'a>(
        view: &super::NativeSenderOutboxV1<'a>,
        instance: &[u8; 16],
        ticket: Ticket,
        caller: CallerView<'_>,
    ) -> Result<RowView<'a>, Failure> {
        if &ticket.instance != instance {
            return Err(Failure::Scope);
        }
        let row = view
            .find(&ticket.invocation)
            .map_err(invalid)?
            .ok_or(Failure::NotRetained)?;
        if row.caller != caller || row.target.host != ticket.host {
            return Err(Failure::Scope);
        }
        Ok(row)
    }
    fn queue(
        &mut self,
        encoded: super::codec::EncodedOutbox,
        completion: Completion,
    ) -> Result<(), Failure> {
        let current = match self.phase.take() {
            Some(Phase::Current(current)) => current,
            other => {
                self.phase = other;
                return Err(Failure::Busy);
            }
        };
        match current.queue_owned(encoded.into_bytes()) {
            Ok(PrivateQueueOutcome::Pending(file)) => self.phase = Some(Phase::Capture(file)),
            Ok(PrivateQueueOutcome::Unchanged(current)) => {
                self.phase = Some(Phase::Current(current))
            }
            Err(failure) => {
                let code = failure.code();
                let (state, bytes) = failure.into_parts();
                drop(bytes);
                self.phase = Some(Phase::Current(state));
                return Err(storage(code));
            }
        }
        self.pending = Some(completion);
        Ok(())
    }
    pub(super) fn begin_new(
        &mut self,
        caller: CallerView<'_>,
        intent: Intent<'_>,
    ) -> Result<Ticket, Failure> {
        let (encoded, ticket) = {
            let state = self.current()?;
            let current = state.borrow_current().map_err(storage)?;
            let view = super::NativeSenderOutboxV1::decode(current.payload()).map_err(invalid)?;
            let id = *uuid::Uuid::new_v4().as_bytes();
            let row = RowView {
                invocation: id,
                caller,
                target: super::codec::TargetView {
                    host: intent.binding.host,
                    peer: intent.binding.peer_id,
                    app: &intent.binding.app_id,
                    record: &intent.binding.record_id,
                    root: intent.binding.root,
                    device: intent.binding.device,
                    binding: intent.binding.target_binding,
                },
                revision: intent.revision,
                resource: intent.resource,
                action: intent.action,
                argument: intent.argument,
                observation: None,
            };
            let encoded = view.insert(&row).map_err(invalid)?;
            (
                encoded,
                Ticket {
                    instance: *current.instance(),
                    host: row.target.host,
                    invocation: id,
                },
            )
        };
        self.queue(encoded, Completion::Began(ticket))?;
        Ok(ticket)
    }
    pub(super) fn advance(&mut self) -> Result<Option<Completion>, Failure> {
        let phase = self.phase.take().ok_or(Failure::Busy)?;
        let phase = match phase {
            Phase::Current(current) => {
                self.phase = Some(Phase::Current(current));
                return Ok(self.pending.take());
            }
            Phase::Capture(file) => match file.capture().map_err(storage) {
                Ok(capture) => Phase::Prepare(file, capture),
                Err(error) => {
                    self.phase = Some(Phase::Capture(file));
                    return Err(error);
                }
            },
            Phase::Prepare(file, capture) => match file.prepare(capture) {
                Ok(write) => Phase::Prepared(write),
                Err(error) => {
                    let code = error.code();
                    let (file, capture) = error.into_parts();
                    self.phase = Some(Phase::Prepare(file, capture));
                    return Err(storage(code));
                }
            },
            Phase::Prepared(write) => {
                return self.published(write.publish());
            }
            Phase::Failed(failed) => {
                return self.published(failed.retry_commit());
            }
            Phase::Refused(refused) => Phase::Prepared(refused.into_prepared()),
            Phase::Committed(committed) => match committed.finish_current() {
                Ok(current) => {
                    self.phase = Some(Phase::Current(current));
                    return Ok(self.pending.take());
                }
                Err(failure) => {
                    let code = failure.code();
                    self.phase = Some(Phase::Finish(failure));
                    return Err(storage(code));
                }
            },
            Phase::Finish(failure) => Phase::Committed(failure.into_committed()),
        };
        self.phase = Some(phase);
        Ok(None)
    }
    fn published(&mut self, outcome: PrivateWriteOutcome) -> Result<Option<Completion>, Failure> {
        match outcome {
            PrivateWriteOutcome::Committed(value) => {
                self.phase = Some(Phase::Committed(value));
                Ok(None)
            }
            PrivateWriteOutcome::Failed(value) => {
                self.phase = Some(Phase::Failed(value));
                Err(storage(SenderStorageCode::Uncertain))
            }
            PrivateWriteOutcome::Refused(value) => {
                let code = value.code();
                self.phase = Some(Phase::Refused(value));
                Err(storage(code))
            }
        }
    }
    pub(super) fn request(
        &mut self,
        ticket: Ticket,
        caller: CallerView<'_>,
        operation: Operation,
        confirmation: &str,
        deadline: Instant,
    ) -> Result<ManagedAttempt, Failure> {
        let existing = self
            .active
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|owner| owner.ticket() == ticket));
        if existing.is_some_and(|index| self.slot_busy(index)) {
            return Err(Failure::Busy);
        }
        let index = existing
            .or_else(|| self.active.iter().position(Option::is_none))
            .ok_or(Failure::Capacity)?;
        let (owner, attempt) = {
            let state = self.current()?;
            let current = state.borrow_current().map_err(storage)?;
            let view = super::NativeSenderOutboxV1::decode(current.payload()).map_err(invalid)?;
            let row = Self::row(&view, current.instance(), ticket, caller)?;
            AttemptOwner::new(
                &row,
                ticket.instance,
                self.room,
                operation,
                confirmation,
                deadline,
            )
            .map_err(invalid)?
        };
        self.active[index] = Some(owner);
        Ok(attempt)
    }
    pub(super) fn observe_result(
        &mut self,
        result: ManagedAttemptResult,
        caller: CallerView<'_>,
    ) -> Result<ReplyDisposition, ObserveRefusal> {
        match self.queue_result(&result, caller) {
            Ok(disposition) => Ok(disposition),
            Err(failure) => Err(ObserveRefusal { result, failure }),
        }
    }
    fn queue_result(
        &mut self,
        result: &ManagedAttemptResult,
        caller: CallerView<'_>,
    ) -> Result<ReplyDisposition, Failure> {
        let owner = self
            .active
            .iter()
            .flatten()
            .find(|owner| owner.matches(result))
            .ok_or(Failure::Scope)?;
        let ticket = owner.ticket();
        let encoded = {
            let state = self.current()?;
            let current = state.borrow_current().map_err(storage)?;
            let view = super::NativeSenderOutboxV1::decode(current.payload()).map_err(invalid)?;
            Self::row(&view, current.instance(), ticket, caller)?;
            let observation = match result.result() {
                Ok(observation) => observation,
                Err(error) => return Ok(ReplyDisposition::Transport(*error)),
            };
            if observation.host != ticket.host || observation.invocation != ticket.invocation {
                return Err(Failure::Scope);
            }
            let Some(response) = &observation.result else {
                return Ok(ReplyDisposition::Absent);
            };
            view.with_observation(&ticket.invocation, response)
                .map_err(invalid)?
        };
        self.queue(encoded, Completion::Observed(ticket))?;
        Ok(ReplyDisposition::Queued)
    }
    pub(super) fn terminal_receipt(
        &self,
        ticket: Ticket,
        caller: CallerView<'_>,
    ) -> Result<TerminalReceipt, Failure> {
        let state = self.current()?;
        let current = state.borrow_current().map_err(storage)?;
        let view = super::NativeSenderOutboxV1::decode(current.payload()).map_err(invalid)?;
        let row = Self::row(&view, current.instance(), ticket, caller)?;
        let observation = row
            .observation
            .filter(|value| value.facts.consumable)
            .ok_or(Failure::Invalid)?;
        Ok(TerminalReceipt {
            ticket,
            digest: digest(observation),
        })
    }
    pub(super) fn consume(
        &mut self,
        receipt: TerminalReceipt,
        caller: CallerView<'_>,
    ) -> Result<(), Failure> {
        let ticket = receipt.ticket;
        let index = self
            .active
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|owner| owner.ticket() == ticket));
        if index.is_some_and(|index| self.slot_busy(index)) {
            return Err(Failure::Busy);
        }
        let encoded = {
            let state = self.current()?;
            let current = state.borrow_current().map_err(storage)?;
            let view = super::NativeSenderOutboxV1::decode(current.payload()).map_err(invalid)?;
            let row = Self::row(&view, current.instance(), ticket, caller)?;
            let observation = row
                .observation
                .filter(|value| value.facts.consumable)
                .ok_or(Failure::Invalid)?;
            if digest(observation) != receipt.digest {
                return Err(Failure::Scope);
            }
            view.consume(&ticket.invocation).map_err(invalid)?
        };
        if let Some(index) = index {
            self.active[index] = None;
        }
        self.queue(encoded, Completion::Consumed(ticket))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests;

#[cfg(all(test, target_os = "macos"))]
pub(crate) mod transport_fixture;
