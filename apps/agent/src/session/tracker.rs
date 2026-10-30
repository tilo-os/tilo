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

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::{Session, TaskAction, TaskActionStop, task_action_from_offer};

/// The tracker's resource kind; an issue's address is
/// `tasks.tracker/issues/<id>`.
const TRACKER_KIND: &str = "tasks.tracker";
const ISSUE_PREFIX: &str = "tasks.tracker/issues/";
/// The longest text a thread message keeps.
const MAX_MESSAGE_CHARS: usize = 16 * 1024;

/// The issue `run` works on, by its id in the tracker.
pub(super) fn issue_of(run: &runtime_collab::TaskRunEnvelope) -> Option<&str> {
    run.task_id
        .strip_prefix(ISSUE_PREFIX)
        .filter(|issue| !issue.is_empty())
}

fn message(text: &str) -> Value {
    let text: String = text.trim().chars().take(MAX_MESSAGE_CHARS).collect();
    if text.is_empty() {
        Value::Null
    } else {
        Value::String(text)
    }
}

/// The tracker action and arguments that tell issue `issue` of `update`;
/// `None` for an update the issue has no use for (a model turn's
/// presentation).
pub(super) fn report_of(
    issue: &str,
    update: &runtime_collab::TaskRunUpdate,
) -> Option<(&'static str, Map<String, Value>)> {
    use runtime_collab::TaskRunUpdate;
    let (action, arguments) = match update {
        TaskRunUpdate::Accepted => ("report-run", json!({ "phase": "accepted" })),
        TaskRunUpdate::Progress {
            message: text,
            presentation: None,
        } => (
            "report-run",
            json!({ "phase": "executing", "text": message(text) }),
        ),
        TaskRunUpdate::Progress { .. } => return None,
        TaskRunUpdate::DirectionsApplied { revision } => {
            ("incorporate", json!({ "through": revision }))
        }
        TaskRunUpdate::Succeeded { message: text } => (
            "finish-run",
            json!({ "outcome": "done", "summary": message(text) }),
        ),
        TaskRunUpdate::Failed { message: text } => (
            "finish-run",
            json!({ "outcome": "failed", "summary": message(text) }),
        ),
        TaskRunUpdate::Cancelled { message: text } => (
            "finish-run",
            json!({ "outcome": "stopped", "summary": message(text) }),
        ),
    };
    let mut arguments = arguments.as_object().cloned().unwrap_or_default();
    arguments.insert("issue".into(), Value::String(issue.to_owned()));
    Some((action, arguments))
}

/// The id of the question about `action` with `arguments`: the same each
/// time the step asks, so asking again finds the person's answer.
pub(super) fn question_id(action: &TaskAction, arguments: &Map<String, Value>) -> String {
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

/// One model turn's receipt, as the run's issue keeps it: which model
/// answered, on which device, and what the turn counted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct StepReceipt {
    /// The turn's number in the run, from 1.
    pub(super) step: u64,
    /// The model that answered it; empty when no model planned the run.
    pub(super) model: String,
    /// The device this agent runs on: the machine that paid for the turn.
    pub(super) device: String,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    /// Calls priced per call, such as a web search.
    pub(super) priced_calls: u64,
    /// What the turn cost, in millionths of `currency`; zero on a local
    /// model, which spends this device's time rather than money.
    pub(super) cost_micros: u64,
    pub(super) currency: String,
}

impl StepReceipt {
    /// The receipt as the issue's `settle-spend` action takes it.
    pub(super) fn into_json(self) -> Value {
        json!({
            "step": self.step,
            "model": self.model,
            "device": self.device,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "priced_calls": self.priced_calls,
            "cost_micros": self.cost_micros,
            "currency": self.currency,
        })
    }
}

impl Session {
    /// Tell the run's issue of `update`, when the run is an issue's.
    pub(super) fn report_to_tracker(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        update: &runtime_collab::TaskRunUpdate,
    ) -> Result<()> {
        let Some((action, arguments)) = issue_of(run).and_then(|issue| report_of(issue, update))
        else {
            return Ok(());
        };
        self.tracker_step(run, action, &arguments)
    }

    /// Record on the run's issue that the step `action` with `arguments`
    /// waits for the person, with the draft they answer.
    pub(super) fn ask_on_tracker(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        (action, arguments): (&TaskAction, &Map<String, Value>),
    ) -> Result<()> {
        let Some(issue) = issue_of(run).map(str::to_owned) else {
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
        let question = json!({
            "id": question_id(action, arguments),
            "app": action.app,
            "resource": tilo_actions::selection::logical_resource_id(&action.resource),
            "action": action.action,
            "arguments_digest": hex::encode(tilo_actions::draft::arguments_digest(arguments)),
            "draft": draft,
        });
        let arguments = json!({ "issue": issue, "question": question });
        self.tracker_step(
            run,
            "ask-confirmation",
            &arguments.as_object().cloned().unwrap_or_default(),
        )
    }

    /// Settle the run's answered question about `action` with `arguments`:
    /// the confirmed step started, so the answer is spent.
    pub(super) fn settle_on_tracker(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        (action, arguments): (&TaskAction, &Map<String, Value>),
    ) -> Result<()> {
        let Some(issue) = issue_of(run).map(str::to_owned) else {
            return Ok(());
        };
        let arguments = json!({ "issue": issue, "id": question_id(action, arguments) });
        self.tracker_step(
            run,
            "settle-confirmation",
            &arguments.as_object().cloned().unwrap_or_default(),
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
        let Some(issue) = issue_of(run) else {
            return Ok(());
        };
        let arguments = json!({ "issue": issue, "resource": resource });
        self.tracker_step(
            run,
            "record-read",
            &arguments.as_object().cloned().unwrap_or_default(),
        )
    }

    /// Record on the run's issue a step it took: the program and its answer.
    pub(super) fn record_step(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        step: &ui::tilo_reference::ProgramStep,
    ) -> Result<()> {
        let Some(issue) = issue_of(run) else {
            return Ok(());
        };
        let arguments = json!({ "issue": issue, "step": step });
        self.tracker_step(
            run,
            "record-step",
            &arguments.as_object().cloned().unwrap_or_default(),
        )
    }

    /// Record on the run's issue its answer as a view: the resource keeping it.
    pub(super) fn record_answer(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        view: &ui::tilo_reference::ResourceAt,
    ) -> Result<()> {
        let Some(issue) = issue_of(run) else {
            return Ok(());
        };
        let arguments = json!({ "issue": issue, "view": view });
        self.tracker_step(
            run,
            "record-answer",
            &arguments.as_object().cloned().unwrap_or_default(),
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
        held: (u64, u64),
    ) -> Result<Option<String>> {
        let (tokens, cost_micros) = held;
        let Some(issue) = issue_of(run).map(str::to_owned) else {
            return Ok(None);
        };
        let arguments = json!({ "issue": issue, "tokens": tokens, "cost_micros": cost_micros });
        self.refusing_tracker_step(
            run,
            "hold-spend",
            &arguments.as_object().cloned().unwrap_or_default(),
        )
    }

    /// Settle the step in flight on the run's issue with the receipt its
    /// provider gave: the model that answered, the device this agent runs
    /// on, and what the turn counted.
    pub(super) fn settle_spend_on_tracker(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        step: StepReceipt,
    ) -> Result<Option<String>> {
        let Some(issue) = issue_of(run).map(str::to_owned) else {
            return Ok(None);
        };
        let arguments = json!({ "issue": issue, "step": step.into_json() });
        self.refusing_tracker_step(
            run,
            "settle-spend",
            &arguments.as_object().cloned().unwrap_or_default(),
        )
    }

    /// Pause the run's issue on the run's own behalf, saying `reason`: the
    /// limit it reached. The run waits for its person to raise it or let it
    /// go on.
    pub(super) fn pause_for_budget_on_tracker(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        reason: &str,
    ) -> Result<()> {
        let Some(issue) = issue_of(run).map(str::to_owned) else {
            return Ok(());
        };
        let arguments = json!({ "issue": issue, "reason": reason });
        let _ = self.refusing_tracker_step(
            run,
            "pause-run-for-budget",
            &arguments.as_object().cloned().unwrap_or_default(),
        )?;
        Ok(())
    }

    /// As [`Self::tracker_step`], answering the issue's refusal instead of
    /// only logging it.
    fn refusing_tracker_step(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Option<String>> {
        let catalogue = self.task_catalogue();
        let Some(offer) = catalogue
            .offers
            .iter()
            .filter(|offer| {
                offer.descriptor.resource_kind == TRACKER_KIND
                    && offer.descriptor.address.action == action
            })
            .min_by_key(|offer| offer.descriptor.address.resource != TRACKER_KIND)
        else {
            tracing::warn!(action, "the Tasks app offers the issue's actions nowhere");
            return Ok(None);
        };
        let child = match task_action_from_offer(offer, arguments.clone()) {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(action, %error, "the issue's action refused its arguments");
                return Ok(None);
            }
        };
        let deadline = self.task_deadline.take();
        let outcome = self.invoke_task_action(run, &child, arguments);
        self.task_deadline = deadline;
        match outcome? {
            Ok(_) => Ok(None),
            // The issue refused it: the text says why, and for a spend step
            // that is the budget it would cross.
            Err(TaskActionStop::Failed(reason)) => Ok(Some(reason)),
            Err(stop @ TaskActionStop::Cancelled(_)) => {
                tracing::warn!(action, ?stop, "the issue did not take the run's report");
                Ok(None)
            }
        }
    }

    /// Invoke the tracker's `action` as a child of `run`. The issue is told
    /// even when the run's time is up: how it ended is part of the run.
    /// A refusal is logged; the run itself goes on.
    fn tracker_step(
        &mut self,
        run: &runtime_collab::TaskRunEnvelope,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<()> {
        let catalogue = self.task_catalogue();
        let offered = catalogue
            .offers
            .iter()
            .filter(|offer| {
                offer.descriptor.resource_kind == TRACKER_KIND
                    && offer.descriptor.address.action == action
            })
            .min_by_key(|offer| offer.descriptor.address.resource != TRACKER_KIND)
            .map(|offer| task_action_from_offer(offer, arguments.clone()));
        let step = match offered {
            Some(Ok(step)) => step,
            Some(Err(error)) => {
                tracing::warn!(action, %error, "the issue's action refused its arguments");
                return Ok(());
            }
            None => {
                tracing::warn!(action, "the Tasks app offers the issue's actions nowhere");
                return Ok(());
            }
        };
        let deadline = self.task_deadline.take();
        let outcome = self.invoke_task_action(run, &step, arguments);
        self.task_deadline = deadline;
        if let Err(stop) = outcome? {
            tracing::warn!(action, ?stop, "the issue did not take the run's report");
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

    #[test]
    fn each_report_is_one_of_the_issues_actions_and_presentations_stay_out_of_the_thread() {
        use runtime_collab::TaskRunUpdate;
        let (action, arguments) = report_of(
            "one",
            &TaskRunUpdate::Progress {
                message: "Read the index".into(),
                presentation: None,
            },
        )
        .unwrap();
        assert_eq!(action, "report-run");
        assert_eq!(arguments["issue"], "one");
        assert_eq!(arguments["text"], "Read the index");
        let (action, arguments) =
            report_of("one", &TaskRunUpdate::DirectionsApplied { revision: 2 }).unwrap();
        assert_eq!((action, &arguments["through"]), ("incorporate", &json!(2)));
        let (action, arguments) = report_of(
            "one",
            &TaskRunUpdate::Succeeded {
                message: String::new(),
            },
        )
        .unwrap();
        assert_eq!(action, "finish-run");
        assert_eq!(arguments["outcome"], "done");
        assert!(
            arguments["summary"].is_null(),
            "an empty summary posts nothing"
        );
    }
}
