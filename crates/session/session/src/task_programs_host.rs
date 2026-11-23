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
use tilo_actions::remote::{ActionProvider, Pending};
use tilo_broker_core::steps::HostStepOutcome;
use tilo_lock::MutexRecover as _;

use crate::platform::programs::{ProgramRequest, Programs};

type Provider = Arc<dyn ActionProvider + Send + Sync>;

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
            *current.0.lock_recover() = Arc::clone(&request.provider);
            return Arc::clone(api);
        }
        let current = Arc::new(Current(
            std::sync::Mutex::new(Arc::clone(&request.provider)),
            Arc::clone(&self.services),
        ));
        let api = Arc::new(
            Api::for_task(
                Arc::clone(&current),
                request.workspace.to_owned(),
                request.agent,
                request.person,
            )
            .with_work_group(request.group.clone())
            .with_task_hosts(
                self.services
                    .task_hosts(Some(
                        request.person.unwrap_or_else(|| self.services.person()),
                    ))
                    .map(|tasks| tasks as Arc<dyn tilo_mcp::TaskHosts>),
            ),
        );
        sessions.insert(key, (current, Arc::clone(&api)));
        api
    }
}

/// The api the broker serves an agent session's latest step on: each turn of the session is
/// served on its own, and its programs call through the turn going now.
pub(crate) struct Current(
    std::sync::Mutex<Provider>,
    /// The session's own services: the records of the packages it runs itself (every tilolang
    /// app it serves) are read and changed there, on the loop's pass.
    Arc<crate::mcp_server::SessionServices>,
);

impl Current {
    fn provider(&self) -> Provider {
        Arc::clone(&self.0.lock_recover())
    }

    fn records(&self) -> TurnRecords {
        TurnRecords {
            kept: crate::mcp_server::ServiceRecords {
                state: None,
                session: Arc::clone(&self.1),
                agent_session: None,
            },
            services: Arc::clone(&self.1),
            provider: self.provider(),
        }
    }
}

/// The records an agent session's programs read and change, as the person's own REPL does at the
/// session's MCP server: the records the session keeps itself (the tilolang apps it serves) on the
/// loop's pass, as the session's services decide them for the caller, and every other app's
/// through the turn's own api. Without the first, a turn's program could neither read Tasks nor
/// add a task ("this desktop applies no task's changes at a record's origin"): the session keeps
/// those records and the broker has no origin for them.
struct TurnRecords {
    kept: crate::mcp_server::ServiceRecords,
    services: Arc<crate::mcp_server::SessionServices>,
    provider: Provider,
}

impl TurnRecords {
    fn elsewhere(
        &self,
    ) -> Result<Arc<dyn tilo_actions::remote::ResourceReads>, tilo_actions::remote::ActionCallError>
    {
        self.provider.resource_reads().ok_or_else(|| {
            tilo_actions::remote::ActionCallError::Unavailable {
                reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.to_owned(),
            }
        })
    }
}

impl tilo_actions::remote::ResourceReads for TurnRecords {
    /// Every app's resources are in the workspace's one store, the apps the session keeps
    /// included: the turn's api answers.
    fn store_read(
        &self,
        caller: &tilo_actions::remote::Caller,
        address: &tilo_actions::remote::ResourceAddress,
        read: serde_json::Value,
    ) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
        self.elsewhere()?.store_read(caller, address, read)
    }

    fn read(
        &self,
        caller: &tilo_actions::remote::Caller,
        address: &tilo_actions::remote::ResourceAddress,
    ) -> Result<tilo_actions::remote::ResourceCopy, tilo_actions::remote::ActionCallError> {
        if self.services.keeps(&address.app) {
            return self.kept.read(caller, address);
        }
        self.elsewhere()?.read(caller, address)
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &tilo_actions::remote::Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> Pending<tilo_actions::remote::RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        caller: &tilo_actions::remote::Caller,
        address: &tilo_actions::remote::ResourceAddress,
        wake: &tilo_actions::remote::ResourceWake,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        // The records the session keeps are read as they are on its pass: nothing to wait on.
        if self.services.keeps(&address.app) {
            return Ok(());
        }
        self.elsewhere()?.watch(caller, address, wake)
    }
}

impl tilo_actions::remote::ResourceChanges for TurnRecords {
    fn apply(
        &self,
        caller: &tilo_actions::remote::Caller,
        change: &tilo_actions::remote::ResourceChange,
    ) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
        if self.services.keeps(&change.app) {
            return self.kept.apply(caller, change);
        }
        self.provider
            .resource_changes()
            .ok_or_else(|| tilo_actions::remote::ActionCallError::Unavailable {
                reason: "this desktop applies no task's changes at a record's origin".to_owned(),
            })?
            .apply(caller, change)
    }
}

impl ActionProvider for Current {
    fn retire_execution(&self, caller: &tilo_actions::remote::Caller) {
        self.provider().retire_execution(caller);
    }

    fn query(
        &self,
        caller: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> Pending<Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>>
    {
        self.provider().query(caller, query)
    }

    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        self.provider().invoke(caller, request)
    }

    fn observe(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        self.provider().observe(caller, id)
    }

    fn next_change(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<Pending<()>> {
        self.provider().next_change(caller, id)
    }

    fn changed(
        &self,
        caller: &tilo_actions::remote::Caller,
        scope: tilo_actions::remote::WatchScope,
    ) -> Option<tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>>>
    {
        self.provider().changed(caller, scope)
    }

    fn cancel(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        self.provider().cancel(caller, id)
    }

    fn confirm(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Pending<
        Result<tilo_actions::remote::ConfirmedInvocation, tilo_actions::remote::ActionCallError>,
    > {
        self.provider().confirm(caller, request)
    }

    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.provider().content_puts()
    }

    fn content_gets(&self) -> Option<Arc<dyn tilo_actions::remote::ContentGets>> {
        self.provider().content_gets()
    }

    fn here(
        &self,
        caller: &tilo_actions::remote::Caller,
    ) -> Option<Vec<tilo_actions::remote::Here>> {
        self.provider().here(caller)
    }

    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        Some(Arc::new(self.records()))
    }

    fn resource_changes(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceChanges>> {
        Some(Arc::new(self.records()))
    }

    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.provider().resource_search()
    }

    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        self.provider().change_feed()
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
        person_text: ran.person_text,
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
