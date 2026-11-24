//! The hosts of the workspaces this device takes part in but does not host.
//!
//! A workspace placed on a host (a home server, a cloud host) has its records changed there: the
//! host decides the person's actions on them with the workspace's policy and runs them in its own
//! workers, and the change reaches this device's copy, and the windows that show it, by sync. The
//! device's one dispatch sends the person's invocations of those records' actions to the host
//! ([`tilo_workspace_service::workspaces::providers::Providers::place_on_hosts`]), and the
//! broker the device's apps attach to does the same for the actions their windows invoke. This
//! device asks the host as itself, from its one endpoint, over the device action service
//! (`tilo-actions/1`), in the workspace it asks about; one connection per host and workspace is
//! kept and reused.
//!
//! A workspace this device hosts itself, or one only desktops keep (which run no workers), is
//! decided here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use tilo_actions::remote::ActionProvider;
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::RemoteDevice;
use tilo_workspace_service::apps::services::HostedWorkspaces;
use tilo_workspace_service::workspaces::standing;

use super::workspaces::ServiceWorkspaces;

/// The hosts this device reaches its hosted workspaces' actions on.
pub(crate) struct Hosts {
    /// The workspaces this device holds: held weakly, since their dispatch holds these hosts.
    workspaces: Weak<ServiceWorkspaces>,
    /// Each workspace's host as last asked, by workspace and the host's device key: a host that
    /// changed is reached anew.
    reached: Mutex<HashMap<WorkspaceId, ([u8; 32], RemoteDevice)>>,
}

impl Hosts {
    /// The hosts of the workspaces `workspaces` holds.
    pub(crate) fn of(workspaces: &Arc<ServiceWorkspaces>) -> Arc<Self> {
        Arc::new(Self {
            workspaces: Arc::downgrade(workspaces),
            reached: Mutex::default(),
        })
    }
}

impl HostedWorkspaces for Hosts {
    fn host_actions(&self, workspace: &str) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        let id = WorkspaceId::parse(workspace)?;
        let workspaces = self.workspaces.upgrade()?;
        let own = workspaces.device_cert().device_public_key;
        let host = standing::host_elsewhere(&workspaces.peer(id)?, &own)?;
        let kept = self
            .reached
            .lock_recover()
            .get(&id)
            .filter(|(device, _)| *device == host.device)
            .map(|(_, actions)| actions.clone());
        if let Some(actions) = kept {
            return Some(Arc::new(actions));
        }
        let service = workspaces.copies().action_service();
        // A server keys its endpoint apart from the device it signs as.
        service.keyed_apart(host.device, *host.address.id.as_bytes());
        let actions = service.remote(&workspaces.endpoint(), &host.address, workspace);
        self.reached
            .lock_recover()
            .insert(id, (host.device, actions.clone()));
        Some(Arc::new(actions))
    }
}
