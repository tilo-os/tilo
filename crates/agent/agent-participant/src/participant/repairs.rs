//! A task's repair budget: a step of a model's plan that fails is planned
//! around, told why and what already ran, a few times per task, so a long
//! task survives more than one slip (a wrong column, an id for a name, a
//! clip placed on a taken track). A refusal that any plan would meet again,
//! the same failure twice, or one step failing over and over still ends it.

use tilo_actions::door::DoorError;
use tilo_actions::remote::ActionHostError;

use super::running::step_brief;
use crate::planner::{Step, StepAction};

/// The most failed steps one task is planned around.
pub(super) const MOST_REPAIRS: u32 = 3;

/// The most repairs in a row for one step (its app, record and operation)
/// with no success of it between.
pub(super) const MOST_IN_A_ROW: u32 = 2;

/// Why a failed step ends its task rather than being planned around.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unrepaired {
    /// No plan changes the refusal: the person declined or nobody was there
    /// to confirm, or the agent may not act there.
    Final,
    /// The step failed before with the same arguments and the same error.
    Repeated,
    /// The same step failed [`MOST_IN_A_ROW`] times in a row.
    InARow,
    /// The task used every repair ([`MOST_REPAIRS`]).
    Spent,
}

/// A failed step, as two failures are told apart.
#[derive(Debug, PartialEq, Eq)]
struct Failure {
    /// The step's app, record and operation ([`step_key`]).
    step: String,
    arguments: String,
    error: String,
}

#[derive(Debug, Default)]
pub(super) struct Repairs {
    count: u32,
    /// Each failure planned around, oldest first.
    failures: Vec<Failure>,
    /// The step the latest repairs were for and how many in a row; a
    /// success of that step ends the run.
    streak: Option<(String, u32)>,
    /// The latest repair, for the plan log: the failed step, briefly, and
    /// its error.
    latest: Option<(String, String)>,
    /// Why the task's last failure was not planned around.
    unrepaired: Option<Unrepaired>,
}

impl Repairs {
    /// Whether `step`, which failed with `error`, is planned around: it is
    /// counted when it is; otherwise why not.
    pub(super) fn admit(&mut self, step: &Step, error: &str) -> Result<u32, Unrepaired> {
        let decided = self.decide(step, error);
        if let Err(reason) = decided {
            self.unrepaired = Some(reason);
        }
        decided
    }

    fn decide(&mut self, step: &Step, error: &str) -> Result<u32, Unrepaired> {
        if is_final(error) {
            return Err(Unrepaired::Final);
        }
        let failure = Failure {
            step: step_key(step),
            arguments: match &step.action {
                StepAction::Invoke { arguments, .. } => {
                    serde_json::Value::Object(arguments.clone()).to_string()
                }
                _ => String::new(),
            },
            error: error.to_owned(),
        };
        if self.failures.contains(&failure) {
            return Err(Unrepaired::Repeated);
        }
        let in_a_row = match &self.streak {
            Some((key, count)) if *key == failure.step => count.saturating_add(1),
            _ => 1,
        };
        if in_a_row > MOST_IN_A_ROW {
            return Err(Unrepaired::InARow);
        }
        if self.count >= MOST_REPAIRS {
            return Err(Unrepaired::Spent);
        }
        self.count = self.count.saturating_add(1);
        self.streak = Some((failure.step.clone(), in_a_row));
        self.latest = Some((step_brief(step), failure.error.clone()));
        self.failures.push(failure);
        Ok(self.count)
    }

    /// `step` succeeded: a later failure of it starts a new run.
    pub(super) fn ran(&mut self, step: &Step) {
        if self
            .streak
            .as_ref()
            .is_some_and(|(key, _)| *key == step_key(step))
        {
            self.streak = None;
        }
    }

    /// Whether the task was planned around a failure or ended on one it
    /// was not.
    pub(super) fn any(&self) -> bool {
        self.count > 0 || self.unrepaired.is_some()
    }

    /// The latest repair for the plan log: its number, of how many, the
    /// step that failed and why.
    pub(super) fn logged_latest(&self) -> Option<serde_json::Value> {
        let (step, reason) = self.latest.as_ref()?;
        Some(serde_json::json!({
            "count": self.count,
            "most": MOST_REPAIRS,
            "step": step,
            "reason": reason,
        }))
    }

    /// The task's repairs for the log of its end: how many, and why its
    /// last failure was not planned around.
    pub(super) fn logged(&self) -> serde_json::Value {
        serde_json::json!({
            "count": self.count,
            "unrepaired": self.unrepaired.map(|reason| format!("{reason:?}")),
        })
    }
}

/// A step's app, record and operation: what makes two failures the same
/// step whatever their arguments.
fn step_key(step: &Step) -> String {
    let operation = match &step.action {
        StepAction::Invoke { address, .. } => format!("{}#{}", address.resource, address.action),
        StepAction::Open => "open".to_owned(),
        StepAction::Continue(_) => "continue".to_owned(),
        StepAction::Wait(_) => "wait".to_owned(),
    };
    format!(
        "{} {} {operation}",
        step.app,
        step.record.as_deref().unwrap_or_default()
    )
}

/// Whether `error` is a refusal no other plan would meet differently: the
/// person declined or nobody was there to confirm, or the agent is not
/// permitted the app or room, or was paused or stopped.
pub(super) fn is_final(error: &str) -> bool {
    [
        DoorError::ConfirmationRequired.code(),
        ActionHostError::NotPermitted.code(),
        "the agent is not delegated",
        "the agent is paused or stopped",
    ]
    .iter()
    .any(|refusal| error.starts_with(refusal))
}
