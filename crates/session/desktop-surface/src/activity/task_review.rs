//! Read-only owner evidence and person-authored commands for Task Center.
//! Hosts authenticate commands; presentation never supplies an actor or grant.

use std::collections::BTreeMap;
use tilo_desktop_model::{WorkspaceTask, WorkspaceTaskControl};

#[derive(Clone, Debug)]
pub struct TaskReviewSnapshot {
    pub room: String,
    pub generation: u64,
    pub sampled_at_secs: i64,
    pub actor: String,
    pub writable: bool,
    pub tasks: Vec<WorkspaceTask>,
    pub controls: BTreeMap<String, WorkspaceTaskControl>,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskReviewRequest {
    pub room: String,
    /// Observation fence; hosts derive authority from the selected certified device.
    pub observed_actor: String,
    pub invocation: uuid::Uuid,
    pub command: TaskReviewCommand,
}
