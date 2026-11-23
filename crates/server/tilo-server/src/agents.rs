//! Agents on this host (feature `agents`): the link between the server and
//! `tilo-agent-session`'s [`AgentService`](tilo_agent_session::AgentService).
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

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_agent_session::{AgentHostLink, ApprovalDone};

use tilo_server_protocol::delegation::DelegateProposal;
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationRequest, DeviceCapabilities, DeviceCert,
};

use crate::certification::CertificationProtocol;
use crate::daemon::Server;
use crate::registry::AgentRegistry;
use tilo_lock::RwLockRecover as _;

mod roster;

/// How long an agent's approval request waits for an approver.
const APPROVAL_WAIT_NS: u64 = 10 * 60 * 1_000_000_000;

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
    /// The link an [`AgentService`](tilo_agent_session::AgentService) supervises this server's agents through.
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
