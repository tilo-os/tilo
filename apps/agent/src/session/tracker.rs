//! A run of an issue in the Tasks app's tracker writes its progress there.
//!
//! The run's task id is the issue's address. Besides the run's reports to
//! the host, the agent tells the issue what it does through the tracker's
//! own actions, invoked as children of the run like any other step: that it
//! started, its progress and findings (posted in the issue's thread), the
//! messages it took in, the question it asks before an action that asks
//! first, and how it ended. Each is an action the agent's grants decide and
//! the issue's history records. The host matches a question's answer
//! against the draft the arguments render, so what the agent records there
//! is never trusted.
//!
//! Every call is built from Tasks' own typed action arguments
//! ([`IssueCall`]): the agent's call and the action's signature are one
//! definition, so a parameter renamed or retyped on either side fails to
//! compile instead of being refused while a run goes.

use anyhow::{Result, anyhow};
use tasks_tracker::model::{Question, RunOutcome, RunPhase, StepSpend, Tracker, tracker};
use ui::tilo_resource::{Id, Invoke, NonBlank};

use super::{Session, TaskAction, TaskActionStop, task_action_from_offer};

/// The tracker's resource kind; an issue's address is
/// `tasks.tracker/issues/<id>`.
const TRACKER_KIND: &str = tasks_tracker::model::KIND;
const ISSUE_PREFIX: &str = "tasks.tracker/issues/";
/// The longest text a thread message keeps.
const MAX_MESSAGE_CHARS: usize = 16 * 1024;

/// One call the run makes on its issue: one of the tracker's actions with
/// its typed arguments.
pub(super) type IssueCall = Invoke<Tracker>;

/// One model turn's receipt, as the run's issue keeps it: Tasks' own
/// [`StepSpend`], the `step` of its `settle-spend`.
pub(super) type StepReceipt = StepSpend;

/// The issue `run` works on, by its id in the tracker.
pub(super) fn issue_of(run: &runtime_collab::TaskRunEnvelope) -> Option<&str> {
    run.task_id
        .strip_prefix(ISSUE_PREFIX)
        .filter(|issue| !issue.is_empty())
}

fn message(text: &str) -> Option<NonBlank> {
    let text: String = text.trim().chars().take(MAX_MESSAGE_CHARS).collect();
    NonBlank::parse(&text)
}

/// The tracker call that tells issue `issue` of `update`; `None` for an
/// update the issue has no use for (a model turn's presentation).
pub(super) fn report_of(issue: &str, update: &runtime_collab::TaskRunUpdate) -> Option<IssueCall> {
    use runtime_collab::TaskRunUpdate;
    let issue = Id::from_raw(issue);
    let finish = |issue, outcome, text: &str| {
        Tracker::FINISH_RUN.with(tracker::FinishRun {
            issue,
            outcome,
            summary: message(text),
        })
    };
    Some(match update {
        TaskRunUpdate::Accepted => Tracker::REPORT_RUN.with(tracker::ReportRun {
            issue,
            phase: RunPhase::Accepted,
            text: None,
        }),
        TaskRunUpdate::Progress {
            message: text,
            presentation: None,
        } => Tracker::REPORT_RUN.with(tracker::ReportRun {
            issue,
            phase: RunPhase::Executing,
            text: message(text),
        }),
        TaskRunUpdate::Progress { .. } => return None,
        TaskRunUpdate::DirectionsApplied { revision } => {
            Tracker::INCORPORATE.with(tracker::Incorporate {
                issue,
                through: *revision,
            })
        }
        TaskRunUpdate::Succeeded { message: text } => finish(issue, RunOutcome::Done, text),
        TaskRunUpdate::Failed { message: text } => finish(issue, RunOutcome::Failed, text),
        TaskRunUpdate::Cancelled { message: text } => finish(issue, RunOutcome::Stopped, text),
    })
}

/// The id of the question about `action` with `arguments`: the same each
/// time the step asks, so asking again finds the person's answer.
pub(super) fn question_id(
    action: &TaskAction,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let digest = tilo_actions::draft::arguments_digest(arguments);
    let key = format!(
        "{}\0{}\0{}",
        tilo_actions::selection::logical_resource_id(&action.resource),
        action.action,
        hex::encode(digest)
    );
    blake3::hash(key.as_bytes())
        .to_hex()
        .as_str()
        .chars()
        .take(32)
        .collect()
}

/// The call that holds `tokens` and `cost_micros` for the next step on
/// `issue`.
pub(super) fn hold_spend(issue: &str, tokens: u64, cost_micros: u64) -> IssueCall {
    Tracker::HOLD_SPEND.with(tracker::HoldSpend {
        issue: Id::from_raw(issue),
        tokens,
        cost_micros,
    })
}

/// The call that settles the step in flight on `issue` with `step`.
pub(super) fn settle_spend(issue: &str, step: StepReceipt) -> IssueCall {
    Tracker::SETTLE_SPEND.with(tracker::SettleSpend {
        issue: Id::from_raw(issue),
        step,
    })
}

/// The call that pauses `issue`'s run for the limit `reason` names; a blank
/// reason says only that a limit was reached.
pub(super) fn pause_for_budget(issue: &str, reason: &str) -> Option<IssueCall> {
    let reason = if reason.trim().is_empty() {
        "The run reached a limit of its budget"
    } else {
        reason
    };
    Some(
        Tracker::PAUSE_RUN_FOR_BUDGET.with(tracker::PauseRunForBudget {
            issue: Id::from_raw(issue),
            reason: NonBlank::parse(reason)?,
        }),
    )
}

impl Session {
    /// Tell the run's issue of `update`, when the run is an issue's.
    pub(super) fn report_to_tracker(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        update: &runtime_collab::TaskRunUpdate,
    ) -> Result<()> {
        let Some(call) = issue_of(run).and_then(|issue| report_of(issue, update)) else {
            return Ok(());
        };
        self.tracker_step(run, &call)
    }

    /// Record on the run's issue that the step `action` with `arguments`
    /// waits for the person, with the draft they answer.
    pub(super) fn ask_on_tracker(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        (action, arguments): (&TaskAction, &serde_json::Map<String, serde_json::Value>),
    ) -> Result<()> {
        let Some(issue) = issue_of(run).map(Id::from_raw) else {
            return Ok(());
        };
        let catalogue = self.task_catalogue();
        let Some(offer) = catalogue.offers.iter().find(|offer| {
            offer.descriptor.address.resource == action.resource
                && offer.descriptor.address.action == action.action
        }) else {
            tracing::warn!(action = %action.action, "the step to ask about is no longer offered");
            return Ok(());
        };
        let draft = tilo_actions::Draft::render(&offer.descriptor, arguments);
        let question = Question {
            id: question_id(action, arguments),
            app: action.app.clone(),
            resource: tilo_actions::selection::logical_resource_id(&action.resource).into_owned(),
            action: action.action.clone(),
            arguments_digest: hex::encode(tilo_actions::draft::arguments_digest(arguments)),
            draft: serde_json::to_value(&draft)?,
            preview: None,
        };
        self.tracker_step(
            run,
            &Tracker::ASK_CONFIRMATION.with(tracker::AskConfirmation { issue, question }),
        )
    }

    /// Settle the run's answered question about `action` with `arguments`:
    /// the confirmed step started, so the answer is spent.
    pub(super) fn settle_on_tracker(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        (action, arguments): (&TaskAction, &serde_json::Map<String, serde_json::Value>),
    ) -> Result<()> {
        let Some(issue) = issue_of(run).map(Id::from_raw) else {
            return Ok(());
        };
        let id = question_id(action, arguments);
        self.tracker_step(
            run,
            &Tracker::SETTLE_CONFIRMATION.with(tracker::SettleConfirmation { issue, id }),
        )
    }

    /// Record on the run's issue that it read `resource`: a resource the run
    /// looked up in system search and took in after the one the request
    /// named. The issue lists it as the link it is, so its person follows it
    /// where the run went and the person who keeps that resource sees a run
    /// reach it.
    pub(super) fn record_read(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        resource: &ui::tilo_reference::ResourceAt,
    ) -> Result<()> {
        let Some(issue) = issue_of(run).map(Id::from_raw) else {
            return Ok(());
        };
        self.tracker_step(
            run,
            &Tracker::RECORD_READ.with(tracker::RecordRead {
                issue,
                resource: resource.clone(),
            }),
        )
    }

    /// Record on the run's issue a step it took: the program and its answer.
    pub(super) fn record_step(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        step: &ui::tilo_reference::ProgramStep,
    ) -> Result<()> {
        let Some(issue) = issue_of(run).map(Id::from_raw) else {
            return Ok(());
        };
        self.tracker_step(
            run,
            &Tracker::RECORD_STEP.with(tracker::RecordStep {
                issue,
                step: step.clone(),
            }),
        )
    }

    /// Record on the run's issue its answer as a view: the resource keeping it.
    pub(super) fn record_answer(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        view: &ui::tilo_reference::ResourceAt,
    ) -> Result<()> {
        let Some(issue) = issue_of(run).map(Id::from_raw) else {
            return Ok(());
        };
        self.tracker_step(
            run,
            &Tracker::RECORD_ANSWER.with(tracker::RecordAnswer {
                issue,
                view: view.clone(),
            }),
        )
    }

    /// Hold what the next model turn is expected to cost on the run's issue,
    /// before it starts: the issue's budget is what the turn may spend.
    ///
    /// Answers the issue's refusal, when the step would take the task past
    /// one of its budgets: the run stops stepping and waits for its person.
    pub(super) fn hold_spend_on_tracker(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        (tokens, cost_micros): (u64, u64),
    ) -> Result<Option<String>> {
        let Some(issue) = issue_of(run) else {
            return Ok(None);
        };
        let call = hold_spend(issue, tokens, cost_micros);
        self.refusing_tracker_step(run, &call)
    }

    /// Settle the step in flight on the run's issue with the receipt its
    /// provider gave: the model that answered, the device this agent runs
    /// on, and what the turn counted.
    pub(super) fn settle_spend_on_tracker(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        step: StepReceipt,
    ) -> Result<Option<String>> {
        let Some(issue) = issue_of(run) else {
            return Ok(None);
        };
        let call = settle_spend(issue, step);
        self.refusing_tracker_step(run, &call)
    }

    /// Pause the run's issue on the run's own behalf, saying `reason`: the
    /// limit it reached. The run waits for its person to raise it or let it
    /// go on.
    pub(super) fn pause_for_budget_on_tracker(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        reason: &str,
    ) -> Result<()> {
        let Some(call) = issue_of(run).and_then(|issue| pause_for_budget(issue, reason)) else {
            return Ok(());
        };
        if let Some(refused) = self.refusing_tracker_step(run, &call)? {
            tracing::warn!(%refused, "the issue did not pause for its budget");
        }
        Ok(())
    }

    /// The tracker's offer of `call`'s action, with the call bound to it.
    ///
    /// # Errors
    /// The call does not bind to Tasks' own arguments (a bug in building
    /// it), the Tasks app offers the action nowhere, or the offer refuses
    /// the arguments.
    fn tracker_call(&mut self, call: &IssueCall) -> Result<TaskAction> {
        if let Some(refusal) = call.refusal() {
            return Err(anyhow!(
                "the issue's `{}` was built with arguments its action does not take: {refusal}",
                call.id
            ));
        }
        let catalogue = self.task_catalogue();
        let offer = catalogue
            .offers
            .iter()
            .filter(|offer| {
                offer.descriptor.resource_kind == TRACKER_KIND
                    && offer.descriptor.address.action == call.id
            })
            .min_by_key(|offer| offer.descriptor.address.resource != TRACKER_KIND)
            .ok_or_else(|| anyhow!("the Tasks app offers the issue's `{}` nowhere", call.id))?;
        task_action_from_offer(offer, call.arguments.clone())
            .map_err(|error| anyhow!("the issue's `{}` refused its arguments: {error}", call.id))
    }

    /// As [`Self::tracker_step`], answering the issue's refusal instead of
    /// only logging it.
    fn refusing_tracker_step(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        call: &IssueCall,
    ) -> Result<Option<String>> {
        let child = match self.tracker_call(call) {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(action = call.id, %error, "the issue's action was not invoked");
                return Ok(None);
            }
        };
        let deadline = self.task_deadline.take();
        let outcome = self.invoke_task_action(run, &child, &call.arguments);
        self.task_deadline = deadline;
        match outcome? {
            Ok(_) => Ok(None),
            // The issue refused it: the text says why, and for a spend step
            // that is the budget it would cross.
            Err(TaskActionStop::Failed(reason)) => Ok(Some(reason)),
            Err(stop @ TaskActionStop::Cancelled(_)) => {
                tracing::warn!(
                    action = call.id,
                    ?stop,
                    "the issue did not take the run's report"
                );
                Ok(None)
            }
        }
    }

    /// Invoke the tracker's `call` as a child of `run`. The issue is told
    /// even when the run's time is up: how it ended is part of the run.
    /// A refusal is logged; the run itself goes on.
    fn tracker_step(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        call: &IssueCall,
    ) -> Result<()> {
        let step = match self.tracker_call(call) {
            Ok(step) => step,
            Err(error) => {
                tracing::warn!(action = call.id, %error, "the issue's action was not invoked");
                return Ok(());
            }
        };
        let deadline = self.task_deadline.take();
        let outcome = self.invoke_task_action(run, &step, &call.arguments);
        self.task_deadline = deadline;
        if let Err(stop) = outcome? {
            tracing::warn!(
                action = call.id,
                ?stop,
                "the issue did not take the run's report"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion"
    )]

    use super::*;
    use ui::tilo_resource::Replica;

    /// A run's agent on the tracker, and the issue its person asked it.
    fn asked() -> (Replica<Tracker>, String) {
        let mut person = Replica::<Tracker>::new().as_author("ada");
        person
            .apply(Tracker::ASK_AGENT.with(tracker::AskAgent {
                ask: tasks_tracker::model::AgentRequest {
                    id: Id::from_raw("one"),
                    title: "What is on today?".into(),
                    agent: "claude".into(),
                    quick: true,
                    ..Default::default()
                },
            }))
            .unwrap();
        (
            person.fork().as_agent_for("agent:claude", "ada"),
            "one".to_owned(),
        )
    }

    fn apply(agent: &mut Replica<Tracker>, call: IssueCall) {
        let id = call.id;
        if let Err(error) = agent.apply(call) {
            panic!("the issue refused the agent's `{id}`: {error}");
        }
    }

    #[test]
    fn each_report_is_one_of_the_issues_actions_and_presentations_stay_out_of_the_thread() {
        use runtime_collab::TaskRunUpdate;
        let call = report_of(
            "one",
            &TaskRunUpdate::Progress {
                message: "Read the index".into(),
                presentation: None,
            },
        )
        .unwrap();
        assert_eq!(call.id, "report-run");
        assert_eq!(call.arguments["issue"], "one");
        assert_eq!(call.arguments["text"], "Read the index");
        let call = report_of("one", &TaskRunUpdate::DirectionsApplied { revision: 2 }).unwrap();
        assert_eq!(
            (call.id, &call.arguments["through"]),
            ("incorporate", &serde_json::json!(2))
        );
        let call = report_of(
            "one",
            &TaskRunUpdate::Succeeded {
                message: String::new(),
            },
        )
        .unwrap();
        assert_eq!(call.id, "finish-run");
        assert_eq!(call.arguments["outcome"], "done");
        assert!(
            call.arguments
                .get("summary")
                .is_none_or(serde_json::Value::is_null),
            "an empty summary posts nothing"
        );
    }

    /// What the agent sends for a run that answers in its first turn is
    /// what Tasks' own actions take: it starts, holds and settles its one
    /// turn, and finishes with its answer, the issue done.
    #[test]
    fn a_run_answered_in_its_first_turn_holds_settles_and_finishes_on_its_issue() {
        use runtime_collab::TaskRunUpdate;
        let (mut agent, issue) = asked();
        apply(
            &mut agent,
            report_of(&issue, &TaskRunUpdate::Accepted).unwrap(),
        );
        apply(&mut agent, hold_spend(&issue, 1_200, 40));
        apply(
            &mut agent,
            settle_spend(
                &issue,
                StepReceipt {
                    step: 0,
                    model: "a model".into(),
                    device: "this laptop".into(),
                    input_tokens: 900,
                    output_tokens: 80,
                    ..StepReceipt::default()
                },
            ),
        );
        apply(
            &mut agent,
            report_of(
                &issue,
                &TaskRunUpdate::Succeeded {
                    message: "Two meetings.".into(),
                },
            )
            .unwrap(),
        );
        let state = agent.state().unwrap();
        let (_, done) = state.issues.iter().next().unwrap();
        let run = done.run.as_ref().unwrap();
        assert_eq!(run.phase, RunPhase::Done);
        assert_eq!(done.status, tasks_tracker::model::Status::Done);
        let spend = run.spend.as_ref().unwrap();
        assert_eq!((spend.input_tokens, spend.output_tokens), (900, 80));
        assert_eq!((spend.held_tokens, spend.held_cost_micros), (0, 0));
    }

    #[test]
    fn a_pause_for_the_budget_is_taken_by_the_issue() {
        let (mut agent, issue) = asked();
        apply(&mut agent, pause_for_budget(&issue, "  ").unwrap());
        let state = agent.state().unwrap();
        let (_, paused) = state.issues.iter().next().unwrap();
        assert!(paused.run.as_ref().unwrap().paused.is_some());
    }
}
