//! An agent's run on an issue.
//!
//! An issue assigned to an agent is queued work; a person's `approve-run`
//! starts run *n*+1, which acts for that person. The agent reports its phase
//! and its findings with the run verbs, which post into the issue's thread,
//! asks before an action whose answer is "ask first", and finishes. Each of
//! these is an action on the tracker, so who may approve, steer, answer or
//! report is decided by the same grants as any other action.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Atom, JsonSchema, Model};

/// The longest turn a budget admits: 15 minutes.
pub const MAX_TURN_MS: u64 = 900_000;
/// The longest run a budget admits: two hours.
pub const MAX_TOTAL_MS: u64 = 7_200_000;

/// Where a run is.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RunPhase {
    /// A person approved it; its agent has not taken it up yet.
    #[default]
    Approved,
    /// Its agent took it up.
    Accepted,
    /// Its agent is deciding what to do.
    Planning,
    /// Its agent is acting.
    Executing,
    /// Its agent waits for a person to answer an action that asks first.
    Waiting,
    /// It finished what it was asked.
    Done,
    /// It could not finish.
    Failed,
    /// A person stopped it.
    Stopped,
}

impl RunPhase {
    /// Whether the run is still going.
    #[must_use]
    pub const fn active(self) -> bool {
        matches!(
            self,
            Self::Approved | Self::Accepted | Self::Planning | Self::Executing | Self::Waiting
        )
    }

    /// Whether an agent may report this phase while it works: the phases a
    /// run moves through on its own.
    #[must_use]
    pub const fn reportable(self) -> bool {
        matches!(self, Self::Accepted | Self::Planning | Self::Executing)
    }

    /// The Fluent id naming the phase.
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Approved => "tasks-run-approved",
            Self::Accepted => "tasks-run-accepted",
            Self::Planning => "tasks-run-planning",
            Self::Executing => "tasks-run-executing",
            Self::Waiting => "tasks-run-waiting",
            Self::Done => "tasks-run-done",
            Self::Failed => "tasks-run-failed",
            Self::Stopped => "tasks-run-stopped",
        }
    }
}

/// How a run ended, as its agent reports it.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RunOutcome {
    /// It did what it was asked: the issue goes to review.
    #[default]
    Done,
    /// It could not: the issue goes back to Todo, to approve again.
    Failed,
    /// It stopped because a person asked it to.
    Stopped,
}

/// Time ceilings for an issue's runs.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The longest one model turn may take, in milliseconds.
    pub turn_ms: u64,
    /// The longest the whole run may take, in milliseconds.
    pub total_ms: u64,
}

impl Budget {
    /// Whether a budget of these ceilings is one a run may have: each within
    /// its range, a turn no longer than the run.
    #[must_use]
    pub fn admits(turn_ms: u64, total_ms: u64) -> bool {
        let turn = (1..=MAX_TURN_MS).contains(&turn_ms);
        let total = (1..=MAX_TOTAL_MS).contains(&total_ms);
        turn && total && turn_ms <= total_ms
    }
}

/// A model of one provider, chosen for an issue's runs in place of the one
/// chosen for its agent.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelChoice {
    /// The provider profile, by id.
    pub profile: String,
    /// The model, by its id at that provider.
    pub model: String,
}

/// What a run asks its person about, as the door that paused it says.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Question {
    /// The question's id.
    pub id: String,
    /// The app whose action it is.
    pub app: String,
    /// The action's id.
    pub action: String,
    /// The resource it acts on.
    pub resource: String,
    /// The digest of the exact arguments asked about.
    pub arguments_digest: String,
    /// The action's draft: one field per parameter, secrets withheld.
    pub draft: serde_json::Value,
    /// The change the step makes, before and after, where it previews one:
    /// the person decides on the difference rather than on the end state
    /// alone. Absent for a step whose change is not a tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<serde_json::Value>,
}

/// Where a confirmation is.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ConfirmationState {
    /// Nobody has answered yet.
    #[default]
    Waiting,
    Confirmed,
    Declined,
}

/// An action a run asks its person before taking.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Confirmation {
    /// The question's id.
    pub id: String,
    /// The app whose action it is.
    pub app: String,
    /// The action's id.
    pub action: String,
    /// The resource it acts on.
    pub resource: String,
    /// Hex of the digest of the exact arguments asked about: a
    /// confirmation admits those arguments (with the person's edits) and no
    /// others.
    pub arguments_digest: String,
    /// What it would do, as a draft the person can edit: one field per
    /// parameter, secrets withheld (`tilo_actions::draft`). Shaped by the
    /// door that paused the run, never by this record.
    pub draft: serde_json::Value,
    /// The change it makes, before and after, where it previews one. Shaped
    /// by the door that paused the run like the draft, and carried so the
    /// host puts the same two sides to the person who answers.
    pub preview: Option<serde_json::Value>,
    pub state: ConfirmationState,
    /// The person's edits to the draft, when they confirmed with edits.
    pub edits: serde_json::Value,
    /// Why the person declined.
    pub reason: String,
    /// Who answered.
    pub answered_by: String,
}

/// One run of an agent on an issue.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Run {
    /// The run's number on its issue, from 1.
    pub epoch: u64,
    /// The agent working, as the door names it (`agent:<id>`).
    pub agent: Atom,
    /// The person who approved it: the agent acts for them.
    pub starter: String,
    pub phase: RunPhase,
    /// The model the run plans with, as the issue chose it when the run
    /// started; the agent's own without.
    pub model: Option<ModelChoice>,
    pub started_ms: i64,
    /// When it last reported, or ended.
    pub updated_ms: i64,
    /// The action it waits on, or the last one it asked about.
    pub confirmation: Option<Confirmation>,
}
