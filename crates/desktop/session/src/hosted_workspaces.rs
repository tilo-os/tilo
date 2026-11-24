//! The host of the workspace a session the host placed in that workspace's VM takes part in.
//!
//! A person's desktop reaches the hosts of its workspaces through the device's daemon, which
//! publishes the person's workspace list with each one's host and sends the person's record
//! actions there on its one dispatch. A session the host
//! placed in a workspace's VM (a hosted Browser, a workspace's Linux apps,
//! [`crate::hosted_browser`]) is a device of its own that runs no daemon: it holds the workspace
//! as a confined member and asks the host, which certified it, for what it may not hold or write
//! itself. It reaches the host here, on a session opened with its own certificate and key from its
//! own endpoint, kept and reused, and opened again once it ended.
//!
//! The broker asks here whether a workspace is placed on a host and sends the person's
//! invocations of its records' actions to the host's actions. A workspace this device hosts
//! itself, or one founded on its own shell (whose host is a desktop, which runs no workers), is
//! decided here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ResourceReadRequest, pending,
};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_session_client::PeerConfig;
use tilo_session_client::device::Device;
use tilo_session_actions::SessionActions;

use crate::engine::Engine;

/// The hosts this device reaches its hosted workspaces' actions on.
#[derive(Clone)]
pub(crate) struct HostedWorkspaces(Arc<Inner>);

struct Inner {
    engine: Arc<Engine>,
    /// Where the endpoint this device runs under its key lands once bound: every host is dialled
    /// from it. `None` for a session that binds none.
    endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
    runtime: tokio::runtime::Handle,
    /// Each host's session, by the host's device key.
    hosts: Mutex<HashMap<[u8; 32], Arc<ServerAddress>>>,
}

/// This device's session with one host, and the workspaces it entered there.
struct ServerAddress {
    device: Device,
    /// Each entered workspace's actions on the host, by its id there.
    entered: Mutex<HashMap<WorkspaceId, SessionActions>>,
    /// Held while a workspace is entered, so two invocations enter it once.
    entering: tokio::sync::Mutex<()>,
}

/// Where a workspace is placed: its host, as its signed hint names it.
struct Placed {
    workspace: String,
    workspace_id: WorkspaceId,
    host: WorkspaceDeviceHintV1,
}

impl HostedWorkspaces {
    /// The hosts of `engine`'s workspaces, reached from this device's `endpoint` on `runtime`;
    /// `None` for a session that holds no workspaces. The session's own runtime reaches them: this
    /// is built where the session starts, which is no task of any runtime's.
    pub(crate) fn service(
        engine: Option<&Arc<Engine>>,
        endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
        runtime: tokio::runtime::Handle,
    ) -> Option<Self> {
        Some(Self(Arc::new(Inner {
            engine: Arc::clone(engine?),
            endpoint,
            runtime,
            hosts: Mutex::default(),
        })))
    }

    /// Where `workspace` is placed, when a holder other than this device takes its work: any
    /// holder that does, the workspace's holders being equal.
    fn placed(&self, workspace: &str) -> Option<Placed> {
        let id = WorkspaceId::parse(workspace)?;
        let hints = self
            .0
            .engine
            .owner()
            .with_current(id, |view| Ok(view.keeping_devices()))
            .ok()?;
        let own = self.0.engine.identity().device_cert.device_public_key;
        hints.iter().find_map(|hint| {
            let host = placed_on_host(hint, own)?;
            Some(Placed {
                workspace: workspace.to_owned(),
                workspace_id: host.workspace_id,
                host,
            })
        })
    }

    /// This device's session with `placed`'s host: the one kept, or a new one once it ended.
    fn host(&self, placed: &Placed) -> Arc<ServerAddress> {
        // The session a host is reached on is a task of this session's runtime, and a caller may
        // be a thread of the broker's: it opens here, inside the runtime that reaches hosts.
        let _inside = self.0.runtime.enter();
        let key = placed.host.device_certificate.device_public_key;
        let mut hosts = self.0.hosts.lock_recover();
        if let Some(host) = hosts.get(&key).filter(|host| host.device.is_live()) {
            return Arc::clone(host);
        }
        let identity = self.0.engine.identity();
        // It reads and asks with this device's own keys, and dials from this device's own
        // endpoint: the device holds one place in each workspace's key group, and one connection
        // per server, which a second key service or endpoint of its own would take.
        let device = Device::over_lent(
            PeerConfig {
                host: tilo_server_client::ServerAddress::at(placed.host.endpoint_addr.clone()),
                cert: identity.device_cert.clone(),
                server_cert: placed.host.device_certificate.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
                entering: None,
            },
            Arc::clone(&identity.signer),
            &identity.device_cert.device_label,
            tilo_session_client::Lent {
                keys: self.0.engine.keys().get(),
                endpoint: self.0.endpoint.clone(),
                own: false,
            },
        );
        let host = Arc::new(ServerAddress {
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

    /// What carries this device's host sessions: where a follower of theirs starts.
    pub(crate) fn runtime(&self) -> tokio::runtime::Handle {
        self.0.runtime.clone()
    }

    /// Read `read`, a record of `workspace`, on the host of the workspaces this device was placed
    /// on, entering `workspace` there first. The read is decided on the host by the grant this
    /// device was issued, which reaches only what its placement allows: a record outside it is
    /// refused there, and this device's grant is not widened for it. `None` while no host takes
    /// this device's work.
    pub(crate) fn read_record(
        &self,
        workspace: &str,
        workspace_id: WorkspaceId,
        read: ResourceReadRequest,
    ) -> Option<Pending<Result<String, ActionCallError>>> {
        let host = self.any_host()?;
        let workspace = workspace.to_owned();
        let (answer, answered) = pending();
        self.0.runtime.spawn(async move {
            let actions = match enter(&host, &workspace, workspace_id).await {
                Ok(actions) => actions,
                Err(error) => {
                    answer.complete(Err(error));
                    return;
                }
            };
            answer.complete(actions.read_record(read).await.unwrap_or_else(|| {
                Err(ActionCallError::Unavailable {
                    reason: "the workspace's host did not answer".into(),
                })
            }));
        });
        Some(answered)
    }

    /// A host one of the workspaces this device holds is placed on.
    fn any_host(&self) -> Option<Arc<ServerAddress>> {
        self.0
            .engine
            .workspaces()
            .into_iter()
            .find_map(|id| self.placed(&id.to_string()).map(|placed| self.host(&placed)))
    }
}

/// The device `hint` places a workspace on, when that is another device than `own` and it runs
/// the workspace's work in its own workers. A workspace a desktop serves runs its records'
/// actions where its windows are.
fn placed_on_host(
    hint: &tilo_peer_state::join::TransportHint,
    own: [u8; 32],
) -> Option<WorkspaceDeviceHintV1> {
    let (device, _) = WorkspaceDeviceHintV1::authenticate_transport_hint(hint).ok()?;
    (device.runs_work && device.device_certificate.device_public_key != own).then_some(device)
}

impl tilo_workspace_service::apps::services::HostedWorkspaces for HostedWorkspaces {
    fn host_actions(&self, workspace: &str) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
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
    host: Arc<ServerAddress>,
    workspace: String,
    workspace_id: WorkspaceId,
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
        ask: impl FnOnce(SessionActions) -> Pending<Result<T, ActionCallError>> + Send + 'static,
    ) -> Pending<Result<T, ActionCallError>> {
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
                Err(ActionCallError::Unavailable {
                    reason: "the workspace's host did not answer".into(),
                })
            }));
        });
        answered
    }
}

/// `host`'s actions of `workspace`, entering it there first when this device is not in it yet.
async fn enter(
    host: &ServerAddress,
    workspace: &str,
    workspace_id: WorkspaceId,
) -> Result<SessionActions, ActionCallError> {
    let _entering = host.entering.lock().await;
    if let Some(actions) = host.entered.lock_recover().get(&workspace_id) {
        return Ok(actions.clone());
    }
    host.device
        .join(workspace)
        .await
        .map_err(|error| ActionCallError::Unavailable {
            reason: format!("the workspace's host did not let this device in: {error}"),
        })?;
    let actions = host.device.actions(workspace_id);
    host.entered
        .lock_recover()
        .insert(workspace_id, actions.clone());
    Ok(actions)
}

impl ActionProvider for OnHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let caller = caller.clone();
        self.asking(move |actions| actions.query(&caller, query))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let caller = caller.clone();
        self.asking(move |actions| actions.invoke(&caller, request))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.entered()?.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.entered()?.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        match self.entered() {
            Some(actions) => actions.cancel(caller, id),
            None => Pending::ready(Err(ActionCallError::UnknownInvocation)),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::placed_on_host;
    use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    /// A hint placing a workspace on the device `capabilities` certify, which runs the
    /// workspace's work or not, and that device's key.
    fn hint(
        runs_work: bool,
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
        let hint = WorkspaceDeviceHintV1 {
            version: 1,
            holds_keys: true,
            runs_work,
            label: String::new(),
            cloud: false,
            origin: None,
            workspace_id: tilo_peer_state::join::WorkspaceId::parse(&workspace).unwrap(),
            endpoint_addr: iroh::EndpointAddr::new(
                iroh::EndpointId::from_bytes(&cert.device_public_key).unwrap(),
            ),
            user_public_key: cert.user_public_key,
            device_certificate: cert.clone(),
            packet_relay_url: "iroh:direct".into(),
        }
        .transport_hint(&key)
        .unwrap();
        (hint, cert.device_public_key)
    }

    #[test]
    fn only_another_device_that_takes_the_work_runs_the_records() {
        let (taker, device) = hint(true, DeviceCapabilities::member());
        assert!(
            placed_on_host(&taker, [7; 32]).is_some(),
            "a device that takes the work runs the workspace's records"
        );
        assert!(
            placed_on_host(&taker, device).is_none(),
            "a device runs the records of the workspaces it serves itself"
        );
        let (shell, _) = hint(false, DeviceCapabilities::primary());
        assert!(
            placed_on_host(&shell, [7; 32]).is_none(),
            "a device that takes no work has its records run where its windows are"
        );
    }
}
