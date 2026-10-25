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
//!   register, which confines the device to its rooms; withdrawn ones are
//!   refused from the next access audit on. Each room this relay governs
//!   that the delegation names puts the agent on its governed roster
//!   (`RoomOperation::DelegateMembership`), and every change to its grant is
//!   carried onto those rosters: a withdrawn or narrowed grant takes it off
//!   the room and its channels.
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
    Confirmation, InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_agent_participant::room_actions::RoomMember;
use tilo_agent_participant::{
    AgentHost, AgentHostLink, AppCatalogue, ApprovalDone, CatalogueFactory, HostActions,
    HostEndpoint, ModelChoice, RoomActions, RoomRecordsLink, SupervisorHost, Witness,
};
use tilo_relay_protocol::certify::{ConfirmationAnswer, PendingConfirmation};

use tilo_relay_protocol::delegation::{DelegateProposal, Delegation};
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationRequest, DeviceCapabilities, DeviceCert,
};

use crate::certification::CertificationProtocol;
use crate::daemon::{Relay, RelayError, RelayIdentity};
use crate::peer::AgentRegistry;
use tilo_lock::RwLockRecover as _;

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
    /// The rooms this relay governs, whose rosters follow each agent's grant.
    rooms: Option<crate::rooms::Rooms>,
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
            rooms: self.rooms().cloned(),
            runtime: tokio::runtime::Handle::current(),
        }
    }
}

impl RelayAgentLink {
    /// Bring agent `device`'s place on the rosters of the governed rooms in
    /// line with the register, off the caller's thread. `cert` is the
    /// agent's certificate when it was just approved or restored.
    fn reconcile_rooms(&self, device: [u8; 32], cert: Option<DeviceCert>) {
        if let Some(rooms) = self.rooms.clone() {
            self.runtime.spawn(async move {
                rooms.reconcile_agent(device, cert.as_ref()).await;
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
            requested: DeviceCapabilities {
                can_own_workspaces: false,
                can_admit_devices: false,
                can_act_as_host: false,
                can_read_all_workspaces: false,
                foreign: false,
                can_administer_devices: false,
            },
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
        delegation
            .verify(&self.user_public_key, tilo_clock::unix_now_ns())
            .map_err(|error| error.to_string())?;
        self.agents
            .write_recover()
            .insert(delegation.device_public_key, Some(delegation.clone()));
        // A replacement may narrow an earlier delegation's rooms.
        crate::authority_epoch::bump();
        if cert.device_public_key != delegation.device_public_key {
            return Err("the delegation names another device".into());
        }
        // The governed rooms it names put it on their rosters.
        self.reconcile_rooms(delegation.device_public_key, Some(cert.clone()));
        Ok(())
    }

    fn withdraw(&self, device_public_key: [u8; 32]) {
        self.agents.write_recover().insert(device_public_key, None);
        crate::authority_epoch::bump();
        // Governed rooms take it off their channels and refuse its writes.
        self.reconcile_rooms(device_public_key, None);
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
        let room = caller
            .room()
            .filter(|room| agent.scope.rooms.iter().any(|granted| granted == room))
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
            room: room.to_owned(),
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(CONFIRMATION_WAIT_NS),
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
    ) -> Pending<Result<Confirmation, ActionHostError>> {
        let question = match self.question(caller, request) {
            Ok(question) => question,
            Err(error) => return Pending::ready(Err(error)),
        };
        let (completer, answer) = tilo_actions::remote::pending();
        let certification = self.certification.clone();
        self.runtime.spawn(async move {
            let token = hex::encode(question.id);
            completer.complete(match certification.confirm(question).await {
                Ok(ConfirmationAnswer::AllowOnce) => Ok(Confirmation::Witnessed(token)),
                Ok(ConfirmationAnswer::Deny) => Err(ActionHostError::NotPermitted),
                Err(_) => Err(ActionHostError::ConfirmationRequired),
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
}

/// The host's app-serving plane: agents and the rooms' page actions.
struct HostActionsPlane(HostActions);

/// The app-serving plane of a host that runs no agents: the rooms' page
/// actions alone. A member's own actions on a room record (linking their
/// account on the room's Discord from the profile card) need nothing more.
struct RoomRecordsPlane(RoomActions);

impl crate::AppServingPlane for RoomRecordsPlane {
    fn serve(
        &self,
        workspace_id: u64,
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
        self.0.serve(workspace_id, caller, message)
    }
}

impl crate::AppServingPlane for HostActionsPlane {
    fn serve(
        &self,
        workspace_id: u64,
        caller: &tilo_actions::remote::Caller,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>> {
        self.0.serve(workspace_id, caller, message)
    }
}

/// The relay's own device, as the room actions service joins its rooms.
struct RelayRecordsLink {
    user_public_key: [u8; 32],
    member: RoomMember,
    rooms: Option<crate::rooms::Rooms>,
    browser_origin: Arc<std::sync::RwLock<Option<Arc<String>>>>,
    web_entry: Option<String>,
}

impl RoomRecordsLink for RelayRecordsLink {
    fn user_public_key(&self) -> [u8; 32] {
        self.user_public_key
    }

    fn member(&self) -> RoomMember {
        self.member.clone()
    }

    fn view_read_policy(
        &self,
        workspace: u64,
        caller: &tilo_actions::remote::Caller,
        app: &str,
        record: &str,
    ) -> Option<tilo_agent_participant::room_actions::ViewReadPolicy> {
        self.member.runtime.block_on(
            self.rooms
                .as_ref()?
                .resource_view_policy(workspace, caller, app, record),
        )
    }

    fn view_document(
        &self,
        workspace: u64,
        caller: &tilo_actions::remote::Caller,
        source: &tilo_browser_peer::DocumentRoute,
    ) -> Result<tilo_agent_participant::room_actions::ViewDocument, String> {
        self.member.runtime.block_on(
            self.rooms
                .as_ref()
                .ok_or("governed rooms unavailable")?
                .resource_view_document(workspace, caller, source),
        )
    }

    fn steward_keys(
        &self,
        route: &tilo_browser_peer::DocumentRoute,
        create: bool,
    ) -> Option<Result<tilo_browser_peer::ChannelContentKeys, String>> {
        self.member
            .runtime
            .block_on(self.rooms.as_ref()?.steward_endpoint_keys(route, create))
    }

    fn view_locator(
        &self,
        room: &str,
        provider: &str,
    ) -> Option<tilo_actions::remote::ResourceViewLocator> {
        let origin = self.browser_origin.read().ok()?.clone()?;
        Some(tilo_actions::remote::ResourceViewLocator {
            origin: (*origin).clone(),
            room: room.to_owned(),
            host_device: hex::encode(self.member.endpoint.host_cert.device_public_key),
            provider: provider.to_owned(),
            web_entry: self.web_entry.clone(),
        })
    }

    fn bot_personas(&self, workspace: u64, installation: &str) -> bool {
        self.rooms.as_ref().is_some_and(|rooms| {
            self.member
                .runtime
                .block_on(rooms.bot_personas(workspace, installation))
        })
    }

    /// What the room's host keeps ([`crate::rooms::Rooms::kept_content`]):
    /// a bridged person's avatar its bots fetched, a file or a feedback
    /// report's picture a member posted. A relay keeping no content for the
    /// room answers none, which refuses an appearance naming an avatar.
    fn content_blob(&self, workspace: u64, hash: &str) -> Option<Vec<u8>> {
        let rooms = self.rooms.as_ref()?;
        self.member
            .runtime
            .block_on(rooms.kept_content(workspace, hash))
    }
}

/// Start `cfg`'s relay serving its rooms' records to pages, without agents:
/// the host mounts a record of an app in `catalogue` (this machine's
/// registry when `None`) and runs an action on it for whoever the room's
/// policy lets ask, as it does beside agents in [`start`].
///
/// # Errors
/// Why the relay did not start.
pub async fn start_serving_records(
    mut cfg: crate::RelayConfig,
    identity: RelayIdentity,
    catalogue: Option<CatalogueFactory>,
) -> Result<Relay, RelayError> {
    let host_cert = identity.device_cert.clone();
    let catalogue = catalogue.unwrap_or_else(|| Arc::new(AppCatalogue::native));
    cfg.services.app_serving = true;
    let plane = Arc::new(RoomRecordsPlane(RoomActions::new(catalogue)));
    let web_entry = cfg.web_entry.clone();
    let relay = Relay::start_with_app_serving(cfg, identity, Some(plane.clone())).await?;
    attach_records(&plane.0, &relay, &host_cert, web_entry)?;
    Ok(relay)
}

/// Give `rooms` the records `relay` holds, joined as its own device.
fn attach_records(
    rooms: &RoomActions,
    relay: &Relay,
    host_cert: &DeviceCert,
    web_entry: Option<String>,
) -> Result<(), RelayError> {
    rooms
        .attach(Arc::new(RelayRecordsLink {
            user_public_key: relay.room_records().user_public_key(),
            rooms: relay.rooms().cloned(),
            browser_origin: Arc::clone(&relay.browser_origin),
            web_entry,
            member: RoomMember {
                endpoint: HostEndpoint::iroh(relay.node_addr(), host_cert.clone()),
                key: relay.room_records().signing_key().clone(),
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
    let catalogue = options
        .catalogue
        .unwrap_or_else(|| Arc::new(AppCatalogue::native));
    let mut agents = AgentHost::new(
        identity.user_public_key,
        options.state_dir,
        options.model,
        Arc::clone(&catalogue),
    );
    cfg.services.app_serving = true;
    let plane = Arc::new(HostActionsPlane(HostActions {
        supervisor: SupervisorHost(agents.supervisor().clone()),
        rooms: RoomActions::new(catalogue),
    }));
    let web_entry = cfg.web_entry.clone();
    let relay = match relay_mode {
        Some(mode) => {
            Relay::start_with_app_serving_and_relay_mode(cfg, identity, Some(plane.clone()), mode)
                .await?
        }
        None => Relay::start_with_app_serving(cfg, identity, Some(plane.clone())).await?,
    };
    attach_records(&plane.0.rooms, &relay, &host_cert, web_entry)?;
    agents
        .start(
            HostEndpoint::iroh(relay.node_addr(), host_cert),
            Arc::new(relay.agent_link()),
        )
        .map_err(RelayError::Identity)?;
    Ok((relay, agents))
}
