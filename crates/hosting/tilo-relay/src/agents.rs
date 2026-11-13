//! Agents on this host (feature `agents`): the link between the relay and
//! `tilo-agent-participant`'s [`AgentHost`].
//!
//! [`RelayAgentLink`] is what the supervisor asks of the relay:
//!
//! - **Approval.** A new grant is a certification request for a fresh agent
//!   key, queued by the host itself with the person's [`DelegateProposal`]. A
//!   root-holding approver answers it with the agent's certificate and the
//!   delegation together (`ApproverDecision::IssueDelegate`), so consent is one
//!   step and the host never holds the root.
//! - **Admission.** Approved delegations are registered in the relay's agent
//!   register, which confines the device to its workspaces; withdrawn ones are
//!   refused from the next access audit on. Each workspace this relay governs
//!   that the delegation names puts the agent on its governed roster
//!   (`WorkspaceOperation::DelegateMembership`), and every change to its grant is
//!   carried onto those rosters: a withdrawn or narrowed grant takes it off
//!   the workspace and its channels.
//! - **Asking first.** An action the person's grants leave to them is put
//!   to their approving device as a question for that one invocation
//!   ([`CertificationProtocol::confirm`]); the agent runs it only on the
//!   witnessed answer, and a question nobody answers in time is refused.
//!
//! [`start`] wires both into a relay: the supervisor answers the agent
//! vocabulary as the host's app-serving plane, and the host's own agents
//! reach it over iroh.

use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CallerPrincipal, CataloguePage, CatalogueQuery,
    Confirmation, ConfirmedInvocation, InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_agent_participant::workspace_actions::{
    ShellRan, Watching, WorkspaceMember, WorkspaceShell,
};
use tilo_agent_participant::{
    AdmissionDone, AgentHost, AgentHostLink, AppCatalogue, ApprovalDone, CatalogueFactory,
    FeedbackReviewer, HostActions, HostEndpoint, ModelChoice, SupervisorHost, Witness,
    WorkspaceActions, WorkspaceRecordsLink,
};
use tilo_relay_protocol::certify::{ConfirmationAnswer, PendingConfirmation};

use tilo_relay_protocol::delegation::{DelegateProposal, Delegation};
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationRequest, DeviceCapabilities, DeviceCert,
};

use crate::certification::CertificationProtocol;
use crate::daemon::{Relay, RelayError, RelayIdentity};
use crate::peer::AgentRegistry;
use tilo_content::WorkspaceStores;
use tilo_lock::RwLockRecover as _;
use crate::WorkspaceId;

/// How long an agent's approval request waits for an approver.
const APPROVAL_WAIT_NS: u64 = 10 * 60 * 1_000_000_000;

/// How long an agent's question about one invocation waits for its person.
const CONFIRMATION_WAIT_NS: u64 = 2 * 60 * 1_000_000_000;

/// The relay as an agent supervisor sees it.
#[derive(Clone)]
pub struct RelayAgentLink {
    user_public_key: [u8; 32],
    certification: CertificationProtocol,
    agents: AgentRegistry,
    /// The workspaces this relay governs, whose rosters follow each agent's grant.
    workspaces: Option<crate::workspaces::Workspaces>,
    runtime: tokio::runtime::Handle,
}

impl Relay {
    /// The link an [`AgentHost`] supervises this relay's agents through.
    /// Call it inside the runtime the relay runs on.
    #[must_use]
    pub fn agent_link(&self) -> RelayAgentLink {
        let web = self.web_context();
        RelayAgentLink {
            user_public_key: web.relay.identity.user_public_key,
            certification: web.certification.clone(),
            agents: web.relay.agents.clone(),
            workspaces: self.workspaces().cloned(),
            runtime: tokio::runtime::Handle::current(),
        }
    }
}

impl RelayAgentLink {
    /// Bring agent `device`'s place on the rosters of the governed workspaces in
    /// line with the register, off the caller's thread. `cert` is the
    /// agent's certificate when it was just approved or restored.
    fn reconcile_rooms(&self, device: [u8; 32], cert: Option<DeviceCert>) {
        if let Some(workspaces) = self.workspaces.clone() {
            self.runtime.spawn(async move {
                workspaces.reconcile_agent(device, cert.as_ref()).await;
            });
        }
    }
}

impl AgentHostLink for RelayAgentLink {
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
        self.reconcile_rooms(delegation.device_public_key, Some(cert.clone()));
        Ok(())
    }

    fn withdraw(&self, device_public_key: [u8; 32]) {
        self.agents.write_recover().insert(device_public_key, None);
        crate::authority_epoch::bump();
        // Governed workspaces take it off their channels and refuse its writes.
        self.reconcile_rooms(device_public_key, None);
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

    fn workspace_name(&self, workspace: WorkspaceId) -> Option<String> {
        self.runtime
            .block_on(self.workspaces.as_ref()?.workspace_name(workspace))
    }

    fn witness(&self, delegation: &Delegation) -> Option<Witness> {
        Some(Arc::new(RelayWitness {
            certification: self.certification.clone(),
            delegation: delegation.clone(),
            runtime: self.runtime.clone(),
        }))
    }
}

/// The person's side of one agent's actions that ask first: each is a
/// question on the person's approving devices, through the channel their
/// grants are approved on. It runs nothing itself.
struct RelayWitness {
    certification: CertificationProtocol,
    delegation: Delegation,
    runtime: tokio::runtime::Handle,
}

impl RelayWitness {
    /// The question `request` puts to the person, when `caller` is this
    /// witness's agent.
    fn question(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<PendingConfirmation, ActionHostError> {
        let agent = &self.delegation;
        // Only agents ask their person; a bot runs unattended.
        let agent_id = agent
            .delegate
            .agent_id()
            .ok_or(ActionHostError::NotPermitted)?;
        let its_own = caller.principal() == &CallerPrincipal::Agent(agent_id.to_owned())
            && caller.user_public_key() == Some(&agent.user_public_key)
            && caller.device_public_key() == Some(&agent.device_public_key);
        if !its_own {
            return Err(ActionHostError::NotPermitted);
        }
        let workspace = caller
            .workspace()
            .filter(|workspace| {
                agent
                    .scope
                    .workspaces
                    .iter()
                    .any(|granted| &granted.to_string() == *workspace)
            })
            .ok_or(ActionHostError::NotPermitted)?;
        let mut id = [0_u8; 32];
        getrandom::fill(&mut id).map_err(|error| ActionHostError::Unavailable {
            reason: error.to_string(),
        })?;
        let address = &request.target.address;
        let context = request.context.as_ref();
        Ok(PendingConfirmation {
            id,
            agent_id: agent_id.to_owned(),
            agent_label: agent.label.clone(),
            action_label: context
                .and_then(|context| context.purpose.clone())
                .unwrap_or_else(|| address.action.clone()),
            resource_label: context
                .and_then(|context| context.resource.as_ref())
                .map_or_else(
                    || address.resource.clone(),
                    |resource| resource.label.clone(),
                ),
            risk: context
                .and_then(|context| {
                    context
                        .facets
                        .iter()
                        .find(|facet| facet.key == tilo_actions::RISK_FACET)
                })
                .map_or_else(|| "destructive".to_owned(), |facet| facet.value.clone()),
            workspace: workspace.to_owned(),
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(CONFIRMATION_WAIT_NS),
            ..PendingConfirmation::default()
        })
    }
}

impl ActionHost for RelayWitness {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Err(ActionHostError::UnknownAction))
    }

    fn invoke(
        &self,
        _: &Caller,
        _: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        Pending::ready(Err(ActionHostError::UnknownAction))
    }

    fn observe(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &Caller,
        _: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        tilo_actions::remote::Pending::ready(Err(ActionHostError::UnknownInvocation))
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionHostError>> {
        let question = match self.question(caller, request) {
            Ok(question) => question,
            Err(error) => return Pending::ready(Err(error)),
        };
        let (completer, answer) = tilo_actions::remote::pending();
        let certification = self.certification.clone();
        self.runtime.spawn(async move {
            let token = hex::encode(question.id);
            completer.complete(match certification.confirm(question).await {
                // The question shows no draft, so nothing in it is changed.
                Ok(ConfirmationAnswer::AllowOnce) => Ok(ConfirmedInvocation::unchanged(
                    Confirmation::Witnessed(token),
                )),
                Ok(ConfirmationAnswer::Deny) => Err(ActionHostError::NotPermitted),
                Err(_) => Err(ActionHostError::confirmation_required()),
            });
        });
        answer
    }
}

/// What [`start`] runs agents with.
pub struct AgentOptions {
    /// Grants and agents are kept here.
    pub state_dir: PathBuf,
    /// The model natural-language tasks are planned with.
    pub model: ModelChoice,
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
    Ok(Arc::new(
        match crate::placed_shells::PlacedShells::shared_from_env()? {
            Some(placed) => HostShells::Placed(placed),
            None => HostShells::Here(ush::workspace_shell::WorkspaceShells::from_env(state)),
        },
    ))
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
    /// Ask VM hosts' members, if any, as `relay`'s own device.
    fn serve_from(&self, relay: &Relay) {
        if let (Self::Placed(placed), Some(workspaces)) = (self, relay.workspaces()) {
            placed.ask_from(workspaces.plane(), relay.endpoint());
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

/// The host's app-serving plane: agents and the workspaces' page actions,
/// keeping what they make in the host's content.
struct HostActionsPlane {
    host: HostActions,
    stores: Arc<WorkspaceStores>,
    /// The Browser this host places for its workspaces, when it places one.
    browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>,
}

/// The app-serving plane of a host that runs no agents: the workspaces' page
/// actions alone. A member's own actions on a workspace record (linking their
/// account on the workspace's Discord from the profile card) need nothing more.
struct WorkspaceRecordsPlane {
    actions: WorkspaceActions,
    stores: Arc<WorkspaceStores>,
    /// The Browser this host places for its workspaces, when it places one.
    browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>,
}

impl crate::AppServingPlane for WorkspaceRecordsPlane {
    fn serve(
        &self,
        workspace_id: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        let agents = match &message {
            tilo_actions::remote::RemoteActionMessage::CatalogueRequest { query, .. } => {
                query.provider.as_deref() == Some(tilo_relay_protocol::agents::AGENTS_PROVIDER)
            }
            tilo_actions::remote::RemoteActionMessage::Invoke(request) => {
                request.target.provider == tilo_relay_protocol::agents::AGENTS_PROVIDER
            }
            _ => false,
        };
        if agents {
            return None;
        }
        serve_with_browser(
            self.browsers.as_ref(),
            workspace_id,
            caller,
            message,
            |message| self.actions.serve(workspace_id, caller, message),
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

    fn content(&self) -> Option<Arc<WorkspaceStores>> {
        Some(Arc::clone(&self.stores))
    }

    fn forget_workspace(&self, workspace_id: WorkspaceId) {
        self.actions.forget_workspace(workspace_id);
    }

    fn run_arrived(&self, workspace_id: WorkspaceId, caller: &tilo_actions::remote::Caller) {
        self.actions.run_arrived(workspace_id, caller);
    }
}

impl crate::AppServingPlane for HostActionsPlane {
    fn serve(
        &self,
        workspace_id: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        serve_with_browser(
            self.browsers.as_ref(),
            workspace_id,
            caller,
            message,
            |message| self.host.serve(workspace_id, caller, message),
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

    fn content(&self) -> Option<Arc<WorkspaceStores>> {
        Some(Arc::clone(&self.stores))
    }

    fn forget_workspace(&self, workspace_id: WorkspaceId) {
        self.host.actions.forget_workspace(workspace_id);
    }

    fn run_arrived(&self, workspace_id: WorkspaceId, caller: &tilo_actions::remote::Caller) {
        self.host.actions.run_arrived(workspace_id, caller);
    }
}

/// Serve a workspace's message, placing its Browser after an answered
/// `app.so.tilo.browser#open`: the open answers once the window is live, and
/// a Browser this host cannot place is refused in words that say so rather
/// than as an app it does not know.
fn serve_with_browser(
    browsers: Option<&Arc<crate::hosted_browser::HostedBrowsers>>,
    workspace_id: WorkspaceId,
    _caller: &tilo_actions::remote::Caller,
    message: tilo_actions::remote::RemoteActionMessage,
    serve: impl FnOnce(
        tilo_actions::remote::RemoteActionMessage,
    ) -> Option<Pending<tilo_actions::remote::RemoteActionMessage>>,
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
        return Some(serve(message)?.map(move |message| {
            crate::hosted_browser::placed_linux_app(
                &browsers,
                workspace_id,
                invocation,
                &launch,
                message,
            )
        }));
    }
    let opens = match &message {
        RemoteActionMessage::Invoke(request) if crate::hosted_browser::opens_browser(request) => {
            Some(request.invocation)
        }
        _ => None,
    };
    let Some(invocation) = opens else {
        return serve(message);
    };
    let Some(browsers) = browsers else {
        return Some(Pending::ready(crate::hosted_browser::refused(
            invocation,
            crate::hosted_browser::unplaced(),
        )));
    };
    let browsers = Arc::clone(browsers);
    Some(serve(message)?.map(move |message| {
        crate::hosted_browser::placed(&browsers, workspace_id, invocation, message)
    }))
}

/// The relay's own device, as the workspace actions service joins its workspaces.
struct RelayRecordsLink {
    user_public_key: [u8; 32],
    member: WorkspaceMember,
    workspaces: Option<crate::workspaces::Workspaces>,
    browser_origin: Arc<std::sync::RwLock<Option<Arc<String>>>>,
    web_entry: Option<String>,
    feedback_reviewer: Option<Arc<dyn FeedbackReviewer>>,
    shell: Option<Arc<dyn WorkspaceShell>>,
    stores: Arc<WorkspaceStores>,
    /// Where a person is asked about an action run for them.
    certification: CertificationProtocol,
}

impl WorkspaceRecordsLink for RelayRecordsLink {
    /// On the approving devices of the person `caller` is or acts for: the
    /// host's own person's, or a member's own root, as their device requests
    /// are answered.
    fn ask_person(
        &self,
        _workspace: WorkspaceId,
        caller: &Caller,
        question: tilo_actions::remote::ConfirmationQuestion,
    ) -> Option<Pending<Result<tilo_actions::remote::ConfirmationReply, ActionHostError>>> {
        let person = *caller.user_public_key()?;
        let agent_id = match caller.principal() {
            CallerPrincipal::Agent(agent) => agent.clone(),
            _ => String::new(),
        };
        let pending = PendingConfirmation {
            id: question.id,
            agent_id,
            agent_label: question.device_label,
            run: question.run,
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
        self.member.runtime.spawn(async move {
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
                Err(_) => Err(ActionHostError::confirmation_required()),
            });
        });
        Some(answer)
    }

    fn workspace_content(&self, workspace: WorkspaceId) -> Option<tilo_content::ContentStore> {
        let identity = self
            .member
            .runtime
            .block_on(self.workspaces.as_ref()?.workspace_identity(workspace))?;
        self.stores.get(&identity).ok()
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

    fn member(&self) -> WorkspaceMember {
        self.member.clone()
    }

    fn workspace_name(&self, workspace: WorkspaceId) -> Option<String> {
        self.member
            .runtime
            .block_on(self.workspaces.as_ref()?.workspace_name(workspace))
    }

    fn admits(&self, workspace: WorkspaceId, caller: &tilo_actions::remote::Caller) -> bool {
        self.workspaces.as_ref().is_some_and(|workspaces| {
            self.member
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
    ) -> Option<tilo_agent_participant::workspace_actions::ViewReadPolicy> {
        self.member.runtime.block_on(
            self.workspaces
                .as_ref()?
                .resource_view_policy(workspace, caller, app, record),
        )
    }

    fn workspace_policy(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
    ) -> Option<tilo_agent_participant::workspace_actions::ViewReadPolicy> {
        self.member.runtime.block_on(
            self.workspaces
                .as_ref()?
                .workspace_policy(workspace, caller),
        )
    }

    fn view_document(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        source: &tilo_peer_state::document_frame::DocumentRoute,
    ) -> Result<tilo_agent_participant::workspace_actions::ViewDocument, String> {
        self.member.runtime.block_on(
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
    ) -> Option<Result<channel_crypto::ChannelContentKeys, String>> {
        self.member
            .runtime
            .block_on(self.workspaces.as_ref()?.own_route_keys(route, create))
    }

    fn records(&self, channel: &str) -> Option<Vec<(String, String)>> {
        let workspaces = self.workspaces.as_ref()?;
        self.member.runtime.block_on(workspaces.records(channel))
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
            host_device: hex::encode(self.member.endpoint.host_cert.device_public_key),
            provider: provider.to_owned(),
            web_entry: self.web_entry.clone(),
        })
    }

    fn bot_personas(&self, workspace: WorkspaceId, installation: &str) -> bool {
        self.workspaces.as_ref().is_some_and(|workspaces| {
            self.member
                .runtime
                .block_on(workspaces.bot_personas(workspace, installation))
        })
    }

    /// What the workspace's host keeps ([`crate::workspaces::Workspaces::kept_content`]):
    /// a bridged person's avatar its bots fetched, a file or a feedback
    /// report's picture a member posted. A relay keeping no content for the
    /// workspace answers none, which refuses an appearance naming an avatar.
    fn content_blob(&self, workspace: WorkspaceId, hash: &str) -> Option<Vec<u8>> {
        let workspaces = self.workspaces.as_ref()?;
        self.member
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

/// Start `cfg`'s relay serving its workspaces' records to pages, without agents:
/// the host mounts a record of an app in `catalogue` (this machine's
/// registry when `None`) and runs an action on it for whoever the workspace's
/// policy lets ask, as it does beside agents in [`start`]. Its workspaces'
/// reporters may have `feedback_reviewer` look over their reports.
///
/// # Errors
/// Why the relay did not start.
pub async fn start_serving_records(
    mut cfg: crate::RelayConfig,
    identity: RelayIdentity,
    catalogue: Option<CatalogueFactory>,
    feedback_reviewer: Option<Arc<dyn FeedbackReviewer>>,
    shell: Option<Arc<HostShells>>,
    browsers: Option<Arc<crate::hosted_browser::HostedBrowsers>>,
) -> Result<Relay, RelayError> {
    let host_cert = identity.device_cert.clone();
    let catalogue = app_catalogue(catalogue, browsers.as_ref());
    cfg.services.app_serving = true;
    let plane = Arc::new(WorkspaceRecordsPlane {
        actions: WorkspaceActions::new(catalogue),
        stores: crate::content::stores(&cfg.state_root),
        browsers: browsers.clone(),
    });
    let web_entry = cfg.web_entry.clone();
    let relay = Relay::start_with_app_serving(cfg, identity, Some(plane.clone())).await?;
    attach_records(
        &plane.actions,
        &relay,
        &host_cert,
        web_entry,
        (feedback_reviewer, shell, browsers),
    )?;
    Ok(relay)
}

/// Give `workspaces` the records `relay` holds, joined as its own device.
fn attach_records(
    actions: &WorkspaceActions,
    relay: &Relay,
    host_cert: &DeviceCert,
    web_entry: Option<String>,
    (feedback_reviewer, shell, browsers): Lent,
) -> Result<(), RelayError> {
    if let Some(shell) = &shell {
        shell.serve_from(relay);
    }
    if let Some(browsers) = &browsers {
        // A session in a workspace's VM joins as a device of the person's,
        // which a stand-in certifies: the one this host keeps for them, else
        // a host that is itself one.
        let user = relay.workspace_records().user_public_key();
        let (key, cert) = relay.stand_in(&user).unwrap_or_else(|| {
            (
                relay.workspace_records().signing_key().clone(),
                host_cert.clone(),
            )
        });
        browsers.serve_from(crate::hosted_browser::Authority {
            user_public_key: user,
            cert,
            key,
            // The sessions in the environments dial this host from a machine
            // that reaches the public internet and nothing of this network:
            // the address they are given is the public one.
            relay: crate::hosted_browser::relay_address().map_err(RelayError::Identity)?,
            workspaces: relay.workspaces().cloned(),
            environments: relay.web_context().relay.environments.clone(),
            runtime: tokio::runtime::Handle::current(),
            certification: relay.confirmations(),
            hand: relay.hand(),
        });
        // The VM hosts start the sessions when this host's own device asks.
        if let Some(workspaces) = relay.workspaces() {
            browsers.ask_from(workspaces.plane(), relay.endpoint());
        }
    }
    let shell = shell.map(|shell| shell as Arc<dyn WorkspaceShell>);
    actions
        .attach(Arc::new(RelayRecordsLink {
            shell,
            user_public_key: relay.workspace_records().user_public_key(),
            workspaces: relay.workspaces().cloned(),
            browser_origin: Arc::clone(&relay.browser_origin),
            web_entry,
            feedback_reviewer,
            stores: relay.content(),
            certification: relay.web_context().certification.clone(),
            member: WorkspaceMember {
                endpoint: HostEndpoint::iroh(relay.node_addr(), host_cert.clone()),
                key: relay.workspace_records().signing_key().clone(),
                runtime: tokio::runtime::Handle::current(),
            },
        }))
        .map_err(RelayError::Identity)
}

/// Start `cfg`'s relay with an agent supervisor as its app-serving plane,
/// then start the supervisor's agents: those the state directory says were
/// running resume.
pub async fn start(
    cfg: crate::RelayConfig,
    identity: RelayIdentity,
    options: AgentOptions,
) -> Result<(Relay, AgentHost), RelayError> {
    start_inner(cfg, identity, options, None).await
}

/// Start the app-serving host with an explicit transport relay policy.
///
/// # Errors
/// The same as [`start`].
pub async fn start_with_relay_mode(
    cfg: crate::RelayConfig,
    identity: RelayIdentity,
    options: AgentOptions,
    relay_mode: iroh::RelayMode,
) -> Result<(Relay, AgentHost), RelayError> {
    start_inner(cfg, identity, options, Some(relay_mode)).await
}

async fn start_inner(
    mut cfg: crate::RelayConfig,
    identity: RelayIdentity,
    options: AgentOptions,
    relay_mode: Option<iroh::RelayMode>,
) -> Result<(Relay, AgentHost), RelayError> {
    let host_cert = identity.device_cert.clone();
    let catalogue = app_catalogue(options.catalogue, options.browsers.as_ref());
    let content = crate::content::stores(&cfg.state_root);
    let mut agents = AgentHost::new(
        identity.user_public_key,
        (options.state_dir, Arc::clone(&content)),
        options.model,
        Arc::clone(&catalogue),
    );
    cfg.services.app_serving = true;
    let plane = Arc::new(HostActionsPlane {
        host: HostActions {
            supervisor: SupervisorHost(agents.supervisor().clone()),
            actions: WorkspaceActions::new(catalogue),
        },
        stores: content,
        browsers: options.browsers.clone(),
    });
    let web_entry = cfg.web_entry.clone();
    let relay = match relay_mode {
        Some(mode) => {
            Relay::start_with_app_serving_and_relay_mode(cfg, identity, Some(plane.clone()), mode)
                .await?
        }
        None => Relay::start_with_app_serving(cfg, identity, Some(plane.clone())).await?,
    };
    attach_records(
        &plane.host.actions,
        &relay,
        &host_cert,
        web_entry,
        (options.feedback_reviewer, options.shell, options.browsers),
    )?;
    drop(plane);
    agents
        .start(
            HostEndpoint::iroh(relay.node_addr(), host_cert),
            Arc::new(relay.agent_link()),
        )
        .map_err(RelayError::Identity)?;
    Ok((relay, agents))
}
