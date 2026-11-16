//! The programs a Linux session runs for agent sessions: each session's api over its own
//! tilolang host process.
//!
//! One agent session has one session of the api: its own host, the `let`s and references its
//! programs made, and the steps `undo` takes back. Every call a program makes is done as the agent,
//! for the person it acts for, attributed to the session, and decided by the workspace's policy
//! like the agent's own calls.

use std::collections::HashMap;
use std::sync::Arc;

use tilo_actions::remote::task_programs as wire;
use tilo_actions::remote::{ActionHost, Pending};
use tilo_broker_core::steps::HostStepOutcome;
use tilo_lock::MutexRecover as _;

use crate::platform::programs::{ProgramRequest, Programs};

type Host = Arc<dyn ActionHost + Send + Sync>;

/// One agent session's api, over its own host.
type Api = tilo_mcp::McpServer<Arc<Current>>;

/// An agent session, by the workspace it works in, its agent and its id.
type SessionKey = (String, String, String);

/// Each agent session's api, with the api the broker serves its latest step on.
type Sessions = HashMap<SessionKey, (Arc<Current>, Arc<Api>)>;

/// The agent sessions' services.
pub(crate) struct TaskProgramHost {
    /// What a session's host asks the session's own services before it starts: the records the
    /// packages the session runs itself keep.
    services: Arc<crate::mcp_server::SessionServices>,
    sessions: std::sync::Mutex<Sessions>,
}

impl TaskProgramHost {
    /// The sessions' programs, whose hosts read the session's own records through `services`.
    pub(crate) fn new(services: Arc<crate::mcp_server::SessionServices>) -> Self {
        Self {
            services,
            sessions: std::sync::Mutex::default(),
        }
    }

    /// The agent session's api, started when it has none, calling through `host`: the api the
    /// broker serves the session's latest step on, so everything a program does is a step the
    /// session admits even as one turn of it follows another.
    fn session_of(&self, request: &ProgramRequest<'_>) -> Arc<Api> {
        let key = (
            request.workspace.to_owned(),
            request.agent.to_owned(),
            request.session.to_owned(),
        );
        let mut sessions = self.sessions.lock_recover();
        if let Some((current, api)) = sessions.get(&key) {
            *current.0.lock_recover() = Arc::clone(&request.host);
            return Arc::clone(api);
        }
        let current = Arc::new(Current(std::sync::Mutex::new(Arc::clone(&request.host))));
        let api = Arc::new(
            Api::for_task(
                Arc::clone(&current),
                request.workspace.to_owned(),
                request.agent,
                request.person,
            )
            .with_work_group(request.group.clone())
            .with_task_hosts(Some(Arc::new(crate::mcp_server::TaskHosts::new(
                request.person.unwrap_or_else(|| self.services.person()),
                Arc::clone(&self.services),
            )))),
        );
        sessions.insert(key, (current, Arc::clone(&api)));
        api
    }
}

/// The api the broker serves an agent session's latest step on: each turn of the session is
/// served on its own, and its programs call through the turn going now.
pub(crate) struct Current(std::sync::Mutex<Host>);

impl Current {
    fn host(&self) -> Host {
        Arc::clone(&self.0.lock_recover())
    }
}

impl ActionHost for Current {
    fn retire_execution(&self, caller: &tilo_actions::remote::Caller) {
        self.host().retire_execution(caller);
    }

    fn query(
        &self,
        caller: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> Pending<Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionHostError>>
    {
        self.host().query(caller, query)
    }

    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError>,
    > {
        self.host().invoke(caller, request)
    }

    fn observe(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        self.host().observe(caller, id)
    }

    fn next_change(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<Pending<()>> {
        self.host().next_change(caller, id)
    }

    fn cancel(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), tilo_actions::remote::ActionHostError>> {
        self.host().cancel(caller, id)
    }

    fn confirm(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Pending<
        Result<tilo_actions::remote::ConfirmedInvocation, tilo_actions::remote::ActionHostError>,
    > {
        self.host().confirm(caller, request)
    }

    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.host().content_puts()
    }

    fn content_gets(&self) -> Option<Arc<dyn tilo_actions::remote::ContentGets>> {
        self.host().content_gets()
    }

    fn here(
        &self,
        caller: &tilo_actions::remote::Caller,
    ) -> Option<Vec<tilo_actions::remote::Here>> {
        self.host().here(caller)
    }

    fn record_reads(&self) -> Option<Arc<dyn tilo_actions::remote::RecordReads>> {
        self.host().record_reads()
    }

    fn record_changes(&self) -> Option<Arc<dyn tilo_actions::remote::RecordChanges>> {
        self.host().record_changes()
    }

    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.host().resource_search()
    }

    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        self.host().change_feed()
    }
}

/// The session's program could not start or run at all.
fn unavailable(message: String) -> HostStepOutcome {
    Err(Box::new(tilo_actions::ActionFailure {
        code: "task_program_unavailable".into(),
        message,
        details: None,
        retryable: true,
    }))
}

fn outcome(ran: tilo_mcp::TaskProgram) -> tilo_actions::ActionOutput {
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
        let api = self.session_of(&request);
        let (program, confirm, this) = (request.program, request.confirm, request.this);
        let (complete, answered) = tilo_actions::remote::pending();
        // The program waits on the session's host and on every call it makes: never on the broker's
        // thread.
        std::thread::Builder::new()
            .name("task-program".into())
            .spawn(move || {
                let answered = match tilo_rt::try_app() {
                    Ok(runtime) => {
                        match runtime.block_on(api.task_program(&program, confirm, this.as_deref()))
                        {
                            Ok(ran) => Ok(outcome(ran)),
                            Err(error) => unavailable(error.to_string()),
                        }
                    }
                    Err(error) => unavailable(error.to_string()),
                };
                complete.complete(answered);
            })
            .map_err(|error| error.to_string())?;
        Ok(answered)
    }
}
