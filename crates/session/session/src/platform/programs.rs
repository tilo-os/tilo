//! Where an agent's task programs run on this host.
//!
//! An agent working a task writes tilolang programs; the session runs each as one step of the
//! task's run, and every call the program makes is done as that agent, through the run's own
//! door. The program itself runs in the forked task's host — a process of its own on a Linux
//! desktop, a module of the page on the web — so a role names this seam and never a process, a
//! socket or an engine.

use tilo_actions::WorkGroup;
use tilo_actions::remote::Pending;
use tilo_broker_core::steps::{HostStepOutcome, TaskRun};

/// One program an agent asked for, as the step that carries it.
pub struct ProgramRequest<'a> {
    /// The workspace the task runs in.
    pub workspace: &'a str,
    /// The run the program is a step of: the door it calls through admits every call it makes.
    pub run: &'a dyn TaskRun,
    /// The agent whose program it is, by its id without the `agent:` prefix.
    pub agent: &'a str,
    /// The person the agent acts for, when one delegated it: `me` in the program.
    pub person: Option<&'a str>,
    /// The issue the run works, by its address.
    pub issue: &'a str,
    /// The run, as the door names it in history.
    pub group: WorkGroup,
    pub program: String,
    /// Send again a program whose answer was a preview.
    pub confirm: bool,
    /// What the person pointed at, as a resource literal.
    pub this: Option<String>,
}

/// The task programs this host runs, and how it runs them.
pub trait Programs: Send + Sync {
    /// Run `request`'s program and answer what it said, off the caller's pass: the program
    /// waits on its host and on every call it makes.
    fn run(&self, request: ProgramRequest<'_>) -> Result<Pending<HostStepOutcome>, String>;
}
