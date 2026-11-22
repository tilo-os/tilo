//! The runtime's broker behind the workspace host it runs: the record actions
//! of the profile's workspace, answered for the person's keys that entered it
//! there ([`crate::key_sessions`]).
//!
//! A key's run is admitted as an MCP client of this runtime is: the profile's
//! person on this device, with an agent named after the key acting under a key of its own.

use super::services::RuntimeActionProvider;
use collab_types::WorkspaceId;
use std::sync::Arc;
use tilo_actions::remote::Caller;
use tilo_user_identity::DeviceCert;

/// The profile's workspace's actions, as the keys of its person reach them.
pub(super) struct HostRuns {
    pub(super) provider: RuntimeActionProvider,
    /// The profile's workspace, by its UUID.
    pub(super) workspace: String,
    pub(super) actor: String,
    pub(super) policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
}

impl crate::key_sessions::Sessions for HostRuns {
    fn workspace(&self, workspace_id: WorkspaceId) -> Option<String> {
        (tilo_desktop_model::WorkspaceId::parse(&self.workspace) == Some(workspace_id))
            .then(|| self.workspace.clone())
    }

    fn open(
        &self,
        workspace: &str,
        key: &DeviceCert,
    ) -> Result<(crate::key_sessions::SessionHost, Caller), String> {
        let (run, caller) = self
            .provider
            .agent_session(workspace, &self.actor, &self.policy_time, |_| {
                (
                    tilo_mcp::devices::agent_id("agent", key),
                    key.device_label.clone(),
                )
            })
            .map_err(|error| error.to_string())?;
        Ok((Arc::new(run), caller))
    }
}
