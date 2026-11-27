//! The person's shell, as this device's daemon answers it on its dispatch
//! (`tilo_app_host::shell`): the desktop session of the person signed in on this
//! computer, which reaches their workspaces through the daemon and never on its own.
//!
//! - Who a shell is: the process the daemon started as the shell, holding the link it made for it
//!   (`Daemon::start_shell`); no other process, whatever account it runs as.
//! - Its documents: the daemon's own reads and writes of the workspaces it holds, each write
//!   sealed with the keys of the document's group this device reads and signed as the device.
//! - Its calls: every action it invokes reaches this device's one dispatch, as the person
//!   ([`PersonDispatch`]), the dispatch the person's own runs reach. Founding a workspace here and
//!   joining one by an invitation are the person's workspace list's actions; a change the person
//!   makes to a workspace's people and settings is written as this device's own, into the
//!   workspace this device keeps or its member copy; any other key access and membership action is
//!   answered where this device keeps the workspace, and asked of the device keeping it over
//!   tilo-actions/1 otherwise; the host's own operations are this device's workspace service's
//!   ([`Kept`]).
//! - How its workspaces stand: [`Standing`], read again whenever a workspace changes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use collab_types::signer::DeviceSigner;
use runtime_collab::wire::{Event, Receipt};
use serde_json::{Map, Value};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_app_host::services::{PublishedResources, Reading, ResourceWatch};
use tilo_app_host::shell::{ShellActions, ShellCaller, ShellDispatch, ShellLinks, ShellServices};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::workspace_view::{
    BRANCH, CREATE, DELETE, JOIN, LIST, Listed, MEMBERSHIP, Part, part as part_resource, resource,
};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::standing;
use tokio::sync::{Notify, mpsc, oneshot};

use super::services::PersonDispatch;
use super::workspaces::ServiceWorkspaces;

/// What this device's dispatch answers its person's shell with, read again as `workspaces`
/// changes. Its calls reach `dispatching` once the daemon serves it.
/// What the shell is served beside its workspaces and links: the identity it takes its
/// connections for, its live links, and what it says of what it speaks.
pub(crate) struct Serving {
    pub identity: Arc<dyn Fn(tilo_socket::Stream) + Send + Sync>,
    pub live: Option<tilo_app_host::shell::LiveLinks>,
    pub vpn: Option<tilo_app_host::shell::DeviceVpnLinks>,
    pub speakers: Option<tilo_app_host::shell::SpeakersLinks>,
    pub speaking: Arc<dyn tilo_app_host::shell::ShellSpeaking>,
}

pub(crate) fn services(
    workspaces: &Arc<ServiceWorkspaces>,
    links: &Arc<ShellLinks>,
    socket: std::path::PathBuf,
    dispatching: Arc<Dispatching>,
    published: Arc<dyn PublishedResources>,
    serving: Serving,
) -> ShellServices {
    let Serving {
        identity,
        live,
        vpn,
        speakers,
        speaking,
    } = serving;
    let standing = Standing::new(
        workspaces.copies().clone(),
        workspaces.device_cert(),
        workspaces.signer(),
    );
    standing.follow();
    let copies = workspaces.copies().clone();
    let api = Arc::clone(&dispatching);
    ShellServices {
        links: Arc::clone(links),
        socket,
        documents: Arc::new(ServiceDocuments::of(copies.clone())),
        writes: Arc::new(ServiceWrites::of(copies.clone())),
        document_writes: Arc::new({
            let copies = copies.clone();
            move || copies.document_writes()
        }),
        workspaces: copies,
        live,
        vpn,
        speakers,
        speaking: Some(speaking),
        actions: Arc::new(Actions {
            kept: Arc::new(Kept {
                workspaces: Arc::clone(workspaces),
                changed: standing.changed(),
            }),
            dispatching,
            by_workspace: Mutex::default(),
            agents: Arc::default(),
        }),
        standing,
        published: Some(published),
        identity: Some(identity),
        api: Some(Arc::new(move |shell: &ShellCaller, stream| {
            api.api(shell, stream)
        })),
    }
}

/// The one dispatch the person's calls reach, once the daemon's broker serves it, and Tilo's API
/// their own programs reach, once the daemon serves that.
#[derive(Default)]
pub(crate) struct Dispatching {
    person: OnceLock<PersonDispatch>,
    /// The person the API acts as, and the API.
    api: OnceLock<([u8; 32], Arc<tilo_api::direct::Direct>)>,
}

impl Dispatching {
    /// The person's calls reach `person` from now on.
    pub(crate) fn serve(&self, person: PersonDispatch) {
        let _ = self.person.set(person);
    }

    /// The connections the person's shell takes for their programs are served by `api`, acting
    /// as `person`, from now on.
    pub(crate) fn serve_api(&self, person: [u8; 32], api: Arc<tilo_api::direct::Direct>) {
        let _ = self.api.set((person, api));
    }

    /// Serve `stream`, a connection a program of `shell`'s person made at their API socket, on
    /// the daemon's runtime: only the shell of the person the API acts as hands one over.
    fn api(&self, shell: &ShellCaller, stream: tilo_socket::Stream) -> Result<(), String> {
        let (person, api) = self
            .api
            .get()
            .ok_or("this device's API for its person's programs is not up yet")?;
        if shell.person != *person {
            return Err("this device serves its API to its own person's programs only".to_owned());
        }
        let api = Arc::clone(api);
        let runtime = tilo_rt::app().handle();
        let stream = {
            let _inside = runtime.enter();
            tilo_socket::tokio::from_std(stream)
                .map_err(|error| format!("a program's API connection cannot be served: {error}"))?
        };
        runtime.spawn(async move { api.serve_admitted(stream).await });
        Ok(())
    }
}

/// What a shell's calls reach.
struct Actions {
    kept: Arc<Kept>,
    dispatching: Arc<Dispatching>,
    /// The dispatch of each workspace a shell called in, admitted once.
    by_workspace: Mutex<HashMap<String, Arc<dyn ActionProvider + Send + Sync>>>,
    /// The run of each agent the shell named as a caller, in each workspace, admitted once.
    agents: Arc<AgentRuns>,
}

/// The runs of the agents the person admitted on their shell, by workspace and agent as the
/// shell names it ([`PersonDispatch::agent_provider`]).
type AgentRuns = Mutex<HashMap<String, Arc<dyn ActionProvider + Send + Sync>>>;

impl ShellActions for Actions {
    fn dispatch(
        &self,
        caller: &ShellCaller,
        workspace: Option<String>,
        pressed: Option<runtime_collab::wire::shell::ShellPress>,
    ) -> ShellDispatch {
        let kept = Arc::clone(&self.kept);
        let shell = *caller;
        let Some(workspace) = workspace else {
            let cert = kept.workspaces.device_cert();
            let person = Caller::authenticated(cert.user_public_key, cert.device_public_key);
            return Box::pin(std::future::ready(Ok((
                Arc::new(Shell {
                    kept,
                    caller: shell,
                    workspace: None,
                    agents: None,
                }) as Arc<dyn ActionProvider + Send + Sync>,
                person,
            ))));
        };
        // A device placed for records of one workspace admits nothing itself: its session's calls
        // there, its `settings#set` among them, go to the workspace's host over the link this
        // device dialled, which decides each as this placed device.
        if kept.workspaces.confined().is_some() {
            let cert = kept.workspaces.device_cert();
            let device = Caller::visitor(cert.device_public_key).in_workspace(workspace.clone());
            return Box::pin(std::future::ready(
                super::confined::forwarded(&kept.workspaces, Some(&workspace)).map(|host| {
                    (
                        Arc::new(Shell {
                            kept,
                            caller: shell,
                            workspace: Some(host),
                            agents: None,
                        }) as Arc<dyn ActionProvider + Send + Sync>,
                        device,
                    )
                }),
            ));
        }
        let Some(person) = self.dispatching.person.get().cloned() else {
            return Box::pin(std::future::ready(Err(
                "this device's dispatch is not up yet".to_owned(),
            )));
        };
        if shell.person != kept.workspaces.device_cert().user_public_key {
            return Box::pin(std::future::ready(Err(
                "this device acts for its own person only".to_owned(),
            )));
        }
        // A press names a window of its own, which the cached dispatch does not: only a call
        // nobody pressed shares the workspace's one.
        let keep = pressed.is_none();
        let cached = keep.then(|| {
            self.by_workspace
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&workspace)
                .cloned()
        });
        let caller = person.caller(&workspace);
        let agents = AgentsHere {
            person: person.clone(),
            workspace: workspace.clone(),
            runs: Arc::clone(&self.agents),
        };
        let admitted = match cached.flatten() {
            Some(dispatch) => Ok(dispatch),
            None => person
                .provider_pressed(&workspace, pressed)
                .map(|provider| Arc::new(provider) as Arc<dyn ActionProvider + Send + Sync>)
                .map_err(|error| error.to_string())
                .inspect(|dispatch| {
                    if keep {
                        self.by_workspace
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(workspace.clone(), Arc::clone(dispatch));
                    }
                }),
        };
        Box::pin(std::future::ready(admitted.map(|dispatch| {
            (
                Arc::new(Shell {
                    kept,
                    caller: shell,
                    workspace: Some(dispatch),
                    agents: Some(agents),
                }) as Arc<dyn ActionProvider + Send + Sync>,
                caller,
            )
        })))
    }
}

/// A shell's calls: those of the person's workspace list, a workspace's key access and
/// membership and the host's own operations answered where they are decided ([`Kept`]), and every
/// other one by the person's dispatch in the workspace it names.
struct Shell {
    kept: Arc<Kept>,
    caller: ShellCaller,
    workspace: Option<Arc<dyn ActionProvider + Send + Sync>>,
    /// Where the calls the shell makes for an agent its person admitted go, in the workspace
    /// it names.
    agents: Option<AgentsHere>,
}

/// The runs of the person's agents in the one workspace a shell's call names.
struct AgentsHere {
    person: PersonDispatch,
    workspace: String,
    runs: Arc<AgentRuns>,
}

impl AgentsHere {
    /// The run `agent` calls in, admitted the first time it calls here.
    fn run_of(
        &self,
        agent: &Caller,
    ) -> Result<Arc<dyn ActionProvider + Send + Sync>, ActionCallError> {
        let key = format!("{}\n{:?}", self.workspace, agent.kept());
        if let Some(run) = self
            .runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return Ok(Arc::clone(run));
        }
        let run: Arc<dyn ActionProvider + Send + Sync> = Arc::new(
            self.person
                .agent_provider(&self.workspace, agent)
                .map_err(|error| ActionCallError::Refused {
                    reason: error.to_string(),
                })?,
        );
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, Arc::clone(&run));
        Ok(run)
    }
}

impl Shell {
    fn in_workspace(&self) -> Result<&Arc<dyn ActionProvider + Send + Sync>, ActionCallError> {
        self.workspace
            .as_ref()
            .ok_or_else(|| ActionCallError::Refused {
                reason: "the shell calls that in a workspace, and named none".into(),
            })
    }

    /// Whether the shell names `caller` as an agent its person admitted: such a caller's calls
    /// are its own, and reach the run its grant decides, never what this device answers for its
    /// person.
    fn names_an_agent(&self, caller: &Caller) -> bool {
        self.agents.is_some() && caller.acting().is_agent() && caller.is_session_delegated()
    }

    /// Whether this device answers `request` itself for `caller`: its workspaces' governance,
    /// the host's own operations and the person's workspace list are the person's own answers to
    /// their calls and to an app's acting for them. An agent the person admitted on their shell
    /// is never the device, whatever its grant: its call reaches its own run.
    fn answers_itself(&self, caller: &Caller, request: &InvokeRequest) -> bool {
        kept_here(request) && !self.names_an_agent(caller)
    }

    /// Whether `request` is an agent's call to `workspace`'s governance, which the agent's own
    /// grant decides: the device's own answers are the person's, and the host's own operations
    /// are refused to an agent whole ([`Self::dispatch_for`]).
    fn agent_answers_kept_here(&self, caller: &Caller, request: &InvokeRequest) -> bool {
        kept_here(request)
            && request.target.provider.as_str()
                != tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER
            && self.names_an_agent(caller)
    }

    /// The dispatch `caller`'s calls reach: an agent the person admitted on their shell calls in
    /// a run of its own, delegated by them; the person, and an app acting for them, in theirs.
    fn reaching(
        &self,
        caller: &Caller,
    ) -> Result<Arc<dyn ActionProvider + Send + Sync>, ActionCallError> {
        match &self.agents {
            Some(agents) if self.names_an_agent(caller) => agents.run_of(caller),
            _ => self.in_workspace().map(Arc::clone),
        }
    }

    /// The dispatch `caller`'s `request` reaches, where this device does not answer it: an agent
    /// the person admitted on their shell reaches the run its grant decides, and the host's own
    /// operations — this device's own for its person, asked as it — are none of an agent's.
    fn dispatch_for(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Arc<dyn ActionProvider + Send + Sync>, ActionCallError> {
        let hosts_own = request.target.provider.as_str()
            == tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER;
        if hosts_own && self.names_an_agent(caller) {
            return Err(ActionCallError::NotPermitted);
        }
        self.reaching(caller)
    }
}

/// Whether `request` is the person's workspace list's, a workspace's key access and membership,
/// or the host's own: what this device answers where it is decided ([`ask`]).
fn kept_here(request: &InvokeRequest) -> bool {
    request.target.provider == tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER
        || tilo_actions::remote::workspace_governance_operation(&request.target.address).is_some()
}

impl ActionProvider for Shell {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let dispatch = match self.reaching(caller) {
            Ok(dispatch) => dispatch,
            Err(refused) => return Pending::ready(Err(refused)),
        };
        if self.kept.workspaces.confined().is_none() || !query.cursor.is_empty() {
            return dispatch.query(caller, query);
        }
        // A device placed for records sends its own settings actions to its host, which keeps
        // them where its certificate says: they lead what the host lists of what this device may
        // call there, so `tilo do settings#set` finds them as on any device.
        let own = super::ticket::settings_catalogue(&query);
        dispatch.query(caller, query).map(move |answered| {
            answered.map(|mut page| {
                let mut rows = own.rows;
                rows.append(&mut page.rows);
                page.rows = rows;
                page
            })
        })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // The device's one search, answered from its index: the person's search spans every
        // workspace they see.
        if tilo_workspace_records::search::is_search(&request.target) {
            let Some(search) = self.kept.workspaces.search() else {
                return Pending::ready(Err(ActionCallError::Unavailable {
                    reason: "this device holds no records to search".into(),
                }));
            };
            return tilo_workspace_records::search::serve(&*search, caller, &request);
        }
        if self.answers_itself(caller, &request) {
            return self.kept.invoke(self.caller, request);
        }
        let by_its_grant = self.agent_answers_kept_here(caller, &request);
        match self.dispatch_for(caller, &request) {
            Ok(dispatch) if by_its_grant => {
                let answer = dispatch.invoke(caller, request);
                answer.map(|answer| match answer {
                    // The workspace publishes this operation and does not let this agent
                    // invoke it: its grant does not cover it, and what it may not do is not
                    // an action nobody knows.
                    Err(ActionCallError::UnknownAction) => Err(ActionCallError::NotPermitted),
                    answer => answer,
                })
            }
            Ok(dispatch) => dispatch.invoke(caller, request),
            Err(refused) => Pending::ready(Err(refused)),
        }
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.reaching(caller).ok()?.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.reaching(caller).ok()?.next_change(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        match self.reaching(caller) {
            Ok(dispatch) => dispatch.cancel(caller, id),
            Err(refused) => Pending::ready(Err(refused)),
        }
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: tilo_actions::remote::Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if self.answers_itself(caller, &request) {
            // The device answers this one itself: it starts nothing that outlives the answer.
            return self.kept.invoke(self.caller, request);
        }
        let by_its_grant = self.agent_answers_kept_here(caller, &request);
        match self.dispatch_for(caller, &request) {
            Ok(dispatch) if by_its_grant => {
                let answer = dispatch.run(caller, request, admitted);
                answer.map(|answer| match answer {
                    Err(ActionCallError::UnknownAction) => Err(ActionCallError::NotPermitted),
                    answer => answer,
                })
            }
            Ok(dispatch) => dispatch.run(caller, request, admitted),
            Err(refused) => Pending::ready(Err(refused)),
        }
    }

    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &tilo_actions::remote::Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        if self.answers_itself(caller, request) {
            return None;
        }
        self.dispatch_for(caller, request)
            .ok()
            .and_then(|dispatch| dispatch.asks_first(caller, request, admitted))
    }

    fn watch(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::LiveView, String> {
        if self.answers_itself(caller, request) {
            return Err("what this device answers itself has no live result".to_owned());
        }
        match self.dispatch_for(caller, request) {
            Ok(dispatch) => dispatch.watch(caller, request),
            Err(refused) => Err(refused.to_string()),
        }
    }
}

/// What this device answers of a shell's calls itself: its workspace service, and the devices
/// keeping the workspaces it holds as a member, over tilo-actions/1.
struct Kept {
    workspaces: Arc<ServiceWorkspaces>,
    /// Told once an ask settled, so the workspaces' standing is read again.
    changed: Arc<Notify>,
}

impl Kept {
    /// `request` asked for `shell`, answered once it settled.
    fn invoke(
        &self,
        shell: ShellCaller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (done, receipt) = tilo_actions::remote::pending();
        let workspaces = Arc::clone(&self.workspaces);
        let changed = Arc::clone(&self.changed);
        let invocation = request.invocation;
        tilo_rt::app().spawn(async move {
            let answer = Box::pin(ask(&workspaces, shell, request)).await;
            changed.notify_one();
            let status = match answer {
                Ok(value) => ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(value),
                        ..tilo_actions::ActionOutput::default()
                    },
                },
                Err(message) => ReceiptStatus::Failed {
                    error: tilo_actions::ActionFailure {
                        code: "refused".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            };
            done.complete(Ok(InvocationReceipt { invocation, status }));
        });
        receipt
    }
}

/// `request` asked as this device for `caller`, answered where it is decided.
async fn ask(
    workspaces: &ServiceWorkspaces,
    caller: ShellCaller,
    request: InvokeRequest,
) -> Result<Value, String> {
    let cert = workspaces.device_cert().clone();
    if caller.person != cert.user_public_key {
        return Err("this device acts for its own person only".to_owned());
    }
    let address = &request.target.address;
    if request.target.provider == tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER {
        let arguments = named(request.argument)?;
        // The person's workspace list: founding one here, and joining one.
        if address.resource == LIST {
            return Box::pin(listed(workspaces, &address.action, &arguments)).await;
        }
        // The host's own operations are this device's workspace service's.
        return workspaces
            .service()
            .ask_own(&cert, &address.action, arguments)
            .await;
    }
    let workspace = tilo_actions::remote::workspace_governance_operation(address)
        .map(|(workspace, _)| workspace.to_owned())
        .ok_or(
            "the shell asks a workspace's key access and membership or the host, and nothing else",
        )?;
    let id = WorkspaceId::parse(&workspace).ok_or("that is not a workspace")?;
    // A change to the workspace's people and settings is this device's own, signed with its key:
    // the workspace's admission decides it as any member's.
    if address.action == MEMBERSHIP {
        let command = named(request.argument)?
            .remove("command")
            .ok_or("a change to a workspace's people names its `command`")?;
        let command = serde_json::from_value(command)
            .map_err(|error| format!("the change does not read: {error}"))?;
        workspaces
            .copies()
            .commit_own_change(&workspace, command, None)
            .await?;
        return Ok(Value::Null);
    }
    // A workspace this device keeps answers here, as it answers every member's device.
    if workspaces.copies().administers_workspace(&workspace).await {
        let as_device = Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace);
        let provider = workspaces.copies().device_actions();
        let receipt: InvocationReceipt = provider
            .invoke(&as_device, request)
            .await
            .ok_or("the workspace dropped the request")?
            .map_err(|error| error.to_string())?;
        return match receipt.status {
            ReceiptStatus::Accepted => {
                follow(provider.as_ref(), &as_device, &receipt.invocation).await
            }
            status => settled(status),
        };
    }
    // Kept elsewhere: asked of the device keeping it, at the address that device signed.
    let peer = workspaces
        .peer(id)
        .ok_or("this device does not hold the workspace")?;
    let (_, addr) = standing::keeping_device_elsewhere(&peer, &cert.device_public_key)
        .ok_or("this device reaches no device keeping the workspace")?;
    let (actions, endpoint, _) = workspaces
        .turn_action_service()
        .ok_or("this device runs no action service")?;
    let followed = Arc::new(actions.remote(&endpoint, &addr, &workspace))
        .invoke_until_settled(request, tilo_peer_state::actions::FOLLOW_DEADLINE)
        .await;
    match followed {
        Some(Ok(update)) => finished(update),
        Some(Err(error)) => Err(error.to_string()),
        None => Err("the workspace's keeping device dropped the request".to_owned()),
    }
}

/// The named arguments `argument` carries.
fn named(argument: InvokeArgument) -> Result<Map<String, Value>, String> {
    match argument {
        InvokeArgument::Json(arguments) => Ok(arguments),
        InvokeArgument::None => Ok(Map::new()),
        InvokeArgument::Text(_) => Err("the device's actions take named arguments".to_owned()),
    }
}

/// The person's workspace list's `action` with `arguments`: [`CREATE`] founds a workspace this
/// device keeps, [`JOIN`] joins the workspace an invitation is to, [`BRANCH`] copies one of the
/// person's own into a new one, and [`DELETE`] deletes one where this device keeps it. Answers
/// the workspace the action made, when it makes one.
async fn listed(
    workspaces: &ServiceWorkspaces,
    action: &str,
    arguments: &Map<String, Value>,
) -> Result<Value, String> {
    let text = |name: &str| {
        arguments
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("`{action}` names its `{name}`"))
    };
    match action {
        CREATE => {
            let workspace = WorkspaceId::parse(text("id")?).ok_or("that is not a workspace")?;
            workspaces.found(workspace, text("name")?).await?;
            Ok(Value::String(workspace.to_string()))
        }
        JOIN => Ok(Value::String(
            Box::pin(workspaces.copies().join_member(text("invite")?))
                .await?
                .to_string(),
        )),
        BRANCH => {
            let source = text("workspace")?.to_owned();
            let name = text("name")?.to_owned();
            let branch = match arguments.get("id").and_then(Value::as_str) {
                Some(id) => uuid::Uuid::parse_str(id)
                    .map_err(|_| "`id` is a workspace id (a UUID)".to_owned())?
                    .to_string(),
                None => uuid::Uuid::new_v4().to_string(),
            };
            let device = &workspaces.device_cert();
            let made = super::services::branch_here(
                workspaces,
                device,
                device,
                source,
                name,
                branch,
                tilo_server_protocol::workspaces::WorkspaceLabels::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
            Ok(Value::String(made.workspace.workspace))
        }
        DELETE => {
            let workspace = text("workspace")?.to_owned();
            let device = &workspaces.device_cert();
            super::services::delete_here(workspaces, device, device, &workspace)
                .await
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        _ => Err(format!("the person's workspaces have no action `{action}`")),
    }
}

/// Follow `invocation` on `provider` until it finishes, as long as a request is followed.
async fn follow(
    provider: &(dyn tilo_actions::remote::ActionProvider + Send + Sync),
    caller: &Caller,
    invocation: &tilo_actions::InvocationId,
) -> Result<Value, String> {
    let followed = tokio::time::timeout(tilo_peer_state::actions::FOLLOW_DEADLINE, async {
        loop {
            let state = provider
                .observe(caller, invocation)
                .ok_or("the workspace lost the request")?;
            if !matches!(
                state.update,
                InvocationUpdate::Accepted { .. }
                    | InvocationUpdate::Progress { .. }
                    | InvocationUpdate::Item { .. }
            ) {
                return finished(state.update);
            }
            provider
                .next_change(caller, invocation)
                .ok_or("the workspace does not say when the request moves")?
                .await;
        }
    })
    .await;
    followed.unwrap_or_else(|_| Err("the workspace did not finish the request in time".to_owned()))
}

/// What a finished invocation comes to.
fn finished(update: InvocationUpdate) -> Result<Value, String> {
    match update {
        InvocationUpdate::Succeeded { output, .. } => Ok(output.value.unwrap_or_default()),
        InvocationUpdate::Failed { error, .. } => Err(error.message),
        InvocationUpdate::Cancelled { message, .. } => {
            Err(message.unwrap_or_else(|| "the request was cancelled".to_owned()))
        }
        InvocationUpdate::Accepted { .. }
        | InvocationUpdate::Progress { .. }
        | InvocationUpdate::Item { .. } => Err("the request did not finish".to_owned()),
    }
}

/// What a receipt the workspace answered at once comes to.
fn settled(status: ReceiptStatus) -> Result<Value, String> {
    match status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error.to_string()),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Cancelled { message } => {
            Err(message.unwrap_or_else(|| "the request was cancelled".to_owned()))
        }
        ReceiptStatus::Accepted => Err("the request did not finish".to_owned()),
    }
}

/// How the person's workspaces stand, as the shell watches them: [`LIST`], and each workspace's
/// reading at [`resource`], read again whenever something changes there and published to every
/// watch whose resource reads differently.
pub struct Standing {
    /// The workspaces this device holds, read as it holds them.
    workspaces: Workspaces,
    /// The device whose person the readings are made for, and which signs them.
    cert: DeviceCert,
    signer: Arc<dyn DeviceSigner>,
    /// Told whenever a workspace may read differently; many tellings before a read are one.
    changed: Arc<Notify>,
    state: Mutex<Published>,
}

#[derive(Default)]
struct Published {
    revision: u64,
    /// The readings as last published, by resource.
    readings: BTreeMap<String, Vec<u8>>,
    watches: BTreeMap<u64, (String, mpsc::UnboundedSender<Event>)>,
    next_watch: u64,
}

impl Standing {
    /// How the workspaces `workspaces` stand for the device `cert`, signing its readings with
    /// `signer`.
    pub fn new(
        workspaces: Workspaces,
        cert: DeviceCert,
        signer: Arc<dyn DeviceSigner>,
    ) -> Arc<Self> {
        let standing = Arc::new(Self {
            workspaces,
            cert,
            signer,
            changed: Arc::new(Notify::new()),
            state: Mutex::default(),
        });
        standing.read_again();
        standing
    }

    /// Told whenever a workspace may read differently: what a change this device itself made
    /// tells, and what its own workspaces' changes tell.
    pub fn changed(&self) -> Arc<Notify> {
        Arc::clone(&self.changed)
    }

    /// Read again whenever a workspace changes here: its key access or membership changed, another
    /// device's write was taken in, this device took a workspace in, or a shell's ask settled.
    pub fn follow(self: &Arc<Self>) {
        let copies = self.workspaces.clone();
        let tell = |changed: &Arc<Notify>| {
            let changed = Arc::clone(changed);
            move || changed.notify_one()
        };
        let mut decided = copies.governance_changes();
        let hosted = tell(&self.changed);
        tilo_rt::app().spawn(async move {
            while !matches!(
                decided.recv().await,
                Err(tokio::sync::broadcast::error::RecvError::Closed)
            ) {
                hosted();
            }
        });
        let (written, lagged) = (tell(&self.changed), tell(&self.changed));
        tilo_rt::app().spawn(tilo_app_host::collab_inbound::follow_admitted_writes(
            &copies,
            move |_| written(),
            lagged,
        ));
        {
            let taken = copies.members_held();
            let took = tell(&self.changed);
            tilo_rt::app().spawn(async move {
                loop {
                    taken.notified().await;
                    took();
                }
            });
        }
        let standing = Arc::downgrade(self);
        let changed = Arc::clone(&self.changed);
        tilo_rt::app().spawn(async move {
            loop {
                changed.notified().await;
                let Some(standing) = standing.upgrade() else {
                    return;
                };
                // The read waits on the workspaces' own locks: off the runtime's workers.
                if tokio::task::spawn_blocking(move || standing.read_again())
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
    }

    /// Read every resource again, and publish those that read differently now.
    fn read_again(&self) {
        let readings = self.read_all();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let changed: BTreeSet<String> = readings
            .keys()
            .chain(state.readings.keys())
            .filter(|resource| readings.get(*resource) != state.readings.get(*resource))
            .cloned()
            .collect();
        if changed.is_empty() {
            return;
        }
        state.revision = state.revision.wrapping_add(1);
        state.readings = readings;
        let revision = state.revision;
        let Published {
            readings, watches, ..
        } = &mut *state;
        watches.retain(|_, (resource, sink)| {
            !changed.contains(resource)
                || sink
                    .send(Event::Reading {
                        resource: resource.clone(),
                        revision,
                        reading: readings.get(resource).cloned(),
                    })
                    .is_ok()
        });
        drop(state);
    }

    fn read_all(&self) -> BTreeMap<String, Vec<u8>> {
        let cert = &self.cert;
        let reader = standing::Reader {
            person: cert.user_public_key,
            device: cert.device_public_key,
            signer: self.signer.as_ref(),
        };
        let mut readings = BTreeMap::new();
        let mut listed = Vec::new();
        let mut held = self.workspaces.copies_now();
        held.sort_by_key(|(_, copy)| copy.member);
        for (workspace, copy) in held {
            // The device that keeps it, which is this one unless another runs the workspace's work:
            // what a read of it and its changes are asked of.
            let (name, peer) = (copy.name, copy.peer);
            let holder = standing::keeping_device_elsewhere(&peer, &cert.device_public_key)
                .map_or(cert.device_public_key, |(device, _)| device);
            let reading = standing::reading(workspace, &peer, &reader, Some(holder), Vec::new());
            listed.push(Listed {
                id: workspace.to_string(),
                name,
                labels: Vec::new(),
                own: reading.owner.as_deref() == Some(hex::encode(cert.user_public_key).as_str()),
                host: standing::host_elsewhere(&peer, &cert.device_public_key)
                    .and_then(|host| host.written()),
            });
            match serde_json::to_vec(&reading) {
                Ok(bytes) => {
                    readings.insert(resource(&workspace.to_string()), bytes);
                }
                Err(error) => {
                    tracing::error!(%workspace, %error, "a workspace's standing does not encode");
                }
            }
            for part in Part::ALL {
                match standing::part(&peer, cert.device_public_key, part) {
                    Ok(bytes) => {
                        readings.insert(part_resource(&workspace.to_string(), part), bytes);
                    }
                    Err(error) => {
                        tracing::error!(%workspace, ?part, %error, "a workspace's part does not encode");
                    }
                }
            }
        }
        match serde_json::to_vec(&listed) {
            Ok(bytes) => {
                readings.insert(LIST.to_owned(), bytes);
            }
            Err(error) => tracing::error!(%error, "the workspace list does not encode"),
        }
        readings
    }
}

impl PublishedResources for Standing {
    fn snapshot(
        &self,
        _caller: &tilo_app_host::services::Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (answer, answered) = oneshot::channel();
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = answer.send(Ok((state.revision, state.readings.get(resource).cloned())));
        answered
    }

    fn watch(
        &self,
        _caller: &tilo_app_host::services::Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<Event>,
    ) -> Result<ResourceWatch, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let id = state.next_watch;
        state.next_watch = id.wrapping_add(1);
        let _ = sink.send(Event::Reading {
            resource: resource.to_owned(),
            revision: state.revision,
            reading: state.readings.get(resource).cloned(),
        });
        state.watches.insert(id, (resource.to_owned(), sink));
        drop(state);
        Ok(ResourceWatch::new(id))
    }

    fn unwatch(&self, watch: ResourceWatch) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .watches
            .remove(&watch.id());
    }

    fn invoke(
        &self,
        _caller: &tilo_app_host::services::Caller,
        resource: &str,
        action: &str,
        _arguments: Vec<u8>,
    ) -> oneshot::Receiver<Receipt> {
        let (answer, answered) = oneshot::channel();
        let _ = answer.send(Receipt::Refused {
            code: "unknown_action".to_owned(),
            message: format!(
                "{resource} is only read; {action} is one of the daemon's account actions"
            ),
        });
        answered
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
