//! The task programs a Linux session runs: each task's door over its own tilolang host process.
//!
//! One task has one session of the door: its own host, the `let`s and references its programs
//! made, and the steps `undo` takes back. Every call the program makes is done as the agent
//! working the task, for the person who started it, attributed to the run, and decided by the
//! workspace's policy like the agent's own calls. A later run of the issue starts another.

use std::collections::HashMap;
use std::sync::Arc;

use tilo_actions::remote::Pending;
use tilo_actions::remote::task_programs as wire;
use tilo_broker_core::steps::{HostStepOutcome, TaskRun};
use tilo_lock::MutexRecover as _;

use crate::platform::programs::{ProgramRequest, Programs};

/// One task's session of the door, over the run's own host.
type Door = tilo_runtime::mcp::McpServer<Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>>;

/// A task, by the workspace it runs in, its agent and its issue.
type TaskKey = (String, String, String);

/// The tasks' sessions of the door, with the run each is of.
pub(crate) struct TaskProgramHost {
    /// What a task's host asks the session's own doors before it starts: the records the packages
    /// the session runs itself keep.
    doors: Arc<crate::agent_door::SessionDoors>,
    sessions: std::sync::Mutex<HashMap<TaskKey, (String, Arc<Door>)>>,
}

impl TaskProgramHost {
    /// The tasks' programs, whose hosts read the session's own records through `doors`.
    pub(crate) fn new(doors: Arc<crate::agent_door::SessionDoors>) -> Self {
        Self {
            doors,
            sessions: std::sync::Mutex::default(),
        }
    }

    /// The task's session of the door, started for its run `group` when it has none or its last
    /// run's: over `host`, the run's own host, so everything its programs do is a step the run
    /// admits.
    fn session_of(
        &self,
        host: &dyn TaskRun,
        workspace: &str,
        agent: &str,
        person: Option<&str>,
        issue: &str,
        group: tilo_actions::WorkGroup,
    ) -> Arc<Door> {
        let key = (workspace.to_owned(), agent.to_owned(), issue.to_owned());
        let mut sessions = self.sessions.lock_recover();
        if let Some((run, door)) = sessions.get(&key)
            && *run == group.id
        {
            return Arc::clone(door);
        }
        let run = group.id.clone();
        let door = Arc::new(
            Door::for_task(host.door(), workspace.to_owned(), agent, person)
                .with_work_group(group)
                .with_task_hosts(Some(Arc::new(crate::agent_door::TaskHosts::new(
                    person.unwrap_or_else(|| self.doors.person()),
                    Arc::clone(&self.doors),
                )))),
        );
        // A later run of the issue ends the earlier one's session, its host with it.
        sessions.insert(key, (run, Arc::clone(&door)));
        door
    }
}

/// The run's program could not start or run at all.
fn unavailable(message: String) -> HostStepOutcome {
    Err(Box::new(tilo_actions::ActionFailure {
        code: "task_program_unavailable".into(),
        message,
        details: None,
        retryable: true,
    }))
}

fn outcome(ran: tilo_runtime::mcp::TaskProgram) -> tilo_actions::ActionOutput {
    let answer = wire::ProgramAnswer {
        text: ran.text,
        failed: ran.failed,
        answer: ran.answer,
        refs: ran.refs,
    };
    tilo_actions::ActionOutput {
        value: Some(serde_json::to_value(answer).unwrap_or_default()),
        resources: Vec::new(),
    }
}

impl Programs for TaskProgramHost {
    fn run(&self, request: ProgramRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        let door = self.session_of(
            request.run,
            request.workspace,
            request.agent,
            request.person,
            request.issue,
            request.group,
        );
        let (program, confirm, this) = (request.program, request.confirm, request.this);
        let (complete, answered) = tilo_actions::remote::pending();
        // The program waits on the task's host and on every call it makes: never on the broker's
        // thread.
        std::thread::Builder::new()
            .name("task-program".into())
            .spawn(move || {
                let answered = match tilo_rt::try_app() {
                    Ok(runtime) => match runtime.block_on(door.task_program(
                        &program,
                        confirm,
                        this.as_deref(),
                    )) {
                        Ok(ran) => Ok(outcome(ran)),
                        Err(error) => unavailable(error.to_string()),
                    },
                    Err(error) => unavailable(error.to_string()),
                };
                complete.complete(answered);
            })
            .map_err(|error| error.to_string())?;
        Ok(answered)
    }
}
