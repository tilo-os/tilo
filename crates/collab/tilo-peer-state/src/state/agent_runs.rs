//! Whether an agent may still act in the workspace: what a run of it checks
//! before each action.

use super::SystemTime;
use super::facades::PeerWorkspace;
use crate::join::WorkspaceId;
use crate::join::agent_cert::AgentCert;

impl PeerWorkspace<'_> {
    /// Whether `agent_id` is still admitted to the workspace. A check of existing
    /// capabilities, never a grant.
    ///
    /// An agent is admitted by its live grant on the workspace, or by the
    /// agent certificate its run carries (`certificate`, as
    /// `AgentCert::encode` wrote it): it verifies as a chain from the
    /// workspace's device list, whose certificate for the signing device is
    /// current, not revoked and its person's, a member here.
    pub fn check_admitted_agent(
        self,
        agent_id: &str,
        certificate: Option<&[u8]>,
        now: SystemTime,
    ) -> Result<(), String> {
        let doc = self
            .peer
            .core
            .workspace_doc
            .as_deref()
            .ok_or("admitted workspace document unavailable")?;
        let policy = crate::workspace::policy_of(doc, &self.peer.core.workspace);
        if let Some(certificate) = certificate {
            let agent_cert = AgentCert::decode(certificate).map_err(|error| error.to_string())?;
            if agent_cert.agent_id != agent_id {
                return Err("the run's certificate names another agent".into());
            }
            let cert = self
                .peer
                .core
                .certificates
                .get(&agent_cert.device_public_key)
                .ok_or("the certifying device is not in the workspace's device list")?;
            if self
                .peer
                .core
                .revocations
                .is_revoked(&cert.user_public_key, &agent_cert.device_public_key)
            {
                return Err("the certifying device has been revoked".into());
            }
            if policy
                .active_role(&hex::encode(agent_cert.user_public_key))
                .is_none()
            {
                return Err("the certifying person is not a member".into());
            }
            return agent_cert
                .verify(
                    cert.as_ref(),
                    WorkspaceId::parse(&self.peer.core.workspace)
                        .ok_or("the workspace is not a workspace id")?,
                    super::now_ns(now),
                )
                .map_err(|error| format!("the agent certificate is refused: {error}"));
        }
        if !super::agent_is_member(&policy, agent_id, now) {
            return Err("the agent is no longer admitted".into());
        }
        Ok(())
    }
}
