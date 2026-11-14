//! The runtime's broker behind the workspace host it runs: the record actions
//! of the profile's workspace, answered for the person's keys that entered it
//! there ([`crate::key_runs`]).
//!
//! A key's run is admitted as an MCP client of this runtime is: the profile's
//! person on this device, handing the run to an agent named after the key.

use super::{Arc, Caller, RuntimeActionHost};
use collab_types::WorkspaceId;
use tilo_user_identity::DeviceCert;

/// The profile's workspace's actions, as the keys of its person reach them.
pub(super) struct HostRuns {
    pub(super) host: RuntimeActionHost,
    /// The profile's workspace, by its UUID.
    pub(super) workspace: String,
    pub(super) actor: String,
    pub(super) policy_time: tilo_action_broker::policy_clock::PolicyTime,
}

impl crate::key_runs::Runs for HostRuns {
    fn workspace(&self, workspace_id: WorkspaceId) -> Option<String> {
        (tilo_desktop_model::WorkspaceId::parse(&self.workspace) == Some(workspace_id))
            .then(|| self.workspace.clone())
    }

    fn open(
        &self,
        workspace: &str,
        key: &DeviceCert,
    ) -> Result<(crate::key_runs::RunHost, Caller), String> {
        let (run, caller, _) = self
            .host
            .agent_run(workspace, &self.actor, &self.policy_time, |_| {
                crate::remote_door::agent_id("agent", key)
            })
            .map_err(|error| error.to_string())?;
        Ok((Arc::new(run), caller))
    }
}
