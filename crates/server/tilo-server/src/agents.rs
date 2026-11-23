//! Agents on this host (feature `agents`): the link between the server and
//! `tilo-agent-session`'s [`AgentService`].
//!
//! [`ServerAgentLink`] is what the supervisor asks of the server:
//!
//! - **Approval.** A new grant is a certification request for a fresh agent
//!   key, queued by the host itself with the person's [`DelegateProposal`]. A
//!   root-holding approver answers it with the agent's certificate and the
//!   delegation together (`ApproverDecision::IssueDelegate`), so consent is one
//!   step and the host never holds the root.
//! - **Admission.** Approved delegations are registered in the server's agent
//!   register, which confines the device to its workspaces; withdrawn ones are
//!   refused from the next access audit on. Each workspace this server governs
//!   that the delegation names puts the agent on its governed roster
//!   (`WorkspaceOperation::DelegateMembership`), and every change to its grant is
//!   carried onto those rosters: a withdrawn or narrowed grant takes it off
//!   the workspace and its documents.
//! - **Asking first.** An action the person's grants leave to them is kept as
//!   its asked invocation record, and runs on the person's approve of it.
//!
//! [`start`] wires both into a server: the supervisor answers the agent
//! vocabulary as the host's app action server, and the host's own agents
//! reach it over iroh.

use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::{ActionCallError, Caller, CatalogueResource, CatalogueRow, Pending};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ExecutionLocus, InputSpec,
    OperationKind, RiskLevel,
};
use tilo_server_protocol::page::{
    COPY_INVITE_LINK, DEVICES_AND_ACCESS, PAGE_ACTIONS_PROVIDER, WAITING_TO_JOIN,
};

mod roster;

/// Run `future` to its answer on `runtime`. Called from one of the runtime's own workers, the
/// worker steps aside first: a worker that blocks on its own runtime would stop the tasks it
/// drives, and the runtime refuses to be entered twice.
fn settled<F: std::future::Future>(runtime: &tokio::runtime::Handle, future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| runtime.block_on(future))
    } else {
        runtime.block_on(future)
    }
}
use roster::RosterProvisioning;
use tilo_agent_session::workspace_actions::{
    HostDevice, Opened, Opening, PackagedCall, Recorder, ShellRan, Watching, WorkspaceShell,
};
use tilo_agent_session::{
    AgentHostLink, AgentService, AgentSupervisor, AppCatalogue, ApprovalDone, CatalogueFactory,
    FeedbackReviewer, ServerEndpoint, WorkspaceActions, WorkspaceDocumentsLink,
};

use tilo_server_protocol::delegation::DelegateProposal;
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationRequest, DeviceCapabilities, DeviceCert,
};

use crate::WorkspaceId;
use crate::certification::CertificationProtocol;
use crate::daemon::{Server, ServerError, ServerIdentity};
use crate::peer::AgentRegistry;
use tilo_content::WorkspaceStores;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

/// How long an agent's approval request waits for an approver.
const APPROVAL_WAIT_NS: u64 = 10 * 60 * 1_000_000_000;

/// How long a short invite link made ahead of a press is handed over for: an
/// invitation is good for a week, and one made longer ago than this is made
/// again rather than handed on half spent.
const READY_LINK_FOR_NS: u64 = 30 * 60 * 1_000_000_000;

/// A short invite link made before it was asked for, and when: copying it is
/// then the press itself, which is what lets a browser take the write.
#[derive(Clone)]
struct ReadyLink {
    link: String,
    made_at_ns: u64,
}

/// The server as an agent supervisor sees it.
#[derive(Clone)]
pub struct ServerAgentLink {
    certification: CertificationProtocol,
    agents: AgentRegistry,
    /// The workspaces this server governs, whose rosters follow each agent's grant.
    workspaces: Option<crate::workspaces::Workspaces>,
    runtime: tokio::runtime::Handle,
}

impl Server {
    /// The link an [`AgentService`] supervises this server's agents through.
    /// Call it inside the runtime the server runs on.
    #[must_use]
    pub fn agent_link(&self) -> ServerAgentLink {
        let web = self.web_context();
        ServerAgentLink {
            certification: web.certification.clone(),
            agents: web.server.agents.clone(),
            workspaces: self.workspaces().cloned(),
            runtime: tokio::runtime::Handle::current(),
        }
    }
}

impl ServerAgentLink {
    /// Resolve an existing roster entry to the exact authenticated signing session.
    pub async fn roster_session_device(
        &self,
        sessions: &tilo_agent_session::AgentSessions,
        workspace: WorkspaceId,
        agent: &str,
        acts_for: &[u8; 32],
    ) -> Option<[u8; 32]> {
        let workspaces = self.workspaces.as_ref()?;
        let entry = workspaces
            .hosted_roster_agent(workspace, agent, acts_for, None)
            .await?;
        let device = self
            .approved_session_device(sessions, workspace, agent, acts_for)
            .await?;
        sessions
            .matches_roster(&device, workspace, agent, acts_for, entry.created_at)
            .then_some(device)
    }

    /// Resolve a governed delegation to the authenticated session serving its workspace.
    pub async fn approved_session_device(
        &self,
        sessions: &tilo_agent_session::AgentSessions,
        workspace: WorkspaceId,
        agent: &str,
        acts_for: &[u8; 32],
    ) -> Option<[u8; 32]> {
        let device = self
            .workspaces
            .as_ref()?
            .approved_agent_device(workspace, agent, acts_for)
            .await?;
        let link = sessions.link(&device)?;
        let status = link.status();
        (status.authenticated
            && status.phase == tilo_session_client::TransportPhase::Connected
            && status.device == device
            && status.workspaces.get(&workspace) == Some(&true)
            && status.serving.contains(&workspace))
        .then_some(device)
    }

    fn enqueue_approval(
        &self,
        acts_for: [u8; 32],
        key: &SigningKey,
        proposal: &DelegateProposal,
    ) -> Result<([u8; 32], String), String> {
        let now = tilo_clock::unix_now_ns();
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
        let request = CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: acts_for,
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("Agent: {}", proposal.label),
            requested: DeviceCapabilities::nothing(),
            nonce,
            invite: [0; 16],
            issued_at_ns: now,
            expires_at_ns: now.saturating_add(APPROVAL_WAIT_NS),
            signature: [0; 64],
        }
        .sign(key)
        .map_err(|error| error.to_string())?;
        self.certification
            .enqueue_delegate(request, proposal.clone(), now)
    }

    fn withdraw_device(&self, device: [u8; 32]) {
        self.agents.write_recover().insert(device, None);
        crate::authority_epoch::bump();
        self.reconcile_workspaces(device, None);
    }

    /// Bring agent `device`'s place on the rosters of the governed workspaces in
    /// line with the register, off the caller's thread. `cert` is the
    /// agent's certificate when it was just approved or restored.
    fn reconcile_workspaces(&self, device: [u8; 32], cert: Option<DeviceCert>) {
        if let Some(workspaces) = self.workspaces.clone() {
            self.runtime.spawn(async move {
                workspaces.reconcile_agent(device, cert.as_ref()).await;
            });
        }
    }
}

impl tilo_workspace_service::agents::SessionDevices for ServerAgentLink {
    fn session_device<'a>(
        &'a self,
        sessions: &'a tilo_agent_session::AgentSessions,
        workspace: WorkspaceId,
        agent: &'a str,
        person: &'a [u8; 32],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<[u8; 32]>> + Send + 'a>> {
        Box::pin(self.roster_session_device(sessions, workspace, agent, person))
    }
}

impl AgentHostLink for ServerAgentLink {
    fn request_roster_approval(
        &self,
        key: &SigningKey,
        proposal: &DelegateProposal,
        created_at: i64,
        requester: &DeviceCert,
        done: ApprovalDone,
    ) -> Result<String, String> {
        self.approve_roster(key, proposal, created_at, requester, done)
    }

    fn admit_roster_session(
        &self,
        identity: &tilo_agent_session::AgentIdentity,
        created_at: i64,
        lifetime: tokio::sync::oneshot::Receiver<()>,
        done: tilo_agent_session::grants::RosterAdmissionDone,
    ) {
        self.join_roster(identity, created_at, lifetime, done);
    }

    fn withdraw(&self, device_public_key: [u8; 32]) {
        self.withdraw_device(device_public_key);
    }
}

/// What a host serving its workspaces' documents runs with.
pub struct AgentOptions {
    /// The roster agents' signing grants are kept here.
    pub state_dir: PathBuf,
    /// The apps agents may open, and whose records pages may act on through
    /// the host; this machine's registry when `None`.
    pub catalogue: Option<CatalogueFactory>,
    /// The model the host lends its workspaces' reporters to look over their
    /// reports' reviews, if any.
    pub feedback_reviewer: Option<Arc<dyn FeedbackReviewer>>,
    /// Where the workspaces' Terminal sessions run their commands on this
    /// host ([`host_shells`]); `None` leaves them to the devices running
    /// them.
    pub shell: Option<Arc<HostShells>>,
    /// The Browser this host places for its workspaces
    /// ([`crate::hosted_browser::HostedBrowsers`]); `None` places none, and
    /// opening Browser in a hosted workspace is refused in words that say so.
    pub browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>,
}

/// The shells of the workspaces this host serves.
///
/// A host that pays for its workspaces' Linux machines without running them
/// ([`LinuxDevices::from_env`](crate::linux_devices::LinuxDevices::from_env))
/// runs a workspace's shell on the device of the workspace that takes its
/// Linux turn. Otherwise it runs here, its files under `state`, its Linux programs in the hosted VMs the
/// host's environment names (`TILO_HOSTED_VM_SOCKET`, `TILO_HOSTED_ACCOUNT`,
/// `TILO_HOSTED_VM_SIZE`, `TILO_HOSTED_PLANS`), and without those, ush's own
/// commands only.
///
/// # Errors
///
/// The host's shells cannot be set up.
pub fn host_shells(state: PathBuf) -> Result<Arc<HostShells>, String> {
    if let Some(linux) = crate::linux_devices::LinuxDevices::from_env() {
        return Ok(Arc::new(HostShells::at(ShellsAt::Devices(Arc::new(linux)))));
    }
    let agents = Arc::new(crate::machine_agents::MachineAgentsServer::new(Some(
        state.clone(),
    )));
    let shells = ush::workspace_shell::WorkspaceShells::from_env(state)
        .serving_agents(Arc::clone(&agents) as Arc<dyn ush::hosted_vm::MachineAgents>);
    let terminals = ush::environment_terminals::EnvironmentTerminals::new(Arc::new(shells));
    Ok(Arc::new(HostShells::at(ShellsAt::Here(terminals, agents))))
}

/// What a host lends its workspaces' records service: a model that looks
/// over feedback reviews, where Terminal sessions run their commands, and the
/// Browser it places.
type Lent = (
    Option<Arc<dyn FeedbackReviewer>>,
    Option<Arc<HostShells>>,
    Option<Arc<crate::hosted_browser::HostedBrowsers>>,
);

/// The shells whose terminal streams a host serves on
/// [`ALPN`](ush::environment_terminals::ALPN), once they run.
///
/// The person's device asks the session's `open-terminal`; the host's shells start the
/// session's shell on a terminal and hand its stream to that device, which dials this host
/// as itself and presents the stream's one-use id: a stream handed to another device is
/// refused. Shells that run here carry the stream themselves; a shell on another device of
/// the workspace is spliced to it through this host's own device.
#[derive(Clone, Default)]
pub struct TerminalShells(Arc<std::sync::OnceLock<Arc<HostShells>>>);

impl TerminalShells {
    /// Serve the terminal streams `shells` hand out.
    pub fn serve_for(&self, shells: Arc<HostShells>) {
        let _ = self.0.set(shells);
    }

    /// What answers [`ALPN`](ush::environment_terminals::ALPN) on the host's endpoint.
    #[must_use]
    pub fn route(&self) -> ush::environment_terminals::quic::Route {
        use ush::environment_terminals::wire;
        let shells = Arc::clone(&self.0);
        ush::environment_terminals::quic::Route::new(move |device, from, to| {
            let served = match shells.get().map(|shells| shells.shells()) {
                Some(ShellsAt::Here(terminals, _)) => {
                    terminals.serve_handed(device, from, to).map(drop)
                }
                Some(ShellsAt::Devices(linux)) => linux.splice(device, from, to),
                None => Err(wire::refuse(to, "this host runs no workspace shells")),
            };
            if let Err(error) = served {
                tracing::debug!(%error, "a terminal stream ended in error");
            }
        })
    }
}

/// Where the workspace actions run a session's commands and open its terminal, and the
/// secrets the workspaces' machines hand the programs started there.
pub struct HostShells {
    at: ShellsAt,
    /// The workspaces whose machine secrets the programs get, and the runtime they are read
    /// on: set once the server serves ([`Self::serve_from`]). Until then no program gets any.
    secrets: std::sync::OnceLock<(crate::workspaces::Workspaces, tokio::runtime::Handle)>,
}

/// Where a host's shells are.
pub enum ShellsAt {
    /// Here: ush's workspace shells, in this process, with their terminals, and who serves
    /// the agents their programs in the workspaces' machines start.
    Here(
        ush::environment_terminals::EnvironmentTerminals,
        Arc<crate::machine_agents::MachineAgentsServer>,
    ),
    /// On the device of the workspace that takes its Linux turn.
    Devices(Arc<crate::linux_devices::LinuxDevices>),
}

/// How long reading a workspace's machine secrets may take before the start is refused.
const SECRETS_PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

impl HostShells {
    /// The shells `at`, handing no secrets until they serve a server's workspaces.
    #[must_use]
    pub fn at(at: ShellsAt) -> Self {
        Self {
            at,
            secrets: std::sync::OnceLock::new(),
        }
    }

    /// Where they are.
    #[must_use]
    pub const fn shells(&self) -> &ShellsAt {
        &self.at
    }

    /// Who serves the agents the programs in the workspaces' machines start, where this host
    /// runs its shells itself.
    #[must_use]
    pub fn machine_agents(&self) -> Option<&Arc<crate::machine_agents::MachineAgentsServer>> {
        match &self.at {
            ShellsAt::Here(_, agents) => Some(agents),
            ShellsAt::Devices(_) => None,
        }
    }

    /// Ask the workspaces' devices that run Linux, if the shells run there, as
    /// `server`'s own device, have the workspaces' machines branched and
    /// deleted with them, and serve the
    /// terminal streams handed out on `server`'s endpoint. The programs started from now on
    /// get the machine secrets `server`'s workspaces name.
    fn serve_from(self: &Arc<Self>, server: &Server, actions: &WorkspaceActions) {
        server.terminal_shells().serve_for(Arc::clone(self));
        if let (Some(workspaces), Ok(runtime)) =
            (server.workspaces(), tokio::runtime::Handle::try_current())
        {
            let _ = self.secrets.set((workspaces.clone(), runtime));
        }
        if let (ShellsAt::Here(_, agents), Some(workspaces), Ok(runtime)) = (
            &self.at,
            server.workspaces(),
            tokio::runtime::Handle::try_current(),
        ) {
            agents.bind(
                workspaces.clone(),
                actions.clone(),
                server.confirmations(),
                runtime,
            );
        }
        if let (ShellsAt::Devices(linux), Some(workspaces), Ok(runtime)) = (
            &self.at,
            server.workspaces(),
            tokio::runtime::Handle::try_current(),
        ) {
            linux.ask_from(
                workspaces.action_service(),
                server.endpoint(),
                workspaces.clone(),
                runtime,
            );
        }
        if let (Some(machines), Some(workspaces)) = (self.machines(), server.workspaces()) {
            workspaces.machines(machines);
        }
    }

    /// The secrets a program `by` starts in `workspace`'s machine gets, read once, now
    /// ([`crate::workspaces::Workspaces::machine_secrets`]).
    ///
    /// # Errors
    ///
    /// Why the program may not start with them: it is not started without them.
    pub fn secrets_for(
        &self,
        workspace: &str,
        by: &Caller,
    ) -> Result<Vec<tilo_actions::remote::environment_commands::VmSecret>, String> {
        let Some((workspaces, runtime)) = self.secrets.get() else {
            return Ok(Vec::new());
        };
        // Made on the runtime it is read on, whose clock its patience is kept by.
        let reading = async {
            tokio::time::timeout(SECRETS_PATIENCE, workspaces.machine_secrets(workspace, by)).await
        };
        // Waited for here: a command or a terminal starts on a thread of its own, and a coding
        // agent's program may start on one of the runtime's.
        let read = if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| runtime.block_on(reading))
        } else {
            runtime.block_on(reading)
        };
        read.map_err(|_| "the workspace's machine secrets could not be read in time".to_owned())?
    }

    /// What branches and deletes the workspaces' Linux machines: the devices
    /// keeping them, or the hosted-VM service a host running its shells here names;
    /// `None` when its shells run no Linux programs, so there are no machines.
    fn machines(&self) -> Option<Arc<dyn crate::workspaces::WorkspaceMachines>> {
        match &self.at {
            ShellsAt::Devices(linux) => {
                Some(Arc::clone(linux) as Arc<dyn crate::workspaces::WorkspaceMachines>)
            }
            ShellsAt::Here(terminals, _) => terminals.shells().hosted().map(|target| {
                Arc::new(crate::linux_devices::LocalMachines::new(target.clone()))
                    as Arc<dyn crate::workspaces::WorkspaceMachines>
            }),
        }
    }
}

impl WorkspaceShell for HostShells {
    fn run(
        &self,
        workspace: &str,
        session: &str,
        command: &str,
        by: &Caller,
        agent: bool,
        person: Option<&tilo_actions::remote::environment_commands::Person>,
        watching: Watching<'_>,
    ) -> Result<ShellRan, String> {
        let secrets = self.secrets_for(workspace, by)?;
        let terminals = match &self.at {
            ShellsAt::Here(terminals, _) => terminals,
            ShellsAt::Devices(linux) => {
                let asked = tilo_actions::remote::environment_commands::CommandRequest {
                    session: session.to_owned(),
                    command: command.to_owned(),
                    agent,
                    // The account and size are the ones this host pays for.
                    account: String::new(),
                    size: String::new(),
                    secrets,
                    person: person.cloned(),
                };
                return linux.run_command(workspace, asked, watching);
            }
        };
        let ran = terminals
            .shells()
            .run_for(
                workspace,
                session,
                command,
                agent,
                ush::workspace_shell::RunWith {
                    person,
                    secrets: &secrets,
                    stopping: watching.stop,
                    seen: watching.seen,
                    ..ush::workspace_shell::RunWith::default()
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(ShellRan {
            command: ran.command,
            cwd: ran.cwd,
            exit: ran.exit,
            output: ran.output,
            truncated: ran.truncated,
            cancelled: ran.cancelled,
            started_ms: ran.started_ms,
            ended_ms: ran.ended_ms,
            refusal: ran.refusal,
        })
    }

    fn started(&self, workspace: &str, session: &str, caller: &Caller) {
        if let ShellsAt::Here(_, agents) = &self.at {
            agents.started(workspace, session, caller);
        }
    }

    fn open(&self, opening: &Opening, by: &Caller, record: Recorder) -> Result<Opened, String> {
        // Read for a new session's shell, which every program it starts is given; coming back
        // to a kept one starts nothing.
        let secrets = match opening.attach {
            None => self.secrets_for(&opening.workspace, by)?,
            Some(_) => Vec::new(),
        };
        let terminals = match &self.at {
            ShellsAt::Here(terminals, _) => terminals,
            ShellsAt::Devices(linux) => return linux.open_terminal(opening, secrets, record),
        };
        let opened = terminals.open_for(
            opening.device,
            &ush::environment_terminals::TerminalAsk {
                workspace: &opening.workspace,
                session: &opening.session,
                payer: None,
                person: opening.person.as_ref(),
                cols: opening.cols,
                rows: opening.rows,
                ran: Some(Arc::new(move |ran| {
                    record(crate::linux_devices::terminal_ran(ran));
                })),
                attach: opening.attach.as_deref(),
                secrets: &secrets,
            },
        )?;
        Ok(Opened {
            stream: opened.stream,
            session: opened.session,
        })
    }
}

/// The workspace action server, with the roster agents' grants where the host keeps them.
struct WorkspaceActionsServer {
    supervisor: Option<Arc<AgentSupervisor>>,
    roster: Option<RosterProvisioning>,
    actions: WorkspaceActions,
    pages: ActionPages,
    stores: Arc<WorkspaceStores>,
    /// The Browser this host places for its workspaces, when it places one.
    browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>,
    /// The events this host runs for its workspaces, once they are up.
    events: Arc<std::sync::OnceLock<tilo_workspace_service::agents::events::WorkspaceEvents>>,
    /// What its workspaces' assistants find to do on their own, once they are up.
    initiative: Arc<std::sync::OnceLock<tilo_workspace_service::agents::initiative::Initiative>>,
}

/// The workspaces' apps as web pages, each held by the action service as the
/// member who opened it ([`WorkspaceActions::open_page`]).
struct ActionPages {
    actions: WorkspaceActions,
}

impl ActionPages {
    fn new(actions: WorkspaceActions) -> Self {
        Self { actions }
    }
}

impl crate::app_serving::AppPages for ActionPages {
    fn open(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        address: tilo_server_protocol::page::PageAddress,
    ) -> Pending<
        Result<tilo_server_protocol::page::OpenedPage, tilo_server_protocol::page::PageRefusal>,
    > {
        self.actions.open_page(workspace_id, caller, address)
    }

    fn act(
        &self,
        workspace_id: WorkspaceId,
        page: u64,
        act: Option<(u32, Option<String>, tilo_server_protocol::page::PageAction)>,
    ) -> Pending<
        Result<Vec<tilo_server_protocol::page::PagePatch>, tilo_server_protocol::page::PageRefusal>,
    > {
        self.actions.act_on_page(workspace_id, page, act)
    }

    fn close(&self, workspace_id: WorkspaceId, page: u64) {
        self.actions.close_page(workspace_id, page);
    }
}

impl crate::AppActionServer for WorkspaceActionsServer {
    fn serve(
        &self,
        workspace_id: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        // An agent goes by its session's name in all a call makes (its Chat byline, its History
        // rows, its questions), as its window's mark names it.
        let caller = &caller.clone().in_named_session(&message);
        // Without a device behind the call — a page, or a turn this server
        // carries out for a member — the workspace's policy decides it for
        // that member as themselves.
        if let Some(answer) = self.serve_roster(workspace_id, caller, message.clone(), None) {
            return Some(answer);
        }
        let observe: Arc<crate::hosted_browser::ObserveInvocation> = {
            let actions = self.actions.clone();
            Arc::new(move |caller, invocation| actions.observe(caller, invocation))
        };
        serve_with_browser(
            self.browsers.as_ref(),
            workspace_id,
            caller,
            message,
            |message| self.actions.serve(workspace_id, caller, message),
            Some(&observe),
        )
    }

    fn serve_certified(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        certificate: &DeviceCert,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<Pending<tilo_actions::remote::RemoteActionMessage>> {
        // The workspace's events and its assistant's initiative answer for the
        // device the handshake verified.
        let named = caller.clone().in_named_session(&message);
        if let Some(answer) =
            self.serve_roster(workspace, &named, message.clone(), Some(certificate))
        {
            return Some(answer);
        }
        if let (Some(roster), Some(supervisor)) = (&self.roster, &self.supervisor)
            && let Some(answer) = roster.serve(workspace, caller, certificate, &message, supervisor)
        {
            return Some(answer);
        }
        self.serve(workspace, caller, message)
    }

    fn route(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: &tilo_actions::remote::RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        self.browsers.as_ref()?.route(workspace_id, caller, message)
    }

    fn route_certified(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: &tilo_user_identity::DeviceCert,
        message: &tilo_actions::remote::RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        if self
            .roster
            .as_ref()
            .is_some_and(|roster| roster.is_signer(caller))
        {
            return None;
        }
        self.browsers
            .as_ref()?
            .route_certified(workspace_id, caller, certificate, message)
    }

    fn relayed(
        &self,
        workspace_id: WorkspaceId,
        from: [u8; 32],
        message: &tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::RemoteActionMessage> {
        self.browsers.as_ref()?.relayed(workspace_id, from, message)
    }

    fn tracks(&self, workspace_id: WorkspaceId) -> bool {
        self.browsers
            .as_ref()
            .is_some_and(|browsers| browsers.tracks(workspace_id))
    }

    fn answers_for_host(&self, workspace_id: WorkspaceId, from: [u8; 32], to: [u8; 32]) -> bool {
        self.browsers
            .as_ref()
            .is_some_and(|browsers| browsers.answers_for_host(workspace_id, from, to))
    }

    fn content(&self) -> Option<Arc<WorkspaceStores>> {
        Some(Arc::clone(&self.stores))
    }

    fn forget_workspace(&self, workspace_id: WorkspaceId) {
        self.actions.forget_workspace(workspace_id);
    }

    fn session_arrived(&self, workspace_id: WorkspaceId, caller: &tilo_actions::remote::Caller) {
        self.actions.session_arrived(workspace_id, caller);
    }

    fn pages(&self) -> Option<&dyn crate::app_serving::AppPages> {
        Some(&self.pages)
    }
}

/// Serve a workspace's message, placing its Browser after an answered
/// `app.so.tilo.browser#open`: the open answers once the window is live, and
/// a Browser this host cannot place is refused in words that say so rather
/// than as an app it does not know.
fn serve_with_browser(
    browsers: Option<&Arc<crate::hosted_browser::HostedBrowsers>>,
    workspace_id: WorkspaceId,
    caller: &tilo_actions::remote::Caller,
    message: tilo_actions::remote::RemoteActionMessage,
    serve: impl FnOnce(
        tilo_actions::remote::RemoteActionMessage,
    ) -> Option<Pending<tilo_actions::remote::RemoteActionMessage>>,
    observe: Option<&Arc<crate::hosted_browser::ObserveInvocation>>,
) -> Option<Pending<tilo_actions::remote::RemoteActionMessage>> {
    use tilo_actions::remote::RemoteActionMessage;
    // A Linux app opens in the workspace's VM the way Browser does: the open
    // makes its record, and the app is started in the workspace's apps
    // session on it.
    if let RemoteActionMessage::Invoke(request) = &message
        && let Some(launch) = crate::hosted_browser::opens_linux_app(request)
    {
        let invocation = request.invocation;
        let launch = match (launch, browsers) {
            (Ok(launch), Some(_)) => launch,
            (Err(reason), _) => {
                return Some(Pending::ready(crate::hosted_browser::refused(
                    invocation, reason,
                )));
            }
            (Ok(_), None) => {
                return Some(Pending::ready(crate::hosted_browser::refused(
                    invocation,
                    crate::hosted_browser::unplaced_linux_apps(),
                )));
            }
        };
        let browsers = Arc::clone(browsers?);
        let caller = caller.clone();
        let observe = observe.cloned();
        let answer = serve(message)?;
        return Some(placed_when_ready(
            &browsers,
            invocation,
            answer,
            "The app couldn't start. Try again in a minute.".to_owned(),
            move |browsers: &Arc<crate::hosted_browser::HostedBrowsers>, message| {
                let browsers = Arc::clone(browsers);
                Box::pin(async move {
                    crate::hosted_browser::placed_linux_app(
                        &browsers,
                        workspace_id,
                        invocation,
                        &launch,
                        message,
                        observe,
                        &caller,
                    )
                    .await
                })
            },
        ));
    }
    let opens = match &message {
        RemoteActionMessage::Invoke(request) if crate::hosted_browser::opens_browser(request) => {
            Some(request.invocation)
        }
        _ => None,
    };
    let Some(invocation) = opens else {
        // A listing of what runs in the workspace names the Browser window
        // its VM holds beside what this host serves itself.
        if let (Some(browsers), RemoteActionMessage::CatalogueRequest { query, .. }) =
            (browsers, &message)
        {
            let (browsers, query) = (Arc::clone(browsers), query.clone());
            return Some(
                serve(message)?.map(move |answer| browsers.listed(workspace_id, &query, answer)),
            );
        }
        return serve(message);
    };
    let Some(browsers) = browsers else {
        return Some(Pending::ready(crate::hosted_browser::refused(
            invocation,
            crate::hosted_browser::unplaced(),
        )));
    };
    let browsers = Arc::clone(browsers);
    let caller = caller.clone();
    let observe = observe.cloned();
    let answer = serve(message)?;
    Some(placed_when_ready(
        &browsers,
        invocation,
        answer,
        "Browser is still starting. Try again in a minute.".to_owned(),
        move |browsers: &Arc<crate::hosted_browser::HostedBrowsers>, message| {
            let browsers = Arc::clone(browsers);
            Box::pin(async move {
                crate::hosted_browser::placed(
                    &browsers,
                    workspace_id,
                    invocation,
                    message,
                    observe,
                    &caller,
                )
                .await
            })
        },
    ))
}

/// `answer`, an open's answer, settled with the window `place` gives it: an
/// open answers with its window only once the host names the workspace and its
/// VM's session is up, both of which wait, so the answer is settled from a
/// task of its own rather than on whichever thread produced it. `still_starting`
/// is what a host that places nothing yet refuses it with.
/// An open's answer, once the window it asks for is up: what
/// [`placed_when_ready`]'s placement answers when it runs.
type Placed = std::pin::Pin<
    Box<dyn std::future::Future<Output = tilo_actions::remote::RemoteActionMessage> + Send>,
>;

fn placed_when_ready(
    browsers: &Arc<crate::hosted_browser::HostedBrowsers>,
    invocation: tilo_actions::InvocationId,
    answer: Pending<tilo_actions::remote::RemoteActionMessage>,
    still_starting: String,
    place: impl FnOnce(
        &Arc<crate::hosted_browser::HostedBrowsers>,
        tilo_actions::remote::RemoteActionMessage,
    ) -> Placed
    + Send
    + 'static,
) -> Pending<tilo_actions::remote::RemoteActionMessage> {
    let Some(runtime) = browsers.runtime() else {
        return Pending::ready(crate::hosted_browser::refused(invocation, still_starting));
    };
    let browsers = Arc::clone(browsers);
    let (completer, pending) = tilo_actions::remote::pending();
    runtime.spawn(async move {
        // A caller that stopped waiting takes its open with it: dropping the
        // answer is what tells the service behind it.
        if completer.is_abandoned() {
            return;
        }
        let Some(message) = answer.await else {
            return;
        };
        if completer.is_abandoned() {
            return;
        }
        completer.complete(place(&browsers, message).await);
    });
    pending
}

/// A page action this host runs itself, as its page's search lists it: not a
/// record's and not the workspace's governance, but the host's own business a
/// person does here in one press.
fn page_row(
    workspace: WorkspaceId,
    action: &str,
    kind: ActionKind,
    label: &str,
    resource_label: &str,
    description: &str,
) -> CatalogueRow {
    let resource = format!("page:{workspace}");
    CatalogueRow {
        offered: None,
        provider: PAGE_ACTIONS_PROVIDER.to_owned(),
        app_id: PAGE_ACTIONS_PROVIDER.to_owned(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.clone(),
            kind: "page".to_owned(),
            label: resource_label.to_owned(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: "page".to_owned(),
            resource_label: resource_label.to_owned(),
            action_label: label.to_owned(),
            keywords: Vec::new(),
            icon: None,
            description: Some(description.to_owned()),
            locus: ExecutionLocus::ServerPreferred,
            kind: OperationKind::Effect,
            contract: ActionContract {
                kind,
                risk: RiskLevel::None,
                ..ActionContract::default()
            },
            input: InputSpec::None,
            params: Vec::new(),
            view: None,
        }),
        has_children: false,
        visibility: None,
        surface: None,
    }
}

/// The server's own device, as the workspace actions service joins its workspaces.
struct ServerRecordsLink {
    user_public_key: [u8; 32],
    host_device: HostDevice,
    /// The runtime the records' work waits on: the same one its device's
    /// requests answer from.
    runtime: tokio::runtime::Handle,
    workspaces: Option<crate::workspaces::Workspaces>,
    browser_origin: Arc<std::sync::RwLock<Option<Arc<String>>>>,
    web_entry: Option<String>,
    feedback_reviewer: Option<Arc<dyn FeedbackReviewer>>,
    shell: Option<Arc<dyn WorkspaceShell>>,
    stores: Arc<WorkspaceStores>,
    /// The links to the workspaces' files the host's web origin serves.
    content_links: Arc<crate::content_links::ContentLinks>,
    sessions: Option<tilo_agent_session::AgentSessions>,
    /// The short invite links made ahead of the press that hands one over,
    /// per workspace and person: a page carries one on its row, and the press
    /// copies it before the host answers with the next.
    ready_links: std::sync::Mutex<std::collections::HashMap<(WorkspaceId, [u8; 32]), ReadyLink>>,
}

impl ServerRecordsLink {
    /// The host an actor's address in `workspace` is at: the origin this host publishes its pages
    /// at, else the name its device gives itself, when either names a network; else the
    /// workspace's own (`team.tilo`). A machine's loopback names nobody's work.
    fn address_host(&self, workspaces: &crate::workspaces::Workspaces, workspace: &str) -> String {
        if let Some(origin) = self
            .browser_origin
            .read()
            .ok()
            .and_then(|held| held.as_ref().cloned())
            && names_a_network(&origin)
        {
            return (*origin).clone();
        }
        let label = workspaces.serving_certificate().device_label.trim();
        if names_a_network(label) {
            return label.to_owned();
        }
        let name = self
            .host_device
            .runtime
            .block_on(workspaces.summary(workspace))
            .map(|summary| summary.name)
            .unwrap_or_default();
        format!("{}.tilo", address_slug(&name))
    }

    /// The actions this host offers a page of the workspace `workspace`, of
    /// its own business beside the workspace's governance, whose words hold
    /// `needle`: what its person may do there. A host without the workspaces
    /// offers none.
    fn page_rows(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        needle: &str,
    ) -> Vec<CatalogueRow> {
        let Some(workspaces) = self.workspaces.as_ref() else {
            return Vec::new();
        };
        let wants = |title: &str| {
            let title = title.to_lowercase();
            needle
                .split_whitespace()
                .all(|word| title.contains(&word.to_lowercase()))
        };
        let mut offered: Vec<(&str, ActionKind, &str, &str, &str)> = Vec::new();
        // Inviting is the workspace's to allow, which only the host can say:
        // it is asked for a word that could name one of the two, and never
        // for the words of a search that cannot.
        let inviting = wants("Copy an invite link") || wants("Waiting to join");
        if inviting
            && self
                .host_device
                .runtime
                .block_on(workspaces.page_may_invite(workspace, caller))
        {
            for (action, kind, label, resource, description) in [
                (
                    COPY_INVITE_LINK,
                    ActionKind::Outside,
                    "Copy an invite link",
                    "Invite",
                    "Anyone with it joins as an editor",
                ),
                (
                    WAITING_TO_JOIN,
                    ActionKind::Read,
                    "Waiting to join",
                    "Waiting",
                    "Invite people who left their address at the door",
                ),
            ] {
                if wants(label) {
                    offered.push((action, kind, label, resource, description));
                }
            }
        }
        if self.web_entry.is_some() {
            offered.push((
                DEVICES_AND_ACCESS,
                ActionKind::Read,
                "Devices and access",
                "Devices",
                "Set an agent or another computer up with this workspace",
            ));
        }
        offered
            .into_iter()
            .filter(|(_, _, label, ..)| wants(label))
            .map(|(action, kind, label, resource, description)| {
                page_row(workspace, action, kind, label, resource, description)
            })
            .collect()
    }

    /// Run one of the host's own page actions, `request` naming it, for
    /// `caller` on the page of the workspace `workspace`.
    fn run_page_action(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<serde_json::Value, ActionCallError> {
        let workspaces = self
            .workspaces
            .as_ref()
            .ok_or(ActionCallError::UnknownAction)?;
        match request.target.address.action.as_str() {
            COPY_INVITE_LINK => {
                let link = self
                    .host_device
                    .runtime
                    .block_on(workspaces.page_invite_link(workspace, caller))?;
                // The link the row carried was handed over: the next one is
                // hers until it is.
                if let Some(person) = caller.user_public_key() {
                    self.ready_links
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&(workspace, *person));
                }
                Ok(link)
            }
            WAITING_TO_JOIN => self
                .host_device
                .runtime
                .block_on(workspaces.page_waiting_to_join(workspace, caller)),
            // Setting another device up is the site's setup screen: it needs
            // the person's identity, which the site's page holds and this one
            // does not.
            DEVICES_AND_ACCESS => self
                .web_entry
                .as_ref()
                .map(|site| serde_json::json!({"devices": {"setup": site}}))
                .ok_or(ActionCallError::UnknownAction),
            _ => Err(ActionCallError::UnknownAction),
        }
    }
}

impl WorkspaceDocumentsLink for ServerRecordsLink {
    /// The workspaces' documents this server holds: what its workspace actions
    /// read their records from and write them through.
    fn documents(&self) -> Option<Arc<dyn crate::workspace_documents::DeviceDocuments>> {
        let workspaces = self.workspaces.clone()?;
        Some(Arc::new(
            tilo_workspace_service::device_documents::ServiceDocuments::of(workspaces),
        ))
    }

    fn writes(&self) -> Option<Arc<dyn crate::workspace_documents::DeviceWrites>> {
        let workspaces = self.workspaces.clone()?;
        Some(Arc::new(
            tilo_workspace_service::device_documents::ServiceWrites::of(workspaces),
        ))
    }

    fn workspace_agent(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
    ) -> Vec<tilo_action_policy::Rule> {
        let Some(workspaces) = self.workspaces.as_ref() else {
            return Vec::new();
        };
        self.host_device
            .runtime
            .block_on(tilo_workspace_service::agents::workspace_agent(
                workspaces, workspace, caller,
            ))
    }

    fn runs_unattended(&self, workspace: WorkspaceId, caller: &Caller) -> bool {
        self.workspaces.as_ref().is_some_and(|workspaces| {
            self.host_device
                .runtime
                .block_on(workspaces.runs_unattended(workspace, caller))
        })
    }

    fn workspace_content(&self, workspace: WorkspaceId) -> Option<tilo_content::ContentStore> {
        let identity = settled(
            &self.host_device.runtime,
            self.workspaces.as_ref()?.workspace_identity(workspace),
        )?;
        self.stores.get(&identity).ok()
    }

    /// The workspace's tree changed where this host keeps it, by the tree's own sync.
    fn change_tree(
        &self,
        workspace: WorkspaceId,
        acting: Option<tilo_actions::ActionContext>,
        change: &mut dyn FnMut(
            &mut tilo_fs::namespace::Namespace,
        ) -> Result<serde_json::Value, String>,
    ) -> Result<serde_json::Value, String> {
        let workspaces = self
            .workspaces
            .as_ref()
            .ok_or_else(|| "this host holds no workspaces".to_owned())?;
        settled(
            &self.host_device.runtime,
            workspaces.puts().change(workspace, acting, change),
        )
    }

    /// What the workspace's tree holds, read where this host keeps it.
    fn read_tree(
        &self,
        workspace: WorkspaceId,
        read: &mut dyn FnMut(
            &mut tilo_fs::namespace::Namespace,
        ) -> Result<serde_json::Value, String>,
    ) -> Result<serde_json::Value, String> {
        let workspaces = self
            .workspaces
            .as_ref()
            .ok_or_else(|| "this host holds no workspaces".to_owned())?;
        settled(
            &self.host_device.runtime,
            workspaces.puts().read(workspace, read),
        )
    }

    fn content_link(
        &self,
        _workspace: WorkspaceId,
        store: tilo_content::ContentStore,
        link: &tilo_actions::remote::ContentLinkAsk,
    ) -> Option<tilo_actions::remote::ContentLink> {
        Some(self.content_links.mint(store, link.clone()))
    }

    fn feedback_reviewer(&self) -> Option<Arc<dyn FeedbackReviewer>> {
        self.feedback_reviewer.clone()
    }

    fn shell(&self) -> Option<Arc<dyn WorkspaceShell>> {
        self.shell.clone()
    }

    fn user_public_key(&self) -> [u8; 32] {
        self.user_public_key
    }

    fn own_device(&self) -> HostDevice {
        self.host_device.clone()
    }

    fn admits(&self, workspace: WorkspaceId, caller: &tilo_actions::remote::Caller) -> bool {
        self.workspaces.as_ref().is_some_and(|workspaces| {
            self.host_device
                .runtime
                .block_on(workspaces.admits(workspace, caller))
        })
    }

    fn own_actions(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        needle: &str,
    ) -> Vec<tilo_actions::remote::CatalogueRow> {
        let mut rows = self
            .workspaces
            .as_ref()
            .map_or_else(Vec::new, |workspaces| {
                self.host_device
                    .runtime
                    .block_on(workspaces.page_actions(workspace, caller, needle))
                    .rows
            });
        rows.extend(self.page_rows(workspace, caller, needle));
        rows
    }

    fn run_own_action(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
        if request.target.provider == PAGE_ACTIONS_PROVIDER {
            return self.run_page_action(workspace, caller, request);
        }
        let workspaces = self
            .workspaces
            .as_ref()
            .ok_or(tilo_actions::remote::ActionCallError::UnknownAction)?;
        self.host_device
            .runtime
            .block_on(workspaces.page_invoke(workspace, caller, request))
    }

    fn ready_invite_link(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
    ) -> Option<String> {
        let workspaces = self.workspaces.as_ref()?;
        let person = caller.user_public_key()?;
        let now = tilo_clock::unix_now_ns();
        let key = (workspace, *person);
        if let Some(held) = self
            .ready_links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .filter(|held| now.saturating_sub(held.made_at_ns) < READY_LINK_FOR_NS)
        {
            return Some(held.link.clone());
        }
        let link = self
            .host_device
            .runtime
            .block_on(workspaces.page_invite_link(workspace, caller))
            .ok()?
            .get("link")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)?;
        self.ready_links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                key,
                ReadyLink {
                    link: link.clone(),
                    made_at_ns: now,
                },
            );
        Some(link)
    }

    fn view_read_policy(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        app: &str,
        record: &str,
    ) -> Option<tilo_agent_session::workspace_actions::ViewReadPolicy> {
        self.host_device.runtime.block_on(
            self.workspaces
                .as_ref()?
                .resource_view_policy(workspace, caller, app, record),
        )
    }

    fn workspace_policy(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
    ) -> Option<tilo_agent_session::workspace_actions::ViewReadPolicy> {
        self.host_device.runtime.block_on(
            self.workspaces
                .as_ref()?
                .workspace_policy(workspace, caller),
        )
    }

    fn roster_policy(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        inside: Option<(&str, &str)>,
    ) -> Option<tilo_agent_session::workspace_actions::ViewReadPolicy> {
        let (peer, delegation) = self.sessions.as_ref()?.live_roster(workspace, caller)?;
        self.runtime
            .block_on(self.workspaces.as_ref()?.roster_action_policy(
                workspace,
                caller,
                (&peer.engine().identity().device_cert, &delegation),
                inside,
            ))
    }

    fn revocations(&self) -> tilo_user_identity::RevocationList {
        self.workspaces
            .as_ref()
            .map(crate::workspaces::Workspaces::revocations)
            .unwrap_or_default()
    }

    fn key_revoked(&self, workspace: WorkspaceId, caller: &tilo_actions::remote::Caller) -> bool {
        let Some(workspaces) = self.workspaces.as_ref() else {
            return false;
        };
        self.runtime
            .block_on(workspaces.key_revoked(workspace, caller))
    }

    /// The formats and the zone the person `caller` is, or acts for, keeps on this server: what
    /// their page told it of Region & Formats and of the browser's zone. A person with only a
    /// browser has no device that could render their view, so the server renders it as they
    /// read it.
    fn region_of(&self, caller: &Caller) -> Option<tilo_agent_session::worker::PersonRegion> {
        let workspaces = self.workspaces.as_ref()?;
        let acting = caller.acting();
        let person = acting.person_id().or_else(|| acting.acted_for_person())?;
        let person: [u8; 32] = hex::decode(person).ok()?.try_into().ok()?;
        let (formats, zone) = self.runtime.block_on(workspaces.region(&person))?;
        Some(tilo_agent_session::worker::PersonRegion { formats, zone })
    }

    /// The workspace's members as its roster tells a person's page of them: its people, as its
    /// governance names them, and its agents, as their delegations name them and its list of
    /// them does. What a view this device renders for an agent names its rows' authors and
    /// writers by, so they read as they do on the person's own page.
    fn members(&self, workspace: &str) -> Vec<(tilo_actions::MemberId, String)> {
        let Some(workspaces) = self.workspaces.clone() else {
            return Vec::new();
        };
        self.own_device().runtime.block_on(async move {
            let mut named = Vec::new();
            if let Ok(summary) = workspaces.summary(workspace).await {
                named.extend(
                    summary
                        .members
                        .into_iter()
                        .filter(|member| !member.removed)
                        .map(|member| (member.id, member.name)),
                );
            }
            // A delegated agent is called what its delegation names it
            // ([`tilo_peer_state::workspace::GovernedAgent::label`]): the name the workspace's
            // roster shows it by, whether or not its app's list of agents names it too.
            if let Ok(agents) = workspaces.agents(workspace).await {
                named.extend(
                    agents
                        .into_iter()
                        .filter(|agent| agent.withdrawn_at_ns.is_none())
                        .map(|agent| (tilo_actions::MemberId::agent(agent.agent_id), agent.label)),
                );
            }
            let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
            if let Ok(Some(bytes)) = workspaces
                .resource_document(workspace, tilo_workspace_agent::OWNER, &list)
                .await
                && let Ok(mut doc) = automerge::AutoCommit::load(&bytes)
                && let Ok(agents) = tilo_workspace_service::agents::opened::<
                    tilo_workspace_agent::WorkspaceAgents,
                >(&mut doc)
            {
                named.extend(
                    agents
                        .agents
                        .iter()
                        .filter(|agent| agent.is_active())
                        .map(|agent| {
                            (
                                tilo_actions::MemberId::agent(agent.id.0.clone()),
                                agent.name.clone(),
                            )
                        }),
                );
            }
            named
        })
    }

    /// Who `caller` runs commands as in `workspace`: an agent by its own name, else the person it
    /// acts for, as the workspace's member list or their own profile names them, at the address
    /// the workspace is reached by (`eu1.tilo.sh`, the origin the host published for its Browser;
    /// the workspace's own `team.tilo` where the host names no site of its own). A commit made
    /// in the workspace's VM names whoever asked for it as the workspace does, never by a key.
    fn person(
        &self,
        workspace: &str,
        caller: &tilo_actions::remote::Caller,
    ) -> Option<tilo_actions::remote::environment_commands::Person> {
        let workspaces = self.workspaces.as_ref()?;
        let mut names = self.members(workspace);
        // A person's own name is theirs, kept in their profile: it names them where the
        // workspace's member list has no row for them or keeps one with no word in it.
        let acting = caller.acting();
        if let Some(person) = acting
            .person_id()
            .filter(|id| !id.is_empty())
            .or_else(|| acting.acted_for_person())
            && !names
                .iter()
                .any(|(member, name)| member.person_id() == Some(person) && !name.trim().is_empty())
            && let Some(root) = hex::decode(person)
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            && let Some(name) = workspaces.person_profile_name(&root)
            && let Some(member) = tilo_actions::MemberId::parsed_person(person)
        {
            names.push((member, name));
        }
        let host = self.address_host(workspaces, workspace);
        tilo_agent_session::workspace_actions::person_of(caller, &names, &host)
    }

    fn view_document(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        source: &tilo_peer_state::document_frame::DocumentRoute,
    ) -> Result<tilo_agent_session::workspace_actions::ViewDocument, String> {
        self.host_device.runtime.block_on(
            self.workspaces
                .as_ref()
                .ok_or("governed workspaces unavailable")?
                .resource_view_document(workspace, caller, source),
        )
    }

    fn serving_keys(
        &self,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        create: bool,
    ) -> Option<Result<key_group::ContentKeys, String>> {
        settled(
            &self.host_device.runtime,
            self.workspaces.as_ref()?.own_route_keys(route, create),
        )
    }

    fn records(&self, channel: &str) -> Option<Vec<(String, String)>> {
        let workspaces = self.workspaces.as_ref()?;
        settled(&self.host_device.runtime, workspaces.records(channel))
    }

    fn installed_package(&self, workspace: WorkspaceId, app: &str) -> Option<Vec<u8>> {
        let workspaces = self.workspaces.as_ref()?;
        self.runtime
            .block_on(workspaces.installed_package(&workspace.to_string(), app))
    }

    fn apply_packaged_action(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        call: PackagedCall<'_>,
    ) -> Result<Option<serde_json::Value>, String> {
        let workspaces = self
            .workspaces
            .as_ref()
            .ok_or("governed workspaces unavailable")?;
        if caller.roster_created_at().is_some() {
            let sessions = self.sessions.as_ref().ok_or("no roster signing sessions")?;
            return self
                .runtime
                .block_on(
                    workspaces.apply_roster_packaged_action(workspace, sessions, caller, call),
                )
                .map(Some);
        }
        let PackagedCall {
            app,
            kind,
            document,
            package,
            action,
            arguments,
            now_ms,
            ..
        } = call;
        // The invocation is the caller's: its person, agent or bot is the
        // change's actor, though the server's own device signs it.
        let member = caller
            .acting()
            .member_id()
            .cloned()
            .ok_or("the caller is no member")?;
        self.runtime
            .block_on(workspaces.apply_packaged_action(
                &workspace.to_string(),
                workspaces.governor(),
                tilo_workspace_service::workspaces::PackagedAction {
                    app,
                    kind,
                    document,
                    package,
                    action,
                    arguments,
                    member,
                    acting: caller.acting().clone(),
                    now_ms,
                },
            ))
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn packaged_surface(&self, workspace: WorkspaceId, app: &str) -> Option<Vec<u8>> {
        let workspaces = self.workspaces.as_ref()?;
        let package = self
            .runtime
            .block_on(workspaces.installed_package(&workspace.to_string(), app))?;
        tilo_workspace_service::workspaces::package_surface(&package, app).ok()
    }

    fn view_locator(
        &self,
        workspace: &str,
        provider: &str,
    ) -> Option<tilo_actions::remote::ResourceViewLocator> {
        let origin = self.browser_origin.read().ok()?.clone()?;
        Some(tilo_actions::remote::ResourceViewLocator {
            origin: (*origin).clone(),
            workspace: workspace.to_owned(),
            host_device: hex::encode(self.host_device.endpoint.server_cert.device_public_key),
            provider: provider.to_owned(),
            web_entry: self.web_entry.clone(),
        })
    }

    fn bot_personas(&self, workspace: WorkspaceId, installation: &str) -> bool {
        self.workspaces.as_ref().is_some_and(|workspaces| {
            self.host_device
                .runtime
                .block_on(workspaces.bot_personas(workspace, installation))
        })
    }

    /// What the workspace's server keeps ([`crate::workspaces::Workspaces::kept_content`]):
    /// a bridged person's avatar its bots fetched, a file or a feedback
    /// report's picture a member posted. A server keeping no content for the
    /// workspace answers none, which refuses an appearance naming an avatar.
    fn content_blob(&self, workspace: WorkspaceId, hash: &str) -> Option<Vec<u8>> {
        let workspaces = self.workspaces.as_ref()?;
        self.host_device
            .runtime
            .block_on(workspaces.kept_content(workspace, hash))
    }
}

/// The apps this host's workspaces offer: `catalogue` (this machine's
/// registry when `None`), and Browser and the workspace's Linux apps when
/// this host places their windows
/// ([`HostedBrowsers`](crate::hosted_browser::HostedBrowsers)). A workspace
/// lists Browser and opens a record for it like any app, though no worker of
/// this host mounts it: the window runs in the workspace's VM.
fn app_catalogue(
    catalogue: Option<CatalogueFactory>,
    browsers: Option<&Arc<crate::hosted_browser::HostedBrowsers>>,
) -> CatalogueFactory {
    let catalogue = catalogue.unwrap_or_else(|| Arc::new(AppCatalogue::native));
    if browsers.is_none() {
        return catalogue;
    }
    let placed = crate::hosted_browser::placed_app();
    let linux = crate::hosted_browser::placed_linux_apps();
    Arc::new(move || catalogue().placing(placed).placing(linux))
}

/// Start `cfg`'s server serving its workspaces' records to pages, without the agent supervisor:
/// the host mounts a record of an app in `catalogue` (this machine's
/// registry when `None`) and runs an action on it for whoever the workspace's
/// policy lets ask, as it does beside agents in [`start`]. Its workspaces'
/// reporters may have `feedback_reviewer` look over their reports. Answers the server and the
/// action service it serves the records with, which the agents the host runs for its workspaces
/// ([`tilo_workspace_service::agents`]) act through.
///
/// # Errors
/// Why the server did not start.
pub async fn start_serving_documents(
    cfg: crate::ServerConfig,
    identity: ServerIdentity,
    catalogue: Option<CatalogueFactory>,
    feedback_reviewer: Option<Arc<dyn FeedbackReviewer>>,
    shell: Option<Arc<HostShells>>,
    browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>,
) -> Result<(Server, WorkspaceActions), ServerError> {
    let content = crate::content::stores(&cfg.state_root);
    let state_dir = cfg.state_root.join("agents");
    // The server this starts is a large future of its own; boxing it keeps every caller's
    // future small, so a caller that awaits several starts does not build a large one.
    let (server, actions) = Box::pin(start_app_server(
        cfg,
        identity,
        AgentOptions {
            state_dir,
            catalogue,
            feedback_reviewer,
            shell,
            browsers,
        },
        content,
        None,
    ))
    .await?;
    Ok((server, actions.actions.clone()))
}

/// The services the roster turns use, on the same action server as the supervisor.
pub struct RosterServices {
    pub sessions: tilo_agent_session::AgentSessions,
    pub actions: WorkspaceActions,
    pub events: Option<tilo_workspace_service::agents::events::WorkspaceEvents>,
    pub initiative: Option<tilo_workspace_service::agents::initiative::Initiative>,
}

impl WorkspaceActionsServer {
    fn start_events(
        &self,
        server: &Server,
        sessions: tilo_agent_session::AgentSessions,
    ) -> RosterServices {
        let (initiative, events) = server
            .workspaces()
            .cloned()
            .map(|workspaces| {
                tilo_workspace_service::agents::initiative::Initiative::start(
                    workspaces,
                    Some(self.actions.clone()),
                    std::sync::Arc::new(crate::workspace_bots::installed::EventRunner),
                )
            })
            .unzip();
        if let Some(events) = &events {
            let _ = self.events.set(events.clone());
        }
        if let Some(initiative) = &initiative {
            let _ = self.initiative.set(initiative.clone());
        }
        RosterServices {
            sessions,
            actions: self.actions.clone(),
            events,
            initiative,
        }
    }

    /// Answer `message` for the workspace's events or its assistant's
    /// initiative, as the workspace's policy decides it: for `certificate`'s
    /// device, or with none for the member a turn this server carries out
    /// acts as. `None` for any other message.
    fn serve_roster(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        message: tilo_actions::remote::RemoteActionMessage,
        certificate: Option<&DeviceCert>,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        use tilo_workspace_service::agents::events::Authority;

        self.roster.as_ref()?;
        let authority = certificate.map_or(Authority::Turn, Authority::of);
        if tilo_workspace_service::agents::events::addresses(&message) {
            return match self.events.get() {
                Some(events) => events.serve(workspace, caller, authority, message),
                None => tilo_workspace_service::agents::events::not_served(message),
            };
        }
        if tilo_workspace_service::agents::initiative::addresses(&message) {
            return match self.initiative.get() {
                Some(initiative) => initiative.serve(workspace, caller, authority, message),
                None => tilo_workspace_service::agents::events::not_served(message),
            };
        }
        None
    }
}

/// Give `workspaces` the records `server` holds, joined as its own device.
fn attach_resources(
    actions: &WorkspaceActions,
    server: &Server,
    server_cert: &DeviceCert,
    web_entry: Option<String>,
    (feedback_reviewer, shell, browsers): Lent,
    sessions: Option<tilo_agent_session::AgentSessions>,
) -> Result<(), ServerError> {
    if let Some(sessions) = &sessions {
        actions.attach_agent_sessions(sessions.clone());
    }
    if let Some(shell) = &shell {
        shell.serve_from(server, actions);
    }
    if let Some(browsers) = &browsers {
        // A session in a workspace's VM joins as a device of the person's,
        // which a stand-in certifies: the one this host keeps for them, else
        // a host that is itself one.
        let user = server.own_user_public_key();
        let (key, cert) = server
            .stand_in(&user)
            .unwrap_or_else(|| (server.device_signing_key().clone(), server_cert.clone()));
        browsers.serve_from(crate::hosted_browser::Authority {
            user_public_key: user,
            cert,
            key,
            // The sessions in the environments dial this host from a machine
            // that reaches the public internet and nothing of this network:
            // the address they are given is the public one.
            server: crate::hosted_browser::server_address().map_err(ServerError::Identity)?,
            workspaces: server.workspaces().cloned(),
            environments: server.web_context().server.environments.clone(),
            runtime: tokio::runtime::Handle::current(),
            certification: server.confirmations(),
            hand: server.hand(),
        });
        // The VM hosts start the sessions when this host's own device asks.
        if let Some(workspaces) = server.workspaces() {
            browsers.ask_from(
                workspaces.action_service(),
                server.endpoint(),
                workspaces.clone(),
                tokio::runtime::Handle::current(),
            );
        }
    }
    let shell = shell.map(|shell| shell as Arc<dyn WorkspaceShell>);
    actions
        .attach(Arc::new(ServerRecordsLink {
            shell,
            user_public_key: server.own_user_public_key(),
            workspaces: server.workspaces().cloned(),
            browser_origin: Arc::clone(&server.browser_origin),
            web_entry,
            feedback_reviewer,
            stores: server.content(),
            content_links: server.content_links(),
            sessions,
            ready_links: std::sync::Mutex::default(),
            runtime: tokio::runtime::Handle::current(),
            host_device: HostDevice {
                endpoint: ServerEndpoint::iroh(server.node_addr(), server_cert.clone()),
                key: server.device_signing_key().clone(),
                runtime: tokio::runtime::Handle::current(),
                keys: server
                    .workspace_keys()
                    .map(|keys| tilo_session_client::Lent {
                        keys: Some(keys),
                        endpoint: None,
                        own: false,
                    }),
            },
        }))
        .map_err(ServerError::Identity)
}

/// Serve workspace records, the roster agents' signing grants and the people's events.
pub async fn start_workspace_documents(
    cfg: crate::ServerConfig,
    identity: ServerIdentity,
    options: AgentOptions,
) -> Result<(Server, AgentService, RosterServices), ServerError> {
    let (server, agents, actions) = start_inner(cfg, identity, options).await?;
    let roster = actions.start_events(&server, agents.agent_sessions());
    Ok((server, agents, roster))
}

async fn start_inner(
    cfg: crate::ServerConfig,
    identity: ServerIdentity,
    options: AgentOptions,
) -> Result<(Server, AgentService, Arc<WorkspaceActionsServer>), ServerError> {
    let content = crate::content::stores(&cfg.state_root);
    let mut agents = AgentService::new((options.state_dir.clone(), Arc::clone(&content)));
    // The server this starts is a large future of its own; boxing it keeps every caller's
    // future small, so a caller that awaits several starts does not build a large one.
    let (server, actions) = Box::pin(start_app_server(
        cfg,
        identity,
        options,
        content,
        Some(&mut agents),
    ))
    .await?;
    Ok((server, agents, actions))
}

async fn start_app_server(
    mut cfg: crate::ServerConfig,
    identity: ServerIdentity,
    options: AgentOptions,
    content: Arc<WorkspaceStores>,
    agents: Option<&mut AgentService>,
) -> Result<(Server, Arc<WorkspaceActionsServer>), ServerError> {
    let server_cert = identity.device_cert.clone();
    let catalogue = app_catalogue(options.catalogue, options.browsers.as_ref());
    // The apps this host serves are known at start: build the catalogue once here, so every
    // app's surface is registered in this process before any record the host founds names one.
    // A founder that does not know an app's surface stamps a genesis without it, and the next
    // founder that does writes a different genesis under the same actor: the two changes then
    // collide as a duplicate sequence number. A host that serves a workspace's apps names their
    // surfaces from the moment it serves them.
    let _ = catalogue();
    let sessions = agents.as_ref().map(|agents| agents.agent_sessions());
    cfg.services.app_serving = true;
    let actions = WorkspaceActions::new(catalogue);
    let action_server = Arc::new(WorkspaceActionsServer {
        supervisor: agents.as_ref().map(|agents| agents.supervisor().clone()),
        roster: sessions.clone().map(RosterProvisioning::new),
        pages: ActionPages::new(actions.clone()),
        actions,
        stores: content,
        browsers: options.browsers.clone(),
        events: Arc::default(),
        initiative: Arc::default(),
    });
    let web_entry = cfg.web_entry.clone();
    let server = Server::start_with_app_serving(cfg, identity, Some(action_server.clone())).await?;
    attach_resources(
        &action_server.actions,
        &server,
        &server_cert,
        web_entry,
        (options.feedback_reviewer, options.shell, options.browsers),
        sessions,
    )?;
    if let Some(agents) = agents {
        let link = Arc::new(server.agent_link());
        if let Some(roster) = &action_server.roster {
            roster.start(&link);
        }
        let endpoint = ServerEndpoint::iroh(server.node_addr(), server_cert);
        if let Err(error) = agents.start(endpoint, link) {
            server.shutdown().await;
            return Err(ServerError::Identity(error));
        }
    }
    Ok((server, action_server))
}

/// Whether `host` names something on a network (`eu1.tilo.sh`, `tilo.sh`): a host a person reads
/// in an address, not the loopback a machine bound to (`127.0.0.1`, `[::1]:8080`, `localhost`).
fn names_a_network(host: &str) -> bool {
    let name = host.trim();
    let name = name.split_once("://").map_or(name, |(_, rest)| rest);
    let name = name.split(['/', '?', '#']).next().unwrap_or(name);
    let without_port = match name.rsplit_once(':') {
        Some((head, port))
            if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            head
        }
        _ => name,
    };
    let unbound = |text: &str| {
        text.trim_start_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_owned()
    };
    [unbound(name), unbound(without_port)]
        .iter()
        .all(|candidate| {
            candidate.contains('.')
                && !candidate.eq_ignore_ascii_case("localhost")
                && candidate.parse::<std::net::IpAddr>().is_err()
        })
}

/// `name` as the host part of an address: lowercased, its words joined by `-`, only letters and
/// digits left, up to 63 of them. `workspace` when the name is empty or has no such words.
fn address_slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if slug.len() >= 63 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "workspace".to_owned()
    } else {
        slug.to_owned()
    }
}

#[cfg(test)]
mod address_tests {
    use super::{address_slug, names_a_network};

    #[test]
    fn an_address_host_names_a_network_and_never_a_loopback() {
        for host in [
            "eu1.tilo.sh",
            "https://eu1.tilo.sh/app/",
            "tilo.sh",
            "team.tilo",
            "http://tilo.sh:8443",
        ] {
            assert!(names_a_network(host), "{host} names a network");
        }
        for host in [
            "",
            "   ",
            "tilo-sh",
            "server",
            "127.0.0.1",
            "127.0.0.1:8080",
            "http://127.0.0.1:8080/",
            "localhost",
            "LOCALHOST:3000",
            "[::1]:8080",
            "::1",
        ] {
            assert!(!names_a_network(host), "{host} names no network");
        }
    }

    #[test]
    fn a_workspace_names_an_address_and_one_without_words_is_a_workspace() {
        assert_eq!(address_slug("Team"), "team");
        assert_eq!(address_slug("Applicant: Jane Doe"), "applicant-jane-doe");
        assert_eq!(address_slug("  "), "workspace");
        assert_eq!(address_slug("Мария"), "workspace");
        assert_eq!(
            address_slug("A very long workspace name that runs on far past sixty-three characters"),
            "a-very-long-workspace-name-that-runs-on-far-past-sixty-three-ch"
        );
    }
}
