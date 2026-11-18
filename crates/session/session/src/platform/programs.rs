//! Where an agent's task programs run on this host.
//!
//! An agent writes tilolang programs in its session; the session runs each as one step of it, and
//! every call the program makes is done as that agent, through the session's own API. The program
//! itself runs in the session's host — a process of its own on a Linux
//! desktop, a module of the page on the web — so a role names this service and never a process, a
//! socket or an engine.

use tilo_actions::WorkGroup;
use tilo_actions::remote::Pending;
use tilo_broker_core::steps::HostStepOutcome;

/// One program an agent asked for, as the step that carries it.
pub struct ProgramRequest<'a> {
    /// The workspace the session works in.
    pub workspace: &'a str,
    /// The session's own API: it admits every call the program makes.
    pub provider: std::sync::Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
    /// The agent whose program it is, by its id without the `agent:` prefix.
    pub agent: &'a str,
    /// The person the agent acts for, when one delegated it: `me` in the program.
    pub person: Option<&'a str>,
    /// The agent session the program is a step of.
    pub session: &'a str,
    /// The session's work, as history names it.
    pub group: WorkGroup,
    pub program: String,
    /// Send again a program whose answer was a preview.
    pub confirm: bool,
    /// What the person pointed at, as a resource literal.
    pub this: Option<String>,
}

/// The agent sessions' programs this host runs, and how it runs them.
pub trait Programs: Send + Sync {
    /// Run `request`'s program and answer what it said, off the caller's pass: the program
    /// waits on its host and on every call it makes.
    fn run(&self, request: ProgramRequest<'_>) -> Result<Pending<HostStepOutcome>, String>;
}
