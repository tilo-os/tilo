//! The keys of the agents the person admits at this desktop, as the device's daemon keeps them:
//! the daemon makes each from the device key, has the person's root certify it, and signs as it.
//! The session holds no agent's key and no device key; it asks over its shell link
//! ([`runtime_collab::wire::shell::AgentKeyCall`]), from the thread an agent's connection is
//! served on.

use runtime_collab::wire::shell::{AgentKeyAnswer, AgentKeyCall, ShellCall, ShellReply};
use tilo_actions::remote::InvokeRequest;
use tilo_desktop_model::WorkspaceId;
use tilo_session_client::daemon_documents::DaemonShell;
use tilo_user_identity::{DeviceCert, RevocationEntry};
use tilo_workspace_service::agents::agent_keys::AgentKeys;

/// The agents' keys the device's daemon keeps, reached over the shell link.
pub(crate) struct AtDaemon(pub(crate) DaemonShell);

impl AtDaemon {
    fn ask(&self, call: AgentKeyCall) -> Result<AgentKeyAnswer, String> {
        match self.0.ask_blocking(ShellCall::AgentKey(call))? {
            ShellReply::AgentKey(answer) => answer,
            other => Err(format!("the daemon answered an agent's key with {other:?}")),
        }
    }
}

impl AgentKeys for AtDaemon {
    fn grant(
        &self,
        workspace: WorkspaceId,
        agent: &str,
        name: &str,
        person: [u8; 32],
    ) -> Result<[u8; 32], String> {
        match self.ask(AgentKeyCall::Grant {
            workspace: workspace.to_string(),
            agent: agent.to_owned(),
            name: name.to_owned(),
            person,
        })? {
            AgentKeyAnswer::Granted(key) => Ok(key),
            other => Err(format!("the daemon answered a key's grant with {other:?}")),
        }
    }

    fn sign(
        &self,
        workspace: WorkspaceId,
        key: &[u8; 32],
        request: &mut InvokeRequest,
    ) -> Result<(), String> {
        let asked = serde_json::to_vec(&*request).map_err(|error| error.to_string())?;
        match self.ask(AgentKeyCall::Sign {
            workspace: workspace.to_string(),
            key: *key,
            request: asked,
        })? {
            AgentKeyAnswer::Signed(signed) => {
                *request = serde_json::from_slice(&signed).map_err(|error| error.to_string())?;
                Ok(())
            }
            other => Err(format!("the daemon answered a signing with {other:?}")),
        }
    }

    /// No key leaves the daemon: a connection to another device is carried there by the daemon.
    fn made(&self, _key: &[u8; 32]) -> Option<(DeviceCert, ed25519_dalek::SigningKey)> {
        None
    }

    fn revocation(
        &self,
        person: &[u8; 32],
        key: &[u8; 32],
    ) -> Result<Option<RevocationEntry>, String> {
        match self.ask(AgentKeyCall::Revocation {
            person: *person,
            key: *key,
        })? {
            AgentKeyAnswer::Revocation(entry) => entry
                .map(|entry| serde_json::from_slice(&entry).map_err(|error| error.to_string()))
                .transpose(),
            other => Err(format!("the daemon answered a revocation with {other:?}")),
        }
    }
}
