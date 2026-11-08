//! The turn a run answers, recorded in its conversation.
//!
//! A person asks an agent by writing in a conversation the agent takes part
//! in; the run the broker delivers is that message's turn, and its task id is
//! the message's address in the workspace. The answer is written in the same
//! conversation: the run tells the reply to its message what it was taken
//! for, what it does at each step, and how it ended, by invoking chat's
//! `record-work` as a child of the run. The reply is the message its id and
//! the summons derive (chat keeps that rule), so nothing here has to be
//! stored to find it, and the conversation needs no record of its own.
//!
//! What the run records is what a person reads: its state, the route that
//! decided it, the operations it dispatched, and — once, when it ends — its
//! words. Every record a run makes lands in one group, so its turn is undone
//! whole.

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::{Session, TaskAction, TaskActionStop};

/// What a conversation's address in the workspace starts with: the chat
/// record's kind and the collection its conversations are kept in.
const CONVERSATION_PREFIX: &str = "chat.workspace/channels/";
/// What separates a message's own id from its conversation's address.
const MESSAGES: &str = "/messages/";
/// The chat action that records a turn on the reply to its message.
const RECORD_WORK: &str = "record-work";
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
    /// The turn `run` answers, read from its task id, or `None` when the run
    /// answers no conversation's message.
    pub(super) fn of(run: &runtime_collab::TaskRunEnvelope) -> Option<Self> {
        let (address, message) = run.task_id.rsplit_once(MESSAGES)?;
        let conversation = address.strip_prefix(CONVERSATION_PREFIX)?;
        let bounded = |id: &str| !id.is_empty() && id.len() <= MAX_ID_CHARS && !id.contains('/');
        (bounded(conversation) && bounded(message)).then(|| Self {
            conversation: conversation.to_owned(),
            message: message.to_owned(),
        })
    }
}

/// The turn a run answers, as it is being told: the cause, how the run
/// decided, and the operations it has dispatched so far.
#[derive(Clone, Debug)]
pub(super) struct Record {
    cause: Cause,
    /// `model` or `floor`: which path chose the run's steps.
    route: &'static str,
    /// The decider's account of why, in its own words.
    why: String,
    /// The operations dispatched so far, each keyed by the id it was given
    /// when it was dispatched; every record repeats them whole.
    steps: Vec<Value>,
}

/// The turn `run` answers, checked against what a conversation gives and
/// what a turn of one may carry.
///
/// # Errors
/// The run names no conversation message, or its title, instruction, lease or
/// action context is not one this agent may act on.
pub(super) fn validate(run: &runtime_collab::TaskRunEnvelope) -> Result<Cause, String> {
    if run.version != 1 || run.run_id == 0 {
        return Err("The run has an unsupported version or empty run id".into());
    }
    let cause = Cause::of(run).ok_or("The run answers no conversation's message")?;
    if run.title.trim().is_empty()
        || run.title.len() > 4 * 1024
        || run.instruction().trim().is_empty()
        || run.instruction().len() > 32 * 1024
        || run.lease.len() > 128
        || runtime_collab::native_task_revision(&run.lease).is_none()
    {
        return Err("The run has an invalid title, instruction, or lease".into());
    }
    if let Some(budget) = run.execution_budget {
        budget.validate().map_err(str::to_owned)?;
    }
    let context = serde_json::from_value::<tilo_actions::ActionContext>(run.action_context.clone())
        .map_err(|_| "The run has an invalid action context".to_string())?;
    let valid_group = context.group.as_ref().is_some_and(|group| {
        group.id == run.group() && group.label == run.title && group.kind == "task"
    });
    if context.purpose.as_deref() != Some(run.title.as_str()) || !valid_group {
        return Err("The run's action context does not bind its title and id".into());
    }
    Ok(cause)
}

impl Session {
    /// Start recording the turn `run` answers: what the reply will be told
    /// is kept from here, and which route decides the run.
    pub(super) fn start_turn(&mut self, cause: Cause, (route, why): (&'static str, String)) {
        tracing::info!(
            conversation = %cause.conversation,
            message = %cause.message,
            "answering the conversation's turn"
        );
        self.record = Some(Record {
            cause,
            route,
            why,
            steps: Vec::new(),
        });
    }

    /// Tell the conversation what `run` reports now, as the reply to its
    /// message keeps it.
    pub(super) fn show_turn(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        update: &runtime_collab::TaskRunUpdate,
    ) -> Result<()> {
        use runtime_collab::TaskRunUpdate;
        match update {
            TaskRunUpdate::Accepted => self.record_state(run, "taken", json!({}), None),
            TaskRunUpdate::Progress {
                message,
                presentation: None,
            } => self.record_state(run, "doing", json!({"did": message}), None),
            // A streamed presentation is the model's own output, shown where
            // it streams, and what a run spent is its keeper's count: neither
            // is a state of the turn.
            TaskRunUpdate::Progress { .. }
            | TaskRunUpdate::DirectionsApplied { .. }
            | TaskRunUpdate::Spent { .. } => Ok(()),
            TaskRunUpdate::Succeeded { message } => {
                self.record_state(run, "provider_complete", json!({}), Some(message))
            }
            TaskRunUpdate::Failed { message } => {
                self.record_state(run, "refused", json!({}), Some(message))
            }
            TaskRunUpdate::Cancelled { message } => {
                self.record_state(run, "provider_cancelled", json!({}), Some(message))
            }
        }
    }

    /// Add one dispatched operation to the reply's step list: where it acted,
    /// what it was, and what came of it.
    pub(super) fn record_action(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        resource: &str,
        label: &str,
        settled: Result<&super::StepOutcome, &TaskActionStop>,
    ) -> Result<()> {
        let (outcome, why) = match settled {
            Ok(_) => ("applied", None),
            Err(TaskActionStop::Failed(why)) => ("refused", Some(why.as_str())),
            Err(TaskActionStop::Cancelled(why)) => ("refused", why.as_deref()),
        };
        self.push_step(run, resource, label, outcome, why)
    }

    /// Add one `tilo` program the run ran to the reply's step list.
    pub(super) fn record_program(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        step: &ui::tilo_reference::ProgramStep,
    ) -> Result<()> {
        use ui::tilo_reference::StepOutcome;
        let label = step
            .changes
            .first()
            .map(|row| row.action.clone())
            .or_else(|| step.previews.first().map(|row| row.action.clone()))
            .unwrap_or_else(|| "a program".to_owned());
        let resource = step
            .changes
            .first()
            .map(|change| change.resource.reference.id.clone())
            .or_else(|| step.refs.first().map(|at| at.reference.id.clone()));
        let resource = resource.as_deref().unwrap_or("tilo.program");
        let said = step.answer.join(" ").trim().to_owned();
        let (outcome, why) = match step.outcome {
            StepOutcome::Done => ("applied", None),
            StepOutcome::Preview | StepOutcome::Asked => ("asked", None),
            StepOutcome::Rejected | StepOutcome::Failed => {
                ("refused", Some(said).filter(|said| !said.is_empty()))
            }
        };
        self.push_step(run, resource, &label, outcome, why.as_deref())
    }

    /// Record one step of the turn, keyed by the place it takes in the list.
    fn push_step(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        resource: &str,
        label: &str,
        outcome: &str,
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
        step.insert("outcome".into(), json!(outcome));
        if let Some(why) = why {
            step.insert("why".into(), json!(why));
        }
        record.steps.push(Value::Object(step));
        self.record_state(run, "doing", json!({"did": label}), None)
    }

    /// Invoke chat's `record-work` on the reply to the turn's message, in
    /// `state` with `fields`, and with `say` for the words the reply gets
    /// when it has none yet. A state that ends the turn is only started: the
    /// run is over, and following the record would read that as its
    /// cancellation.
    fn record_state(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        state: &str,
        fields: Value,
        say: Option<&str>,
    ) -> Result<()> {
        let Some(record) = self.record.as_ref() else {
            return Ok(());
        };
        let mut work = fields;
        let Some(work) = work.as_object_mut() else {
            return Ok(());
        };
        work.insert("state".into(), json!(state));
        work.insert("route".into(), json!(record.route));
        work.insert("why".into(), json!(record.why));
        work.insert("trace".into(), Value::Array(record.steps.clone()));
        let mut arguments = Map::new();
        arguments.insert("message".into(), json!(record.cause.message.clone()));
        arguments.insert("work".into(), Value::Object(work.clone()));
        if let Some(say) = say.map(str::trim).filter(|say| !say.is_empty()) {
            arguments.insert("say".into(), json!(say));
        }
        let Some(child) = self.record_work_action(&arguments) else {
            return Ok(());
        };
        // Recording is not part of the run's time budget: a record refused
        // for running out of time would drop the last thing the turn has to
        // say.
        let deadline = self.task_deadline.take();
        let started = self.start_task_invocation(run, &child, &arguments);
        self.task_deadline = deadline;
        match started? {
            Ok(following) if ENDS.contains(&state) => {
                // Started, not followed: the record ends the turn, and the
                // host reads a turn that ended as the run's cancellation.
                drop(following);
                Ok(())
            }
            Ok(following) => {
                let mut followed = [following];
                self.follow_native(run, &mut followed)?;
                if let Some(Err(stop)) = followed[0].outcome.as_ref() {
                    tracing::warn!(
                        state,
                        ?stop,
                        "the conversation did not take the run's report"
                    );
                }
                Ok(())
            }
            Err(stop) => {
                tracing::warn!(state, ?stop, "the conversation refused the run's report");
                Ok(())
            }
        }
    }

    /// The chat action that records work on the reply to the turn's message,
    /// as the broker publishes it to this agent: absent when the agent's
    /// catalogue offers no `record-work` on that reply, which is what a
    /// workspace whose Chat is closed — or whose agent was admitted without
    /// the chat app's actions — leaves, and the reason the reply a person
    /// asked for never appears.
    fn record_work_action(&mut self, arguments: &Map<String, Value>) -> Option<TaskAction> {
        let message = arguments.get("message").and_then(Value::as_str)?;
        let reply = format!("{MESSAGES}{message}");
        let offer = {
            let catalogue = self.task_catalogue();
            catalogue
                .offers
                .iter()
                .find(|offer| {
                    offer.descriptor.address.action == RECORD_WORK
                        && offer.descriptor.address.resource.ends_with(&reply)
                })
                .cloned()
        };
        let Some(offer) = offer else {
            tracing::warn!(
                reply = %reply,
                action = RECORD_WORK,
                "the conversation publishes no `record-work` on the turn's reply: \
                 the person's message gets no answer, and this agent's catalogue is why"
            );
            return None;
        };
        match super::task_action_from_offer(&offer, arguments.clone()) {
            Ok(action) => Some(action),
            Err(error) => {
                tracing::warn!(%error, "the conversation's `record-work` refused its arguments");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run that answers `asked` in the conversation `conversation`, with
    /// its action context bound as the broker binds it.
    fn run(task_id: &str) -> runtime_collab::TaskRunEnvelope {
        let mut run = runtime_collab::TaskRunEnvelope {
            paused: None,
            model: None,
            version: 1,
            task_id: task_id.into(),
            title: "Split the launch checklist".into(),
            instruction: Some("Split the launch checklist into subtasks".into()),
            subject: None,
            run_id: 7,
            lease: "native:00000000000000000000000000000000:0".into(),
            planning: runtime_collab::TaskRunPlanning::Model,
            presentation_version: Some(1),
            execution_budget: None,
            spend: None,
            action_context: Value::Null,
        };
        run.action_context = serde_json::to_value(
            tilo_actions::ActionContext::default()
                .with_purpose(run.title.as_str())
                .with_group(run.group(), run.title.as_str(), "task"),
        )
        .unwrap();
        run
    }

    #[test]
    fn a_runs_task_id_names_the_conversations_message_it_answers() {
        let cause = validate(&run("chat.workspace/channels/launch/messages/asked")).unwrap();
        assert_eq!(cause.conversation, "launch");
        assert_eq!(cause.message, "asked");
        let address = "chat.workspace/channels/launch/messages/asked";
        assert_eq!(Cause::of(&run(address)).unwrap(), cause);
    }

    #[test]
    fn a_run_that_answers_no_conversations_message_is_refused() {
        for task_id in [
            "tasks.tracker/issues/42",
            "chat.workspace/channels/launch",
            "chat.workspace/channels//messages/asked",
            "chat.workspace/channels/launch/messages/",
            "conversation/messages/asked",
        ] {
            assert!(
                validate(&run(task_id)).is_err(),
                "{task_id} names no message of a conversation"
            );
        }
    }

    #[test]
    fn a_run_whose_context_does_not_bind_its_turn_is_refused() {
        let mut unbound = run("chat.workspace/channels/launch/messages/asked");
        unbound.action_context = Value::Null;
        assert!(validate(&unbound).is_err());
        let mut other_group = run("chat.workspace/channels/launch/messages/asked");
        other_group.action_context = serde_json::to_value(
            tilo_actions::ActionContext::default()
                .with_purpose(other_group.title.as_str())
                .with_group("another-run", other_group.title.as_str(), "task"),
        )
        .unwrap();
        assert!(validate(&other_group).is_err());
        let mut wrong_purpose = run("chat.workspace/channels/launch/messages/asked");
        wrong_purpose.action_context = serde_json::to_value(
            tilo_actions::ActionContext::default()
                .with_purpose("something else")
                .with_group(wrong_purpose.group(), wrong_purpose.title.as_str(), "task"),
        )
        .unwrap();
        assert!(validate(&wrong_purpose).is_err());
    }
}
