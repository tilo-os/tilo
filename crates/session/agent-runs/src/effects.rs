//! The actions of the runs: queuing an ask, and what a run takes while it
//! goes.

use tilo_resource::Effect;

use crate::wire;

/// The `agent-runs` resource's actions: what a person asks of an agent.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.agent-runs")]
pub enum RunsAction {
    /// Ask an agent to do something: a run of its own, started at once and
    /// approved by the asking itself.
    #[effect(
        label = "Ask an agent",
        icon = "sparkles",
        keywords = "ask agent, ask claude, hand over, delegate, task, agent run, frage agent"
    )]
    Ask {
        /// The ask's id, if the caller names one: the run is known by it, and
        /// asking again with the same id replaces nothing — it is refused.
        #[param("The ask's id; one is minted without")]
        id: Option<String>,
        #[param("What the agent is asked, in a line")]
        title: String,
        #[param("What the agent is asked to do, in markdown")]
        body: Option<String>,
        #[param("The agent to ask, as agent:<id>")]
        agent: String,
        #[param("The model it plans with, in place of the agent's own")]
        model: Option<wire::RunModel>,
        #[param("Where to act, when the ask is about a selection; it grants nothing")]
        subject: Option<serde_json::Value>,
        #[param("Whether it is a question: a small task whose answer ends it")]
        quick: Option<bool>,
    },
}

/// The `agent-run:<id>` resource's actions: steering a run, stopping it, and
/// what its agent records of the work it did.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.agent-run")]
pub enum RunAction {
    /// Tell the run something: its agent takes the message in as it goes.
    #[effect(
        label = "Send a message",
        icon = "message-square",
        keywords = "steer, tell it, correct, answer, reply, redirect, nachricht"
    )]
    SendMessage {
        #[param("What to tell the agent")]
        text: String,
    },
    /// Stop the run: its agent takes no further step. What it already did
    /// stays.
    #[effect(
        idempotent,
        label = "Stop",
        icon = "square",
        keywords = "stop, cancel, end, halt, abort, abbrechen"
    )]
    StopRun,
    /// Record a program the run ran, with what it answered.
    #[effect(
        audience = "agents",
        label = "Record a step",
        icon = "terminal",
        keywords = "record step, program, tilolang"
    )]
    RecordStep {
        #[param("The step: its program and what it answered, as a `ProgramStep`")]
        step: serde_json::Value,
    },
    /// Record the run's answer as a view: the resource that keeps it.
    #[effect(
        audience = "agents",
        label = "Record an answer",
        icon = "file",
        keywords = "record answer, view, result"
    )]
    RecordAnswer {
        #[param("The resource that keeps the answer's view, as a `ResourceAt`")]
        view: serde_json::Value,
    },
}
