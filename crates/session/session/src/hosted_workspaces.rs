//! The hosts of the workspaces this device takes part in but does not host.
//!
//! A workspace placed on a host (a home server, a cloud host) has its records
//! changed there: the host decides the person's actions on them with the
//! workspace's policy and runs them in its own workers, and the change reaches
//! this desktop's windows by sync. The broker asks here whether a workspace
//! is placed so, and sends the person's invocations to the host's actions.
//! This desktop reaches each host as one of the person's devices, on a
//! session opened with its own certificate and key, the way the person's
//! other devices enter a workspace on its host; the session is kept and
//! reused for every workspace on that host, and opened again once it ended.
//!
//! A workspace this device hosts itself, or one founded on its own desk
//! (whose host is a desktop, which runs no workers), is decided here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, pending,
};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_session_core::remote::SessionActions;
use tilo_session_host::PeerConfig;
use tilo_session_host::device::Device;

use crate::engine::Engine;

/// The hosts this device reaches its hosted workspaces' actions on.
#[derive(Clone)]
pub(crate) struct HostedWorkspaces(Arc<Inner>);

struct Inner {
    engine: Arc<Engine>,
    runtime: tokio::runtime::Handle,
    /// Each host's session, by the host's device key.
    hosts: Mutex<HashMap<[u8; 32], Arc<Host>>>,
}

/// This device's session with one host, and the workspaces it entered there.
struct Host {
    device: Device,
    /// Each entered workspace's actions on the host, by its id there.
    entered: Mutex<HashMap<u64, SessionActions>>,
    /// Held while a workspace is entered, so two invocations enter it once.
    entering: tokio::sync::Mutex<()>,
}

/// Where a workspace is placed: its host, as its signed hint names it.
struct Placed {
    workspace: String,
    workspace_id: u64,
    host: IrohWorkspaceHostHintV1,
}

impl HostedWorkspaces {
    /// The hosts of `engine`'s workspaces, reached on `runtime`; `None` for a session that holds
    /// no workspaces.
    pub(crate) fn service(
        engine: Option<&Arc<Engine>>,
    ) -> Option<Arc<dyn tilo_action_broker::services::HostedWorkspaces>> {
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        Some(Arc::new(Self(Arc::new(Inner {
            engine: Arc::clone(engine?),
            runtime,
            hosts: Mutex::default(),
        }))))
    }

    /// Where `workspace` is placed, when a host other than this device runs its records.
    fn placed(&self, workspace: &str) -> Option<Placed> {
        let id = WorkspaceId::parse(workspace)?;
        let hint = self
            .0
            .engine
            .owner()
            .with_current(id, |view| Ok(view.transport_hint()?.clone()))
            .ok()?;
        let own = self.0.engine.identity().device_cert.device_public_key;
        placed_on_host(&hint, own).map(|host| Placed {
            workspace: workspace.to_owned(),
            workspace_id: host.workspace_id,
            host,
        })
    }

    /// This device's session with `placed`'s host: the one kept, or a new one once it ended.
    fn host(&self, placed: &Placed) -> Arc<Host> {
        let key = placed.host.host_device_certificate.device_public_key;
        let mut hosts = self.0.hosts.lock_recover();
        if let Some(host) = hosts.get(&key).filter(|host| host.device.is_live()) {
            return Arc::clone(host);
        }
        let identity = self.0.engine.identity();
        let device = Device::over(
            PeerConfig {
                host: tilo_relay_client::Host::at(placed.host.endpoint_addr.clone()),
                cert: identity.device_cert.clone(),
                host_cert: placed.host.host_device_certificate.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            Arc::new(identity.device_signing_key.clone()),
            &identity.device_cert.device_label,
        );
        let host = Arc::new(Host {
            device,
            entered: Mutex::default(),
            entering: tokio::sync::Mutex::new(()),
        });
        let ended = hosts.insert(key, Arc::clone(&host));
        drop(hosts);
        if let Some(ended) = ended {
            ended.device.close();
        }
        host
    }
}

/// The host `hint` places a workspace on, when that is a dedicated host other than the device
/// `own`: one that runs its records' actions in its own workers. A workspace a desktop hosts
/// (its own desk's) runs them where its windows are.
fn placed_on_host(
    hint: &tilo_peer_state::join::TransportHint,
    own: [u8; 32],
) -> Option<IrohWorkspaceHostHintV1> {
    let (host, _) = IrohWorkspaceHostHintV1::authenticate_transport_hint(hint).ok()?;
    (host.mode == WorkspaceHostMode::CertifiedHost
        && host.host_device_certificate.device_public_key != own)
        .then_some(host)
}

impl tilo_action_broker::services::HostedWorkspaces for HostedWorkspaces {
    fn host_actions(&self, workspace: &str) -> Option<Arc<dyn ActionHost + Send + Sync>> {
        let placed = self.placed(workspace)?;
        let host = self.host(&placed);
        Some(Arc::new(OnHost {
            host,
            workspace: placed.workspace,
            workspace_id: placed.workspace_id,
            runtime: self.0.runtime.clone(),
        }))
    }
}

/// One workspace's actions on its host, entering it there on first use.
struct OnHost {
    host: Arc<Host>,
    workspace: String,
    workspace_id: u64,
    runtime: tokio::runtime::Handle,
}

impl OnHost {
    /// The workspace's actions on the host, once this device entered it there.
    fn entered(&self) -> Option<SessionActions> {
        self.host
            .entered
            .lock_recover()
            .get(&self.workspace_id)
            .cloned()
    }

    /// Ask `ask` of the workspace's actions on the host once this device is in it there.
    fn asking<T: Send + 'static>(
        &self,
        ask: impl FnOnce(SessionActions) -> Pending<Result<T, ActionHostError>> + Send + 'static,
    ) -> Pending<Result<T, ActionHostError>> {
        let (answer, answered) = pending();
        let host = Arc::clone(&self.host);
        let workspace = self.workspace.clone();
        let workspace_id = self.workspace_id;
        self.runtime.spawn(async move {
            let actions = match enter(&host, &workspace, workspace_id).await {
                Ok(actions) => actions,
                Err(error) => {
                    answer.complete(Err(error));
                    return;
                }
            };
            answer.complete(ask(actions).await.unwrap_or_else(|| {
                Err(ActionHostError::Unavailable {
                    reason: "the workspace's host did not answer".into(),
                })
            }));
        });
        answered
    }
}

/// `host`'s actions of `workspace`, entering it there first when this device is not in it yet.
async fn enter(
    host: &Host,
    workspace: &str,
    workspace_id: u64,
) -> Result<SessionActions, ActionHostError> {
    let _entering = host.entering.lock().await;
    if let Some(actions) = host.entered.lock_recover().get(&workspace_id) {
        return Ok(actions.clone());
    }
    let entered =
        host.device
            .join(workspace)
            .await
            .map_err(|error| ActionHostError::Unavailable {
                reason: format!("the workspace's host did not let this device in: {error}"),
            })?;
    let actions = host.device.actions(entered);
    host.entered
        .lock_recover()
        .insert(workspace_id, actions.clone());
    Ok(actions)
}

impl ActionHost for OnHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let caller = caller.clone();
        self.asking(move |actions| actions.query(&caller, query))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let caller = caller.clone();
        self.asking(move |actions| actions.invoke(&caller, request))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.entered()?.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.entered()?.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        match self.entered() {
            Some(actions) => actions.cancel(caller, id),
            None => Pending::ready(Err(ActionHostError::UnknownInvocation)),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a test's unwrap is its assertion")]

    use super::placed_on_host;
    use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    /// A hint placing a workspace on the device `capabilities` certify, in `mode`, and that
    /// device's key.
    fn hint(
        mode: WorkspaceHostMode,
        capabilities: DeviceCapabilities,
    ) -> (tilo_peer_state::join::TransportHint, [u8; 32]) {
        let root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "host".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned();
        let hint = IrohWorkspaceHostHintV1 {
            version: 1,
            mode,
            workspace_id: tilo_peer_state::join::room_workspace_id(&workspace).unwrap(),
            room: workspace,
            endpoint_addr: iroh::EndpointAddr::new(
                iroh::EndpointId::from_bytes(&cert.device_public_key).unwrap(),
            ),
            host_user_public_key: cert.user_public_key,
            host_device_certificate: cert.clone(),
            packet_relay_url: "iroh:direct".into(),
        }
        .transport_hint(&key)
        .unwrap();
        (hint, cert.device_public_key)
    }

    #[test]
    fn only_a_dedicated_host_of_another_device_runs_the_records() {
        let (dedicated, host) = hint(WorkspaceHostMode::CertifiedHost, DeviceCapabilities::host());
        assert!(
            placed_on_host(&dedicated, [7; 32]).is_some(),
            "a dedicated host runs its workspaces' records"
        );
        assert!(
            placed_on_host(&dedicated, host).is_none(),
            "this device runs those it hosts itself"
        );
        let (desk, _) = hint(
            WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
            DeviceCapabilities::primary(),
        );
        assert!(
            placed_on_host(&desk, [7; 32]).is_none(),
            "a desktop's own workspace runs its records where its windows are"
        );
    }
}
