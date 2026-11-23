//! The programs a Linux session runs for agent sessions and for the person sitting here: each
//! session's api over its own tilolang host process.
//!
//! One agent session has one session of the api: its own host, the `let`s and references its
//! programs made, and the steps `undo` takes back. Every call a program makes is done as the agent,
//! for the person it acts for, attributed to the session, and decided by the workspace's policy
//! like the agent's own calls.
//!
//! The person has one too, and it is theirs: a line typed in a conversation runs through the
//! session's `for_person` api, so every call it makes is the person's own, and what it changes is
//! theirs in History. What the person keeps between lines is theirs too: the `let`s and the
//! references a later line goes on with.

use std::collections::HashMap;
use std::sync::Arc;

use tilo_actions::remote::{ActionProvider, Outcome, Pending};
use tilo_lock::MutexRecover as _;

use tilo_workspace_service::workspaces::task_programs::{ProgramCaller, ProgramRequest, Programs};

type Provider = Arc<dyn ActionProvider + Send + Sync>;

/// One session's api, over its own host.
type Api = tilo_mcp::McpServer<Arc<Current>>;

/// Whose session it is: an agent's, by the workspace it works in, its agent and its id; or the
/// person's, by the workspace and who sits there.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Agent {
        workspace: String,
        agent: String,
        session: String,
    },
    Person {
        workspace: String,
        person: String,
        session: Option<String>,
    },
}

/// Each session's api, with the api the broker serves its latest step on.
type Sessions = HashMap<Key, (Arc<Current>, Arc<Api>)>;

/// The programs' services.
pub(crate) struct TaskProgramHost {
    /// What a session's host asks the session's own services before it starts: the records the
    /// packages the session runs itself keep.
    services: Arc<crate::mcp_server::SessionServices>,
    /// The state this desktop serves, once it serves one: what the person's own line calls
    /// through, decided where it lands exactly as their own click is.
    state:
        std::sync::OnceLock<Arc<tilo_workspace_service::apps::collab_app_broker::AppBrokerState>>,
    sessions: std::sync::Mutex<Sessions>,
}

impl TaskProgramHost {
    /// The sessions' programs, whose hosts read the session's own records through `services`.
    pub(crate) fn new(services: Arc<crate::mcp_server::SessionServices>) -> Self {
        Self {
            services,
            state: std::sync::OnceLock::new(),
            sessions: std::sync::Mutex::default(),
        }
    }

    /// The state the person's own line's calls are decided by, once the app wire serves one.
    pub(crate) fn keep_state(
        &self,
        state: Arc<tilo_workspace_service::apps::collab_app_broker::AppBrokerState>,
    ) {
        let _ = self.state.set(state);
    }

    /// The person's own api: a step of theirs through the session's `for_person` api, so what the
    /// line does is theirs, and `stop` ends it.
    fn person_api(&self, stop: Arc<std::sync::atomic::AtomicBool>) -> Provider {
        let api =
            crate::mcp_server::Host::new(self.state.get().cloned(), Arc::clone(&self.services))
                .with_session_actions(self.services.actions())
                .for_local_person();
        Arc::new(Stopped { api, stop })
    }

    /// The session's api in `request`, started when it has none: the api the broker serves the
    /// session's latest step on, so everything a program does is a step the session admits even
    /// as one turn of it follows another.
    fn session_of(&self, request: &ProgramRequest<'_>) -> Result<Arc<Api>, String> {
        let (key, workspace, person, agent, session) = match request.caller {
            ProgramCaller::Agent {
                agent,
                person,
                session,
            } => (
                Key::Agent {
                    workspace: request.workspace.to_owned(),
                    agent: agent.to_owned(),
                    session: session.to_owned(),
                },
                request.workspace.to_owned(),
                person.map(str::to_owned),
                Some(agent),
                Some(session),
            ),
            ProgramCaller::Person { person, session } => (
                Key::Person {
                    workspace: request.workspace.to_owned(),
                    person: person.to_owned(),
                    session: session.map(str::to_owned),
                },
                request.workspace.to_owned(),
                Some(person.to_owned()),
                None,
                None,
            ),
        };
        let provider = self.provider_of(request)?;
        let mut sessions = self.sessions.lock_recover();
        if let Some((current, api)) = sessions.get(&key) {
            current.refresh(provider);
            return Ok(Arc::clone(api));
        }
        let current = Arc::new(Current {
            provider: std::sync::Mutex::new(provider),
            services: Arc::clone(&self.services),
        });
        let hosts = self
            .services
            .task_hosts(Some(
                person.as_deref().unwrap_or_else(|| self.services.person()),
            ))
            .map(|tasks| tasks as Arc<dyn tilo_mcp::TaskHosts>);
        let api = match (&agent, &session) {
            (Some(agent), Some(_)) => {
                Api::for_task(Arc::clone(&current), workspace, agent, person.as_deref())
                    .with_work_group(request.group.clone())
                    .with_task_hosts(hosts)
            }
            _ => Api::for_person(
                Arc::clone(&current),
                workspace,
                person.as_deref().unwrap_or_default(),
            )
            .with_work_group(request.group.clone())
            .with_task_hosts(hosts),
        };
        let api = Arc::new(api);
        sessions.insert(key, (current, Arc::clone(&api)));
        drop(sessions);
        Ok(api)
    }

    /// The api `request` calls through: the turn's own for an agent, and the person's own for
    /// their line.
    fn provider_of(&self, request: &ProgramRequest<'_>) -> Result<Provider, String> {
        match request.caller {
            ProgramCaller::Agent { .. } => request
                .provider
                .clone()
                .ok_or_else(|| "an agent runs programs only through its session's API".to_owned()),
            ProgramCaller::Person { .. } => Ok(self.person_api(
                request
                    .stop
                    .clone()
                    .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false))),
            )),
        }
    }
}

/// The person's api, while a line of theirs runs: a call it makes after the person stopped the
/// line is refused, so a stopped line makes no further change.
struct Stopped {
    api: crate::mcp_server::Host,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl ActionProvider for Stopped {
    fn retire_execution(&self, caller: &tilo_actions::remote::Caller) {
        self.api.retire_execution(caller);
    }

    fn query(
        &self,
        caller: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> Pending<Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>>
    {
        self.api.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        if self.stop.load(std::sync::atomic::Ordering::Acquire) {
            return Pending::ready(Err(tilo_actions::remote::ActionCallError::Refused {
                reason: "the person stopped this line".to_owned(),
            }));
        }
        self.api.invoke(caller, request)
    }

    fn observe(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        self.api.observe(caller, id)
    }

    fn next_change(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<Pending<()>> {
        self.api.next_change(caller, id)
    }

    fn changed(
        &self,
        caller: &tilo_actions::remote::Caller,
        scope: tilo_actions::remote::WatchScope,
    ) -> Option<tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>>>
    {
        self.api.changed(caller, scope)
    }

    fn cancel(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        self.api.cancel(caller, id)
    }


    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.api.content_puts()
    }

    fn content_gets(&self) -> Option<Arc<dyn tilo_actions::remote::ContentGets>> {
        self.api.content_gets()
    }

    fn here(
        &self,
        caller: &tilo_actions::remote::Caller,
    ) -> Option<Vec<tilo_actions::remote::Here>> {
        self.api.here(caller)
    }

    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        self.api.resource_reads()
    }

    fn resource_changes(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceChanges>> {
        self.api.resource_changes()
    }

    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.api.resource_search()
    }

    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        self.api.change_feed()
    }
}

/// The api the broker serves a session's latest step on: each turn of an agent's session, and
/// each line of the person's, is served on its own, and its programs call through it.
pub(crate) struct Current {
    provider: std::sync::Mutex<Provider>,
    /// The session's own services: the records of the packages it runs itself (every tilolang
    /// app it serves) are read and changed there, on the loop's pass.
    services: Arc<crate::mcp_server::SessionServices>,
}

impl Current {
    /// Serve this run's calls through `provider`.
    fn refresh(&self, provider: Provider) {
        *self.provider.lock_recover() = provider;
    }

    fn provider(&self) -> Provider {
        Arc::clone(&self.provider.lock_recover())
    }

    fn records(&self) -> TurnRecords {
        TurnRecords {
            kept: crate::mcp_server::ServiceRecords {
                state: None,
                session: Arc::clone(&self.services),
                agent_session: None,
            },
            services: Arc::clone(&self.services),
            provider: self.provider(),
        }
    }
}

/// The records a session's programs read and change, as the person's own REPL does at the
/// session's MCP server: the records the session keeps itself (the tilolang apps it serves) on the
/// loop's pass, as the session's services decide them for the caller, and every other app's
/// through the run's own api. Without the first, a turn's program could neither read Tasks nor
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
    /// included: the run's api answers.
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

impl Programs for TaskProgramHost {
    fn run(&self, request: ProgramRequest<'_>) -> Result<Pending<Outcome>, String> {
        let api = self.session_of(&request)?;
        tilo_mcp::task_runs::run(
            api,
            request.program,
            request.confirm,
            request.this,
            request.stop,
        )
    }
}
