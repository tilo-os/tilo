//! Agents on this host (feature `agents`): the link between the relay and
//! `tilo-agent-participant`'s [`AgentHost`].
//!
//! [`RelayAgentLink`] is what the supervisor asks of the relay:
//!
//! - **Approval.** A new grant is a certification request for a fresh agent
//!   key, queued by the host itself with the person's [`AgentProposal`]. A
//!   root-holding approver answers it with the agent's certificate and the
//!   delegation together (`ApproverDecision::IssueAgent`), so consent is one
//!   step and the host never holds the root.
//! - **Admission.** Approved delegations are registered in the relay's agent
//!   register, which confines the device to its rooms; withdrawn ones are
//!   refused from the next access audit on.
//!
//! [`start`] wires both into a relay: the supervisor answers the agent
//! vocabulary as the host's app-serving plane, and the host's own agents
//! reach it over iroh.

use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_agent_participant::room_actions::RoomMember;
use tilo_agent_participant::{
    AgentHost, AgentHostLink, AppCatalogue, ApprovalDone, CatalogueFactory, HostActions,
    HostEndpoint, ModelChoice, RoomActions, RoomRecordsLink, SupervisorHost,
};

use tilo_relay_protocol::agents::{AgentDelegation, AgentProposal};
use tilo_user_identity::{CERTIFICATION_SCHEMA_VERSION, CertificationRequest, DeviceCapabilities};

use crate::certification::CertificationProtocol;
use crate::daemon::{Relay, RelayError, RelayIdentity};
use crate::peer::AgentRegistry;

/// How long an agent's approval request waits for an approver.
const APPROVAL_WAIT_NS: u64 = 10 * 60 * 1_000_000_000;

/// The relay as an agent supervisor sees it.
#[derive(Clone)]
pub struct RelayAgentLink {
    user_public_key: [u8; 32],
    certification: CertificationProtocol,
    agents: AgentRegistry,
    runtime: tokio::runtime::Handle,
}

impl Relay {
    /// The link an [`AgentHost`] supervises this relay's agents through.
    /// Call it inside the runtime the relay runs on.
    #[must_use]
    pub fn agent_link(&self) -> RelayAgentLink {
        let web = self.web_context();
        RelayAgentLink {
            user_public_key: web.relay.own_user_pk,
            certification: web.certification.clone(),
            agents: web.relay.agents.clone(),
            runtime: tokio::runtime::Handle::current(),
        }
    }
}

impl AgentHostLink for RelayAgentLink {
    fn request_approval(
        &self,
        key: &SigningKey,
        proposal: &AgentProposal,
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
            issued_at_ns: now,
            expires_at_ns: now.saturating_add(APPROVAL_WAIT_NS),
            signature: [0; 64],
        }
        .sign(key)
        .map_err(|error| error.to_string())?;
        let (digest, pairing_code) =
            self.certification
                .enqueue_agent(request, proposal.clone(), now)?;
        let certification = self.certification.clone();
        self.runtime.spawn(async move {
            done(certification.agent_answer(&digest).await);
        });
        Ok(pairing_code)
    }

    fn register(&self, delegation: &AgentDelegation) -> Result<(), String> {
        delegation
            .verify(&self.user_public_key, tilo_clock::unix_now_ns())
            .map_err(|error| error.to_string())?;
        self.agents
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(delegation.agent_device_public_key, Some(delegation.clone()));
        // A replacement may narrow an earlier delegation's rooms.
        crate::authority_epoch::bump();
        Ok(())
    }

    fn withdraw(&self, agent_device_public_key: [u8; 32]) {
        self.agents
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(agent_device_public_key, None);
        crate::authority_epoch::bump();
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
    browser_page: Option<String>,
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
            browser_page: self.browser_page.clone(),
        })
    }
}

/// Start `cfg`'s relay with an agent supervisor as its app-serving plane,
/// then start the supervisor's agents: those the state directory says were
/// running resume.
///
/// # Errors
///
/// Returns why the relay did not start or the agent state could not be read.
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
    let browser_page = cfg.resource_browser_page.clone();
    let relay = match relay_mode {
        Some(mode) => {
            Relay::start_with_app_serving_and_relay_mode(cfg, identity, Some(plane.clone()), mode)
                .await?
        }
        None => Relay::start_with_app_serving(cfg, identity, Some(plane.clone())).await?,
    };
    plane
        .0
        .rooms
        .attach(Arc::new(RelayRecordsLink {
            user_public_key: relay.room_records().user_public_key(),
            rooms: relay.rooms().cloned(),
            browser_origin: Arc::clone(&relay.browser_origin),
            browser_page,
            member: RoomMember {
                endpoint: HostEndpoint::iroh(relay.node_addr(), host_cert.clone()),
                key: relay.room_records().signing_key().clone(),
                runtime: tokio::runtime::Handle::current(),
            },
        }))
        .map_err(RelayError::Identity)?;
    let link = Arc::new(relay.agent_link());
    agents
        .start(HostEndpoint::iroh(relay.node_addr(), host_cert), link)
        .map_err(RelayError::Identity)?;
    Ok((relay, agents))
}
