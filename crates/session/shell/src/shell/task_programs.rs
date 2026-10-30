//! The Tasks agent's tilolang programs, run by the session's door
//! (`docs/designs/app-language/agents.md`, "How a program runs").
//!
//! An agent's task writes programs; the door runs them. An MCP client reaches the door's `tilo`
//! tool on its connection; the Tasks agent, which works its issues through the broker, reaches
//! the same door here, as a host step under [`wire::PROVIDER`]. Each task has one session of the
//! door: its own tilolang host (the task its author), the `let`s and references its programs
//! made, and the steps `undo` takes back. What a program reads and changes is done as the agent,
//! for the person it acts for, attributed to the task's run, and decided by the workspace's
//! policy like every other call of the agent's.
//!
//! A task's session lives while its run does: a later run of the same issue starts another.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tilo_action_broker::services::{HostStepOutcome, HostStepRequest, HostStepServices};
use tilo_actions::remote::task_programs as wire;
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionArgs, ActionParam, ParamType};
use tilo_lock::MutexRecover as _;

use crate::shell::remote_actions::BrokerActionHost;

/// One task's session of the door.
type Door = tilo_runtime::mcp::McpServer<BrokerActionHost>;

/// A task, by its room, its agent and its issue.
type TaskKey = (String, String, String);

/// Each task's session of the door, with the run it is of.
type Sessions = Mutex<HashMap<TaskKey, (String, Arc<Door>)>>;

/// Every task's session, with the run it belongs to.
fn sessions() -> &'static Sessions {
    static SESSIONS: OnceLock<Sessions> = OnceLock::new();
    SESSIONS.get_or_init(Mutex::default)
}

/// Where a task's tilolang host is started: the session's launcher, as for the agent door.
struct TaskHosts;

impl tilo_runtime::mcp::TaskHosts for TaskHosts {
    fn start(&self, room: &str, task: &str) -> std::io::Result<std::os::fd::OwnedFd> {
        crate::shell::launcher::start_task_host(room, task)
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

/// The session's door for the Tasks agent's programs, for the broker.
pub(crate) fn broker_door() -> Arc<dyn HostStepServices> {
    Arc::new(BrokerDoor)
}

struct BrokerDoor;

fn descriptor() -> tilo_actions::ActionDescriptor {
    use crate::shell::i18n::t;
    tilo_actions::ActionDescriptor {
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(wire::RESOURCE, wire::RUN),
        resource_kind: wire::RESOURCE.into(),
        resource_label: t("host-task-programs-resource"),
        action_label: t("host-task-programs-run"),
        keywords: Vec::new(),
        icon: None,
        description: Some(t("host-task-programs-run-detail")),
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

/// The task's session of the door, started for its run `group` when it has none or its last
/// run's.
fn session_of(
    room: &str,
    agent: &str,
    person: Option<&str>,
    issue: &str,
    group: tilo_actions::WorkGroup,
) -> Result<Arc<Door>, String> {
    let key = (room.to_owned(), agent.to_owned(), issue.to_owned());
    let mut sessions = sessions().lock_recover();
    if let Some((run, door)) = sessions.get(&key)
        && *run == group.id
    {
        return Ok(Arc::clone(door));
    }
    let broker = crate::shell::link_open::session_broker()?;
    let run = group.id.clone();
    let door = Arc::new(
        Door::for_task(
            BrokerActionHost::new(broker),
            room.to_owned(),
            agent,
            person,
        )
        .with_work_group(group)
        .with_task_hosts(Some(Arc::new(TaskHosts))),
    );
    // A later run of the issue ends the earlier one's session, its host with it.
    sessions.insert(key, (run, Arc::clone(&door)));
    Ok(door)
}

impl HostStepServices for BrokerDoor {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }

    fn app_id(&self) -> &str {
        crate::shell::launcher::WORKSPACE_AGENT_PACKAGE_ID
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![descriptor()]
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
        let door = session_of(
            request.room,
            agent,
            actor.acting_for(),
            issue,
            tilo_actions::WorkGroup {
                id: run,
                label,
                kind: "task".into(),
            },
        )?;
        let (complete, answered) = pending();
        // The program waits on the task's host and on every call it makes: never on the
        // broker's thread.
        std::thread::Builder::new()
            .name("task-program".into())
            .spawn(move || {
                let ran = tilo_rt::try_app()
                    .map_err(|error| error.to_string())
                    .and_then(|runtime| {
                        runtime
                            .block_on(door.task_program(&program, confirm, this.as_deref()))
                            .map_err(|error| error.to_string())
                    });
                complete.complete(outcome(ran));
            })
            .map_err(|error| error.to_string())?;
        Ok(answered)
    }
}

fn outcome(ran: Result<tilo_runtime::mcp::TaskProgram, String>) -> HostStepOutcome {
    let ran = ran.map_err(|message| {
        Box::new(tilo_actions::ActionFailure {
            code: "task_program_unavailable".into(),
            message,
            details: None,
            retryable: true,
        })
    })?;
    let answer = wire::ProgramAnswer {
        text: ran.text,
        failed: ran.failed,
        answer: ran.answer,
        refs: ran.refs,
    };
    Ok(tilo_actions::ActionOutput {
        value: Some(serde_json::to_value(answer).unwrap_or_default()),
        resources: Vec::new(),
    })
}
