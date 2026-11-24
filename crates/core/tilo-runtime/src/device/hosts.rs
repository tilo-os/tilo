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
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

use iroh::{EndpointAddr, EndpointId};

use tilo_actions::remote::ActionProvider;
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::RemoteDevice;
use tilo_workspace_service::apps::services::{ConnectionActions, HostedWorkspaces};
use tilo_workspace_service::work::{self, ConnectionCall};
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

    /// The device serving `workspace` when another device than this one does, and where it is
    /// reached. It may key its endpoint apart from the device it signs as, which the action
    /// service learns here before anything dials it.
    pub(crate) fn serving(&self, workspace: &str) -> Option<([u8; 32], EndpointAddr)> {
        let id = WorkspaceId::parse(workspace)?;
        let workspaces = self.workspaces.upgrade()?;
        let own = workspaces.device_cert().device_public_key;
        let host = standing::host_elsewhere(&workspaces.peer(id)?, &own)?;
        workspaces
            .copies()
            .action_service()
            .keyed_apart(host.device, *host.address.id.as_bytes());
        Some((host.device, host.address))
    }

    /// The actions of `device`, a device of `workspace`: over the link it holds to this device,
    /// where it holds one; the device serving the workspace itself, or `device` through it, where
    /// another device serves it; and otherwise `device` dialled by its key.
    fn reach(&self, workspace: &str, device: [u8; 32]) -> Option<RemoteDevice> {
        let workspaces = self.workspaces.upgrade()?;
        let service = workspaces.copies().action_service();
        if let Some(linked) = service.remote_held(device, workspace) {
            return Some(linked);
        }
        let endpoint = workspaces.endpoint();
        Some(match self.serving(workspace) {
            Some((serving, address)) if serving == device => {
                service.remote(&endpoint, &address, workspace)
            }
            Some((_, address)) => service.through(&endpoint, &address, workspace, device),
            None => service.remote(
                &endpoint,
                &EndpointAddr::new(EndpointId::from_bytes(&device).ok()?),
                workspace,
            ),
        })
    }
}

impl HostedWorkspaces for Hosts {
    fn host_actions(&self, workspace: &str) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        let id = WorkspaceId::parse(workspace)?;
        let (device, address) = self.serving(workspace)?;
        let kept = self
            .reached
            .lock_recover()
            .get(&id)
            .filter(|(reached, _)| *reached == device)
            .map(|(_, actions)| actions.clone());
        if let Some(actions) = kept {
            return Some(Arc::new(actions));
        }
        let workspaces = self.workspaces.upgrade()?;
        let actions = workspaces.copies().action_service().remote(
            &workspaces.endpoint(),
            &address,
            workspace,
        );
        self.reached
            .lock_recover()
            .insert(id, (device, actions.clone()));
        Some(Arc::new(actions))
    }

    fn connection_actions<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
    ) -> Pin<Box<dyn Future<Output = ConnectionActions> + Send + 'a>> {
        Box::pin(async move {
            let Some(workspaces) = self.workspaces.upgrade() else {
                return ConnectionActions::Unplaced("this device is shutting down".into());
            };
            let device = match work::connection_call(workspaces.copies(), workspace, app).await {
                ConnectionCall::Here => return ConnectionActions::Here,
                ConnectionCall::Unplaced(why) => return ConnectionActions::Unplaced(why),
                ConnectionCall::On(device) => device,
            };
            match self.reach(workspace, device) {
                Some(actions) => ConnectionActions::On(Arc::new(actions)),
                None => ConnectionActions::Unplaced(
                    "the device that runs this app's connection cannot be reached from here".into(),
                ),
            }
        })
    }
}
