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
//! - **Asking first.** An action the person's grants leave to them is put
//!   to their approving device as a question for that one invocation
//!   ([`CertificationProtocol::confirm`]); the agent runs it only on the
//!   approved answer, and a question nobody answers in time is refused.
//!
//! [`start`] wires both into a server: the supervisor answers the agent
//! vocabulary as the host's app action server, and the host's own agents
//! reach it over iroh.

use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::{ActionCallError, Caller, Pending};

mod roster;
use roster::RosterProvisioning;
use tilo_agent_session::workspace_actions::{
    HostDevice, PackagedCall, ShellRan, Watching, WorkspaceShell,
};
use tilo_agent_session::{
    AdmissionDone, AgentService, AgentHostLink, AppCatalogue, ApprovalDone, CatalogueFactory,
    FeedbackReviewer, HostActions, ServerEndpoint, MakeTurns, SupervisorHost, WorkspaceActions,
    WorkspaceDocumentsLink,
};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};

use tilo_server_protocol::delegation::{DelegateProposal, Delegation};
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationRequest, DeviceCapabilities, DeviceCert,
};

use crate::WorkspaceId;
use crate::certification::CertificationProtocol;
use crate::daemon::{Server, ServerError, ServerIdentity};
use crate::peer::AgentRegistry;
use tilo_content::WorkspaceStores;
use tilo_lock::RwLockRecover as _;

/// How long an agent's approval request waits for an approver.
const APPROVAL_WAIT_NS: u64 = 10 * 60 * 1_000_000_000;

/// How long an agent's question about one invocation waits for its person.
const CONFIRMATION_WAIT_NS: u64 = 2 * 60 * 1_000_000_000;
/// How long a question an agent asks in its own words waits for its person:
/// a decision or something only they know may take them a while.
const ASK_WAIT_NS: u64 = 30 * 60 * 1_000_000_000;

/// The server as an agent supervisor sees it.
#[derive(Clone)]
pub struct ServerAgentLink {
    user_public_key: [u8; 32],
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
            user_public_key: web.server.identity.user_public_key,
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
        keeper: &[u8; 32],
    ) -> Option<[u8; 32]> {
        let workspaces = self.workspaces.as_ref()?;
        let entry = workspaces
            .hosted_roster_agent(workspace, agent, keeper, None)
            .await?;
        let device = self
            .approved_session_device(sessions, workspace, agent, keeper)
            .await?;
        sessions
            .matches_roster(&device, workspace, agent, keeper, entry.created_at)
            .then_some(device)
    }

    /// Resolve a governed delegation to the authenticated session serving its workspace.
    pub async fn approved_session_device(
        &self,
        sessions: &tilo_agent_session::AgentSessions,
        workspace: WorkspaceId,
        agent: &str,
        keeper: &[u8; 32],
    ) -> Option<[u8; 32]> {
        let device = self
            .workspaces
            .as_ref()?
            .approved_agent_device(workspace, agent, keeper)
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
        key: &SigningKey,
        proposal: &DelegateProposal,
    ) -> Result<([u8; 32], String), String> {
        let now = tilo_clock::unix_now_ns();
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
        let request = CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: self.user_public_key,
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

impl AgentHostLink for ServerAgentLink {
    fn request_approval(
        &self,
        key: &SigningKey,
        proposal: &DelegateProposal,
        done: ApprovalDone,
    ) -> Result<String, String> {
        let (digest, pairing_code) = self.enqueue_approval(key, proposal)?;
        let certification = self.certification.clone();
        self.runtime.spawn(async move {
            done(certification.delegate_answer(&digest).await);
        });
        Ok(pairing_code)
    }

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
        done: tilo_agent_session::control::RosterAdmissionDone,
    ) {
        self.join_roster(identity, created_at, lifetime, done);
    }

    fn register(&self, cert: &DeviceCert, delegation: &Delegation) -> Result<(), String> {
        crate::daemon::register_agent_in(&self.agents, &self.user_public_key, cert, delegation)?;
        // The governed workspaces it names put it on their rosters.
        self.reconcile_workspaces(delegation.device_public_key, Some(cert.clone()));
        Ok(())
    }

    fn withdraw(&self, device_public_key: [u8; 32]) {
        self.withdraw_device(device_public_key);
    }

    fn when_admitted(&self, cert: &DeviceCert, delegation: &Delegation, done: AdmissionDone) {
        let Some(workspaces) = self.workspaces.clone() else {
            done();
            return;
        };
        let device = delegation.device_public_key;
        let cert = cert.clone();
        // Putting an agent on a workspace's roster settles that workspace,
        // which is work of the runtime's own: it runs there, and the agent's
        // start waits for it rather than joining a workspace that does not
        // hold it yet.
        self.runtime.spawn(async move {
            workspaces.reconcile_agent(device, Some(&cert)).await;
            done();
        });
    }
}

/// What [`start`] runs agents with.
pub struct AgentOptions {
    /// Grants and agents are kept here.
    pub state_dir: PathBuf,
    /// The model the agents work on their tasks with.
    pub model: MakeTurns,
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
/// With VM hosts (a placer, [`crate::placed_shells::PLACER_ENV`]) a
/// workspace's shell runs on the member device that holds its VM
/// ([`PlacedShells`](crate::placed_shells::PlacedShells)). Without, it runs
/// here, its files under `state`, its Linux programs in the hosted VMs the
/// host's environment names (`TILO_HOSTED_VM_SOCKET`, `TILO_HOSTED_ACCOUNT`,
/// `TILO_HOSTED_VM_SIZE`, `TILO_HOSTED_PLANS`), and without those, ush's own
/// commands only.
///
/// # Errors
///
/// A placer is named, but its members or account are not usable.
pub fn host_shells(state: PathBuf) -> Result<Arc<HostShells>, String> {
    let placed = crate::placed_shells::PlacedShells::shared_from_env()?;
    Ok(Arc::new(match placed {
        Some(placed) => HostShells::Placed(placed),
        None => HostShells::Here(ush::workspace_shell::WorkspaceShells::from_env(state)),
    }))
}

/// What a host lends its workspaces' records service: a model that looks
/// over feedback reviews, where Terminal sessions run their commands, and the
/// Browser it places.
type Lent = (
    Option<Arc<dyn FeedbackReviewer>>,
    Option<Arc<HostShells>>,
    Option<Arc<crate::hosted_browser::HostedBrowsers>>,
);

/// Where the workspace actions run a session's commands.
pub enum HostShells {
    /// Here: ush's workspace shells, in this process.
    Here(ush::workspace_shell::WorkspaceShells),
    /// On the member device holding the workspace's VM.
    Placed(Arc<crate::placed_shells::PlacedShells>),
}

impl HostShells {
    /// Ask VM hosts' members, if any, as `server`'s own device, reading which
    /// keeps each workspace's VM from the workspaces' VM leases, and have the
    /// workspaces' machines branched and deleted with them.
    fn serve_from(&self, server: &Server) {
        if let (Self::Placed(placed), Some(workspaces)) = (self, server.workspaces()) {
            placed.ask_from(workspaces.action_service(), server.endpoint());
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                placed.read_leases(workspaces.clone(), runtime);
            }
        }
        if let (Some(machines), Some(workspaces)) = (self.machines(), server.workspaces()) {
            workspaces.machines(machines);
        }
    }

    /// What branches and deletes the workspaces' Linux machines: the VM hosts'
    /// members, or the hosted-VM service a host running its shells here names;
    /// `None` when its shells run no Linux programs, so there are no machines.
    fn machines(&self) -> Option<Arc<dyn crate::workspaces::WorkspaceMachines>> {
        match self {
            Self::Placed(placed) => Some(Arc::new(crate::placed_machines::PlacedMachines::new(
                Arc::clone(placed),
            ))),
            Self::Here(shells) => shells.hosted().map(|target| {
                Arc::new(crate::placed_machines::LocalMachines::new(target.clone()))
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
        agent: bool,
        person: Option<&tilo_actions::remote::environment_commands::Person>,
        watching: Watching<'_>,
    ) -> Result<ShellRan, String> {
        let shells = match self {
            Self::Here(shells) => shells,
            Self::Placed(placed) => {
                return placed.run(workspace, session, command, agent, person, watching);
            }
        };
        let ran = shells
            .run_for(
                workspace,
                session,
                command,
                agent,
                ush::workspace_shell::RunWith {
                    person,
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
}

/// The workspace action server, with the supervisor where the host offers one.
struct WorkspaceActionsServer {
    supervisor: Option<HostActions>,
    serve_supervisor: bool,
    roster: Option<RosterProvisioning>,
    actions: WorkspaceActions,
    pages: ActionPages,
    stores: Arc<WorkspaceStores>,
    /// The Browser this host places for its workspaces, when it places one.
    browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>,
    /// The events this host runs for its people, once its workspaces are up.
    events: Arc<std::sync::OnceLock<crate::workspace_bots::events::PersonEvents>>,
    /// What its people's assistants find to do on their own, once its workspaces are up.
    initiative: Arc<std::sync::OnceLock<crate::workspace_bots::initiative::Initiative>>,
}

/// The workspaces' apps as web pages, each held by the action service as the
/// member who opened it ([`WorkspaceActions::open_page`]).
struct ActionPages(WorkspaceActions);

impl crate::app_serving::AppPages for ActionPages {
    fn open(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        address: tilo_server_protocol::page::PageAddress,
    ) -> Pending<
        Result<tilo_server_protocol::page::OpenedPage, tilo_server_protocol::page::PageRefusal>,
    > {
        self.0.open_page(workspace_id, caller, address)
    }

    fn act(
        &self,
        workspace_id: WorkspaceId,
        page: u64,
        act: Option<(u32, Option<String>, tilo_server_protocol::page::PageAction)>,
    ) -> Pending<
        Result<Vec<tilo_server_protocol::page::PagePatch>, tilo_server_protocol::page::PageRefusal>,
    > {
        self.0.act_on_page(workspace_id, page, act)
    }

    fn close(&self, workspace_id: WorkspaceId, page: u64) {
        self.0.close_page(workspace_id, page);
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
        // A person's events are the host's own to answer, for their device
        // and their page as for their assistant's turn.
        if self.roster.is_some() && crate::workspace_bots::events::addresses(&message) {
            return match self.events.get() {
                Some(events) => events.serve(workspace_id, caller, message),
                None => crate::workspace_bots::events::not_served(message),
            };
        }
        // So is what their assistant finds to do on its own.
        if self.roster.is_some() && crate::workspace_bots::initiative::addresses(&message) {
            return match self.initiative.get() {
                Some(initiative) => initiative.serve(workspace_id, caller, message),
                None => crate::workspace_bots::events::not_served(message),
            };
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
            |message| {
                use tilo_actions::remote::RemoteActionMessage;
                use tilo_server_protocol::agents::{
                    AGENTS_PROVIDER, STATUS_ACTION, SUPERVISOR_RESOURCE,
                };

                if self.serve_supervisor
                    && let Some(supervisor) = &self.supervisor
                {
                    return supervisor.serve(workspace_id, caller, message);
                }
                match &message {
                    RemoteActionMessage::CatalogueRequest { query, .. }
                        if query.provider.as_deref() == Some(AGENTS_PROVIDER) =>
                    {
                        None
                    }
                    RemoteActionMessage::Invoke(request)
                        if request.target.provider == AGENTS_PROVIDER =>
                    {
                        self.roster.as_ref()?;
                        let address = &request.target.address;
                        if address.resource == SUPERVISOR_RESOURCE
                            && address.action == STATUS_ACTION
                        {
                            return self
                                .supervisor
                                .as_ref()?
                                .serve(workspace_id, caller, message);
                        }
                        tilo_actions::remote::refuse(
                            &message,
                            tilo_actions::remote::ActionCallError::Refused {
                                reason: "standalone task agents are disabled on this host".into(),
                            },
                        )
                        .map(Pending::ready)
                    }
                    _ => self.actions.serve(workspace_id, caller, message),
                }
            },
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
        if let (Some(roster), Some(supervisor)) = (&self.roster, &self.supervisor)
            && let Some(answer) = roster.serve(
                workspace,
                caller,
                certificate,
                &message,
                &supervisor.supervisor.0,
            )
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
    /// Where a person is asked about an action run for them.
    certification: CertificationProtocol,
    sessions: Option<tilo_agent_session::AgentSessions>,
}

impl ServerRecordsLink {
    /// The person a turn of this server acts for, and where their own store
    /// is kept, when `caller` is such a turn's agent: one a person's own
    /// session was handed to, working inside a turn. Nothing else is decided
    /// by what stands for a person.
    fn turn_of(&self, caller: &Caller) -> Option<(&crate::workspaces::Workspaces, [u8; 32])> {
        let acting = caller.acting();
        if acting.app().is_some()
            || !acting.is_agent()
            || !caller.is_session_delegated()
            || caller.turn().is_none()
        {
            return None;
        }
        Some((self.workspaces.as_ref()?, *caller.user_public_key()?))
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

    /// What stands for the person the turn acts for, read from their own
    /// store on this server now: for the turn's own agent and nobody else.
    fn stands(
        &self,
        _workspace: WorkspaceId,
        caller: &Caller,
        asked: &tilo_actions::remote::person_asking::Asked,
    ) -> Option<String> {
        let (workspaces, person) = self.turn_of(caller)?;
        if caller.acting().agent_id() != Some(asked.agent.as_str()) {
            return None;
        }
        workspaces.stands(&person, asked)
    }

    fn permission_used(&self, _workspace: WorkspaceId, caller: &Caller, permission: &str) {
        if let Some((workspaces, person)) = self.turn_of(caller) {
            workspaces.permission_used(&person, permission);
        }
    }

    /// On the approving devices of the person `caller` is or acts for: the
    /// host's own person's, or a member's own root, as their device requests
    /// are answered.
    fn ask_person(
        &self,
        _workspace: WorkspaceId,
        caller: &Caller,
        question: tilo_actions::remote::ConfirmationQuestion,
    ) -> Option<Pending<Result<tilo_actions::remote::ConfirmationReply, ActionCallError>>> {
        let person = *caller.user_public_key()?;
        // The agent the person's device asks for, when the device asks over its own session.
        let agent_id = caller
            .acting()
            .agent_id()
            .map_or(question.agent, str::to_owned);
        let pending = PendingConfirmation {
            id: question.id,
            agent_id,
            agent_label: question.device_label,
            title: question.title,
            action_label: question.action_label,
            resource_label: question.resource_label,
            arguments: question.arguments,
            risk: question.risk,
            kind: question.kind,
            undoable: question.undoable,
            workspace: question.workspace,
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(if question.ask.is_some() {
                ASK_WAIT_NS
            } else {
                CONFIRMATION_WAIT_NS
            }),
            page: None,
            allow_always: false,
            ask: question
                .ask
                .map(|ask| tilo_server_protocol::certify::AgentAsk {
                    question: ask.question,
                    choices: ask.choices,
                    typed: ask.typed,
                    // An agent's own question never asks for a secret: only this host words one.
                    secret: false,
                }),
        };
        let (completer, answer) = tilo_actions::remote::pending();
        let certification = self.certification.clone();
        let workspaces = self.workspaces.clone();
        self.host_device.runtime.spawn(async move {
            let mut pending = pending;
            if let Some(workspaces) = workspaces {
                // Who asks is what the workspace calls the agent, never the label of the client
                // that connected it or its id: the person reads a name they know.
                if let Some(name) = workspaces
                    .agent_name(&pending.workspace, &pending.agent_id)
                    .await
                {
                    pending.agent_label = name;
                }
                // The person reads the workspace by its name, not its id.
                if let Ok(summary) = workspaces.summary(&pending.workspace).await {
                    pending.workspace = summary.name;
                }
            }
            completer.complete(match certification.confirm_for(person, pending).await {
                Ok(ConfirmationAnswer::AllowOnce) => {
                    Ok(tilo_actions::remote::ConfirmationReply::AllowOnce)
                }
                Ok(
                    ConfirmationAnswer::Deny
                    | ConfirmationAnswer::AllowAlways
                    | ConfirmationAnswer::Secret(_),
                ) => Ok(tilo_actions::remote::ConfirmationReply::Deny),
                Ok(ConfirmationAnswer::Reply(reply)) => {
                    Ok(tilo_actions::remote::ConfirmationReply::Replied(reply))
                }
                Err(_) => Err(ActionCallError::confirmation_required()),
            });
        });
        Some(answer)
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
            .block_on(crate::workspace_agents::workspace_agent(
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
        let identity = self
            .host_device
            .runtime
            .block_on(self.workspaces.as_ref()?.workspace_identity(workspace))?;
        self.stores.get(&identity).ok()
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
        let (formats, zone) = workspaces.person_region(&person)?;
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
                .resource_document(workspace, "so.tilo.chat", &list)
                .await
                && let Ok(mut doc) = automerge::AutoCommit::load(&bytes)
                && let Ok(agents) =
                    crate::workspace_bots::opened::<tilo_workspace_agent::WorkspaceAgents>(&mut doc)
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
    /// acts for, as the workspace's member list names them, at the address the workspace is
    /// reached by (`eu1.tilo.sh`, the origin the host published for its Browser). A commit made
    /// in the workspace's VM names whoever asked for it as the workspace does.
    fn person(
        &self,
        workspace: &str,
        caller: &tilo_actions::remote::Caller,
    ) -> Option<tilo_actions::remote::environment_commands::Person> {
        let workspaces = self.workspaces.as_ref()?;
        let origin = self
            .browser_origin
            .read()
            .ok()
            .and_then(|held| held.as_ref().cloned());
        let host = origin.map_or_else(
            || workspaces.serving_certificate().device_label.clone(),
            |origin| (*origin).clone(),
        );
        tilo_agent_session::workspace_actions::person_of(caller, &self.members(workspace), &host)
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
        self.host_device
            .runtime
            .block_on(self.workspaces.as_ref()?.own_route_keys(route, create))
    }

    fn records(&self, channel: &str) -> Option<Vec<(String, String)>> {
        let workspaces = self.workspaces.as_ref()?;
        self.host_device
            .runtime
            .block_on(workspaces.records(channel))
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
/// ([`crate::workspace_agents`]) act through.
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
            model: Arc::new(|| None),
            catalogue,
            feedback_reviewer,
            shell,
            browsers,
        },
        content,
        None,
        None,
        Serving::Documents,
    ))
    .await?;
    Ok((server, actions.actions.clone()))
}

/// The services the roster turns use, on the same action server as the supervisor.
pub struct RosterServices {
    pub sessions: tilo_agent_session::AgentSessions,
    pub actions: WorkspaceActions,
    pub events: Option<crate::workspace_bots::events::PersonEvents>,
    pub initiative: Option<crate::workspace_bots::initiative::Initiative>,
}

impl WorkspaceActionsServer {
    fn start_events(
        &self,
        server: &Server,
        events_state: PathBuf,
        initiative_state: PathBuf,
        sessions: tilo_agent_session::AgentSessions,
    ) -> RosterServices {
        let (initiative, events) = server
            .workspaces()
            .cloned()
            .map(|workspaces| {
                crate::workspace_bots::initiative::Initiative::start(
                    workspaces,
                    initiative_state,
                    events_state,
                    Some(self.actions.clone()),
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
        shell.serve_from(server);
    }
    if let Some(browsers) = &browsers {
        // A session in a workspace's VM joins as a device of the person's,
        // which a stand-in certifies: the one this host keeps for them, else
        // a host that is itself one.
        let user = server.workspace_documents().user_public_key();
        let (key, cert) = server.stand_in(&user).unwrap_or_else(|| {
            (
                server.workspace_documents().signing_key().clone(),
                server_cert.clone(),
            )
        });
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
            browsers.ask_from(workspaces.action_service(), server.endpoint());
        }
    }
    let shell = shell.map(|shell| shell as Arc<dyn WorkspaceShell>);
    actions
        .attach(Arc::new(ServerRecordsLink {
            shell,
            user_public_key: server.workspace_documents().user_public_key(),
            workspaces: server.workspaces().cloned(),
            browser_origin: Arc::clone(&server.browser_origin),
            web_entry,
            feedback_reviewer,
            stores: server.content(),
            content_links: server.content_links(),
            certification: server.web_context().certification.clone(),
            sessions,
            runtime: tokio::runtime::Handle::current(),
            host_device: HostDevice {
                endpoint: ServerEndpoint::iroh(server.node_addr(), server_cert.clone()),
                key: server.workspace_documents().signing_key().clone(),
                runtime: tokio::runtime::Handle::current(),
            },
        }))
        .map_err(ServerError::Identity)
}

/// Start `cfg`'s server with an agent supervisor as its app action server,
/// then start the supervisor's agents: those the state directory says were
/// running resume.
pub async fn start(
    cfg: crate::ServerConfig,
    identity: ServerIdentity,
    options: AgentOptions,
) -> Result<(Server, AgentService), ServerError> {
    let (server, agents, _) =
        start_inner(cfg, identity, options, None, Serving::Supervisor).await?;
    Ok((server, agents))
}

/// Start the app-serving host with an explicit transport relay policy.
///
/// # Errors
/// The same as [`start`].
pub async fn start_with_relay_mode(
    cfg: crate::ServerConfig,
    identity: ServerIdentity,
    options: AgentOptions,
    relay_mode: iroh::RelayMode,
) -> Result<(Server, AgentService), ServerError> {
    let (server, agents, _) = start_inner(
        cfg,
        identity,
        options,
        Some(relay_mode),
        Serving::Supervisor,
    )
    .await?;
    Ok((server, agents))
}

/// Start the approval supervisor and the services used by the workspace roster runner.
pub async fn start_with_roster(
    cfg: crate::ServerConfig,
    identity: ServerIdentity,
    options: AgentOptions,
) -> Result<(Server, AgentService, RosterServices), ServerError> {
    start_roster(cfg, identity, options, Serving::Both).await
}

/// Serve workspace records, roster grants and events on a host that does not
/// offer standalone task agents.
pub async fn start_workspace_documents(
    cfg: crate::ServerConfig,
    identity: ServerIdentity,
    options: AgentOptions,
) -> Result<(Server, AgentService, RosterServices), ServerError> {
    start_roster(cfg, identity, options, Serving::Roster).await
}

async fn start_roster(
    cfg: crate::ServerConfig,
    identity: ServerIdentity,
    options: AgentOptions,
    serving: Serving,
) -> Result<(Server, AgentService, RosterServices), ServerError> {
    let events_state = cfg.state_root.join("person-events");
    let initiative_state = cfg.state_root.join("person-initiative");
    let (server, agents, actions) = start_inner(cfg, identity, options, None, serving).await?;
    let roster = actions.start_events(
        &server,
        events_state,
        initiative_state,
        agents.agent_sessions(),
    );
    Ok((server, agents, roster))
}

#[derive(Clone, Copy)]
enum Serving {
    Documents,
    Supervisor,
    Roster,
    Both,
}

async fn start_inner(
    cfg: crate::ServerConfig,
    identity: ServerIdentity,
    options: AgentOptions,
    relay_mode: Option<iroh::RelayMode>,
    serving: Serving,
) -> Result<(Server, AgentService, Arc<WorkspaceActionsServer>), ServerError> {
    let content = crate::content::stores(&cfg.state_root);
    let mut agents = AgentService::new(
        identity.user_public_key,
        (options.state_dir.clone(), Arc::clone(&content)),
        Arc::clone(&options.model),
    );
    // The server this starts is a large future of its own; boxing it keeps every caller's
    // future small, so a caller that awaits several starts does not build a large one.
    let (server, actions) = Box::pin(start_app_server(
        cfg,
        identity,
        options,
        content,
        Some(&mut agents),
        relay_mode,
        serving,
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
    relay_mode: Option<iroh::RelayMode>,
    serving: Serving,
) -> Result<(Server, Arc<WorkspaceActionsServer>), ServerError> {
    let server_cert = identity.device_cert.clone();
    let catalogue = app_catalogue(options.catalogue, options.browsers.as_ref());
    let roster = matches!(serving, Serving::Roster | Serving::Both);
    let serve_supervisor = matches!(serving, Serving::Supervisor | Serving::Both);
    let sessions = agents.as_ref().map(|agents| agents.agent_sessions());
    cfg.services.app_serving = true;
    let actions = WorkspaceActions::new(catalogue);
    let action_server = Arc::new(WorkspaceActionsServer {
        supervisor: agents.as_ref().map(|agents| HostActions {
            supervisor: SupervisorHost(agents.supervisor().clone()),
            actions: actions.clone(),
        }),
        serve_supervisor,
        roster: sessions
            .clone()
            .filter(|_| roster)
            .map(RosterProvisioning::new),
        pages: ActionPages(actions.clone()),
        actions,
        stores: content,
        browsers: options.browsers.clone(),
        events: Arc::default(),
        initiative: Arc::default(),
    });
    let web_entry = cfg.web_entry.clone();
    let server = match relay_mode {
        Some(mode) => {
            Server::start_with_app_serving_and_relay_mode(
                cfg,
                identity,
                Some(action_server.clone()),
                mode,
            )
            .await?
        }
        None => Server::start_with_app_serving(cfg, identity, Some(action_server.clone())).await?,
    };
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
        let started = if serve_supervisor {
            agents.start(endpoint, link)
        } else {
            agents.start_roster(endpoint, link)
        };
        if let Err(error) = started {
            server.shutdown().await;
            return Err(ServerError::Identity(error));
        }
    }
    Ok((server, action_server))
}
