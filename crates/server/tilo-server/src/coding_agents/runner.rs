//! The coding agents of a host's workspaces: a run for each one that has
//! work, started by the first words it is given and at rest when it has
//! none.
//!
//! A coding agent's harness lives in its workspace's machine only while it
//! has something to do. The first words it is given start it; words given
//! while it runs reach the same run as its next message, at its next step;
//! and once it has finished what it was asked and nothing came since, its
//! input is closed, it ends by itself and the machine may sleep. Words that
//! come later start it again, taking its conversation up where it stood by
//! the harness's own id for it. Many run at once, each in its own run.
//!
//! Starting one costs its person money and may change a repository, so a
//! start that nobody was asked about happens only where a lasting yes of
//! its person covers it ([`Permits`]); everywhere else the keeper says that
//! it needs their yes ([`NotTold::NeedsYes`]) and starts nothing.
//!
//! Whoever knows what a coding agent is in the workspace's records (the
//! runner of sub-agents whose work is done in the machine) describes it as a
//! [`CodingAgent`] and hands its words here; nothing in this file reads a
//! record.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;
use ush::served_runs::{ServeRun, wire};

use super::harness::{Harnesses, Start};
use super::run::{CodingRun, Conversation, Ended, Machine, Serves};
use super::working_copy::{self, WorkingCopy};

/// How long a coding agent that finished what it was asked is kept running
/// for its next words before it is put to rest.
pub const IDLE: Duration = Duration::from_secs(120);

/// How long a coding harness is given to end once asked.
const STOP_GRACE: Duration = Duration::from_secs(20);

/// Where served runs are started: a host's shells, in its workspaces'
/// machines.
pub trait Machines: Send + Sync {
    /// Start `start` in `workspace`'s machine and answer its connection.
    fn serve(&self, workspace: &str, start: ServeRun) -> Result<Arc<dyn Machine>, String>;
}

impl Machines for crate::agents::HostShells {
    /// Beside the server, where it starts its workspaces' programs itself. Where their
    /// machines are on other computers, on the member device of the one that holds the
    /// workspace's machine, held from here ([`crate::placed_programs`]).
    fn serve(&self, workspace: &str, start: ServeRun) -> Result<Arc<dyn Machine>, String> {
        match self {
            Self::Here(terminals, _) => terminals
                .shells()
                .serve_run(workspace, None, start)
                .map(|served| Arc::new(served) as Arc<dyn Machine>)
                .map_err(|error| error.to_string()),
            Self::Placed(placed) => crate::placed_programs::serve_placed(placed, workspace, start)
                .map(|carried| carried as Arc<dyn Machine>),
        }
    }
}

/// One coding agent, as the workspace's records describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodingAgent {
    /// The workspace whose machine it works in.
    pub workspace: String,
    /// Its id in the workspace's list of its agents.
    pub id: String,
    /// Which coding harness it is; empty for the first a host knows.
    pub harness: String,
    /// The folder it was started in: one of the workspace's files, or a
    /// path of the machine's own.
    pub repository: String,
    /// Its own working copy of that repository; none works in the folder
    /// itself.
    pub copy: Option<WorkingCopy>,
    /// The harness's own id for its conversation, once it has one.
    pub session: Option<String>,
    /// How its person wants it to work.
    pub instruction: Option<String>,
    /// The person it works for, as the workspace names them.
    pub person: String,
    /// The person's own agent its line of starters ends at: the agent a start that nobody
    /// was asked about is let for.
    pub under: String,
}

/// What says whether a coding agent may start without its person being
/// asked.
pub trait Permits: Send + Sync {
    /// Whether the person `agent` works for let coding agents start in its
    /// workspace's machine without being asked each time.
    fn stands(&self, agent: &CodingAgent) -> bool;
}

/// A host that keeps no lasting yes of anybody: nothing starts unasked.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPermit;

impl Permits for NoPermit {
    fn stands(&self, _: &CodingAgent) -> bool {
        false
    }
}

/// What lets a coding agent start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permit {
    /// Its person's lasting yes for the workspace, where one stands.
    Lasting,
    /// Its person's yes to this one start.
    Once,
}

/// Why words did not reach a coding agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotTold {
    /// It does not run, and starting it needs its person's yes: nothing was
    /// started.
    NeedsYes,
    /// It could not be started: why, in words its person reads.
    Failed(String),
}

/// What is known of a run between two looks at it.
#[derive(Default)]
struct State {
    /// When it finished what it was asked, with nothing said to it since.
    finished: Option<Instant>,
    /// Its input was closed: it is ending by itself.
    resting: bool,
    over: bool,
}

/// A coding agent's conversation, watched for when its run has nothing to
/// do and when it is over.
struct Watched {
    conversation: Arc<dyn Conversation>,
    state: Arc<Mutex<State>>,
}

impl Conversation for Watched {
    fn session(&self, id: &str) {
        self.conversation.session(id);
    }

    fn says(&self, words: &str) {
        self.state.lock_recover().finished = None;
        self.conversation.says(words);
    }

    fn doing(&self, what: &str) {
        self.state.lock_recover().finished = None;
        self.conversation.doing(what);
    }

    fn finished(&self, ok: bool, words: &str) {
        self.state.lock_recover().finished = Some(Instant::now());
        self.conversation.finished(ok, words);
    }

    fn over(&self, ended: &Ended) {
        self.state.lock_recover().over = true;
        self.conversation.over(ended);
    }
}

struct Running {
    run: CodingRun,
    state: Arc<Mutex<State>>,
}

/// The coding agents a host runs now.
pub struct CodingAgents {
    machines: Arc<dyn Machines>,
    harnesses: Harnesses,
    permits: Arc<dyn Permits>,
    idle: Duration,
    running: Mutex<HashMap<(String, String), Running>>,
}

impl CodingAgents {
    #[must_use]
    pub fn new(
        machines: Arc<dyn Machines>,
        harnesses: Harnesses,
        permits: Arc<dyn Permits>,
    ) -> Self {
        Self {
            machines,
            harnesses,
            permits,
            idle: IDLE,
            running: Mutex::new(HashMap::new()),
        }
    }

    /// These, put to rest `idle` after they finished what they were asked.
    #[must_use]
    pub fn resting_after(mut self, idle: Duration) -> Self {
        self.idle = idle;
        self
    }

    /// Give `agent` `words`: its job, a note of whoever started it, or what
    /// its person wrote to it. A run of it that takes messages gets them as
    /// its next one; otherwise its harness is started, taking its
    /// conversation up where it stood, with `serves` behind its endpoints
    /// and what it says written to `conversation`. A start is let by
    /// `permit`: its person's yes to this one, or their lasting yes where
    /// one stands. Its host hands it words one turn at a time, so no two
    /// starts of one agent meet. Answers whether its harness was started
    /// for them.
    ///
    /// # Errors
    ///
    /// It does not run and no yes covers starting it; or the harness is none
    /// this host starts, or its run could not be started. A run that started
    /// and then failed says so in its conversation instead.
    pub fn tell(
        &self,
        agent: &CodingAgent,
        words: &str,
        serves: Serves,
        conversation: Arc<dyn Conversation>,
        permit: Permit,
    ) -> Result<bool, NotTold> {
        let key = (agent.workspace.clone(), agent.id.clone());
        let taken = {
            let mut running = self.running.lock_recover();
            let taken = running.get(&key).is_some_and(|held| {
                let mut state = held.state.lock_recover();
                let taken = !state.over && !state.resting && held.run.message(words);
                if taken {
                    state.finished = None;
                }
                drop(state);
                taken
            });
            if !taken {
                // What is left of an older run ends with its handle.
                running.remove(&key);
            }
            taken
        };
        if taken {
            return Ok(false);
        }
        if permit == Permit::Lasting && !self.permits.stands(agent) {
            return Err(NotTold::NeedsYes);
        }
        let harness = self.harnesses.named(&agent.harness).ok_or_else(|| {
            NotTold::Failed(format!(
                "{:?} is no coding agent this host starts: it starts {}",
                agent.harness,
                self.harnesses.ids().join(", ")
            ))
        })?;
        let program = harness.program(&Start {
            session: agent.session.clone(),
            instruction: agent.instruction.clone(),
        });
        // A working copy is entered by the run itself, from the workspace's
        // files; without one the run starts in the folder it was given.
        let (argv, directory) = match &agent.copy {
            Some(copy) => (working_copy::within(copy, &program.argv), PathBuf::new()),
            None => (program.argv, PathBuf::from(&agent.repository)),
        };
        let machine = self
            .machines
            .serve(
                &agent.workspace,
                ServeRun {
                    argv,
                    env: program.env,
                    directory,
                    endpoints: program.endpoints,
                    stop_grace: STOP_GRACE,
                    agent_program: wire::AGENT_PROGRAM.to_owned(),
                },
            )
            .map_err(NotTold::Failed)?;
        let state = Arc::new(Mutex::new(State::default()));
        let watched = Arc::new(Watched {
            conversation,
            state: Arc::clone(&state),
        });
        let run = CodingRun::follow(machine, harness, serves, watched).map_err(|error| {
            NotTold::Failed(format!(
                "the coding agent's run could not be followed: {error}"
            ))
        })?;
        // A run that ended before it took its words says why in its
        // conversation, where its person reads it.
        run.message(words);
        self.running
            .lock_recover()
            .insert(key, Running { run, state });
        Ok(true)
    }

    /// Ask `agent` of `workspace` to end: `false` when no run of it is held.
    pub fn stop(&self, workspace: &str, agent: &str) -> bool {
        let running = self.running.lock_recover();
        let asked = running
            .get(&(workspace.to_owned(), agent.to_owned()))
            .map(|held| held.run.stop())
            .is_some();
        drop(running);
        asked
    }

    /// Whether a run of `agent` of `workspace` is held and not over.
    #[must_use]
    pub fn runs(&self, workspace: &str, agent: &str) -> bool {
        self.running
            .lock_recover()
            .get(&(workspace.to_owned(), agent.to_owned()))
            .is_some_and(|held| !held.state.lock_recover().over)
    }

    /// How many runs are held and not over.
    #[must_use]
    pub fn count(&self) -> usize {
        self.running
            .lock_recover()
            .values()
            .filter(|held| !held.state.lock_recover().over)
            .count()
    }

    /// What the model calls of `agent`'s process cost so far; none when no
    /// process of it is held.
    #[must_use]
    pub fn spent_micros(&self, workspace: &str, agent: &str) -> Option<u64> {
        self.running
            .lock_recover()
            .get(&(workspace.to_owned(), agent.to_owned()))
            .map(|held| held.run.spent_micros())
    }

    /// The harness `id` names here, as its own id and its name; none when
    /// no such harness is started here.
    #[must_use]
    pub fn harness(&self, id: &str) -> Option<(&'static str, &'static str)> {
        self.harnesses
            .named(id)
            .map(|harness| (harness.id(), harness.name()))
    }

    /// Put to rest every run that finished what it was asked long enough
    /// ago, and let go of the ones that are over. Its host calls this each
    /// time it looks at its workspaces.
    pub fn rest(&self) {
        let mut running = self.running.lock_recover();
        running.retain(|_, held| {
            let mut state = held.state.lock_recover();
            if state.over {
                return false;
            }
            let idle = state
                .finished
                .is_some_and(|finished| finished.elapsed() >= self.idle);
            if idle && !state.resting {
                state.resting = held.run.close();
            }
            drop(state);
            true
        });
    }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
