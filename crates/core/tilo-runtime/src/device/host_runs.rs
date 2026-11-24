//! The runtime's broker behind the workspace host it runs: the record actions of the
//! workspaces on this computer, answered for the person's keys that entered them there
//! ([`crate::key_sessions`]).
//!
//! A key's run is admitted as an MCP client of this runtime is: the profile's person on this
//! device, with an agent named after the key acting under a key of its own. A computer that
//! founded its person's workspace answers that one ([`HostRuns`]); a keeping server answers
//! each workspace it keeps ([`KeptRuns`]), its dispatch built for that workspace.

use super::services::RuntimeActionProvider;
use super::services::action_provider;
use super::workspaces::ServiceWorkspaces;
use collab_types::WorkspaceId;
use std::sync::{Arc, OnceLock};
use tilo_actions::remote::Caller;
use tilo_actions::remote::invocation_record::Invocations;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::apps::collab_broker::AppActionBroker;

/// The workspace a computer that founds its person's workspace answers: its keys' runs and
/// its pages, all in that one workspace.
///
/// A new person's workspace is founded when it is first needed, not when the daemon starts:
/// their desktop may still hand over the one it kept. So its dispatch is admitted when a key
/// first reaches it, founding the workspace then if nothing came in.
pub(super) struct HostRuns {
    pub(super) broker: AppActionBroker,
    pub(super) workspaces: Arc<ServiceWorkspaces>,
    pub(super) invocations: Arc<Invocations>,
    pub(super) first: Arc<super::first_workspace::FirstWorkspace>,
    /// The calls that wait for their person's root, and the device actions its answer runs.
    pub(super) asked: Arc<super::asked::DeviceAsked>,
    pub(super) tickets: Arc<super::ticket::Tickets>,
    /// This device's certificate, which its person's root signed.
    pub(super) cert: DeviceCert,
    /// Whether what the start could not do for a fresh person was done once their workspace
    /// was founded: listing this device with their others, marking their root here, and serving
    /// their root-only asks.
    pub(super) done: OnceLock<()>,
    /// The workspace's dispatch, once a key reached it.
    pub(super) provider: OnceLock<RuntimeActionProvider>,
    /// The person's workspace, by its UUID.
    pub(super) workspace: String,
    pub(super) actor: String,
    pub(super) policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
}

impl HostRuns {
    /// The person's workspace's dispatch: founded, and admitted for the person, the first time.
    ///
    /// A fresh person's workspace is not held when the daemon starts, so what the start does for
    /// a person with one is done here the first time: this device is listed with their others,
    /// their root is marked in their devices, and their root-only asks are served.
    fn provider(&self) -> Result<RuntimeActionProvider, String> {
        if let Some(provider) = self.provider.get() {
            return Ok(provider.clone());
        }
        if self.first.get().is_none() {
            self.first.found_blocking()?;
        }
        self.done.get_or_init(|| self.hold_person());
        let provider = action_provider(
            &self.broker,
            &self.workspaces,
            None,
            &self.invocations,
            &self.actor,
            &self.workspace,
        )
        .map_err(|error| error.to_string())?;
        Ok(self.provider.get_or_init(|| provider).clone())
    }

    /// What the daemon's start does for a person whose workspace it holds, done here for a person
    /// whose workspace was not held then: this device listed with their others, their root marked
    /// in their devices, their root-only asks served here, and their workspaces read as held.
    fn hold_person(&self) {
        use tilo_workspace_service::workspaces::person_workspace::devices;
        let (copies, cert) = (self.workspaces.copies().clone(), self.cert.clone());
        let (asked, tickets) = (Arc::clone(&self.asked), Arc::clone(&self.tickets));
        // Their workspaces, listed now that theirs is held: what this device offers their keys,
        // and what their own tilo reads.
        drop(self.workspaces.workspaces());
        super::workspaces::block_on_owned(async move {
            if let Err(error) = devices::keep(&copies, &cert).await {
                tracing::warn!(%error, "this device is not listed with its person's");
            }
            // The root is this account's own: the person whose workspace this is, read by the
            // account it runs as, not looked up through a sign-in record.
            if let Ok(record) = super::services::device_root_record()
                && let Ok(root) = tilo_identity::store::load_from_path(record)
                && let Err(error) = devices::mark_root_holder(&copies, &root, &cert).await
            {
                tracing::warn!(%error, "this device's root is not marked in its person's devices");
            }
            asked.serve_at_root((*tickets).clone());
        });
    }
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
            .provider()?
            .agent_session(workspace, &self.actor, &self.policy_time, |_| {
                (
                    tilo_mcp::devices::agent_id("agent", key),
                    key.device_label.clone(),
                )
            })
            .map_err(|error| error.to_string())?;
        Ok((Arc::new(run), caller))
    }

    fn dispatch(&self, workspace: &str) -> Option<crate::key_sessions::SessionHost> {
        if workspace != self.workspace {
            return None;
        }
        let provider = self
            .provider()
            .inspect_err(|error| tracing::warn!(%error, "the person's workspace has no dispatch"))
            .ok()?;
        let host: crate::key_sessions::SessionHost = Arc::new(provider);
        Some(host)
    }
}

/// The workspaces a keeping server holds: the person's keys' runs and the pages of each, its
/// dispatch built there from the device's broker.
pub(super) struct KeptRuns {
    pub(super) broker: AppActionBroker,
    pub(super) workspaces: Arc<ServiceWorkspaces>,
    pub(super) apps: Arc<crate::apps::Apps>,
    pub(super) invocations: Arc<Invocations>,
    pub(super) actor: String,
    pub(super) policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
}

impl KeptRuns {
    fn provider(&self, workspace: &str) -> std::io::Result<RuntimeActionProvider> {
        action_provider(
            &self.broker,
            &self.workspaces,
            Some(Arc::clone(&self.apps)),
            &self.invocations,
            &self.actor,
            workspace,
        )
    }
}

impl crate::key_sessions::Sessions for KeptRuns {
    fn workspace(&self, workspace_id: WorkspaceId) -> Option<String> {
        // What this server keeps, and what it holds as a member: the workspaces it answers.
        self.workspaces.peer(workspace_id)?;
        Some(workspace_id.to_string())
    }

    fn open(
        &self,
        workspace: &str,
        key: &DeviceCert,
    ) -> Result<(crate::key_sessions::SessionHost, Caller), String> {
        let (run, caller) = self
            .provider(workspace)
            .map_err(|error| error.to_string())?
            .agent_session(workspace, &self.actor, &self.policy_time, |_| {
                (
                    tilo_mcp::devices::agent_id("agent", key),
                    key.device_label.clone(),
                )
            })
            .map_err(|error| error.to_string())?;
        Ok((Arc::new(run), caller))
    }

    fn dispatch(&self, workspace: &str) -> Option<crate::key_sessions::SessionHost> {
        let provider = self.provider(workspace).ok()?;
        let host: crate::key_sessions::SessionHost = Arc::new(provider);
        Some(host)
    }
}
