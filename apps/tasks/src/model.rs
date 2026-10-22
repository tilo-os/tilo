//! The `tasks.tracker` record: a workspace's issues.
//!
//! An issue has a number, a title, a markdown body, a status, an assignee,
//! labels, a parent (nested issues are sub-issues), links to other resources
//! and a thread. An agent task is an issue assigned to an agent: its runs are
//! actions on the issue ([`run`]), so every step is a permission in the one
//! action system and an entry in the issue's history.
//!
//! The app draws the record; anything else that needs to read or name issues
//! (the agent runner) reads this module.

mod issue;
mod record;
pub mod run;
pub mod search;

pub use issue::{
    AgentRequest, Imported, ImportedMessage, Issue, Link, Message, MessageKind, Status,
    agent_assignee, person_assignee,
};
pub use record::{Tracker, tracker};
pub use run::{
    Budget, Confirmation, ConfirmationState, ModelChoice, Question, Run, RunOutcome, RunPhase,
};

/// The record kind issues live in.
pub const KIND: &str = "tasks.tracker";

/// The app that keeps the record.
pub const APP_ID: &str = "so.tilo.tasks";

/// The resource address of `issue` in the tracker, the one its reference
/// chip, its search hit and its `open` name.
#[must_use]
pub fn issue_resource(issue: &ui::tilo_resource::Id) -> String {
    ui::tilo_resource::items::item_resource_id_in(KIND, "issues", issue)
}

#[cfg(test)]
mod tests;
