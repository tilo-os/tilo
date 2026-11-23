//! Where an agent's task programs and the person's own line run on this host.
//!
//! An agent writes tilolang programs in its session; the session runs each as one step of it, and
//! every call the program makes is done as that agent, through the session's own API. A person
//! types a line in a conversation, and it runs as them: every call it makes is theirs, decided by
//! the workspace as their own click is. The program itself runs in the session's host — a process
//! of its own on a Linux desktop, a module of the page on the web — so a role names this service
//! and never a process, a socket or an engine.

use std::sync::Arc;

use tilo_actions::WorkGroup;
use tilo_actions::remote::{Outcome, Pending};

/// Who a program runs as.
#[derive(Clone, Copy)]
pub enum ProgramCaller<'a> {
    /// An agent's program: a step of its session, its work the session's.
    Agent {
        /// The agent whose program it is, by its id without the `agent:` prefix.
        agent: &'a str,
        /// The person the agent acts for, when one delegated it: `me` in the program.
        person: Option<&'a str>,
        /// The agent session the program is a step of.
        session: &'a str,
    },
    /// The person's own line: every call it makes is theirs, and its work is their own.
    Person {
        /// The person at this device.
        person: &'a str,
    },
}

/// One program the person or an agent asked for, as the step that carries it.
pub struct ProgramRequest<'a> {
    /// The workspace the session works in.
    pub workspace: &'a str,
    /// The API the program's calls go through, when its caller brings one: an agent session's
    /// turn does. The person's line runs through the session's own `for_person` api instead.
    pub provider: Option<Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>>,
    pub caller: ProgramCaller<'a>,
    /// The session's work, as history names it.
    pub group: WorkGroup,
    pub program: String,
    /// Send again a program whose answer was a preview.
    pub confirm: bool,
    /// What the person pointed at, as a resource literal.
    pub this: Option<String>,
    /// Set when the person stopped the run: the program makes no further call, and the run
    /// answers as stopped.
    pub stop: Option<Arc<std::sync::atomic::AtomicBool>>,
}

/// The programs this host runs, and how it runs them.
pub trait Programs: Send + Sync {
    /// Run `request`'s program and answer what it said, off the caller's pass: the program
    /// waits on its host and on every call it makes.
    fn run(&self, request: ProgramRequest<'_>) -> Result<Pending<Outcome>, String>;
}
