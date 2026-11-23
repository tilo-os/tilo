//! A session's programs: the tilolang an agent writes as one step of its session, run here, in
//! the same API an MCP client reaches for its `tilo` tool (`docs/designs/app-language/agents.md`).
//! An agent reaches it through the broker as a host step under
//! [`task_programs::PROVIDER`](tilo_actions::remote::task_programs::PROVIDER), admitted by the
//! session the step is a step of, and gets back what the program answered.
//! One agent session has one session of the host: its own tilolang host, the `let`s and references
//! its programs made, and the steps `undo` takes back. Every call the program makes is done as the
//! agent, for the person it acts for, attributed to the session, and decided by the workspace's
//! policy like the agent's own calls.
//!
//! The person's own line runs through the same [`Programs`] host, from the session's own actions
//! ([`crate::session_actions`]): theirs, through the session's `for_person` api.
//!
//! Where a program runs is the host's: [`Programs`], a process of its own on a Linux desktop and
//! the page's in-process host on the web.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page starts the task programs its own session runs next"
    )
)]

use std::sync::Arc;

use tilo_actions::remote::Pending;
use tilo_actions::remote::task_programs as task_wire;
use tilo_actions::{ActionArgs, ActionParam, ParamType};
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices};

use crate::platform::programs::{ProgramCaller, ProgramRequest, Programs};

/// An agent session's programs, as this host runs them.
pub(crate) struct TaskPrograms {
    /// Where a program runs: the session's host.
    programs: Arc<dyn Programs>,
}

impl TaskPrograms {
    pub(crate) fn new(programs: Arc<dyn Programs>) -> Self {
        Self { programs }
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
const PARAMS: &[ActionParam] = &[PROGRAM, CONFIRM, THIS];

/// The session's program, described for the broker's catalogue.
fn descriptor() -> tilo_actions::ActionDescriptor {
    use tilo_session_core::desktop::i18n::t;
    tilo_actions::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(task_wire::RESOURCE, task_wire::RUN),
        resource_kind: task_wire::RESOURCE.into(),
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

/// `argument` , in its slot form, bound to `params` .
fn bind(params: &'static [ActionParam], argument: Option<&str>) -> Result<ActionArgs, String> {
    let supplied = match argument {
        None | Some("") => serde_json::Map::new(),
        Some(slot) => {
            let json = slot
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .ok_or("a program takes JSON arguments")?;
            serde_json::from_str(json).map_err(|error| error.to_string())?
        }
    };
    ActionArgs::bind_json(params, &supplied).map_err(|error| error.to_string())
}

impl HostStepServices for TaskPrograms {
    fn provider(&self) -> &'static str {
        task_wire::PROVIDER
    }

    fn app_id(&self) -> &str {
        task_wire::APP
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![descriptor()]
    }

    fn for_agent_sessions(&self) -> bool {
        true
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        if request.address.resource != task_wire::RESOURCE
            || request.address.action != task_wire::RUN
        {
            return Err("unknown step of a session's programs".into());
        }
        let actor = request.actor;
        if !actor.is_agent() {
            return Err("a session's programs are its agent's".into());
        }
        let session = actor
            .session()
            .ok_or("an agent runs programs as a step of its session")?;
        let provider = request
            .provider
            .clone()
            .ok_or("an agent runs programs only through its session's API")?;
        let args = bind(PARAMS, request.argument)?;
        let program = args.text("program").unwrap_or_default().to_owned();
        let confirm = args.boolean("confirm").unwrap_or(false);
        let this = args
            .text("this")
            .filter(|this| !this.trim().is_empty())
            .map(str::to_owned);
        let agent = actor.member().id();
        let group = request
            .group
            .cloned()
            .unwrap_or_else(|| tilo_actions::WorkGroup {
                id: session.to_owned(),
                label: None,
                kind: tilo_actions::context::AGENT_SESSION_GROUP.to_owned(),
                declared_id: None,
                session: None,
            });
        self.programs.run(ProgramRequest {
            workspace: request.workspace,
            provider: Some(provider),
            caller: ProgramCaller::Agent {
                agent,
                person: actor.acting_for(),
                session,
            },
            group,
            program,
            confirm,
            this,
            stop: Some(Arc::clone(&request.stop)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionAddress;
    use tilo_broker_core::BrokerActor;

    /// A host no step reaches: every case below is refused before a program runs.
    struct NoPrograms;

    impl Programs for NoPrograms {
        fn run(&self, _: ProgramRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
            Err("no program runs in these cases".into())
        }
    }

    /// The step as the session serves it.
    fn programs() -> TaskPrograms {
        TaskPrograms::new(Arc::new(NoPrograms))
    }

    fn request<'a>(
        actor: &'a BrokerActor,
        address: &'a ActionAddress,
        argument: &'a str,
    ) -> HostStepRequest<'a> {
        HostStepRequest {
            workspace: "the-workspace",
            actor,
            address,
            argument: Some(argument),
            provider: None,
            group: None,
            stop: std::sync::Arc::default(),
        }
    }

    tilo_test_env::scenario! {
        fn only_an_agents_own_session_step_is_offered_and_taken;

        phase a_program_step_is_the_agents_and_is_offered_in_its_sessions;
        // The step is the agent's, offered to an agent in a session and nowhere else.
        {
            let programs = programs();
            assert_eq!(programs.provider(), task_wire::PROVIDER);
            assert_eq!(programs.app_id(), task_wire::APP);
            assert!(
                programs.for_agent_sessions(),
                "an agent session's catalogue offers it"
            );
            let described = programs.descriptors();
            assert_eq!(described.len(), 1);
            let step = &described[0];
            assert_eq!(step.address, ActionAddress::new(task_wire::RESOURCE, task_wire::RUN));
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

        phase only_an_agents_program_in_its_session_is_taken;
        // A program is a step of an agent's session, through the session's API: anyone else, and
        // an agent outside a session, is refused before anything starts.
        {
            let programs = programs();
            let address = ActionAddress::new(task_wire::RESOURCE, task_wire::RUN);
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "program": "ls" })
            );
            let took = |actor: &BrokerActor| programs.start(&request(actor, &address, &argument)).err();
            assert_eq!(
                took(&BrokerActor::person("Pat")),
                Some("a session's programs are its agent's".to_owned())
            );
            assert_eq!(
                took(&BrokerActor::agent("claude")),
                Some("an agent runs programs as a step of its session".to_owned())
            );
            assert_eq!(
                took(&BrokerActor::agent("claude").in_session("agent-session:1")),
                Some("an agent runs programs only through its session's API".to_owned())
            );
        }
    }
}
