// Signed private-worker RPC fixtures shared by the owner, lazy, reliable and
// durable invocation tests: a revocable current-host policy, real directory
// endpoints and actual Unix-socket Cap'n Proto connections.
use super::*;
use std::sync::atomic::AtomicUsize;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

/// Current host policy whose certificate can be revoked or replaced mid-scenario.
pub struct CurrentHost {
    pub route: CrdtRoute,
    pub certificate: Mutex<Option<DeviceCert>>,
    pub additional_certificates: Mutex<Vec<DeviceCert>>,
    pub checks: AtomicUsize,
    pub time: Option<crate::peer_actions::PolicyTime>,
}

impl CurrentHost {
    pub fn new(
        route: &CrdtRoute,
        cert: &DeviceCert,
        time: Option<crate::peer_actions::PolicyTime>,
    ) -> Arc<Self> {
        Arc::new(Self {
            route: route.clone(),
            certificate: Mutex::new(Some(cert.clone())),
            additional_certificates: Mutex::new(vec![]),
            checks: AtomicUsize::new(0),
            time,
        })
    }
}

impl crate::peer_actions::CertificateAuthority for CurrentHost {
    fn policy_time(&self) -> Result<Option<crate::peer_actions::PolicyTime>, String> {
        Ok(self.time.clone())
    }
    fn authorize(&self, room: uuid::Uuid, cert: &DeviceCert) -> Result<(), String> {
        if room.to_string() != self.route.cluster_id
            || (self.certificate.lock().unwrap().as_ref() != Some(cert)
                && !self.additional_certificates.lock().unwrap().contains(cert))
        {
            return Err("host certificate changed or revoked".into());
        }
        Ok(())
    }
    // A permissive ordinary provider still cannot create descriptor provenance.
    fn authorize_join(&self, room: uuid::Uuid, cert: &DeviceCert) -> Result<(), String> {
        self.authorize(room, cert)
    }
    fn authorize_host_worker(&self, route: &CrdtRoute, cert: &DeviceCert) -> Result<(), String> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        if route != &self.route || !cert.capabilities.can_act_as_host {
            return Err("wrong host worker route or role".into());
        }
        self.authorize(route.cluster_id.parse().unwrap(), cert)
    }
}

/// A directory that reports each query/resolve entry and then waits for release.
pub struct HeldHostDirectory {
    pub entered: mpsc::UnboundedSender<&'static str>,
    pub release: Rc<tokio::sync::Notify>,
    pub row: runtime_collab::ActionDirectoryRow,
}

impl action_directory::Server for HeldHostDirectory {
    fn query(
        self: Rc<Self>,
        _params: action_directory::QueryParams,
        mut results: action_directory::QueryResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            self.entered.send("query").unwrap();
            self.release.notified().await;
            write_directory_page(
                results.get().init_page(),
                &runtime_collab::ActionDirectoryPage {
                    revision: "directory-r1".into(),
                    rows: vec![self.row.clone()],
                    next_cursor: vec![],
                    exhausted: true,
                    stale_cursor: false,
                    budget_exhausted: false,
                },
            )
        })
    }
    fn resolve(
        self: Rc<Self>,
        _params: action_directory::ResolveParams,
        mut results: action_directory::ResolveResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            self.entered.send("resolve").unwrap();
            self.release.notified().await;
            write_directory_row(results.get().init_resolution().init_found(), &self.row);
            Ok(())
        })
    }
}

/// A directory resolving the fixture action as a cancellable background effect.
pub struct DurableDirectory {
    pub state: Rc<RefCell<TestActionDirectory>>,
}

impl action_directory::Server for DurableDirectory {
    fn resolve(
        self: Rc<Self>,
        params: action_directory::ResolveParams,
        mut results: action_directory::ResolveResults,
    ) -> Promise<(), capnp::Error> {
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let revision = match request
            .get_revision()
            .and_then(|text| text.to_str().map_err(capnp::Error::from))
        {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let mut state = self.state.borrow_mut();
        state.resolve_count += 1;
        if revision != state.revision {
            results.get().init_resolution().set_stale_revision(());
            return Promise::ok(());
        }
        let mut row = state.row();
        let contract = tilo_actions::ActionContract {
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            ..tilo_actions::ActionContract::default()
        };
        assert!(contract.validate_declaration().is_empty());
        let encoded = serde_json::to_string(&contract).unwrap();
        let descriptor = row.descriptor.as_mut().unwrap();
        descriptor.operation_kind = "effect".into();
        descriptor.contract_json.clone_from(&encoded);
        row.visibility_contract_json = encoded;
        write_directory_row(results.get().init_resolution().init_found(), &row);
        Promise::ok(())
    }
}

/// A found-mode inert directory state at `revision`.
pub fn directory_state(revision: &str) -> Rc<RefCell<TestActionDirectory>> {
    Rc::new(RefCell::new(TestActionDirectory {
        revision: revision.into(),
        resolve_mode: DirectoryResolveMode::Found,
        result: tilo_actions::ResultSpec::None,
        confirmation_required: false,
        query_count: 0,
        resolve_count: 0,
    }))
}

pub fn directory_server(state: &Rc<RefCell<TestActionDirectory>>) -> action_directory::Client {
    capnp_rpc::new_client(TestActionDirectoryServer {
        state: state.clone(),
    })
}

pub fn empty_directories() -> Rc<LocalActionDirectoryRegistry> {
    let (_, rx) = std::sync::mpsc::channel();
    Rc::new(LocalActionDirectoryRegistry::new(rx))
}

/// The certified join presentation `cert`/`key` sign for `room`.
pub fn presentation(room: &str, cert: &DeviceCert, key: &SigningKey) -> Vec<u8> {
    runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.into(),
        device_certificate: serde_json::to_vec(cert).unwrap(),
        proof: key
            .sign(&runtime_identity::collaboration_join_signing_bytes(room))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap()
}

/// Drive the broker side of `server` and bootstrap a real RPC client over `client`.
pub fn rpc_pair(
    state: Arc<CapnpBrokerState>,
    peer: u64,
    server: std::os::unix::net::UnixStream,
    client: std::os::unix::net::UnixStream,
    worker: Option<WorkerBinding>,
    directories: Rc<LocalActionDirectoryRegistry>,
) -> (
    runtime_collab::CollabClient,
    tokio::task::JoinHandle<Result<(), capnp::Error>>,
    tokio::task::JoinHandle<()>,
) {
    server.set_nonblocking(true).unwrap();
    client.set_nonblocking(true).unwrap();
    let server_task = tokio::task::spawn_local(drive_connection(
        tokio::net::UnixStream::from_std(server).unwrap(),
        state,
        directories,
        peer,
        worker,
    ));
    let (r, w) = tokio::net::UnixStream::from_std(client)
        .unwrap()
        .into_split();
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(r.compat()),
        futures::io::BufWriter::new(w.compat_write()),
        rpc_twoparty_capnp::Side::Client,
        capnp::message::ReaderOptions::default(),
    ));
    let mut rpc = RpcSystem::new(network, None);
    let client = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
    let client_task = tokio::task::spawn_local(async move {
        let _ = rpc.await;
    });
    (
        runtime_collab::CollabClient::from_raw(client),
        server_task,
        client_task,
    )
}

/// A joined channel whose connection tasks stop when it is dropped.
pub struct Connection {
    pub handle: runtime_collab::ChannelHandle,
    pub server: tokio::task::JoinHandle<Result<(), capnp::Error>>,
    pub client: tokio::task::JoinHandle<()>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.server.abort();
        self.client.abort();
    }
}

/// Register a private host worker for `route`, connect it as `peer` and join
/// with the signed presentation. Returns the connection and its lease authorizer.
pub async fn join_worker(
    state: &Arc<CapnpBrokerState>,
    directories: &Rc<LocalActionDirectoryRegistry>,
    route: &CrdtRoute,
    cert: &DeviceCert,
    key: &SigningKey,
    peer: u64,
) -> (
    Connection,
    ui::invocation_scope::PublishedInvocationAuthorizer,
) {
    let (socket, authorize) = state
        .connect_local_worker_with_authorizer(route.clone(), cert)
        .unwrap();
    let pending = state.workers.lock().unwrap().pop().unwrap();
    let (rpc, server, client) = rpc_pair(
        state.clone(),
        peer,
        pending.stream,
        socket,
        Some(pending.binding),
        directories.clone(),
    );
    let handle = rpc
        .join_channel(
            &route.channel_key(),
            &presentation(&route.cluster_id, cert, key),
        )
        .await
        .unwrap();
    (
        Connection {
            handle,
            server,
            client,
        },
        authorize,
    )
}

/// A device certificate for `key`, signed by `root`.
pub fn signed_device(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
    expires_at_ns: Option<u64>,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// A broker whose only workspace grants the signed original worker capabilities
/// that expire at 100s, under a test policy clock starting at 50s.
pub struct OriginalRoom {
    pub key: SigningKey,
    pub cert: DeviceCert,
    pub origin: crate::peer_actions::PolicyOrigin,
    pub time_control: crate::peer_actions::PolicyClockTestControl,
    pub route: CrdtRoute,
    pub membership: WorkspaceTreeHandle,
    pub state: Arc<CapnpBrokerState>,
    pub authority: Arc<CurrentHost>,
    pub directories: Rc<LocalActionDirectoryRegistry>,
    _clock: crate::peer_actions::OwnerPolicyClock,
    _owner_mailbox: crate::test_owner::owner_commit::OwnerCommitMailbox,
}

impl OriginalRoom {
    pub fn new(
        capabilities: DeviceCapabilities,
        expires_at_ns: Option<u64>,
        persist_path: Option<std::path::PathBuf>,
    ) -> Self {
        let root = SigningKey::from_bytes(&[83; 32]);
        let key = SigningKey::from_bytes(&[84; 32]);
        let cert = signed_device(&root, &key, "original worker", capabilities, expires_at_ns);
        let (clock, time_control) = crate::peer_actions::OwnerPolicyClock::for_test(50_000_000_000);
        let origin = clock.new_origin();
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(83),
            "Original room",
        );
        workspace.set_owner(principal_id_for_root(cert.user_public_key));
        workspace.owner = None;
        workspace.capabilities = workspace
            .capabilities
            .into_iter()
            .map(|cap| cap.with_expiry(100))
            .collect();
        let route = CrdtRoute::record(
            "so.tilo.word-processor",
            workspace.room.to_string(),
            "document-1",
        );
        let mut tree = crate::test_support::WorkspaceTree::default();
        tree.workspaces.push(workspace);
        tree.persist_path = persist_path;
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (owner_handle, owner_mailbox) =
            crate::test_owner::owner_commit::OwnerCommitMailbox::bounded(8);
        let publisher =
            PeerPublisher::with_owner_commit(Arc::new(LifecycleSink), Arc::new(owner_handle));
        let (state, _) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            publisher,
            Some(membership.clone()),
            None,
        );
        let authority = CurrentHost::new(&route, &cert, Some(origin.capture().unwrap()));
        assert!(state.certificate_authority.set(authority.clone()).is_ok());
        Self {
            key,
            cert,
            origin,
            time_control,
            route,
            membership,
            state,
            authority,
            directories: empty_directories(),
            _clock: clock,
            _owner_mailbox: owner_mailbox,
        }
    }

    pub async fn join_worker(
        &self,
        peer: u64,
    ) -> (
        Connection,
        ui::invocation_scope::PublishedInvocationAuthorizer,
    ) {
        join_worker(
            &self.state,
            &self.directories,
            &self.route,
            &self.cert,
            &self.key,
            peer,
        )
        .await
    }
}
