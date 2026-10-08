//! Broker-addressed conversational Messages for a managed Agent.
//!
//! Message delivery carries one explicit text value, not a Chat replica and
//! not an action grant. The worker accepts owner custody before asking the
//! model, gives the model an empty action catalogue, and retains immutable
//! output across an uncertain publication so redelivery cannot run the model
//! twice.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use runtime_collab::AgentMessageInput;
use ui::collaboration::CollabRequest;

use crate::model::{Chooser, Route};
use crate::tools::Answer;
#[cfg(test)]
use crate::wire::AgentMessageRpcOutcome;
use crate::wire::Link;

const MAX_RETAINED_MESSAGES: usize = 128;
const RPC_RESPONSE_WINDOW: Duration = Duration::from_secs(10);
const MAX_OBSERVATIONS_PER_PHASE: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PendingPhase {
    Acceptance,
    Preparation,
    Commit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ManagedMessageOutcome {
    Completed,
    PendingModel,
    Refused,
    Uncertain(PendingPhase),
    Disconnected,
}

enum RetainedStage {
    AwaitingAcceptance,
    Generating,
    PreparePending(String),
    CommitPending(String),
}

struct RetainedMessage {
    input: AgentMessageInput,
    stage: RetainedStage,
    request_ids: [u64; 3],
    pending_rpc: Option<PendingRpc>,
    uncertain: Option<PendingPhase>,
    observation_attempts: u8,
}

#[derive(Clone, Copy)]
struct PendingRpc {
    request_id: u64,
    phase: PendingPhase,
    deadline: Instant,
    observing: bool,
}

/// Session-private custody for delivered Messages and their immutable output.
///
/// Only unsettled work consumes the bounded process-local retention. A
/// definitive commit acknowledgement removes it: the durable owner receipt
/// then rejects stale delivery before the model can run again. Generated
/// output stays immutable after either an uncertain or refused publication.
pub(super) struct ManagedMessages {
    recipient: [String; 3],
    retained: BTreeMap<String, RetainedMessage>,
    worker: Option<ModelWorker>,
}

struct ModelWorker {
    jobs: Sender<(String, String)>,
    replies: Receiver<(String, String)>,
}

#[cfg(test)]
trait MessageRpc {
    fn accept(&mut self, input: &AgentMessageInput, request_id: u64) -> AgentMessageRpcOutcome;
    fn prepare(
        &mut self,
        input: &AgentMessageInput,
        text: &str,
        request_id: u64,
    ) -> AgentMessageRpcOutcome;
    fn commit(&mut self, input: &AgentMessageInput, request_id: u64) -> AgentMessageRpcOutcome;
}

#[cfg(test)]
impl MessageRpc for Link {
    fn accept(&mut self, input: &AgentMessageInput, request_id: u64) -> AgentMessageRpcOutcome {
        if self
            .send(&CollabRequest::AcceptAgentMessage {
                request_id,
                invocation: input.invocation.clone(),
                source_digest: input.source_digest,
            })
            .is_err()
        {
            return AgentMessageRpcOutcome::Disconnected(String::new());
        }
        self.wait_agent_message_reply(request_id)
            .unwrap_or_else(|_| AgentMessageRpcOutcome::Disconnected(String::new()))
    }

    fn prepare(
        &mut self,
        input: &AgentMessageInput,
        text: &str,
        request_id: u64,
    ) -> AgentMessageRpcOutcome {
        if self
            .send(&CollabRequest::PrepareAgentMessageReply {
                request_id,
                invocation: input.invocation.clone(),
                source_digest: input.source_digest,
                text: text.to_owned(),
            })
            .is_err()
        {
            return AgentMessageRpcOutcome::Disconnected(String::new());
        }
        self.wait_agent_message_reply(request_id)
            .unwrap_or_else(|_| AgentMessageRpcOutcome::Disconnected(String::new()))
    }

    fn commit(&mut self, input: &AgentMessageInput, request_id: u64) -> AgentMessageRpcOutcome {
        if self
            .send(&CollabRequest::CommitAgentMessageReply {
                request_id,
                invocation: input.invocation.clone(),
                source_digest: input.source_digest,
            })
            .is_err()
        {
            return AgentMessageRpcOutcome::Disconnected(String::new());
        }
        self.wait_agent_message_reply(request_id)
            .unwrap_or_else(|_| AgentMessageRpcOutcome::Disconnected(String::new()))
    }
}

trait Responder: Send {
    fn answer(&self, text: &str) -> String;
}

impl Responder for Chooser {
    fn answer(&self, text: &str) -> String {
        conversational_reply(self, text)
    }
}

impl ManagedMessages {
    pub(super) fn new(room: &str, agent: &str, launch: &str, chooser: Chooser) -> Self {
        Self::with_worker(room, agent, launch, chooser)
    }

    fn with_worker(
        room: &str,
        agent: &str,
        launch: &str,
        responder: impl Responder + 'static,
    ) -> Self {
        let (jobs_tx, jobs_rx) = channel::<(String, String)>();
        let (replies_tx, replies_rx) = channel();
        let spawned = std::thread::Builder::new()
            .name("agent-message-model".into())
            .spawn(move || {
                while let Ok((invocation, text)) = jobs_rx.recv() {
                    if replies_tx
                        .send((invocation, responder.answer(&text)))
                        .is_err()
                    {
                        break;
                    }
                }
            });
        Self {
            recipient: [room, agent, launch].map(str::to_owned),
            retained: BTreeMap::new(),
            worker: spawned.ok().map(|_| ModelWorker {
                jobs: jobs_tx,
                replies: replies_rx,
            }),
        }
    }

    pub(super) fn handle(
        &mut self,
        link: &mut Link,
        input: AgentMessageInput,
        request_ids: [u64; 3],
    ) -> ManagedMessageOutcome {
        if input.validate().is_err() || !self.matches_recipient(&input) {
            return ManagedMessageOutcome::Refused;
        }
        let invocation = input.invocation.clone();
        if let Some(retained) = self.retained.get_mut(&invocation) {
            if !same_source(&retained.input, &input) || prepared_conflicts(retained, &input) {
                return ManagedMessageOutcome::Refused;
            }
            if retained.pending_rpc.is_some() {
                return pending_outcome(&retained.stage);
            }
            retained.request_ids = request_ids;
            if let Some(phase) = retained.uncertain {
                return self.observe_current(link, &invocation, phase);
            }
        } else {
            if self.retained.len() >= MAX_RETAINED_MESSAGES {
                return ManagedMessageOutcome::Refused;
            }
            self.retained.insert(
                invocation.clone(),
                RetainedMessage {
                    input,
                    stage: RetainedStage::AwaitingAcceptance,
                    request_ids,
                    pending_rpc: None,
                    uncertain: None,
                    observation_attempts: 0,
                },
            );
        }
        self.send_current(link, &invocation)
    }

    /// Incorporate completed model turns without waiting for one.
    pub(super) fn poll_ready(&mut self, link: &mut Link) -> Vec<ManagedMessageOutcome> {
        let Some(worker) = &self.worker else {
            return Vec::new();
        };
        let mut replies = Vec::new();
        while let Ok(reply) = worker.replies.try_recv() {
            replies.push(reply);
        }
        let mut outcomes = Vec::with_capacity(replies.len());
        for (invocation, reply) in replies {
            if matches!(
                self.retained.get(&invocation).map(|message| &message.stage),
                Some(RetainedStage::Generating)
            ) {
                self.set_stage(&invocation, RetainedStage::PreparePending(reply));
                outcomes.push(self.send_current(link, &invocation));
            }
        }
        outcomes
    }

    /// Advance one exact correlated broker acknowledgement without waiting.
    pub(super) fn handle_reply(
        &mut self,
        link: &mut Link,
        request_id: u64,
        result: &Result<(), String>,
    ) -> ManagedMessageOutcome {
        let Some(invocation) = self.retained.iter().find_map(|(invocation, retained)| {
            retained
                .pending_rpc
                .filter(|pending| pending.request_id == request_id && !pending.observing)
                .map(|_| invocation.clone())
        }) else {
            return ManagedMessageOutcome::Refused;
        };
        let Some(retained) = self.retained.get_mut(&invocation) else {
            return ManagedMessageOutcome::Refused;
        };
        let Some(pending) = retained.pending_rpc.take() else {
            return ManagedMessageOutcome::Refused;
        };
        if result.is_err() {
            retained.uncertain = Some(pending.phase);
            return self.observe_current(link, &invocation, pending.phase);
        }
        self.advance_observed(link, &invocation, pending.phase)
    }

    fn advance_observed(
        &mut self,
        link: &mut Link,
        invocation: &str,
        phase: PendingPhase,
    ) -> ManagedMessageOutcome {
        let Some(retained) = self.retained.get_mut(invocation) else {
            return ManagedMessageOutcome::Refused;
        };
        retained.uncertain = None;
        match phase {
            PendingPhase::Acceptance => {
                if let Some((_, reply)) = &retained.input.prepared_reply {
                    retained.stage = RetainedStage::CommitPending(reply.clone());
                    self.send_current(link, invocation)
                } else {
                    let Some(worker) = &self.worker else {
                        return ManagedMessageOutcome::Disconnected;
                    };
                    if worker
                        .jobs
                        .send((invocation.to_owned(), retained.input.text.clone()))
                        .is_err()
                    {
                        return ManagedMessageOutcome::Disconnected;
                    }
                    retained.stage = RetainedStage::Generating;
                    ManagedMessageOutcome::PendingModel
                }
            }
            PendingPhase::Preparation => {
                let RetainedStage::PreparePending(reply) = &retained.stage else {
                    return ManagedMessageOutcome::Refused;
                };
                retained.stage = RetainedStage::CommitPending(reply.clone());
                self.send_current(link, invocation)
            }
            PendingPhase::Commit => {
                self.retained.remove(invocation);
                ManagedMessageOutcome::Completed
            }
        }
    }

    /// Observe after a lost effect acknowledgement; never resend that effect.
    /// An unanswered observation remains uncertain with immutable custody held.
    pub(super) fn expire(&mut self, link: &mut Link, now: Instant) -> Vec<ManagedMessageOutcome> {
        let expired = self
            .retained
            .iter()
            .filter_map(|(id, retained)| {
                retained
                    .pending_rpc
                    .filter(|pending| pending.deadline <= now)
                    .map(|pending| (id.clone(), pending))
            })
            .collect::<Vec<_>>();
        let mut outcomes = Vec::with_capacity(expired.len());
        for (id, pending) in expired {
            let Some(retained) = self.retained.get_mut(&id) else {
                outcomes.push(ManagedMessageOutcome::Refused);
                continue;
            };
            retained.pending_rpc = None;
            retained.uncertain = Some(pending.phase);
            let outcome = if pending.observing
                && retained.observation_attempts >= MAX_OBSERVATIONS_PER_PHASE
            {
                ManagedMessageOutcome::Uncertain(pending.phase)
            } else {
                self.observe_current(link, &id, pending.phase)
            };
            outcomes.push(outcome);
        }
        outcomes
    }

    fn observe_current(
        &mut self,
        link: &mut Link,
        invocation: &str,
        phase: PendingPhase,
    ) -> ManagedMessageOutcome {
        let Some(retained) = self.retained.get_mut(invocation) else {
            return ManagedMessageOutcome::Refused;
        };
        retained.uncertain = Some(phase);
        if retained.observation_attempts >= MAX_OBSERVATIONS_PER_PHASE {
            return ManagedMessageOutcome::Uncertain(phase);
        }
        retained.observation_attempts += 1;
        let [accept, prepare, commit] = retained.request_ids;
        let request_id = match phase {
            PendingPhase::Acceptance => accept,
            PendingPhase::Preparation => prepare,
            PendingPhase::Commit => commit,
        };
        if link
            .send(&CollabRequest::ObserveAgentMessage {
                request_id,
                invocation: retained.input.invocation.clone(),
                source_digest: retained.input.source_digest,
            })
            .is_err()
        {
            return ManagedMessageOutcome::Disconnected;
        }
        retained.pending_rpc = Some(PendingRpc {
            request_id,
            phase,
            deadline: Instant::now()
                + RPC_RESPONSE_WINDOW * u32::from(retained.observation_attempts),
            observing: true,
        });
        ManagedMessageOutcome::Uncertain(phase)
    }

    /// Only actual owner observation can resolve an uncertain acknowledgement.
    /// Foreign output, obsolete source and transport errors retain uncertainty.
    pub(super) fn handle_observation(
        &mut self,
        link: &mut Link,
        request_id: u64,
        result: &Result<runtime_collab::AgentMessageObservation, String>,
    ) -> ManagedMessageOutcome {
        use runtime_collab::AgentMessageObservation as Observed;
        let Some((invocation, pending)) = self.retained.iter().find_map(|(id, retained)| {
            retained
                .pending_rpc
                .filter(|p| p.observing && p.request_id == request_id)
                .map(|p| (id.clone(), p))
        }) else {
            return ManagedMessageOutcome::Refused;
        };
        let Some(retained) = self.retained.get_mut(&invocation) else {
            return ManagedMessageOutcome::Refused;
        };
        retained.pending_rpc = None;
        let Ok(observed) = result else {
            return ManagedMessageOutcome::Uncertain(pending.phase);
        };
        if observed.validate().is_err() {
            return ManagedMessageOutcome::Uncertain(pending.phase);
        }
        if let Observed::Prepared { reply, .. } | Observed::Committed { reply } = observed
            && reply != &expected_reply(&retained.input)
        {
            return ManagedMessageOutcome::Uncertain(pending.phase);
        }
        match observed {
            Observed::Accepted if pending.phase == PendingPhase::Acceptance => {
                self.advance_observed(link, &invocation, PendingPhase::Acceptance)
            }
            Observed::Prepared { reply, text } if pending.phase != PendingPhase::Commit => {
                let matches_output = match &retained.stage {
                    RetainedStage::PreparePending(output)
                    | RetainedStage::CommitPending(output) => output == text,
                    RetainedStage::AwaitingAcceptance => retained
                        .input
                        .prepared_reply
                        .as_ref()
                        .is_some_and(|(id, output)| id == reply && output == text),
                    RetainedStage::Generating => false,
                };
                if !matches_output {
                    return ManagedMessageOutcome::Uncertain(pending.phase);
                }
                retained.stage = RetainedStage::CommitPending(text.clone());
                retained.uncertain = None;
                self.send_current(link, &invocation)
            }
            Observed::Committed { .. } if pending.phase == PendingPhase::Commit => {
                self.retained.remove(&invocation);
                ManagedMessageOutcome::Completed
            }
            Observed::NeedsAttention => ManagedMessageOutcome::Refused,
            _ => ManagedMessageOutcome::Uncertain(pending.phase),
        }
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.retained
            .values()
            .filter_map(|retained| retained.pending_rpc.map(|pending| pending.deadline))
            .min()
    }

    fn send_current(&mut self, link: &mut Link, invocation: &str) -> ManagedMessageOutcome {
        let Some(retained) = self.retained.get_mut(invocation) else {
            return ManagedMessageOutcome::Refused;
        };
        let (request, request_id, phase) = match &retained.stage {
            RetainedStage::AwaitingAcceptance => (
                CollabRequest::AcceptAgentMessage {
                    request_id: retained.request_ids[0],
                    invocation: retained.input.invocation.clone(),
                    source_digest: retained.input.source_digest,
                },
                retained.request_ids[0],
                PendingPhase::Acceptance,
            ),
            RetainedStage::PreparePending(reply) => (
                CollabRequest::PrepareAgentMessageReply {
                    request_id: retained.request_ids[1],
                    invocation: retained.input.invocation.clone(),
                    source_digest: retained.input.source_digest,
                    text: reply.clone(),
                },
                retained.request_ids[1],
                PendingPhase::Preparation,
            ),
            RetainedStage::CommitPending(_) => (
                CollabRequest::CommitAgentMessageReply {
                    request_id: retained.request_ids[2],
                    invocation: retained.input.invocation.clone(),
                    source_digest: retained.input.source_digest,
                },
                retained.request_ids[2],
                PendingPhase::Commit,
            ),
            RetainedStage::Generating => return ManagedMessageOutcome::PendingModel,
        };
        if link.send(&request).is_err() {
            return ManagedMessageOutcome::Disconnected;
        }
        retained.pending_rpc = Some(PendingRpc {
            request_id,
            phase,
            deadline: Instant::now() + RPC_RESPONSE_WINDOW,
            observing: false,
        });
        retained.observation_attempts = 0;
        ManagedMessageOutcome::Uncertain(phase)
    }

    #[cfg(test)]
    fn handle_with(
        &mut self,
        rpc: &mut impl MessageRpc,
        responder: &impl Responder,
        input: &AgentMessageInput,
        request_ids: [u64; 3],
    ) -> ManagedMessageOutcome {
        self.receive_inner(rpc, input, request_ids, Some(responder))
    }

    #[cfg(test)]
    fn receive_inner(
        &mut self,
        rpc: &mut impl MessageRpc,
        input: &AgentMessageInput,
        request_ids: [u64; 3],
        synchronous: Option<&impl Responder>,
    ) -> ManagedMessageOutcome {
        if input.validate().is_err() || !self.matches_recipient(input) {
            return ManagedMessageOutcome::Refused;
        }
        let invocation = input.invocation.clone();
        if let Some(retained) = self.retained.get(&invocation) {
            if !same_source(&retained.input, input) || prepared_conflicts(retained, input) {
                return ManagedMessageOutcome::Refused;
            }
            let retained_reply = match &retained.stage {
                RetainedStage::AwaitingAcceptance | RetainedStage::Generating => None,
                RetainedStage::PreparePending(reply) => {
                    Some((reply.clone(), input.prepared_reply.is_some()))
                }
                RetainedStage::CommitPending(reply) => Some((reply.clone(), true)),
            };
            if let Some((reply, prepared)) = retained_reply {
                let outcome = if prepared {
                    commit_reply(rpc, input, request_ids[2])
                } else {
                    let (outcome, prepared) =
                        prepare_then_commit(rpc, input, &reply, request_ids[1], request_ids[2]);
                    if prepared {
                        self.set_stage(&invocation, RetainedStage::CommitPending(reply.clone()));
                    }
                    outcome
                };
                self.record_reply_outcome(&invocation, &outcome);
                return outcome;
            }
            if matches!(retained.stage, RetainedStage::Generating) {
                return ManagedMessageOutcome::PendingModel;
            }
            // An explicit, authenticated redelivery is the only retry door
            // after a lost Accept acknowledgement. The host revalidates the
            // same process and source; no timer drives this path.
        } else {
            if self.retained.len() >= MAX_RETAINED_MESSAGES {
                return ManagedMessageOutcome::Refused;
            }
            self.retained.insert(
                invocation.clone(),
                RetainedMessage {
                    input: input.clone(),
                    stage: RetainedStage::AwaitingAcceptance,
                    request_ids,
                    pending_rpc: None,
                    uncertain: None,
                    observation_attempts: 0,
                },
            );
        }
        let acceptance = rpc.accept(input, request_ids[0]);
        match acceptance {
            AgentMessageRpcOutcome::Accepted => {}
            AgentMessageRpcOutcome::Uncertain => {
                return ManagedMessageOutcome::Uncertain(PendingPhase::Acceptance);
            }
            AgentMessageRpcOutcome::Disconnected(_) => {
                return ManagedMessageOutcome::Disconnected;
            }
        }

        let Some(reply) = input.prepared_reply.as_ref().map(|(_, text)| text.clone()) else {
            if let Some(responder) = synchronous {
                let reply = responder.answer(&input.text);
                self.set_stage(&invocation, RetainedStage::PreparePending(reply.clone()));
                let (outcome, prepared) =
                    prepare_then_commit(rpc, input, &reply, request_ids[1], request_ids[2]);
                if prepared {
                    self.set_stage(&invocation, RetainedStage::CommitPending(reply));
                }
                self.record_reply_outcome(&invocation, &outcome);
                return outcome;
            }
            let Some(worker) = &self.worker else {
                return ManagedMessageOutcome::Disconnected;
            };
            if worker
                .jobs
                .send((invocation.clone(), input.text.clone()))
                .is_err()
            {
                return ManagedMessageOutcome::Disconnected;
            }
            self.set_stage(&invocation, RetainedStage::Generating);
            return ManagedMessageOutcome::PendingModel;
        };
        let outcome = if input.prepared_reply.is_some() {
            self.set_stage(&invocation, RetainedStage::CommitPending(reply));
            commit_reply(rpc, input, request_ids[2])
        } else {
            self.set_stage(&invocation, RetainedStage::PreparePending(reply.clone()));
            let (outcome, prepared) =
                prepare_then_commit(rpc, input, &reply, request_ids[1], request_ids[2]);
            if prepared {
                self.set_stage(&invocation, RetainedStage::CommitPending(reply));
            }
            outcome
        };
        self.record_reply_outcome(&invocation, &outcome);
        outcome
    }

    fn set_stage(&mut self, invocation: &str, stage: RetainedStage) {
        if let Some(retained) = self.retained.get_mut(invocation) {
            retained.stage = stage;
        }
    }

    fn matches_recipient(&self, input: &AgentMessageInput) -> bool {
        self.recipient
            == [
                input.room.clone(),
                input.agent.clone(),
                input.launch.clone(),
            ]
    }

    #[cfg(test)]
    fn record_reply_outcome(&mut self, invocation: &str, outcome: &ManagedMessageOutcome) {
        if *outcome == ManagedMessageOutcome::Completed {
            self.retained.remove(invocation);
        }
    }
}

fn pending_outcome(stage: &RetainedStage) -> ManagedMessageOutcome {
    match stage {
        RetainedStage::AwaitingAcceptance => {
            ManagedMessageOutcome::Uncertain(PendingPhase::Acceptance)
        }
        RetainedStage::Generating => ManagedMessageOutcome::PendingModel,
        RetainedStage::PreparePending(_) => {
            ManagedMessageOutcome::Uncertain(PendingPhase::Preparation)
        }
        RetainedStage::CommitPending(_) => ManagedMessageOutcome::Uncertain(PendingPhase::Commit),
    }
}

#[cfg(test)]
fn prepare_then_commit(
    rpc: &mut impl MessageRpc,
    input: &AgentMessageInput,
    reply: &str,
    prepare_request: u64,
    commit_request: u64,
) -> (ManagedMessageOutcome, bool) {
    match rpc.prepare(input, reply, prepare_request) {
        AgentMessageRpcOutcome::Accepted => (commit_reply(rpc, input, commit_request), true),
        AgentMessageRpcOutcome::Uncertain => (
            ManagedMessageOutcome::Uncertain(PendingPhase::Preparation),
            false,
        ),
        AgentMessageRpcOutcome::Disconnected(_) => (ManagedMessageOutcome::Disconnected, false),
    }
}

#[cfg(test)]
fn commit_reply(
    rpc: &mut impl MessageRpc,
    input: &AgentMessageInput,
    request_id: u64,
) -> ManagedMessageOutcome {
    match rpc.commit(input, request_id) {
        AgentMessageRpcOutcome::Accepted => ManagedMessageOutcome::Completed,
        AgentMessageRpcOutcome::Uncertain => ManagedMessageOutcome::Uncertain(PendingPhase::Commit),
        AgentMessageRpcOutcome::Disconnected(_) => ManagedMessageOutcome::Disconnected,
    }
}

fn conversational_reply(chooser: &Chooser, text: &str) -> String {
    match chooser.choose_conversation(text, &[]) {
        Ok(answer) => textual_reply(answer),
        Err(Route::Model { .. } | Route::Keywords(_)) => {
            crate::i18n::t("agent-message-model-unavailable")
        }
    }
}

fn textual_reply(answer: Answer) -> String {
    if !answer.actions.is_empty() {
        return crate::i18n::t("agent-message-actions-not-run");
    }
    let reply = answer
        .reply
        .filter(|reply| !reply.trim().is_empty())
        .unwrap_or_else(|| crate::i18n::t("agent-message-no-reply"));
    if reply.len() > chat::doc::MAX_MESSAGE_TEXT_BYTES {
        return crate::i18n::t("agent-message-reply-too-long");
    }
    reply
}

fn same_source(retained: &AgentMessageInput, delivered: &AgentMessageInput) -> bool {
    retained.invocation == delivered.invocation
        && retained.room == delivered.room
        && retained.agent == delivered.agent
        && retained.channel == delivered.channel
        && retained.request_row == delivered.request_row
        && retained.text == delivered.text
        && retained.source_digest == delivered.source_digest
        && retained.launch == delivered.launch
}

fn expected_reply(input: &AgentMessageInput) -> String {
    collab_types::document_artifact_id(
        "tilo/native-agent-message-reply/v1",
        &input.agent,
        &format!("{}:{}", input.room, input.invocation),
    )
    .0
    .to_string()
}

fn prepared_conflicts(retained: &RetainedMessage, delivered: &AgentMessageInput) -> bool {
    let Some(delivered_prepared) = &delivered.prepared_reply else {
        return false;
    };
    if let Some(original_prepared) = &retained.input.prepared_reply
        && original_prepared != delivered_prepared
    {
        return true;
    }
    matches!(
        &retained.stage,
        RetainedStage::PreparePending(reply) | RetainedStage::CommitPending(reply)
            if reply != &delivered_prepared.1
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct FakeResponder {
        calls: Cell<usize>,
        reply: String,
    }

    impl Responder for FakeResponder {
        fn answer(&self, _: &str) -> String {
            self.calls.set(self.calls.get() + 1);
            self.reply.clone()
        }
    }

    struct WorkerResponder {
        calls: Arc<AtomicUsize>,
        reply: String,
    }

    impl Responder for WorkerResponder {
        fn answer(&self, _: &str) -> String {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.reply.clone()
        }
    }

    #[derive(Default)]
    struct FakeRpc {
        accepts: VecDeque<AgentMessageRpcOutcome>,
        prepares: VecDeque<AgentMessageRpcOutcome>,
        commits: VecDeque<AgentMessageRpcOutcome>,
        accepted: Vec<(String, [u8; 32], u64)>,
        prepared: Vec<(String, [u8; 32], String, u64)>,
        committed: Vec<(String, [u8; 32], u64)>,
    }

    impl MessageRpc for FakeRpc {
        fn accept(&mut self, input: &AgentMessageInput, request_id: u64) -> AgentMessageRpcOutcome {
            self.accepted
                .push((input.invocation.clone(), input.source_digest, request_id));
            self.accepts.pop_front().unwrap()
        }

        fn prepare(
            &mut self,
            input: &AgentMessageInput,
            text: &str,
            request_id: u64,
        ) -> AgentMessageRpcOutcome {
            self.prepared.push((
                input.invocation.clone(),
                input.source_digest,
                text.to_owned(),
                request_id,
            ));
            self.prepares.pop_front().unwrap()
        }

        fn commit(&mut self, input: &AgentMessageInput, request_id: u64) -> AgentMessageRpcOutcome {
            self.committed
                .push((input.invocation.clone(), input.source_digest, request_id));
            self.commits.pop_front().unwrap()
        }
    }

    struct MessagePeer {
        sent: Arc<Mutex<Vec<CollabRequest>>>,
        events: Vec<ui::collaboration::CollabEvent>,
    }

    impl ui::collaboration::PeerTransport for MessagePeer {
        fn is_connected(&self) -> bool {
            true
        }

        fn send(&mut self, request: CollabRequest) -> std::result::Result<(), String> {
            self.sent.lock().unwrap().push(request);
            Ok(())
        }

        fn poll(&mut self) -> Vec<ui::collaboration::CollabEvent> {
            std::mem::take(&mut self.events)
        }
    }

    fn input() -> AgentMessageInput {
        AgentMessageInput {
            invocation: "12345678-1234-1234-1234-123456789abc".into(),
            room: "22345678-1234-1234-1234-123456789abc".into(),
            agent: "agent-1".into(),
            channel: "agent-conversation".into(),
            request_row: "m_12345678123412341234123456789abc".into(),
            text: "Please explain the result".into(),
            source_digest: [9; 32],
            launch: "32345678-1234-1234-1234-123456789abc".into(),
            prepared_reply: None,
        }
    }

    fn messages() -> ManagedMessages {
        ManagedMessages {
            recipient: [
                "22345678-1234-1234-1234-123456789abc",
                "agent-1",
                "32345678-1234-1234-1234-123456789abc",
            ]
            .map(str::to_owned),
            retained: BTreeMap::new(),
            worker: None,
        }
    }

    #[test]
    fn action_plan_is_reported_as_not_run() {
        let answer = Answer {
            actions: vec!["invented#action".into()],
            reply: Some("I changed it".into()),
            ..Answer::default()
        };
        assert_eq!(
            textual_reply(answer),
            crate::i18n::t("agent-message-actions-not-run")
        );
    }

    #[test]
    fn oversized_generated_reply_becomes_bounded_localized_status() {
        let answer = Answer {
            reply: Some("x".repeat(chat::doc::MAX_MESSAGE_TEXT_BYTES + 1)),
            ..Answer::default()
        };
        let reply = textual_reply(answer);
        assert_eq!(reply, crate::i18n::t("agent-message-reply-too-long"));
        assert!(reply.len() <= chat::doc::MAX_MESSAGE_TEXT_BYTES);
    }

    #[test]
    fn duplicate_identity_requires_every_original_source_field() {
        let original = input();
        let mut changed = original.clone();
        changed.text = "Different text".into();
        assert!(!same_source(&original, &changed));
        changed = original.clone();
        changed.source_digest = [8; 32];
        assert!(!same_source(&original, &changed));
        changed = original.clone();
        changed.prepared_reply = Some((
            "42345678-1234-1234-1234-123456789abc".into(),
            "Owner-retained reply".into(),
        ));
        assert!(same_source(&original, &changed));
    }

    #[test]
    fn owner_prepared_output_must_match_retained_immutable_output() {
        let mut delivered = input();
        delivered.prepared_reply = Some((
            "42345678-1234-1234-1234-123456789abc".into(),
            "Another reply".into(),
        ));
        let retained = RetainedMessage {
            input: input(),
            stage: RetainedStage::CommitPending("Original reply".into()),
            request_ids: [1, 2, 3],
            pending_rpc: None,
            uncertain: None,
            observation_attempts: 0,
        };
        assert!(prepared_conflicts(&retained, &delivered));
        delivered.prepared_reply.as_mut().unwrap().1 = "Original reply".into();
        assert!(!prepared_conflicts(&retained, &delivered));

        let mut originally_prepared = retained;
        originally_prepared.input.prepared_reply = delivered.prepared_reply.clone();
        delivered.prepared_reply.as_mut().unwrap().0 =
            "52345678-1234-1234-1234-123456789abc".into();
        assert!(prepared_conflicts(&originally_prepared, &delivered));
    }

    #[test]
    fn lost_accept_ack_waits_for_exact_redelivery_before_one_model_call() {
        let responder = FakeResponder {
            calls: Cell::new(0),
            reply: "One answer".into(),
        };
        let mut rpc = FakeRpc {
            accepts: VecDeque::from([
                AgentMessageRpcOutcome::Uncertain,
                AgentMessageRpcOutcome::Accepted,
            ]),
            prepares: VecDeque::from([AgentMessageRpcOutcome::Accepted]),
            commits: VecDeque::from([AgentMessageRpcOutcome::Accepted]),
            ..FakeRpc::default()
        };
        let mut messages = messages();
        let delivered = input();

        assert_eq!(
            messages.handle_with(&mut rpc, &responder, &delivered, [1, 2, 3]),
            ManagedMessageOutcome::Uncertain(PendingPhase::Acceptance)
        );
        assert_eq!(responder.calls.get(), 0);
        assert_eq!(
            messages.handle_with(&mut rpc, &responder, &delivered, [4, 5, 6]),
            ManagedMessageOutcome::Completed
        );
        assert_eq!(responder.calls.get(), 1);
        assert_eq!(rpc.accepted.len(), 2);
        assert_eq!(rpc.prepared.len(), 1);
        assert_eq!(rpc.prepared[0].2, "One answer");
        assert_eq!(rpc.committed.len(), 1);
    }

    #[test]
    fn lost_prepare_ack_reuses_identical_output_without_model_regeneration() {
        let responder = FakeResponder {
            calls: Cell::new(0),
            reply: "Immutable answer".into(),
        };
        let mut rpc = FakeRpc {
            accepts: VecDeque::from([AgentMessageRpcOutcome::Accepted]),
            prepares: VecDeque::from([
                AgentMessageRpcOutcome::Uncertain,
                AgentMessageRpcOutcome::Accepted,
            ]),
            commits: VecDeque::from([AgentMessageRpcOutcome::Accepted]),
            ..FakeRpc::default()
        };
        let mut messages = messages();
        let delivered = input();

        assert_eq!(
            messages.handle_with(&mut rpc, &responder, &delivered, [1, 2, 3]),
            ManagedMessageOutcome::Uncertain(PendingPhase::Preparation)
        );
        assert_eq!(
            messages.handle_with(&mut rpc, &responder, &delivered, [4, 5, 6]),
            ManagedMessageOutcome::Completed
        );
        assert_eq!(responder.calls.get(), 1);
        assert_eq!(rpc.accepted.len(), 1);
        assert_eq!(rpc.prepared.len(), 2);
        assert_eq!(rpc.prepared[0].2, rpc.prepared[1].2);
        assert_eq!(rpc.prepared[0].0, rpc.prepared[1].0);
        assert_eq!(rpc.prepared[0].1, rpc.prepared[1].1);
        assert_eq!(rpc.committed.len(), 1);
        rpc.accepts.push_back(AgentMessageRpcOutcome::Uncertain);
        let completed = input();
        assert_eq!(
            messages.handle_with(&mut rpc, &responder, &completed, [7, 8, 9]),
            ManagedMessageOutcome::Uncertain(PendingPhase::Acceptance)
        );
        assert_eq!(responder.calls.get(), 1);
        assert_eq!(rpc.accepted.len(), 2);
        assert_eq!(rpc.prepared.len(), 2);
        assert_eq!(rpc.committed.len(), 1);
    }

    #[test]
    fn lost_commit_ack_retries_only_commit_with_immutable_output() {
        let responder = FakeResponder {
            calls: Cell::new(0),
            reply: "Immutable answer".into(),
        };
        let mut rpc = FakeRpc {
            accepts: VecDeque::from([AgentMessageRpcOutcome::Accepted]),
            prepares: VecDeque::from([AgentMessageRpcOutcome::Accepted]),
            commits: VecDeque::from([
                AgentMessageRpcOutcome::Uncertain,
                AgentMessageRpcOutcome::Accepted,
            ]),
            ..FakeRpc::default()
        };
        let mut messages = messages();
        let delivered = input();
        assert_eq!(
            messages.handle_with(&mut rpc, &responder, &delivered, [1, 2, 3]),
            ManagedMessageOutcome::Uncertain(PendingPhase::Commit)
        );
        assert_eq!(
            messages.handle_with(&mut rpc, &responder, &delivered, [4, 5, 6]),
            ManagedMessageOutcome::Completed
        );
        assert_eq!(responder.calls.get(), 1);
        assert_eq!(rpc.prepared.len(), 1);
        assert_eq!(rpc.committed.len(), 2);
        assert_eq!(rpc.committed[0].0, rpc.committed[1].0);
        assert_eq!(rpc.committed[0].1, rpc.committed[1].1);
    }

    #[test]
    fn owner_prepared_reply_bypasses_model_and_changed_source_cannot_replay() {
        let responder = FakeResponder {
            calls: Cell::new(0),
            reply: "Must not be used".into(),
        };
        let mut rpc = FakeRpc {
            accepts: VecDeque::from([AgentMessageRpcOutcome::Accepted]),
            commits: VecDeque::from([AgentMessageRpcOutcome::Uncertain]),
            ..FakeRpc::default()
        };
        let mut delivered = input();
        delivered.prepared_reply = Some((
            "42345678-1234-1234-1234-123456789abc".into(),
            "Owner-retained answer".into(),
        ));
        let mut messages = messages();
        assert_eq!(
            messages.handle_with(&mut rpc, &responder, &delivered, [1, 2, 3]),
            ManagedMessageOutcome::Uncertain(PendingPhase::Commit)
        );
        assert_eq!(responder.calls.get(), 0);
        assert!(rpc.prepared.is_empty());
        assert_eq!(rpc.committed.len(), 1);

        delivered.source_digest = [8; 32];
        assert_eq!(
            messages.handle_with(&mut rpc, &responder, &delivered, [4, 5, 6]),
            ManagedMessageOutcome::Refused
        );
        assert_eq!(rpc.committed.len(), 1);
    }

    #[test]
    fn production_link_accepts_then_replies_with_exact_owner_prepared_input() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let peer = MessagePeer {
            sent: Arc::clone(&sent),
            events: vec![
                ui::collaboration::CollabEvent::AgentMessageReply {
                    request_id: 10,
                    result: Ok(()),
                },
                ui::collaboration::CollabEvent::RemoteCursorMoved {
                    user_id: "preserved".into(),
                    x: 4.0,
                    y: 2.0,
                },
                ui::collaboration::CollabEvent::AgentMessageReply {
                    request_id: 12,
                    result: Ok(()),
                },
                ui::collaboration::CollabEvent::AgentMessageReply {
                    request_id: 13,
                    result: Err("generic service error".into()),
                },
            ],
        };
        let mut link = crate::wire::test_link(peer);
        let responder = FakeResponder {
            calls: Cell::new(0),
            reply: "Must not be used".into(),
        };
        let mut delivered = input();
        delivered.prepared_reply = Some((
            "42345678-1234-1234-1234-123456789abc".into(),
            "Owner-retained answer".into(),
        ));
        let mut messages = messages();

        assert_eq!(
            messages.handle_with(&mut link, &responder, &delivered, [10, 11, 12]),
            ManagedMessageOutcome::Completed
        );
        let sent_guard = sent.lock().unwrap();
        assert!(matches!(
            &sent_guard[0],
            CollabRequest::AcceptAgentMessage {
                request_id: 10,
                invocation,
                source_digest,
            } if invocation == &delivered.invocation && source_digest == &delivered.source_digest
        ));
        assert!(matches!(
            &sent_guard[1],
            CollabRequest::CommitAgentMessageReply {
                request_id: 12,
                invocation,
                source_digest,
            } if invocation == &delivered.invocation
                && source_digest == &delivered.source_digest
        ));
        drop(sent_guard);
        assert!(matches!(
            link.recv(ui::time::Duration::ZERO).unwrap(),
            Some(ui::collaboration::CollabEvent::RemoteCursorMoved { user_id, .. })
                if user_id == "preserved"
        ));
        assert_eq!(
            messages.handle_with(&mut link, &responder, &delivered, [13, 14, 15]),
            ManagedMessageOutcome::Uncertain(PendingPhase::Acceptance)
        );
        let sent = sent.lock().unwrap();
        assert!(matches!(
            &sent[2],
            CollabRequest::AcceptAgentMessage { request_id: 13, .. }
        ));
        assert_eq!(
            sent.len(),
            3,
            "an uncertain service error cannot publish a second reply"
        );
        drop(sent);
    }

    #[test]
    fn production_link_prepares_generated_output_before_exact_commit() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let peer = MessagePeer {
            sent: Arc::clone(&sent),
            events: vec![
                ui::collaboration::CollabEvent::AgentMessageReply {
                    request_id: 20,
                    result: Ok(()),
                },
                ui::collaboration::CollabEvent::AgentMessageReply {
                    request_id: 21,
                    result: Ok(()),
                },
                ui::collaboration::CollabEvent::AgentMessageReply {
                    request_id: 22,
                    result: Ok(()),
                },
            ],
        };
        let mut link = crate::wire::test_link(peer);
        let responder = FakeResponder {
            calls: Cell::new(0),
            reply: "Generated once".into(),
        };
        let delivered = input();
        let mut messages = messages();
        assert_eq!(
            messages.handle_with(&mut link, &responder, &delivered, [20, 21, 22]),
            ManagedMessageOutcome::Completed
        );
        assert_eq!(responder.calls.get(), 1);
        let sent = sent.lock().unwrap();
        assert!(matches!(
            &sent[0],
            CollabRequest::AcceptAgentMessage { request_id: 20, .. }
        ));
        assert!(matches!(
            &sent[1],
            CollabRequest::PrepareAgentMessageReply {
                request_id: 21,
                invocation,
                source_digest,
                text,
            } if invocation == &delivered.invocation
                && source_digest == &delivered.source_digest
                && text == "Generated once"
        ));
        assert!(matches!(
            &sent[2],
            CollabRequest::CommitAgentMessageReply {
                request_id: 22,
                invocation,
                source_digest,
            } if invocation == &delivered.invocation && source_digest == &delivered.source_digest
        ));
        drop(sent);
    }

    #[test]
    fn production_worker_advances_only_from_correlated_events() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let peer = MessagePeer {
            sent: Arc::clone(&sent),
            events: Vec::new(),
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let mut messages = ManagedMessages::with_worker(
            "22345678-1234-1234-1234-123456789abc",
            "agent-1",
            "32345678-1234-1234-1234-123456789abc",
            WorkerResponder {
                calls: Arc::clone(&calls),
                reply: "Generated asynchronously".into(),
            },
        );
        let mut link = crate::wire::test_link(peer);
        let delivered = input();

        assert_eq!(
            messages.handle(&mut link, delivered, [40, 41, 42]),
            ManagedMessageOutcome::Uncertain(PendingPhase::Acceptance)
        );
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert_eq!(
            messages.handle_reply(&mut link, 40, &Ok(())),
            ManagedMessageOutcome::PendingModel
        );
        let outcomes = (0..100)
            .find_map(|_| {
                let outcomes = messages.poll_ready(&mut link);
                if outcomes.is_empty() {
                    std::thread::sleep(Duration::from_millis(1));
                    None
                } else {
                    Some(outcomes)
                }
            })
            .expect("model worker reply");
        assert_eq!(
            outcomes,
            vec![ManagedMessageOutcome::Uncertain(PendingPhase::Preparation)]
        );
        assert_eq!(
            messages.handle_reply(&mut link, 41, &Ok(())),
            ManagedMessageOutcome::Uncertain(PendingPhase::Commit)
        );
        assert_eq!(
            messages.handle_reply(&mut link, 42, &Ok(())),
            ManagedMessageOutcome::Completed
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let sent = sent.lock().unwrap();
        assert!(matches!(
            &sent[..],
            [
                CollabRequest::AcceptAgentMessage { request_id: 40, .. },
                CollabRequest::PrepareAgentMessageReply {
                    request_id: 41,
                    text,
                    ..
                },
                CollabRequest::CommitAgentMessageReply { request_id: 42, .. },
            ] if text == "Generated asynchronously"
        ));
        drop(sent);
    }

    #[test]
    fn missing_ack_observes_without_effect_retry_or_redelivery() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let peer = MessagePeer {
            sent: Arc::clone(&sent),
            events: Vec::new(),
        };
        let mut messages = ManagedMessages::with_worker(
            "22345678-1234-1234-1234-123456789abc",
            "agent-1",
            "32345678-1234-1234-1234-123456789abc",
            WorkerResponder {
                calls: Arc::new(AtomicUsize::new(0)),
                reply: "Unused".into(),
            },
        );
        let mut link = crate::wire::test_link(peer);
        let delivered = input();

        assert_eq!(
            messages.handle(&mut link, delivered.clone(), [50, 51, 52]),
            ManagedMessageOutcome::Uncertain(PendingPhase::Acceptance)
        );
        assert!(messages.next_deadline().is_some());
        assert_eq!(
            messages.expire(&mut link, Instant::now() + RPC_RESPONSE_WINDOW),
            vec![ManagedMessageOutcome::Uncertain(PendingPhase::Acceptance)]
        );
        assert!(messages.next_deadline().is_some());
        assert_eq!(
            sent.lock().unwrap().len(),
            2,
            "timer observes but never retries an effect"
        );
        assert_eq!(
            messages.handle(&mut link, delivered, [60, 61, 62]),
            ManagedMessageOutcome::Uncertain(PendingPhase::Acceptance)
        );
        let sent = sent.lock().unwrap();
        assert!(matches!(
            &sent[..],
            [
                CollabRequest::AcceptAgentMessage { request_id: 50, .. },
                CollabRequest::ObserveAgentMessage { request_id: 50, .. },
            ]
        ));
        drop(sent);
    }

    #[test]
    fn actual_worker_observes_each_lost_ack_without_second_model_or_effect_retry() {
        use runtime_collab::AgentMessageObservation as Observed;
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut link = crate::wire::test_link(MessagePeer {
            sent: Arc::clone(&sent),
            events: Vec::new(),
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let mut messages = ManagedMessages::with_worker(
            "22345678-1234-1234-1234-123456789abc",
            "agent-1",
            "32345678-1234-1234-1234-123456789abc",
            WorkerResponder {
                calls: Arc::clone(&calls),
                reply: "One immutable output".into(),
            },
        );
        let delivered = input();
        messages.handle(&mut link, delivered, [70, 71, 72]);
        messages.expire(&mut link, Instant::now() + RPC_RESPONSE_WINDOW);
        assert_eq!(
            messages.handle_reply(&mut link, 70, &Ok(())),
            ManagedMessageOutcome::Refused,
            "late effect acknowledgement cannot override pending observation"
        );
        assert_eq!(
            messages.handle_observation(&mut link, 70, &Ok(Observed::Accepted)),
            ManagedMessageOutcome::PendingModel
        );
        (0..100)
            .find_map(|_| {
                if messages.poll_ready(&mut link).is_empty() {
                    std::thread::sleep(Duration::from_millis(1));
                    None
                } else {
                    Some(())
                }
            })
            .expect("actual model worker must finish");
        messages.expire(&mut link, Instant::now() + RPC_RESPONSE_WINDOW);
        let reply = expected_reply(&input());
        assert_eq!(
            messages.handle_observation(
                &mut link,
                71,
                &Ok(Observed::Prepared {
                    reply: reply.clone(),
                    text: "One immutable output".into()
                })
            ),
            ManagedMessageOutcome::Uncertain(PendingPhase::Commit)
        );
        messages.expire(&mut link, Instant::now() + RPC_RESPONSE_WINDOW);
        assert_eq!(
            messages.handle_observation(&mut link, 72, &Ok(Observed::Committed { reply })),
            ManagedMessageOutcome::Completed
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(messages.retained.is_empty());
        let requests = sent.lock().unwrap();
        assert!(
            matches!(
                &requests[..],
                [
                    CollabRequest::AcceptAgentMessage { .. },
                    CollabRequest::ObserveAgentMessage { .. },
                    CollabRequest::PrepareAgentMessageReply { .. },
                    CollabRequest::ObserveAgentMessage { .. },
                    CollabRequest::CommitAgentMessageReply { .. },
                    CollabRequest::ObserveAgentMessage { .. },
                ]
            ),
            "each effect emitted once; recovery independent of stream redelivery"
        );
        drop(requests);
    }

    #[test]
    fn unknown_or_conflicting_observation_preserves_immutable_custody() {
        use runtime_collab::AgentMessageObservation as Observed;
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut link = crate::wire::test_link(MessagePeer {
            sent: Arc::clone(&sent),
            events: Vec::new(),
        });
        let mut messages = messages();
        let delivered = input();
        messages.retained.insert(
            delivered.invocation.clone(),
            RetainedMessage {
                input: delivered.clone(),
                stage: RetainedStage::PreparePending("Original output".into()),
                request_ids: [80, 81, 82],
                pending_rpc: None,
                uncertain: Some(PendingPhase::Preparation),
                observation_attempts: 0,
            },
        );
        messages.observe_current(&mut link, &delivered.invocation, PendingPhase::Preparation);
        assert_eq!(
            messages.handle_observation(
                &mut link,
                81,
                &Ok(Observed::Prepared {
                    reply: expected_reply(&input()),
                    text: "Substituted output".into()
                })
            ),
            ManagedMessageOutcome::Uncertain(PendingPhase::Preparation)
        );
        messages.handle(&mut link, delivered, [90, 91, 92]);
        assert_eq!(
            messages.handle_observation(&mut link, 91, &Err("Unknown".into())),
            ManagedMessageOutcome::Uncertain(PendingPhase::Preparation)
        );
        assert_eq!(messages.retained.len(), 1);
        let requests = sent.lock().unwrap();
        assert!(
            requests
                .iter()
                .all(|request| matches!(request, CollabRequest::ObserveAgentMessage { .. }))
        );
        drop(requests);
    }

    #[test]
    fn prepared_observation_after_lost_commit_never_reissues_commit() {
        use runtime_collab::AgentMessageObservation as Observed;
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut link = crate::wire::test_link(MessagePeer {
            sent: Arc::clone(&sent),
            events: Vec::new(),
        });
        let mut messages = messages();
        let delivered = input();
        messages.retained.insert(
            delivered.invocation.clone(),
            RetainedMessage {
                input: delivered.clone(),
                stage: RetainedStage::CommitPending("Original output".into()),
                request_ids: [80, 81, 82],
                pending_rpc: None,
                uncertain: Some(PendingPhase::Commit),
                observation_attempts: 0,
            },
        );
        messages.observe_current(&mut link, &delivered.invocation, PendingPhase::Commit);
        assert_eq!(
            messages.handle_observation(
                &mut link,
                82,
                &Ok(Observed::Prepared {
                    reply: expected_reply(&delivered),
                    text: "Original output".into()
                })
            ),
            ManagedMessageOutcome::Uncertain(PendingPhase::Commit)
        );
        assert_eq!(messages.retained.len(), 1);
        let requests = sent.lock().unwrap();
        assert!(
            matches!(&requests[..], [CollabRequest::ObserveAgentMessage { .. }]),
            "a delayed Commit may still execute; observation cannot reissue it"
        );
        drop(requests);
    }

    #[test]
    fn lost_observation_replies_retry_only_reads_with_a_fixed_bound() {
        use runtime_collab::AgentMessageObservation as Observed;
        for success in [true, false] {
            let sent = Arc::new(Mutex::new(Vec::new()));
            let mut link = crate::wire::test_link(MessagePeer {
                sent: Arc::clone(&sent),
                events: Vec::new(),
            });
            let mut messages = messages();
            let delivered = input();
            messages.retained.insert(
                delivered.invocation.clone(),
                RetainedMessage {
                    input: delivered.clone(),
                    stage: RetainedStage::CommitPending("Original output".into()),
                    request_ids: [80, 81, 82],
                    pending_rpc: None,
                    uncertain: Some(PendingPhase::Commit),
                    observation_attempts: 0,
                },
            );
            messages.observe_current(&mut link, &delivered.invocation, PendingPhase::Commit);
            for _ in 0..2 {
                assert_eq!(
                    messages.expire(&mut link, Instant::now() + RPC_RESPONSE_WINDOW * 4),
                    vec![ManagedMessageOutcome::Uncertain(PendingPhase::Commit)]
                );
                assert!(messages.next_deadline().is_some());
            }
            if success {
                assert_eq!(
                    messages.handle_observation(
                        &mut link,
                        82,
                        &Ok(Observed::Committed {
                            reply: expected_reply(&delivered)
                        })
                    ),
                    ManagedMessageOutcome::Completed
                );
                assert!(messages.retained.is_empty());
            } else {
                messages.expire(&mut link, Instant::now() + RPC_RESPONSE_WINDOW * 4);
                assert!(messages.next_deadline().is_none());
                assert_eq!(
                    messages.retained.len(),
                    1,
                    "exhaustion does not drop uncertain custody"
                );
                messages.handle(&mut link, delivered, [90, 91, 92]);
                assert!(
                    messages.next_deadline().is_none(),
                    "redelivery cannot reset the retry budget"
                );
            }
            let requests = sent.lock().unwrap();
            assert_eq!(requests.len(), usize::from(MAX_OBSERVATIONS_PER_PHASE));
            assert!(
                requests
                    .iter()
                    .all(|request| matches!(request, CollabRequest::ObserveAgentMessage { .. })),
                "lost observation replies never retry an effect or invoke the model"
            );
            drop(requests);
        }
    }

    #[test]
    fn definitive_completions_do_not_exhaust_uncertain_retention() {
        let responder = FakeResponder {
            calls: Cell::new(0),
            reply: "Must not be used".into(),
        };
        let mut rpc = FakeRpc::default();
        let mut messages = messages();
        for index in 1_u128..=129 {
            rpc.accepts.push_back(AgentMessageRpcOutcome::Accepted);
            rpc.commits.push_back(AgentMessageRpcOutcome::Accepted);
            let invocation = uuid::Uuid::from_u128(index).to_string();
            let mut delivered = input();
            delivered.invocation = invocation.clone();
            delivered.request_row = format!("m_{}", invocation.replace('-', ""));
            delivered.source_digest = [u8::try_from(index).unwrap(); 32];
            delivered.prepared_reply = Some((
                uuid::Uuid::from_u128(index + 1_000).to_string(),
                format!("Retained answer {index}"),
            ));
            let request = u64::try_from(index).unwrap() * 3;
            assert_eq!(
                messages.handle_with(
                    &mut rpc,
                    &responder,
                    &delivered,
                    [request - 2, request - 1, request],
                ),
                ManagedMessageOutcome::Completed
            );
        }
        assert!(messages.retained.is_empty());
        assert_eq!(responder.calls.get(), 0);
        assert_eq!(rpc.accepted.len(), 129);
        assert_eq!(rpc.committed.len(), 129);
    }

    #[test]
    fn descriptive_input_cannot_cross_the_session_recipient_binding() {
        let responder = FakeResponder {
            calls: Cell::new(0),
            reply: "Must not be used".into(),
        };
        let mut rpc = FakeRpc::default();
        let mut delivered = input();
        delivered.launch = "42345678-1234-1234-1234-123456789abc".into();
        assert_eq!(
            messages().handle_with(&mut rpc, &responder, &delivered, [1, 2, 3]),
            ManagedMessageOutcome::Refused
        );
        assert!(rpc.accepted.is_empty());
        assert_eq!(responder.calls.get(), 0);
    }
}
