//! Which agent run opened a window, as everyone who sees the window is told.

use serde::{Deserialize, Serialize};

/// The agent run that opened a window on a person's behalf: what the shell says
/// beside the window, so everyone sees whose agent put it there and in which
/// run. The window stays the person's: the run is who opened it, not who may
/// close it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OpenedBy {
    /// The run's id, as History groups the run's changes (`agent-run:…`).
    pub run: String,
    /// The agent whose session it is: its id and its person. What it is drawn in, so one
    /// agent is one colour beside every window it opened, on its face and in its avatar,
    /// whichever of its sessions opened it.
    pub agent: tilo_action_policy::MemberId,
    /// What the run is called: the agent's name.
    pub label: String,
    /// What the agent called the step that opened the window ("Fill Jane Doe's
    /// visa application"): its own words, never the agent's or its session's
    /// name; empty when the agent titled no step.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}
