//! An agent's task programs: the tilolang an agent writes as one step of its task, run here.
//! An agent working an issue writes programs; this session runs them, in the same door an MCP
//! client reaches for its `tilo` tool (`docs/designs/app-language/agents.md`). The Tasks agent
//! reaches it through the broker as a host step under
//! [`task_programs::PROVIDER`](tilo_actions::remote::task_programs::PROVIDER), admitted by the run
//! the step is a step of, and gets back what the program answered.
//! One task has one session of the door: its own tilolang host, the `let` s and references its
//! programs made, and the steps `undo` takes back. Every call the program makes is done as the
//! agent working the task, for the person who started it, attributed to the run, and decided by the
//! workspace's policy like the agent's own calls. A later run of the issue starts another.

use std::collections::HashMap;
use std::sync::Arc;

use tilo_action_broker::collab_app_broker::TASK_AGENT_APP;
use tilo_actions::remote::task_programs as wire;
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionArgs, ActionParam, ParamType};
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices, TaskRun};
use tilo_lock::MutexRecover as _;

/// One task's session of the door, over the run's own host.
type Door = tilo_runtime::mcp::McpServer<Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>>;

/// A task, by the workspace it runs in, its agent and its issue.
type TaskKey = (String, String, String);

/// The tasks' sessions of the door, with the run each is of.
pub(crate) struct TaskPrograms {
    /// What a task's host asks the session's own doors before it starts: the records the packages
    /// the session runs itself keep.
    doors: Arc<crate::agent_door::SessionDoors>,
    sessions: std::sync::Mutex<HashMap<TaskKey, (String, Arc<Door>)>>,
}

impl TaskPrograms {
    /// The tasks' programs, whose hosts read the session's own records through `doors` .
    pub(crate) fn new(doors: Arc<crate::agent_door::SessionDoors>) -> Self {
        Self {
            doors,
            sessions: std::sync::Mutex::default(),
        }
    }

    /// The task's session of the door, started for its run `group` when it has none or its last
    /// run's: over `host` , the run's own host, so everything its programs do is a step the run
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

const PROGRAM: ActionParam = ActionParam::required(
    "program",
    ParamType::Text,
    "The tilolang program, or `undo`, `undo task` or `history task`.",
);
const CONFIRM: ActionParam = ActionParam::optional(
    "confirm",
    ParamType::Boolean,
    "Send again a program whose answer was a preview.",
);
const THIS: ActionParam = ActionParam::optional(
    "this",
    ParamType::Text,
    "What the person pointed at, as a resource literal, which `this` names.",
);
const RUN: ActionParam = ActionParam::required(
    "run",
    ParamType::Text,
    "The run of the task the program is a step of: its group.",
);
const RUN_LABEL: ActionParam = ActionParam::optional(
    "label",
    ParamType::Text,
    "The run's title, as history names it.",
);

const PARAMS: &[ActionParam] = &[PROGRAM, CONFIRM, THIS, RUN, RUN_LABEL];

/// The task's program, described for the broker's catalogue.
fn descriptor() -> tilo_actions::ActionDescriptor {
    use crate::i18n::t;
    tilo_actions::ActionDescriptor {
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(wire::RESOURCE, wire::RUN),
        resource_kind: wire::RESOURCE.into(),
        resource_label: t("task-programs-resource"),
        action_label: t("task-programs-run"),
        keywords: Vec::new(),
        icon: None,
        description: Some(t("task-programs-run-detail")),
        locus: tilo_actions::ExecutionLocus::default(),
        // The step itself only reads: every change a program makes is its own invocation,
        // decided where it lands.
        kind: tilo_actions::OperationKind::View,
        contract: tilo_actions::ActionContract {
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({ "type": "object", "required": ["text", "failed"] }),
            },
            ..tilo_actions::ActionContract::view()
        },
        input: tilo_actions::InputSpec::None,
        params: PARAMS.to_vec(),
        view: None,
    }
}

/// `argument` , in its slot form, bound to `PARAMS` .
fn bind(argument: Option<&str>) -> Result<ActionArgs, String> {
    let supplied = match argument {
        None | Some("") => serde_json::Map::new(),
        Some(slot) => {
            let json = slot
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .ok_or("a task's program takes JSON arguments")?;
            serde_json::from_str(json).map_err(|error| error.to_string())?
        }
    };
    ActionArgs::bind_json(PARAMS, &supplied).map_err(|error| error.to_string())
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

impl HostStepServices for TaskPrograms {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }

    fn app_id(&self) -> &str {
        TASK_AGENT_APP
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![descriptor()]
    }

    fn for_tasks(&self) -> bool {
        true
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        if request.address.resource != wire::RESOURCE || request.address.action != wire::RUN {
            return Err("unknown step of a task's programs".into());
        }
        let actor = request.actor;
        if !actor.is_agent() {
            return Err("a task's programs are its agent's".into());
        }
        let issue = actor
            .issue()
            .ok_or("an agent runs programs as a step of a task it works on")?;
        let args = bind(request.argument)?;
        let run = args.text("run").unwrap_or_default().trim().to_owned();
        // The run is one of the issue's: its group names the issue it is a run of.
        if !run.starts_with(&format!("{issue}@")) {
            return Err("the run is not one of the task's".into());
        }
        let program = args.text("program").unwrap_or_default().to_owned();
        let confirm = args.boolean("confirm").unwrap_or(false);
        let this = args
            .text("this")
            .filter(|this| !this.trim().is_empty())
            .map(str::to_owned);
        let label = args.text("label").unwrap_or(issue).to_owned();
        let agent = actor
            .id()
            .strip_prefix("agent:")
            .unwrap_or_else(|| actor.id());
        let door = self.session_of(
            request
                .task_run
                .ok_or("a task's programs run only as steps of its run")?,
            request.workspace,
            agent,
            actor.acting_for(),
            issue,
            tilo_actions::WorkGroup {
                id: run,
                label,
                kind: "task".into(),
            },
        );
        let (complete, answered) = pending();
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_broker::collab_app_broker::BrokerActor;
    use tilo_actions::ActionAddress;

    /// The step as the session serves it, on a door nothing asks beyond.
    fn programs() -> TaskPrograms {
        let wake = tilo_service::LoopWake::default();
        let doors = Arc::new(crate::agent_door::SessionDoors::new(wake.handle(), "Pat"));
        TaskPrograms::new(doors)
    }

    /// The issue an agent's run is of.
    const ISSUE: &str = "tasks.tracker/issues/tidy-the-inbox";

    fn request<'a>(
        actor: &'a BrokerActor,
        address: &'a ActionAddress,
        argument: &'a str,
        task_run: Option<&'a dyn TaskRun>,
    ) -> HostStepRequest<'a> {
        HostStepRequest {
            workspace: "the-workspace",
            actor,
            address,
            argument: Some(argument),
            task_run,
        }
    }

    /// The step is the agent's, offered to a task agent's catalogue and nowhere else: a program is
    /// a step of a run, and only a task agent has one.
    #[test]
    fn a_task_step_is_the_agents_and_is_offered_for_tasks() {
        let programs = programs();
        assert_eq!(programs.provider(), wire::PROVIDER);
        assert_eq!(programs.app_id(), TASK_AGENT_APP);
        assert!(programs.for_tasks(), "a task agent's catalogue offers it");
        let described = programs.descriptors();
        assert_eq!(described.len(), 1);
        let step = &described[0];
        assert_eq!(step.address, ActionAddress::new(wire::RESOURCE, wire::RUN));
        assert_eq!(step.kind, tilo_actions::OperationKind::View);
        assert!(
            matches!(
                &step.contract.result,
                tilo_actions::ResultSpec::Json { schema }
                    if schema["required"] == serde_json::json!(["text", "failed"])
            ),
            "the step answers what a program made: {:?}",
            step.contract.result
        );
    }

    /// A program is a step of a task its agent works on: anyone else, and a program outside a run
    /// of the issue, is refused before anything starts.
    #[test]
    fn only_an_agents_program_of_its_own_run_is_taken() {
        let programs = programs();
        let address = ActionAddress::new(wire::RESOURCE, wire::RUN);
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({ "program": "ls", "run": format!("{ISSUE}@0") })
        );
        let took = |actor: &BrokerActor| {
            programs
                .start(&request(actor, &address, &argument, None))
                .err()
        };
        assert_eq!(
            took(&BrokerActor::person("Pat")),
            Some("a task's programs are its agent's".to_owned())
        );
        assert_eq!(
            took(&BrokerActor::agent("claude")),
            Some("an agent runs programs as a step of a task it works on".to_owned())
        );
        // An agent working the issue, but outside a run: the step names the run it is of.
        assert_eq!(
            took(&BrokerActor::agent("claude").working_on(ISSUE)),
            Some("a task's programs run only as steps of its run".to_owned())
        );
        // A run of another issue is not the task's.
        let other = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({ "program": "ls", "run": "another-issue@0" })
        );
        assert_eq!(
            programs
                .start(&request(
                    &BrokerActor::agent("claude").working_on(ISSUE),
                    &address,
                    &other,
                    None
                ))
                .err(),
            Some("the run is not one of the task's".to_owned())
        );
    }
}
