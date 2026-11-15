//! The turn the agent answers, recorded in its conversation.
//!
//! A person asks an agent by writing in a conversation the agent takes part
//! in; the turn the broker hands over names that message by its address in
//! the workspace. The answer is written in the same conversation: the agent
//! tells the reply to the message what it was taken for, what it does at each
//! step, and how it ended, by invoking chat's `record-work` as a step of the
//! turn. The reply is the message its id and
//! the summons derive (chat keeps that rule), so nothing here has to be
//! stored to find it, and the conversation needs no record of its own.
//!
//! What the run records is what a person reads: its state, the operations it
//! dispatched, and — once, when it ends — its answer. How the run was planned
//! stays in the run's own record. Every record a run makes lands in one group,
//! so its turn is undone whole.

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::{Asked, Session, TaskAction, TaskActionStop, TurnUpdate};

/// What a conversation's address in the workspace starts with: the chat
/// record's kind and the collection its conversations are kept in.
const CONVERSATION_PREFIX: &str = "chat.workspace/channels/";
/// What separates a message's own id from its conversation's address.
const MESSAGES: &str = "/messages/";
/// The chat action that records a turn on the reply to its message.
const RECORD_WORK: &str = "record-work";
/// How long a turn waits for its conversation to offer `record-work` on its message: an agent
/// started for the turn reads Chat's catalogue before Chat has republished the message it answers.
const RECORD_WAIT: std::time::Duration = std::time::Duration::from_secs(15);
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

/// The turn a run answers, as it is being told: the cause, the operations
/// it has dispatched so far, and the answer it ends with.
#[derive(Clone, Debug)]
pub(super) struct Record {
    cause: Cause,
    /// The operations dispatched so far, each keyed by the id it was given
    /// when it was dispatched; every record repeats them whole.
    steps: Vec<Value>,
    /// The words the run answers with, when it has any: the reply's text.
    answer: Option<String>,
    /// Whether the run has already looked for the reply's `record-work`,
    /// waiting for the conversation to publish it: that wait is for a
    /// catalogue republished moments after the run started, so it is paid
    /// once, never on every step of a long run.
    looked_for_work: bool,
}

/// The turn `run` answers, checked against what a conversation gives and
/// what a turn of one may carry.
pub(super) fn validate(run: &runtime_collab::TurnRequest) -> Result<Cause, String> {
    let cause = Cause::of(run).ok_or("The turn answers no conversation's message")?;
    if run.title.trim().is_empty()
        || run.title.len() > 4 * 1024
        || run.instruction.trim().is_empty()
        || run.instruction.len() > 32 * 1024
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
            looked_for_work: false,
        });
    }

    /// End `run` as done: `answer` is the reply's text, and the run's own
    /// record says `done` when there is no answer. A run with neither — a
    /// model-planned run that ended with nothing said — is not reported as a
    /// success with a line nobody wrote: the person reads that the agent
    /// finished without answering.
    pub(super) fn succeed_turn(
        &mut self,
        run: &Asked,
        answer: Option<String>,
        done: Option<String>,
    ) -> Result<()> {
        if let Some(record) = self.record.as_mut() {
            record.answer.clone_from(&answer);
        }
        let Some(message) = answer.or(done) else {
            return self.fail_task(
                run,
                &crate::session::Failure::said(crate::i18n::t("agent-failure-no-answer")),
            );
        };
        self.report_task(run, TurnUpdate::Succeeded { message })
    }

    /// Tell the conversation what `run` reports now, as the reply to its
    /// message keeps it.
    pub(super) fn show_turn(&mut self, run: &Asked, update: &TurnUpdate) -> Result<()> {
        match update {
            TurnUpdate::Accepted => self.record_state(run, "taken", json!({}), None),
            // Progress narrates the run to its own record; the reply shows
            // the operations it dispatched instead. What a run spent is its
            // keeper's count. Neither is a state of the turn.
            TurnUpdate::Progress { .. } | TurnUpdate::Spent { .. } => Ok(()),
            TurnUpdate::Succeeded { .. } => {
                let answer = self
                    .record
                    .as_ref()
                    .and_then(|record| record.answer.clone());
                self.record_state(run, "provider_complete", json!({}), answer.as_deref())
            }
            TurnUpdate::Failed { message } => {
                // What the host heard is the failure's sentence for a person —
                // what happened, and at most what to do — and the run keeps the
                // failure's record beside it for whoever debugs it later.
                let detail = self.failure_detail.clone();
                self.record_state_with_detail(
                    run,
                    "refused",
                    json!({}),
                    Some(message),
                    detail.as_deref(),
                )
            }
            TurnUpdate::Cancelled { message } => {
                self.record_state(run, "provider_cancelled", json!({}), Some(message))
            }
        }
    }

    /// Add one dispatched operation to the reply's step list: where it acted,
    /// what it did in its app's words (what the action declares it did, or
    /// its label, with the title of what it acted on: "Showed on the canvas: All inboxes"),
    /// and what came of it.
    pub(super) fn record_action(
        &mut self,
        run: &Asked,
        action: &TaskAction,
        settled: Result<&super::StepOutcome, &TaskActionStop>,
    ) -> Result<()> {
        let (outcome, why) = match settled {
            Ok(_) => ("applied", None),
            Err(TaskActionStop::Failed(why)) => ("refused", Some(why.as_str())),
            Err(TaskActionStop::Cancelled(why)) => ("refused", why.as_deref()),
        };
        let subject = self.subject_of(action, settled.ok());
        let said = crate::i18n::did(&action.label, action.did.as_deref(), &subject);
        self.push_step(run, &action.resource_kind, &said, outcome, why)
    }

    /// The title of what `action` acted on, as the person knows it: what the
    /// action answered it acted on (a place says the title it stands by), else
    /// the resource an argument names, by its title, else the resource the
    /// action is on.
    fn subject_of(&mut self, action: &TaskAction, done: Option<&super::StepOutcome>) -> String {
        if let Some(title) = done
            .and_then(|done| done.output.value.as_ref())
            .and_then(|value| value.get("title"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            return title.to_owned();
        }
        let catalogue = self.task_catalogue();
        // The resource it shows or acts on first, then any other the arguments name: a `place`
        // names the conversation it stands beside too, which is not what it acted on.
        let named = action.arguments.get("resource").into_iter().chain(
            action
                .arguments
                .iter()
                .filter(|(name, _)| !matches!(name.as_str(), "resource" | "beside"))
                .map(|(_, value)| value),
        );
        named
            .filter_map(Value::as_str)
            .find_map(|named| catalogue.about(named.trim()))
            .map(|facts| facts.label.trim().to_owned())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| action.resource_label.trim().to_owned())
    }

    /// The title `app` publishes itself under, as its application offer names it: `None` when
    /// the catalogue offers no such application, never a title made of its id.
    fn app_title(&mut self, app: &str) -> Option<String> {
        self.task_catalogue()
            .offers
            .iter()
            .find(|offer| {
                offer.app == app
                    && offer.descriptor.resource_kind == crate::surface::APPLICATION_KIND
            })
            .map(|offer| offer.descriptor.resource_label.trim().to_owned())
            .filter(|title| !title.is_empty())
    }

    /// Add one `tilo` program the run ran to the reply's step list.
    pub(super) fn record_program(
        &mut self,
        run: &Asked,
        step: &ui::tilo_reference::ProgramStep,
    ) -> Result<()> {
        use ui::tilo_reference::StepOutcome;
        // What it did, in its app's words: the first change it made, by the action's label with
        // the title of what it changed; else what it proposes; else that it looked up the app its
        // answer names, by that app's own title ("Looked up Tasks").
        let label = step
            .changes
            .first()
            .map(|row| crate::i18n::did(&row.action, None, &row.resource.reference.label))
            .or_else(|| step.previews.first().map(|row| row.action.clone()))
            .unwrap_or_else(|| {
                let app = step.refs.first().map(|at| at.app.clone());
                let title = app.and_then(|app| self.app_title(&app));
                crate::i18n::looked_up(title.as_deref())
            });
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

    /// Add one web search or page the run did to the reply's step list: what
    /// it asked for or read, and what came of it.
    pub(super) fn record_web(
        &mut self,
        run: &Asked,
        call: &crate::tools::web::Call,
        report: &crate::tools::InvokeReport,
    ) -> Result<()> {
        // The same vocabulary the person is told the step in, said as done: the
        // record says what happened, the note what is under way.
        let (resource, label) = match call {
            crate::tools::web::Call::Search { query, .. } => (
                "web.search",
                crate::i18n::step_done(&tilo_actions::task_step::TaskStep::Searching(
                    query.clone(),
                )),
            ),
            crate::tools::web::Call::Page { url, .. } => (
                "web.page",
                crate::i18n::step_done(&tilo_actions::task_step::TaskStep::Reading(url.clone())),
            ),
        };
        let (outcome, why) = if report.succeeded {
            ("applied", None)
        } else {
            ("refused", report.error.as_deref())
        };
        self.push_step(run, resource, &label, outcome, why)
    }

    /// Record one step of the turn, keyed by the place it takes in the list.
    fn push_step(
        &mut self,
        run: &Asked,
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
        run: &Asked,
        state: &str,
        fields: Value,
        say: Option<&str>,
    ) -> Result<()> {
        self.record_state_with_detail(run, state, fields, say, None)
    }

    /// [`Self::record_state`], with the run's working record of a failure kept
    /// on the reply's work beside the words a person reads: the transport's
    /// error, the partial answer, the provider's exposed reasoning — for
    /// whoever debugs it later, never the reply's own sentence.
    fn record_state_with_detail(
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
        let Some(child) = self.record_work_action(&arguments) else {
            return Ok(());
        };
        let started = self.start_task_invocation(run, &child, &arguments);
        match started? {
            Ok(following) if ENDS.contains(&state) => {
                // Started, not followed: the record ends the turn, and the host
                // reads a turn that ended as its answer's cancellation.
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
    /// as the broker publishes it to this agent, once it does: an agent started
    /// for the turn may read the catalogue before Chat republished the message.
    /// Absent when the workspace offers no `record-work` on that reply, which
    /// is what a workspace whose Chat is closed — or whose agent was admitted
    /// without the chat app's actions — leaves, and the reason the reply a
    /// person asked for never appears.
    fn record_work_action(&mut self, arguments: &Map<String, Value>) -> Option<TaskAction> {
        let message = arguments.get("message").and_then(Value::as_str)?;
        let reply = format!("{MESSAGES}{message}");
        let offer = |session: &mut Self| {
            session
                .task_catalogue()
                .offers
                .iter()
                .find(|offer| {
                    offer.descriptor.address.action == RECORD_WORK
                        && offer.descriptor.address.resource.ends_with(&reply)
                })
                .cloned()
        };
        if self.waits_for_record_work() {
            self.await_catalogue(RECORD_WAIT, |session| offer(session).is_some());
        }
        let Some(offer) = offer(self) else {
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

    /// Whether this run still waits for its reply's `record-work` to be
    /// published, at most [`RECORD_WAIT`]: once per run, and only while a
    /// running application publishes `record-work`. What the wait covers is Chat
    /// republishing the message the run answers moments after the run
    /// started. A closed Chat keeps the offers it last published — a
    /// workspace whose Chat is closed, or that publishes none at all, has
    /// nothing coming and would spend the bound on every step.
    fn waits_for_record_work(&mut self) -> bool {
        let Some(record) = self
            .record
            .as_mut()
            .filter(|record| !record.looked_for_work)
        else {
            return false;
        };
        record.looked_for_work = true;
        self.broker_catalogue.surfaces().iter().any(|surface| {
            surface.live
                && surface
                    .actions
                    .iter()
                    .any(|action| action.action == RECORD_WORK)
        })
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
