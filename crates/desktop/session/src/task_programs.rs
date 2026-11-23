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
use tilo_actions::remote::admitted::{offered, settled, top_level};
use tilo_actions::remote::task_programs as task_wire;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, InvocationReceipt, InvokeRequest, Outcome,
};
use tilo_actions::{ActionArgs, ActionParam, ParamType};
use tilo_broker_core::BrokerActor;
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

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

    /// Run the programs of the agent sessions this device admits from now on, their own actions
    /// listed to the agent that works in one.
    pub(crate) fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(
            task_wire::PROVIDER,
            task_wire::APP,
            Reach::AgentSessions { listed: true },
            self,
        );
    }

    /// Carry out an invocation the device's dispatch admitted for `caller`.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let address = &request.target.address;
        if address.resource != task_wire::RESOURCE || address.action != task_wire::RUN {
            return Err("unknown step of a session's programs".into());
        }
        let actor = BrokerActor::for_caller(caller);
        if !actor.is_agent() {
            return Err("a session's programs are its agent's".into());
        }
        let session = actor
            .session()
            .ok_or("an agent runs programs as a step of its session")?;
        let provider = admitted
            .through
            .clone()
            .ok_or("an agent runs programs only through its session's API")?;
        let workspace = caller
            .workspace()
            .ok_or("a session's programs run in its workspace")?;
        let argument = request.argument.to_slot();
        let args = bind(PARAMS, argument.as_deref())?;
        let program = args.text("program").unwrap_or_default().to_owned();
        let confirm = args.boolean("confirm").unwrap_or(false);
        let this = args
            .text("this")
            .filter(|this| !this.trim().is_empty())
            .map(str::to_owned);
        let agent = actor.member().id();
        let group = request
            .context
            .as_ref()
            .and_then(|context| context.group.clone())
            .unwrap_or_else(|| tilo_actions::WorkGroup {
                id: session.to_owned(),
                label: None,
                kind: tilo_actions::context::AGENT_SESSION_GROUP.to_owned(),
                declared_id: None,
                session: None,
            });
        self.programs.run(ProgramRequest {
            workspace,
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
            stop: Some(Arc::clone(&admitted.stop)),
        })
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

impl ActionProvider for TaskPrograms {
    fn query(
        &self,
        _caller: &Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> Pending<Result<tilo_actions::remote::CataloguePage, ActionCallError>> {
        offered(
            task_wire::PROVIDER,
            task_wire::APP,
            top_level(vec![descriptor()]),
            &query,
        )
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.run(caller, request, Admitted::default())
    }

    fn observe(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        settled(request.invocation, self.start(caller, &request, &admitted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{CatalogueQuery, PendingPoll};
    use tilo_actions::{ActionAddress, InvokeArgument, RemoteActionTarget};

    /// A host no step reaches: every case below is refused before a program runs.
    struct NoPrograms;

    impl Programs for NoPrograms {
        fn run(&self, _: ProgramRequest<'_>) -> Result<Pending<Outcome>, String> {
            Err("no program runs in these cases".into())
        }
    }

    /// The step as the session serves it.
    fn programs() -> Arc<TaskPrograms> {
        Arc::new(TaskPrograms::new(Arc::new(NoPrograms)))
    }

    /// The person at this desktop, in the workspace in front.
    fn person() -> Caller {
        Caller::local_person("Pat").in_workspace("the-workspace")
    }

    fn request(address: &ActionAddress, argument: &str) -> InvokeRequest {
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: task_wire::PROVIDER.to_owned(),
                address: address.clone(),
            },
            argument: InvokeArgument::from_slot(Some(argument)),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    tilo_test_env::scenario! {
        fn only_an_agents_own_session_step_is_offered_and_taken;

        phase a_program_step_is_the_agents_and_is_offered_in_its_sessions;
        // The step is the agent's, offered to an agent in a session and nowhere else.
        {
            let programs = programs();
            let providers = Providers::default();
            Arc::clone(&programs).register(&providers);
            let registered = providers
                .get(task_wire::PROVIDER)
                .expect("the programs are registered under their provider");
            assert_eq!(registered.provider, task_wire::PROVIDER);
            assert_eq!(registered.app, task_wire::APP);
            assert_eq!(
                registered.reach,
                Reach::AgentSessions { listed: true },
                "an agent session's catalogue offers it"
            );
            let query = CatalogueQuery {
                provider: Some(task_wire::PROVIDER.to_owned()),
                max_results: 1024,
                ..CatalogueQuery::default()
            };
            let mut page = registered.answers.query(&person(), query);
            let PendingPoll::Ready(Ok(page)) = page.poll_ready() else {
                panic!("the provider lists what it offers at once");
            };
            let described: Vec<_> = page.rows.into_iter().filter_map(|row| row.action).collect();
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
            let took = |caller: Caller| {
                programs
                    .start(&caller, &request(&address, &argument), &Admitted::default())
                    .err()
            };
            assert_eq!(
                took(Caller::local_person("Pat").in_workspace("the-workspace")),
                Some("a session's programs are its agent's".to_owned())
            );
            assert_eq!(
                took(Caller::local_agent("claude").in_workspace("the-workspace")),
                Some("an agent runs programs as a step of its session".to_owned())
            );
            assert_eq!(
                took(
                    Caller::local_agent("claude")
                        .in_workspace("the-workspace")
                        .in_agent_session("agent-session:1")
                ),
                Some("an agent runs programs only through its session's API".to_owned())
            );
        }
    }
}
