//! The runtime's broker behind the workspace host it runs: the record actions of the
//! workspaces on this computer, answered for the person's keys that entered them there
//! ([`crate::key_sessions`]).
//!
//! A key of the person's is admitted as an MCP client of this runtime is: the profile's person
//! on this device, with an agent named after the key acting under a key of its own. Another
//! member's device runs as that member, decided by the workspace's grants for them. A computer that
//! founded its person's workspace answers that one ([`HostRuns`]); a keeping server answers
//! each workspace it keeps ([`KeptRuns`]), its dispatch built for that workspace.

use super::services::RuntimeActionProvider;
use super::services::action_provider;
use super::workspaces::ServiceWorkspaces;
use crate::personal_devices::Whose;
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
    /// a person with one is done the first time, also for a workspace that came in from their
    /// desktop ([`super::first_workspace::FirstWorkspace::found_if_theirs`]).
    fn provider(&self) -> Result<RuntimeActionProvider, String> {
        if let Some(provider) = self.provider.get() {
            return Ok(provider.clone());
        }
        self.first.found_if_theirs(&self.workspace)?;
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
}

/// What the daemon's start does for a person whose workspace is here, for a person whose
/// workspace came later, done once it is
/// ([`super::first_workspace::FirstWorkspace::settle`]): their workspaces read again, this
/// device listed with their others, their root marked in their devices, and their root-only
/// asks served here.
pub(super) fn settling(
    workspaces: &Arc<ServiceWorkspaces>,
    asked: &Arc<super::asked::DeviceAsked>,
    tickets: &Arc<super::ticket::Tickets>,
    cert: &DeviceCert,
) -> super::first_workspace::Settling {
    let workspaces = Arc::downgrade(workspaces);
    let (asked, tickets, cert) = (Arc::downgrade(asked), Arc::clone(tickets), cert.clone());
    Arc::new(move || {
        let (workspaces, asked, tickets, cert) = (
            workspaces.upgrade(),
            asked.upgrade(),
            Arc::clone(&tickets),
            cert.clone(),
        );
        Box::pin(async move {
            let (Some(workspaces), Some(asked)) = (workspaces, asked) else {
                return;
            };
            // Their workspaces, listed now that theirs is held: what this device offers their
            // keys, and what their own tilo reads.
            drop(workspaces.workspaces());
            super::first_workspace::hold_person(workspaces.copies(), &cert).await;
            asked.serve_at_root((*tickets).clone());
        })
    })
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
        whose: Whose,
    ) -> Result<(crate::key_sessions::SessionHost, Caller), String> {
        key_run(
            &self.provider()?,
            workspace,
            (key, whose),
            (&self.actor, &self.policy_time),
        )
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
        whose: Whose,
    ) -> Result<(crate::key_sessions::SessionHost, Caller), String> {
        key_run(
            &self
                .provider(workspace)
                .map_err(|error| error.to_string())?,
            workspace,
            (key, whose),
            (&self.actor, &self.policy_time),
        )
    }

    fn dispatch(&self, workspace: &str) -> Option<crate::key_sessions::SessionHost> {
        let provider = self.provider(workspace).ok()?;
        let host: crate::key_sessions::SessionHost = Arc::new(provider);
        Some(host)
    }
}

/// The run of `key`, `whose` it is, in `workspace` through `provider`: a key of the person's as
/// an agent of `actor`'s named after the key, a member's device as that member.
fn key_run(
    provider: &RuntimeActionProvider,
    workspace: &str,
    (key, whose): (&DeviceCert, Whose),
    (actor, policy_time): (
        &str,
        &tilo_workspace_service::apps::policy_clock::PolicyTime,
    ),
) -> Result<(crate::key_sessions::SessionHost, Caller), String> {
    let (run, caller) = match whose {
        Whose::Own => provider.agent_session(workspace, actor, policy_time, |_| {
            (
                tilo_mcp::devices::agent_id("agent", key),
                key.device_label.clone(),
            )
        }),
        Whose::Member => provider.member_session(workspace, key, policy_time),
    }
    .map_err(|error| error.to_string())?;
    Ok((Arc::new(run), caller))
}
