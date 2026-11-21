//! The turn the agent answers, recorded in its conversation.
//!
//! A person asks an agent by writing in a conversation the agent takes part
//! in; the turn the broker hands over names that message by its address in
//! the workspace. The answer is written in the same conversation: the agent
//! tells the reply to the message what it was taken for, what it does at each
//! step, and how it ended, by invoking chat's `record-work` as a step of the
//! turn. The reply is the message its id and the summons derive (chat keeps
//! that rule), so nothing here has to be stored to find it.
//!
//! What the run records is what a person reads: its state, the steps it took,
//! and — once, when it ends — its answer. Every record a run makes lands in one
//! group, so its turn is undone whole.

use anyhow::Result;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};

use super::{Asked, Session, TurnUpdate};

/// What a conversation's address in the workspace starts with: the chat
/// resource's kind and the collection its conversations are kept in.
const CONVERSATION_PREFIX: &str = "chat.workspace/channels/";
/// What separates a message's own id from its conversation's address.
pub(super) const MESSAGES: &str = "/messages/";
/// The chat action that records a turn on the reply to its message.
const RECORD_WORK: &str = "record-work";
/// How long a turn waits for its conversation to offer its actions on its message: an agent
/// started for the turn reads Chat's catalogue before Chat has republished the message it answers.
const OFFER_WAIT: std::time::Duration = std::time::Duration::from_secs(15);
/// How long a step of the conversation's own is followed before the turn goes on.
const FOLLOW_MS: u32 = 5_000;
/// The longest id a conversation or a message may have.
const MAX_ID_CHARS: usize = 256;
/// The state that ends a turn: the reply's work is finished, and the turn is
/// over.
const ENDS: [&str; 3] = ["provider_complete", "refused", "provider_cancelled"];

/// The turn a run answers: the conversation and the message that asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Cause {
    /// The conversation, by its own id.
    pub(super) conversation: String,
    /// The message, by its id in that conversation.
    pub(super) message: String,
}

impl Cause {
    /// The turn `run` answers, read from the message it names, or `None` when it names no
    /// conversation's message.
    pub(super) fn of(run: &runtime_collab::TurnRequest) -> Option<Self> {
        let (address, message) = run.message.rsplit_once(MESSAGES)?;
        let conversation = address.strip_prefix(CONVERSATION_PREFIX)?;
        let bounded = |id: &str| !id.is_empty() && id.len() <= MAX_ID_CHARS && !id.contains('/');
        (bounded(conversation) && bounded(message)).then(|| Self {
            conversation: conversation.to_owned(),
            message: message.to_owned(),
        })
    }
}

/// The turn a run answers, as it is being told: the cause, the steps it took so far, and the
/// answer it ends with.
#[derive(Clone, Debug)]
pub(super) struct Record {
    cause: Cause,
    /// The steps taken so far; every record repeats them whole.
    pub(super) steps: Vec<Value>,
    /// The words the run answers with, when it has any: the reply's text.
    answer: Option<String>,
    /// Whether the run already waited for the conversation to offer its actions on the message:
    /// that wait is paid once, never on every step of a long run.
    waited: bool,
}

/// The turn `run` answers, checked against what a conversation gives and
/// what a turn of one may carry.
pub(super) fn validate(run: &runtime_collab::TurnRequest) -> Result<Cause, String> {
    let cause = Cause::of(run).ok_or("The turn answers no conversation's message")?;
    if run.title.trim().is_empty()
        || run.title.len() > 4 * 1024
        || run.instruction.trim().is_empty()
        || run.instruction.len() > 32 * 1024
        || run
            .agent_instruction
            .as_ref()
            .is_some_and(|told| told.len() > 8 * 1024)
    {
        return Err("The turn has an invalid title or instruction".into());
    }
    Ok(cause)
}

impl Session {
    /// Start recording the turn `run` answers: what the reply will be told
    /// is kept from here.
    pub(super) fn start_turn(&mut self, cause: Cause) {
        tracing::info!(
            conversation = %cause.conversation,
            message = %cause.message,
            "answering the conversation's turn"
        );
        self.record = Some(Record {
            cause,
            steps: Vec::new(),
            answer: None,
            waited: false,
        });
    }

    /// End `run` with `answer`, the reply's text. A run that ends with nothing said is not
    /// reported as a success with a line nobody wrote: the person reads that the agent finished
    /// without answering.
    pub(super) fn succeed_turn(&mut self, run: &Asked, answer: String) -> Result<()> {
        if answer.trim().is_empty() {
            return self.fail_task(run, &crate::i18n::t("agent-failure-no-answer"), None);
        }
        if let Some(record) = self.record.as_mut() {
            record.answer = Some(answer.clone());
        }
        if run.fold.is_some() {
            // The reply reads its words first: the conversation's summary is kept once the
            // person has their answer, and the turn ends after it.
            self.record_state(run, "doing", json!({}), Some(&answer), None)?;
            self.keep_summary(run)?;
        }
        self.report_task(run, TurnUpdate::Succeeded { message: answer })
    }

    /// Tell the conversation what `run` reports now, as the reply to its
    /// message keeps it.
    pub(super) fn show_turn(&mut self, run: &Asked, update: &TurnUpdate) -> Result<()> {
        match update {
            TurnUpdate::Accepted => self.record_state(run, "taken", json!({}), None, None),
            // Progress narrates the run to its answer; the reply shows the steps it took
            // instead. What a run spent is its keeper's count. Neither is a state of the turn.
            TurnUpdate::Progress { .. } | TurnUpdate::Spent { .. } => Ok(()),
            TurnUpdate::Succeeded { .. } => {
                let answer = self
                    .record
                    .as_ref()
                    .and_then(|record| record.answer.clone());
                self.record_state(run, "provider_complete", json!({}), answer.as_deref(), None)
            }
            TurnUpdate::Failed { message } => {
                // What the host heard is the failure's sentence for a person; the run keeps the
                // failure's detail beside it for whoever debugs it later.
                let detail = self.failure_detail.clone();
                self.record_state(run, "refused", json!({}), Some(message), detail.as_deref())
            }
            TurnUpdate::Cancelled { message } => {
                self.record_state(run, "provider_cancelled", json!({}), Some(message), None)
            }
        }
    }

    /// Add one step to the reply's step list: what it did, on what, and why it failed if it did.
    pub(super) fn push_step(
        &mut self,
        run: &Asked,
        resource: &str,
        label: &str,
        why: Option<&str>,
    ) -> Result<()> {
        let Some(record) = self.record.as_mut() else {
            return Ok(());
        };
        let sequence = u64::try_from(record.steps.len()).unwrap_or(u64::MAX);
        let mut step = Map::new();
        step.insert("id".into(), json!(format!("turn-step-{sequence}")));
        step.insert("sequence".into(), json!(sequence));
        step.insert("resource".into(), json!(resource));
        step.insert("label".into(), json!(label));
        step.insert(
            "outcome".into(),
            json!(if why.is_some() { "refused" } else { "applied" }),
        );
        if let Some(why) = why {
            let why: String = why.chars().take(500).collect();
            step.insert("why".into(), json!(why));
        }
        record.steps.push(Value::Object(step));
        tracing::info!(sequence, %label, %resource, "turn step");
        self.record_state(run, "doing", json!({"did": label}), None, None)
    }

    /// Invoke chat's `record-work` on the reply to the turn's message, in `state` with `fields`,
    /// with `say` for the words the reply gets, and `detail` for what the run keeps of a failure
    /// for whoever debugs it later. A state that ends the turn is only started: the run is over.
    fn record_state(
        &mut self,
        run: &Asked,
        state: &str,
        fields: Value,
        say: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()> {
        let Some(record) = self.record.as_ref() else {
            return Ok(());
        };
        let mut work = fields;
        let Some(work) = work.as_object_mut() else {
            return Ok(());
        };
        work.insert("state".into(), json!(state));
        work.insert("trace".into(), Value::Array(record.steps.clone()));
        if let Some(detail) = detail.map(str::trim).filter(|detail| !detail.is_empty()) {
            work.insert("detail".into(), json!(detail));
        }
        let mut arguments = Map::new();
        arguments.insert("message".into(), json!(record.cause.message.clone()));
        arguments.insert("work".into(), Value::Object(work.clone()));
        if let Some(say) = say.map(str::trim).filter(|say| !say.is_empty()) {
            arguments.insert("say".into(), json!(say));
        }
        let message = format!("{MESSAGES}{}", record.cause.message);
        self.invoke_on_message(
            run,
            &message,
            RECORD_WORK,
            arguments,
            !ENDS.contains(&state),
        )
    }

    /// Invoke `action`, which the conversation offers on the resource ending in `item`, with
    /// `arguments`, as a step of `run`; followed until it ends (at most a few seconds) when
    /// `follow`. A conversation that offers no such action, or refuses it, leaves the turn as it
    /// is: what it would have recorded is said in the log.
    pub(super) fn invoke_on_message(
        &mut self,
        run: &Asked,
        item: &str,
        action: &str,
        arguments: Map<String, Value>,
        follow: bool,
    ) -> Result<()> {
        let Some(target) = self.offered_on(item, action) else {
            tracing::warn!(
                %item,
                action,
                "the conversation offers no such action on the turn's message: the person's \
                 message gets no answer for it, and this agent's catalogue is why"
            );
            return Ok(());
        };
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target,
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: Some(run.invocation),
            appearance: None,
            wait_ms: follow.then_some(FOLLOW_MS),
            signed: None,
        };
        let actions = self.actions.clone();
        let caller = self.caller();
        let answer = self.carry(async move { actions.invoke(&caller, request).await }, None)?;
        match answer {
            Some(Ok(receipt)) => match receipt.status {
                ReceiptStatus::Failed { error } => {
                    tracing::warn!(action, error = %error.message, "the conversation refused the turn's step");
                }
                ReceiptStatus::Rejected { error } => {
                    tracing::warn!(action, %error, "the conversation refused the turn's step");
                }
                _ => {}
            },
            Some(Err(error)) => {
                tracing::warn!(action, %error, "the conversation refused the turn's step");
            }
            None => tracing::warn!(action, "the conversation did not answer the turn's step"),
        }
        Ok(())
    }

    /// Where `action` is offered on the resource ending in `item`, as the broker publishes it to
    /// this agent, once it does: waited for once per run.
    pub(super) fn offered_on(&mut self, item: &str, action: &str) -> Option<RemoteActionTarget> {
        let find = |session: &Self| {
            session
                .broker_catalogue
                .surfaces()
                .iter()
                .find_map(|surface| {
                    if !surface.live && surface.provider.is_none() {
                        return None;
                    }
                    let offered = surface
                        .actions
                        .iter()
                        .find(|offer| offer.action == action && offer.resource.ends_with(item))?;
                    Some(RemoteActionTarget {
                        provider: surface
                            .provider
                            .clone()
                            .unwrap_or_else(|| surface.peer_id.to_string()),
                        address: tilo_actions::ActionAddress::new(&offered.resource, action),
                    })
                })
        };
        if let Some(found) = find(self) {
            return Some(found);
        }
        let waits = self.record.as_mut().is_some_and(|record| {
            let first = !record.waited;
            record.waited = true;
            first
        });
        if !waits {
            return None;
        }
        let deadline = ui::time::now() + OFFER_WAIT;
        while ui::time::now() < deadline {
            let Some(change) = self.link.next_catalogue(deadline) else {
                break;
            };
            self.apply_broker_catalogue(&change);
            if let Some(found) = find(self) {
                return Some(found);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A turn that answers the message at `message`.
    fn asked(message: &str) -> runtime_collab::TurnRequest {
        runtime_collab::TurnRequest {
            message: message.into(),
            title: "Split the launch checklist".into(),
            instruction: "Split the launch checklist into subtasks".into(),
            agent_instruction: None,
            fold: None,
            subject: None,
            spend: None,
        }
    }

    tilo_test_env::scenario! {
        fn a_turn_is_bound_to_the_conversation_message_it_answers;

        phase a_turn_names_the_conversations_message_it_answers;
        {
            let cause = validate(&asked("chat.workspace/channels/launch/messages/asked")).unwrap();
            assert_eq!(cause.conversation, "launch");
            assert_eq!(cause.message, "asked");
            let address = "chat.workspace/channels/launch/messages/asked";
            assert_eq!(Cause::of(&asked(address)).unwrap(), cause);
        }

        phase a_turn_that_answers_no_conversations_message_is_refused;
        {
            for message in [
                "tasks.tracker/issues/42",
                "chat.workspace/channels/launch",
                "chat.workspace/channels//messages/asked",
                "chat.workspace/channels/launch/messages/",
                "conversation/messages/asked",
            ] {
                assert!(
                    validate(&asked(message)).is_err(),
                    "{message} names no message of a conversation"
                );
            }
        }

        phase a_turn_without_words_is_refused;
        {
            let mut silent = asked("chat.workspace/channels/launch/messages/asked");
            silent.instruction = "  ".into();
            assert!(validate(&silent).is_err());
        }
    }
}
