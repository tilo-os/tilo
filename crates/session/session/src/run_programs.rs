//! The programs a run's conversation runs (shell.md §8.1, "One input, two ways in"): the lines
//! a person types in a run's conversation, each run as the person in the run's own program host,
//! so it takes the `let`s the agent's programs bound and shows as a step of theirs.
//!
//! One host per run, entered by two connections: the door's own, which the agent's `tilo` calls
//! write into, and the person's, which this keeps and which the door's `McpServer` serves as a
//! REPL's, so every read and change a line makes passes the door's gate as the person. Both
//! reach the one session the host holds, so the `let`s are one scope. A line runs on a thread of
//! its own, so the session's pass never waits on a program; what it made is recorded on the run
//! when it ends ([`Programs::take_finished`]).

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use tilo_lock::MutexRecover as _;
use tilo_runtime::mcp::PersonStep;

/// The runs' program hosts, by run id: both ends kept, so the connections stay open while no
/// caller serves one.
pub struct Programs {
    hosts: std::sync::Mutex<HashMap<String, Ends>>,
    /// What the lines that ended made, for the session to record on its run.
    finished: std::sync::Mutex<Vec<Finished>>,
    /// The workspace the runs' programs act in, and the door's host a person's line reaches it
    /// through: lent once the session mounts its door.
    door: std::sync::Mutex<Option<Door>>,
    /// Where a run's host is started, when a person's line is the run's first program: the
    /// same starter the agent's `tilo` call uses.
    starter: std::sync::Mutex<Option<Arc<dyn tilo_runtime::mcp::TaskHosts>>>,
}

/// The door a person's line acts through.
#[derive(Clone)]
struct Door {
    workspace: String,
    host: Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>,
}

/// One run's host, by its two connections into it.
struct Ends {
    /// The door's own: a copy kept so the connection never closes while no one serves it, and
    /// what the agent's `tilo` calls write into.
    door: UnixStream,
    /// The person's: what a line in the run's conversation is sent on.
    person: UnixStream,
}

/// What a person's line made, for the session to record on its run.
pub struct Finished {
    pub run: String,
    /// Who typed it: their step is theirs.
    pub person: String,
    /// The line as they typed it.
    pub line: String,
    /// What it made, or why it could not run at all.
    pub step: Result<PersonStep, String>,
}

impl Default for Programs {
    fn default() -> Self {
        Self {
            hosts: std::sync::Mutex::new(HashMap::new()),
            finished: std::sync::Mutex::new(Vec::new()),
            door: std::sync::Mutex::new(None),
            starter: std::sync::Mutex::new(None),
        }
    }
}

impl Programs {
    /// The runs' programs, with no host started.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The door a person's line acts through: the workspace it acts in, and the host that
    /// decides what it reads and changes.
    pub fn lend(
        &self,
        workspace: &str,
        host: Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>,
    ) {
        *self.door.lock_recover() = Some(Door {
            workspace: workspace.to_owned(),
            host,
        });
    }

    /// Where a run's host is started (the agent door's `TaskHosts`), so a person's line can
    /// start the host itself when it is the run's first program.
    pub fn lend_starter(&self, hosts: Arc<dyn tilo_runtime::mcp::TaskHosts>) {
        *self.starter.lock_recover() = Some(hosts);
    }

    /// The run `run`'s door connection, when its host runs: a copy the caller serves, as the
    /// agent's `tilo` calls do. `None` before the host is started.
    ///
    /// # Errors
    /// The connection could not be copied for the caller.
    pub fn door(&self, run: &str) -> Option<std::io::Result<std::os::fd::OwnedFd>> {
        let hosts = self.hosts.lock_recover();
        hosts
            .get(run)
            .map(|ends| Ok(std::os::fd::OwnedFd::from(ends.door.try_clone()?)))
    }

    /// Keep the run `run`'s host's person and door connections, once it is started.
    pub fn keep(&self, run: &str, door: UnixStream, person: UnixStream) {
        self.hosts
            .lock_recover()
            .insert(run.to_owned(), Ends { door, person });
    }

    /// Whether run `run`'s host runs.
    #[must_use]
    pub fn running(&self, run: &str) -> bool {
        self.hosts.lock_recover().contains_key(run)
    }

    /// Run `line`, typed by `person`, in run `run`'s host, on a thread of its own. A line that
    /// is the run's first program starts the host here, with the agent's own starter. What it
    /// made is read back by [`Self::take_finished`]. `false` when no door is lent, or the host
    /// could not start, so the line cannot be decided.
    pub fn start(self: &Arc<Self>, run: &str, person: &str, line: &str) -> bool {
        let Some(door) = self.door.lock_recover().clone() else {
            return false;
        };
        if !self.running(run) {
            let starter = self.starter.lock_recover().clone();
            let Some(Ok(host)) = starter.map(|hosts| hosts.start(&door.workspace, run)) else {
                return false;
            };
            // The starter kept the host's own ends (`TaskHosts::start` copies them); this one is
            // served by the agent's first call, so it goes.
            drop(host);
        }
        let hosts = self.hosts.lock_recover();
        let fd = {
            let Some(ends) = hosts.get(run) else {
                return false;
            };
            let Ok(copy) = ends.person.try_clone() else {
                self.finished.lock_recover().push(Finished {
                    run: run.to_owned(),
                    person: person.to_owned(),
                    line: line.to_owned(),
                    step: Err("the run's program connection could not be reached".into()),
                });
                return true;
            };
            std::os::fd::OwnedFd::from(copy)
        };
        drop(hosts);
        let run = run.to_owned();
        let line = line.to_owned();
        let person = person.to_owned();
        let (thread_run, thread_line, thread_person) = (run.clone(), line.clone(), person.clone());
        let shared = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("tilo-conversation".into())
            .spawn(move || {
                let server = tilo_runtime::mcp::McpServer::for_person(
                    door.host,
                    door.workspace,
                    &thread_person,
                )
                .with_work_group(tilo_actions::WorkGroup {
                    id: thread_run.clone(),
                    label: "Conversation".into(),
                    kind: "agent-run".into(),
                });
                let step = match tilo_rt::try_app() {
                    Ok(runtime) => runtime
                        .block_on(server.program_on(fd, &thread_line))
                        .map_err(|error| error.to_string())
                        .and_then(std::convert::identity),
                    Err(error) => Err(error.to_string()),
                };
                shared.finished.lock_recover().push(Finished {
                    run: thread_run,
                    person: thread_person,
                    line: thread_line,
                    step,
                });
            });
        if let Err(error) = spawned {
            self.finished.lock_recover().push(Finished {
                run,
                person,
                line,
                step: Err(format!("the line could not start: {error}")),
            });
        }
        true
    }

    /// What the lines that ended since the last pass made, in the order they ended.
    pub fn take_finished(&self) -> Vec<Finished> {
        std::mem::take(&mut *self.finished.lock_recover())
    }

    /// The run ended, or its host went away: nothing more reaches it.
    pub fn forget(&self, run: &str) {
        self.hosts.lock_recover().remove(run);
    }
}

/// The runs' programs, shared by the door that starts a host and the conversation that types.
pub type Shared = Arc<Programs>;
