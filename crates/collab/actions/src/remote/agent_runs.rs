//! Runs of asks to this session's agents, as the session publishes them.
//!
//! A person asks an agent to do something — typed over a window or in the
//! agent composer — and the session queues it as a run. [`RESOURCE`] reads
//! every run the session keeps, and [`run`] reads one: a run is followed by
//! watching its own resource, and steering it or stopping it are its
//! resource's actions. Nothing here is an application's: a run's window is
//! the agent's, drawn wherever the ask was typed.
//!
//! The run channel names a run by the issue it works on, [`ISSUE_PREFIX`]
//! and its id, so the broker and the agent read one run under one address
//! whether they hear of it from the session or from the ask.

use serde::{Deserialize, Serialize};

/// The resource every run is read from, and the one an ask queues one on.
pub const RESOURCE: &str = "agent-runs";
/// What one run's resource id begins with, before its own id.
pub const RUN_PREFIX: &str = "agent-run:";

/// The resource [`run`] names for the run `id`.
#[must_use]
pub fn run(id: &str) -> String {
    format!("{RUN_PREFIX}{id}")
}

/// What a run is called on the run channel: the address of the issue whose
/// work it does.
pub const ISSUE_PREFIX: &str = "tasks.tracker/issues/";

/// The issue `id`'s run works on, as the run channel names it.
#[must_use]
pub fn issue(id: &str) -> String {
    format!("{ISSUE_PREFIX}{id}")
}

/// The id [`id_of_issue`] reads out of an issue's address.
#[must_use]
pub fn id_of_issue(issue: &str) -> Option<&str> {
    issue.strip_prefix(ISSUE_PREFIX).filter(|id| !id.is_empty())
}

/// Where a run is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// A person asked; its agent has not taken it up yet.
    #[default]
    Approved,
    /// Its agent took it up.
    Accepted,
    /// Its agent is deciding what to do.
    Planning,
    /// Its agent is acting.
    Executing,
    /// Its agent waits for the person.
    Waiting,
    /// It finished what it was asked.
    Done,
    /// It could not finish.
    Failed,
    /// The person stopped it.
    Stopped,
}

impl Phase {
    /// Whether the run is still going.
    #[must_use]
    pub const fn active(self) -> bool {
        matches!(
            self,
            Self::Approved | Self::Accepted | Self::Planning | Self::Executing | Self::Waiting
        )
    }

    /// The phase this session's run channel reported, in words: what an
    /// agent tells the session it is doing.
    #[must_use]
    pub fn reported(phase: &str) -> Option<Self> {
        match phase {
            "accepted" => Some(Self::Accepted),
            "planning" => Some(Self::Planning),
            "executing" => Some(Self::Executing),
            "waiting" => Some(Self::Waiting),
            _ => None,
        }
    }
}

/// The model a run plans with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunModel {
    /// The provider: `anthropic`, `ollama`.
    pub profile: String,
    pub model: String,
}

/// One message to a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Direction {
    /// Who sent it: the person asking.
    pub author: String,
    pub text: String,
    /// When it was sent, Unix milliseconds.
    pub at_ms: i64,
    /// Its run's agent already took it in.
    pub incorporated: bool,
}

/// How a model turn ended, when it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TurnEnd {
    Complete,
    Truncated,
    Refused,
    Cancelled,
    Failed,
}

/// What a run streams of the model turn it is in now, while it goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Live {
    /// The turn, as its agent names it.
    pub run: String,
    /// Its number within the turn: a later sequence replaces an earlier one.
    pub sequence: u64,
    /// What the model says, as it says it.
    pub text: String,
    /// What it reasons through, in a model that shows it.
    pub reasoning: String,
    pub detail: String,
    /// How the turn ended, once it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<TurnEnd>,
}

/// One run of an ask, as the session publishes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub title: String,
    /// What the agent is asked: the ask's instruction.
    pub instruction: String,
    /// The agent, by its bare id.
    pub agent: String,
    /// The person whose ask started it.
    pub starter: String,
    pub epoch: u64,
    pub phase: Phase,
    /// When it was asked, Unix milliseconds.
    pub at_ms: i64,
    /// Whether it was a question: a small task whose answer ends it.
    pub quick: bool,
    /// The model chosen for it; the agent's own without.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<RunModel>,
    /// The messages sent to it, oldest first: the last few of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directions: Vec<Direction>,
    /// How many messages it was sent, and how many its agent took in.
    pub messages: u64,
    pub incorporated: u64,
    /// What its agent said last in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    /// What its agent reported so far, oldest first: its steps, the last few
    /// of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
    /// The programs it ran so far, oldest first, each as a
    /// `tilo_reference::ProgramStep`: the last few of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub programs: Vec<serde_json::Value>,
    /// The view it answered with, when it answered with one: the resource
    /// keeping it, as a `tilo_reference::ResourceAt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<serde_json::Value>,
    /// The model turn it streams now, while it goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<Live>,
}

/// Every run the session keeps, as [`RESOURCE`] reads them: oldest first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Runs {
    pub runs: Vec<Run>,
}

/// What queueing an ask answered: the run it started, and what that run is
/// called on the run channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asked {
    /// The run's resource id ([`run`]).
    pub run: String,
    /// The issue it works on ([`issue`]), which the broker and the agent name
    /// it by.
    pub issue: String,
}
