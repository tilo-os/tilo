//! Whether an agent may still act in the workspace: what a run of it checks
//! before each action.

use super::SystemTime;
use super::facades::PeerWorkspace;
use crate::join::task_delegation::TaskDelegation;
use crate::join::WorkspaceId;

impl PeerWorkspace<'_> {
    /// Whether `agent_id` is still admitted to the workspace. A check of existing
    /// capabilities, never a grant.
    ///
    /// A collaborator is admitted by its live grant on the workspace. A task agent
    /// is admitted by the task delegation its run carries (`delegation`, as
    /// `TaskDelegation::encode` wrote it): it verifies as a chain from the
    /// workspace's device list, whose certificate for the signing device is
    /// current, not revoked and its person's, a member here.
    pub fn check_admitted_agent(
        self,
        agent_id: &str,
        delegation: Option<&[u8]>,
        now: SystemTime,
    ) -> Result<(), String> {
        let doc = self
            .peer
            .core
            .workspace_doc
            .as_deref()
            .ok_or("admitted workspace document unavailable")?;
        let policy = crate::workspace::policy_of(doc, &self.peer.core.workspace);
        if let Some(delegation) = delegation {
            let task = TaskDelegation::decode(delegation).map_err(|error| error.to_string())?;
            if task.agent_id != agent_id {
                return Err("the run's delegation names another agent".into());
            }
            let cert = self
                .peer
                .core
                .certificates
                .get(&task.device_public_key)
                .ok_or("the delegating device is not in the workspace's device list")?;
            if self
                .peer
                .core
                .revocations
                .is_revoked(&cert.user_public_key, &task.device_public_key)
            {
                return Err("the delegating device has been revoked".into());
            }
            if policy
                .active_role(&hex::encode(task.user_public_key))
                .is_none()
            {
                return Err("the delegating person is not a member".into());
            }
            return task
                .verify(
                    cert.as_ref(),
                    WorkspaceId::parse(&self.peer.core.workspace)
                        .ok_or("the workspace is not a workspace id")?,
                    super::now_ns(now),
                )
                .map_err(|error| format!("the task delegation is refused: {error}"));
        }
        if !super::agent_is_member(&policy, agent_id, now) {
            return Err("the agent is no longer admitted".into());
        }
        Ok(())
    }
}
