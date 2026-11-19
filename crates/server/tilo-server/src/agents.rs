//! Agents on this host (feature `agents`): the link between the server and
//! `tilo-agent-session`'s [`AgentHost`].
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
use tilo_agent_session::workspace_actions::{
    HostDevice, PackagedCall, ShellRan, Watching, WorkspaceShell,
};
use tilo_agent_session::{
    AdmissionDone, AgentHost, AgentHostLink, AppCatalogue, ApprovalDone, CatalogueFactory,
    FeedbackReviewer, HostActions, HostEndpoint, MakeTurns, SupervisorHost, WorkspaceActions,
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
    /// The link an [`AgentHost`] supervises this server's agents through.
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
        let (digest, pairing_code) =
            self.certification
                .enqueue_delegate(request, proposal.clone(), now)?;
        let certification = self.certification.clone();
        self.runtime.spawn(async move {
            done(certification.delegate_answer(&digest).await);
        });
        Ok(pairing_code)
    }

    fn register(&self, cert: &DeviceCert, delegation: &Delegation) -> Result<(), String> {
        crate::daemon::register_agent_in(&self.agents, &self.user_public_key, cert, delegation)?;
        // The governed workspaces it names put it on their rosters.
        self.reconcile_workspaces(delegation.device_public_key, Some(cert.clone()));
        Ok(())
    }

    fn withdraw(&self, device_public_key: [u8; 32]) {
        self.agents.write_recover().insert(device_public_key, None);
        crate::authority_epoch::bump();
        // Governed workspaces take it off their documents and refuse its writes.
        self.reconcile_workspaces(device_public_key, None);
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
        watching: Watching<'_>,
    ) -> Result<ShellRan, String> {
        let shells = match self {
            Self::Here(shells) => shells,
            Self::Placed(placed) => {
                return placed.run(workspace, session, command, agent, watching);
            }
        };
        let ran = shells
            .run_for(
                workspace,
                session,
                command,
                agent,
                None,
                watching.stop,
                watching.seen,
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

/// The host's app action server: agents and the workspaces' page actions,
/// keeping what they make in the host's content.
struct HostActionsServer {
    host: HostActions,
    stores: Arc<WorkspaceStores>,
    /// The Browser this host places for its workspaces, when it places one.
    browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>,
}

/// The app action server of a host that runs no agents: the workspaces' page
/// actions alone. A member's own actions on a workspace record (linking their
/// account on the workspace's Discord from the profile card) need nothing more.
struct WorkspaceRecordsServer {
    actions: WorkspaceActions,
    stores: Arc<WorkspaceStores>,
    /// The Browser this host places for its workspaces, when it places one.
    browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>,
    /// The events this host runs for its people, once its workspaces are up.
    events: Arc<std::sync::OnceLock<crate::workspace_bots::events::PersonEvents>>,
    /// What its people's assistants find to do on their own, once its workspaces are up.
    initiative: Arc<std::sync::OnceLock<crate::workspace_bots::initiative::Initiative>>,
}

impl crate::AppActionServer for WorkspaceRecordsServer {
    fn serve(
        &self,
        workspace_id: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        // A person's events are the host's own to answer, for their device
        // and their page as for their assistant's turn.
        if crate::workspace_bots::events::addresses(&message) {
            return match self.events.get() {
                Some(events) => events.serve(workspace_id, caller, message),
                None => crate::workspace_bots::events::not_served(message),
            };
        }
        // So is what their assistant finds to do on its own.
        if crate::workspace_bots::initiative::addresses(&message) {
            return match self.initiative.get() {
                Some(initiative) => initiative.serve(workspace_id, caller, message),
                None => crate::workspace_bots::events::not_served(message),
            };
        }
        let agents = match &message {
            tilo_actions::remote::RemoteActionMessage::CatalogueRequest { query, .. } => {
                query.provider.as_deref() == Some(tilo_server_protocol::agents::AGENTS_PROVIDER)
            }
            tilo_actions::remote::RemoteActionMessage::Invoke(request) => {
                request.target.provider == tilo_server_protocol::agents::AGENTS_PROVIDER
            }
            _ => false,
        };
        if agents {
            return None;
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
}

impl crate::AppActionServer for HostActionsServer {
    fn serve(
        &self,
        workspace_id: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        let observe: Arc<crate::hosted_browser::ObserveInvocation> = {
            let actions = self.host.actions.clone();
            Arc::new(move |caller, invocation| actions.observe(caller, invocation))
        };
        serve_with_browser(
            self.browsers.as_ref(),
            workspace_id,
            caller,
            message,
            |message| self.host.serve(workspace_id, caller, message),
            Some(&observe),
        )
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
        self.host.actions.forget_workspace(workspace_id);
    }

    fn session_arrived(&self, workspace_id: WorkspaceId, caller: &tilo_actions::remote::Caller) {
        self.host.actions.session_arrived(workspace_id, caller);
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
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(CONFIRMATION_WAIT_NS),
            page: None,
        };
        let (completer, answer) = tilo_actions::remote::pending();
        let certification = self.certification.clone();
        let workspaces = self.workspaces.clone();
        self.host_device.runtime.spawn(async move {
            let mut pending = pending;
            // The person reads the workspace by its name, not its id.
            if let Some(workspaces) = workspaces
                && let Ok(summary) = workspaces.summary(&pending.workspace).await
            {
                pending.workspace = summary.name;
            }
            completer.complete(match certification.confirm_for(person, pending).await {
                Ok(ConfirmationAnswer::AllowOnce) => {
                    Ok(tilo_actions::remote::ConfirmationReply::AllowOnce)
                }
                Ok(ConfirmationAnswer::Deny) => Ok(tilo_actions::remote::ConfirmationReply::Deny),
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

    fn key_revoked(&self, workspace: WorkspaceId, caller: &tilo_actions::remote::Caller) -> bool {
        let Some(workspaces) = self.workspaces.as_ref() else {
            return false;
        };
        self.runtime
            .block_on(workspaces.key_revoked(workspace, caller))
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

    fn host_keys(
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
        let PackagedCall {
            app,
            kind,
            document,
            package,
            action,
            arguments,
            now_ms,
        } = call;
        let workspaces = self
            .workspaces
            .as_ref()
            .ok_or("governed workspaces unavailable")?;
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
    start_serving_documents_and_events(cfg, identity, catalogue, feedback_reviewer, shell, browsers)
        .await
        .map(|(server, actions, ..)| (server, actions))
}

/// [`start_serving_documents`], answering also with the events this host runs for its people
/// ([`crate::workspace_bots::events`]): their scheduled work and what waits for something to
/// happen, kept under the server's state and run from its start, with no device of theirs on. The
/// server answers their actions for a person's device and page; whoever runs the people's agents
/// hands the same events to their turns ([`crate::workspace_agents::HostAgents::with_events`]).
/// With the events comes what the people's assistants find to do on their own
/// ([`crate::workspace_bots::initiative`]): the look-round is one of a person's events, and the
/// server looks before it hands that event's instruction on.
/// None where the server governs no workspaces.
///
/// # Errors
/// Why the server did not start.
pub async fn start_serving_documents_and_events(
    mut cfg: crate::ServerConfig,
    identity: ServerIdentity,
    catalogue: Option<CatalogueFactory>,
    feedback_reviewer: Option<Arc<dyn FeedbackReviewer>>,
    shell: Option<Arc<HostShells>>,
    browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>,
) -> Result<
    (
        Server,
        WorkspaceActions,
        Option<crate::workspace_bots::events::PersonEvents>,
        Option<crate::workspace_bots::initiative::Initiative>,
    ),
    ServerError,
> {
    let server_cert = identity.device_cert.clone();
    let catalogue = app_catalogue(catalogue, browsers.as_ref());
    cfg.services.app_serving = true;
    let events_state = cfg.state_root.join("person-events");
    let initiative_state = cfg.state_root.join("person-initiative");
    let app_server = Arc::new(WorkspaceRecordsServer {
        actions: WorkspaceActions::new(catalogue),
        stores: crate::content::stores(&cfg.state_root),
        browsers: browsers.clone(),
        events: Arc::default(),
        initiative: Arc::default(),
    });
    let web_entry = cfg.web_entry.clone();
    let server = Server::start_with_app_serving(cfg, identity, Some(app_server.clone())).await?;
    attach_resources(
        &app_server.actions,
        &server,
        &server_cert,
        web_entry,
        (feedback_reviewer, shell, browsers),
        None,
    )?;
    let (initiative, events) = server
        .workspaces()
        .cloned()
        .map(|workspaces| {
            crate::workspace_bots::initiative::Initiative::start(
                workspaces,
                initiative_state,
                events_state,
                Some(app_server.actions.clone()),
            )
        })
        .unzip();
    if let Some(events) = &events {
        let _ = app_server.events.set(events.clone());
    }
    if let Some(initiative) = &initiative {
        let _ = app_server.initiative.set(initiative.clone());
    }
    Ok((server, app_server.actions.clone(), events, initiative))
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
    if let Some(sessions) = sessions {
        actions.attach_agent_sessions(sessions);
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
            runtime: tokio::runtime::Handle::current(),
            host_device: HostDevice {
                endpoint: HostEndpoint::iroh(server.node_addr(), server_cert.clone()),
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
) -> Result<(Server, AgentHost), ServerError> {
    start_inner(cfg, identity, options, None).await
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
) -> Result<(Server, AgentHost), ServerError> {
    start_inner(cfg, identity, options, Some(relay_mode)).await
}

async fn start_inner(
    mut cfg: crate::ServerConfig,
    identity: ServerIdentity,
    options: AgentOptions,
    relay_mode: Option<iroh::RelayMode>,
) -> Result<(Server, AgentHost), ServerError> {
    let server_cert = identity.device_cert.clone();
    let catalogue = app_catalogue(options.catalogue, options.browsers.as_ref());
    let content = crate::content::stores(&cfg.state_root);
    let mut agents = AgentHost::new(
        identity.user_public_key,
        (options.state_dir, Arc::clone(&content)),
        options.model,
    );
    cfg.services.app_serving = true;
    let host_actions = Arc::new(HostActionsServer {
        host: HostActions {
            supervisor: SupervisorHost(agents.supervisor().clone()),
            actions: WorkspaceActions::new(catalogue),
        },
        stores: content,
        browsers: options.browsers.clone(),
    });
    let web_entry = cfg.web_entry.clone();
    let server = match relay_mode {
        Some(mode) => {
            Server::start_with_app_serving_and_relay_mode(
                cfg,
                identity,
                Some(host_actions.clone()),
                mode,
            )
            .await?
        }
        None => Server::start_with_app_serving(cfg, identity, Some(host_actions.clone())).await?,
    };
    attach_resources(
        &host_actions.host.actions,
        &server,
        &server_cert,
        web_entry,
        (options.feedback_reviewer, options.shell, options.browsers),
        Some(agents.agent_sessions()),
    )?;
    drop(host_actions);
    agents
        .start(
            HostEndpoint::iroh(server.node_addr(), server_cert),
            Arc::new(server.agent_link()),
        )
        .map_err(ServerError::Identity)?;
    Ok((server, agents))
}
