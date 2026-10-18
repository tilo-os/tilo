//! Read-only owner evidence and person-authored commands for Task Center.
//! Hosts authenticate commands; presentation never supplies an actor or grant.

use std::collections::BTreeMap;
use tilo_desktop_model::{WorkspaceTask, WorkspaceTaskControl};

/// Governor admission-barrier truth, never inferred from a model acknowledgement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskAdmissionState {
    Open,
    Stopping,
    Stopped,
    Unknown,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct TaskAdmissionView {
    pub run_epoch: u64,
    pub state: TaskAdmissionState,
}

#[derive(Clone, Debug)]
pub struct TaskReviewSnapshot {
    pub room: String,
    pub generation: u64,
    pub sampled_at_secs: i64,
    pub actor: String,
    pub writable: bool,
    pub tasks: Vec<WorkspaceTask>,
    pub controls: BTreeMap<String, WorkspaceTaskControl>,
    /// Only a governing owner's exact run ledger may populate a known state.
    pub admissions: BTreeMap<String, TaskAdmissionView>,
}

/// Every edit carries the run and instruction frontier the person reviewed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskReviewCommand {
    Approve {
        task_id: String,
        agent_id: String,
        run_epoch: u64,
        after_revision: u64,
    },
    Inspect {
        task_id: String,
    },
    Steer {
        task_id: String,
        run_epoch: u64,
        after_revision: u64,
        text: String,
    },
    Cancel {
        task_id: String,
        run_epoch: u64,
        after_revision: u64,
    },
    Retry {
        task_id: String,
        run_epoch: u64,
    },
    /// Answer the action the run paused at: confirm it as the person
    /// changed it, or decline it.
    AnswerConfirmation {
        task_id: String,
        run_epoch: u64,
        confirmation_id: String,
        answer: tilo_desktop_model::WorkspaceTaskConfirmationAnswer,
    },
    /// Confirm it, and let the agent take this action without asking from
    /// now on: an Allow for that one action, then the confirmation.
    AlwaysAllow {
        task_id: String,
        run_epoch: u64,
        confirmation_id: String,
        edits: Vec<tilo_actions::DraftEdit>,
    },
    /// Undo everything the agent's run of the task changed, as one: each
    /// document it changed undoes the changes grouped under the task, through
    /// its own history (which also takes back what a change queued outside
    /// it, such as a file renamed on disk). Later edits by others are kept.
    UndoRun {
        task_id: String,
        /// The documents the run changed: each step's app and record.
        steps: Vec<(String, String)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskReviewRequest {
    pub room: String,
    /// Observation fence; hosts derive authority from the selected certified device.
    pub observed_actor: String,
    pub invocation: uuid::Uuid,
    pub command: TaskReviewCommand,
}
