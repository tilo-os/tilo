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
use tilo_workspace_service::service::WorkspaceService;

use crate::engine::Engine;

/// The hosts this device reaches its hosted workspaces' actions on.
#[derive(Clone)]
pub(crate) struct HostedWorkspaces(Arc<Inner>);

struct Inner {
    engine: Arc<Engine>,
    /// The member copies the host sessions read and write through.
    copies: VmMemberCopies,
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
        let engine = Arc::clone(engine?);
        Some(Self(Arc::new(Inner {
            copies: VmMemberCopies::new(Arc::clone(&engine), endpoint.clone(), runtime.clone()),
            engine,
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
                links: Arc::new(self.0.copies.clone()),
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

    /// A device certified under `root` as `label`, signing with `key`.
    fn certified(root: &ed25519_dalek::SigningKey, key: &ed25519_dalek::SigningKey, label: &str) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    /// A session the host placed in a workspace's VM is that workspace's member device, and the
    /// session it opens with its host dials no link of its own: it reads and asks the host's actions
    /// over the workspace's link the VM session's member copy keeps, which the host's admission has
    /// it take in. Without that copy the join waits for a link that never comes, and every host
    /// action over it is refused.
    #[test]
    fn a_vm_sessions_host_action_over_the_workspace_link_is_admitted() {
        use std::sync::Arc;

        use tilo_session_client::PeerConfig;
        use tilo_session_client::device::Device;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
        let host_key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
        let host = certified(&root, &host_key, "The host");
        let state = tempfile::tempdir().unwrap();
        let (server, workspaces, workspace) = runtime.block_on(async {
            let server = tilo_server::daemon::Server::start_with_relay_mode(
                tilo_server::ServerConfig {
                    state_root: state.path().join("host"),
                    ..tilo_server::ServerConfig::default()
                }
                .on_loopback(),
                tilo_server::daemon::ServerIdentity {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_signing_key: host_key.clone(),
                    device_cert: host.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                },
                test_server::relay_mode(),
            )
            .await
            .unwrap();
            let workspaces = server.workspaces().unwrap().clone();
            let workspace = workspaces.create("Browser").await.unwrap().workspace;
            workspaces.enter(&workspace, &host).await.unwrap();
            (server, workspaces, workspace)
        });

        // The device the host placed in the workspace's VM, which the host let in.
        let vm_key = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
        let vm = certified(&root, &vm_key, "The VM");
        runtime
            .block_on(workspaces.enter(&workspace, &vm))
            .unwrap();
        let vm_state = tempfile::tempdir().unwrap();
        let engine = Arc::new(
            crate::engine::Engine::open(
                tilo_network_actor::DeviceIdentity::kept(vm.clone(), vm_key.clone()),
                Some(vm_state.path()),
                std::iter::empty(),
            )
            .unwrap(),
        );
        let bound = Arc::new(std::sync::OnceLock::new());
        let endpoint = runtime
            .block_on(
                iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                    .secret_key(iroh::SecretKey::from_bytes(&vm_key.to_bytes()))
                    .relay_mode(test_server::relay_mode())
                    .bind(),
            )
            .unwrap();
        let _ = bound.set(endpoint);
        let copies = super::VmMemberCopies::new(
            Arc::clone(&engine),
            Some(Arc::clone(&bound)),
            runtime.handle().clone(),
        );

        let _inside = runtime.enter();
        let device = Device::over_lent(
            PeerConfig {
                host: tilo_server_client::ServerAddress::at(server.node_addr()),
                cert: vm.clone(),
                server_cert: host.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
                entering: None,
            },
            Arc::clone(&engine.identity().signer),
            "The VM",
            tilo_session_client::Lent {
                keys: engine.keys().get(),
                links: Arc::new(copies),
                endpoint: Some(bound),
                own: false,
            },
        );
        let workspace_id = runtime
            .block_on(device.join(&workspace))
            .expect("the host lets the VM's device in over the workspace's link its copy keeps");
        // What the host's actions are, asked as the VM's device over the workspace's link: the host
        // answers it only for a member it let in.
        let catalogue = runtime
            .block_on(tilo_actions::remote::ActionProvider::query(
                &device.actions(workspace_id),
                &tilo_actions::remote::Caller::authenticated(
                    vm.user_public_key,
                    vm.device_public_key,
                ),
                tilo_actions::remote::CatalogueQuery::default(),
            ))
            .expect("the host answers");
        assert!(
            catalogue.is_ok(),
            "the host admits the VM's ask over the workspace's link: {catalogue:?}"
        );
        device.close();
        runtime.block_on(server.shutdown());
    }
}

/// The member copies a session placed in a workspace's VM keeps of the workspaces its host lets it
/// into: the links the session it opens with that host reads, writes and asks the host's actions
/// through. The host's session dials no link of its own, so without a copy here every one of its
/// actions over a workspace's link is refused.
///
/// They are kept by a workspace service of the session's own, opened the first time a host lets
/// this device in, on the endpoint the session binds under the device's key. That service exists
/// only because the session in a VM is the member device and runs no daemon to keep its copies;
/// once the VM runs its own daemon as the member device, the daemon keeps them as every device's
/// does, and this goes.
#[derive(Clone)]
struct VmMemberCopies(Arc<VmCopies>);

struct VmCopies {
    engine: Arc<Engine>,
    endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
    runtime: tokio::runtime::Handle,
    /// The service, once it opened; `None` once it could not.
    service: tokio::sync::OnceCell<Option<WorkspaceService>>,
}

impl VmMemberCopies {
    fn new(
        engine: Arc<Engine>,
        endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self(Arc::new(VmCopies {
            engine,
            endpoint,
            runtime,
            service: tokio::sync::OnceCell::new(),
        }))
    }
}

impl VmCopies {
    /// The service keeping the copies, opened on first use.
    async fn service(&self) -> Option<&WorkspaceService> {
        self.service
            .get_or_init(|| async {
                self.open()
                    .await
                    .inspect_err(|error| {
                        tracing::warn!(%error, "this session keeps no member copy for its host's session");
                    })
                    .ok()
            })
            .await
            .as_ref()
    }

    async fn open(&self) -> Result<WorkspaceService, String> {
        let engine = &self.engine;
        let root = engine
            .service_root()
            .ok_or("this session keeps no state to keep a member copy in")?;
        let endpoint = self
            .endpoint
            .as_ref()
            .and_then(|bound| bound.get().cloned())
            .ok_or("this session's endpoint is not bound")?;
        let handed = Arc::downgrade(engine);
        let behind = Arc::downgrade(engine);
        let groups = tilo_workspace_service::own_device::key_groups(
            engine.keys(),
            Some(Box::new(move |workspace| {
                WorkspaceId::parse(workspace).and_then(|id| handed.upgrade()?.handed_keys(id))
            })),
            Some(Arc::new(DeviceKeySink(Arc::downgrade(engine)))),
            Some(Arc::new(move |group, held| {
                if let Some(link) = behind.upgrade().and_then(|engine| engine.link()) {
                    link.want_frames(group, held);
                }
            })),
        );
        let identity = engine.identity();
        tilo_workspace_service::own_device::open(
            root,
            tilo_workspace_service::own_device::Device {
                cert: &identity.device_cert,
                signer: &identity.signer,
                itself: None,
            },
            endpoint,
            groups,
            |config| config,
        )
        .await
        .map_err(|error| error.to_string())
    }
}

impl tilo_session_client::MemberLinks for VmMemberCopies {
    fn link(&self, workspace: WorkspaceId) -> Option<tilo_peer_state::direct::DirectSync> {
        self.0
            .service
            .get()?
            .as_ref()?
            .workspaces()
            .member_link(workspace)
    }

    fn take_in(&self, workspace: WorkspaceId, host: tilo_peer_state::join::TransportHint) {
        let copies = Arc::clone(&self.0);
        self.0.runtime.spawn(async move {
            if let Some(service) = copies.service().await {
                service
                    .workspaces()
                    .hold_as_members([(workspace, host)])
                    .await;
            }
        });
    }
}

/// Where the copies' service hands what the workspaces' members send for their key groups: this
/// device's key service, through the link the session's key groups hear on.
struct DeviceKeySink(std::sync::Weak<Engine>);

impl tilo_workspace_service::workspaces::KeySink for DeviceKeySink {
    fn take(&self, messages: Vec<neighbour_wire::NeighbourMsg>) {
        let Some(link) = self.0.upgrade().and_then(|engine| engine.link()) else {
            return;
        };
        for message in &messages {
            link.heard(message);
        }
    }

    /// Nothing to note: this device's key service reads the workspace's governance itself.
    fn settled(&self, _workspace: &str, _state: tilo_peer_state::StatePeer) {}
}
