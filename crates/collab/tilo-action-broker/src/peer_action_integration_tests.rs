use super::*;
use tilo_delivery_protocol::peer_actions::{ActionMessage, DirectedAction};
use tilo_delivery_protocol::transport_v2::EnvelopeV2;
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::{Envelope, RelayClient, RelayCoeditSession, RelayConfig, WorkspaceBinding};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};
use uuid::Uuid;

struct LiveAuthority {
    room: Uuid,
    root: [u8; 32],
    revocations: Mutex<RevocationList>,
}
impl crate::peer_actions::CertificateAuthority for LiveAuthority {
    fn authorize(&self, room: Uuid, certificate: &DeviceCert) -> Result<(), String> {
        if room != self.room
            || certificate.user_public_key != self.root
            || self
                .revocations
                .lock()
                .unwrap()
                .is_revoked(&certificate.device_public_key)
        {
            return Err("current certified authority denied".into());
        }
        Ok(())
    }
}
fn package_primary(fixture: &mut LocalDirectoryFixture, request: &mut CallerInvocation) {
    fixture.route.record_id.clear();
    request.record_id.clear();
    fixture
        .state
        .surfaces
        .lock()
        .unwrap()
        .get_mut(&71)
        .unwrap()
        .route
        .record_id
        .clear();
    fixture
        .directories
        .directories
        .borrow_mut()
        .get_mut(&71)
        .unwrap()
        .route
        .record_id
        .clear();
}

fn device(seed: u8, host: bool) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let capabilities = if host {
        DeviceCapabilities::host()
    } else {
        DeviceCapabilities::primary()
    };
    let cert = super::super::signed_worker::signed_device(
        &SigningKey::from_bytes(&[51; 32]),
        &key,
        "private test device",
        capabilities,
        None,
    );
    (key, cert)
}
async fn client(relay: &Relay, seed: u8, room: Uuid) -> RelayClient {
    let (key, cert) = device(seed, false);
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .bind()
        .await
        .unwrap();
    let client = RelayClient::connect_v2_to_enrolled_home_peer(
        endpoint,
        relay.node_addr(),
        relay.own_user_public_key(),
        relay.handle().mesh_profile().device,
        cert,
        key,
        RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: tilo_relay::room_workspace_id(&room.to_string()).unwrap(),
        })
        .unwrap();
    tilo_test_env::within("the relay client to be ready", async {
        while !matches!(client.recv().await.unwrap(), Envelope::Ready { .. }) {}
    })
    .await;
    client
        .send_action(EnvelopeV2::EnableActions { room })
        .unwrap();
    assert!(matches!(
        tilo_test_env::within("`client.recv_action()`", client.recv_action()).await,
        Some(EnvelopeV2::ActionsReady { .. })
    ));
    client
        .send_action(EnvelopeV2::EnableBrokerClient { room })
        .unwrap();
    client
}
async fn reply(client: &RelayClient, id: Uuid) -> ActionMessage {
    tilo_test_env::within("the reply to this request", async {
        loop {
            match client.recv_action().await.unwrap() {
                EnvelopeV2::Delivered { action, .. } if action.request_id == id => {
                    return action.message;
                }
                EnvelopeV2::Unavailable {
                    request_id, reason, ..
                } if request_id == id => panic!("private request unavailable: {reason:?}"),
                _ => {}
            }
        }
    })
    .await
}
fn send(client: &RelayClient, room: Uuid, seed: u8, message: ActionMessage) -> Uuid {
    let request_id = Uuid::new_v4();
    client
        .send_action(EnvelopeV2::Directed(DirectedAction {
            room,
            request_id,
            source: device(seed, false).1.device_public_key,
            target: device(54, false).1.device_public_key,
            message,
        }))
        .unwrap();
    request_id
}
fn invoke(request: &CallerInvocation) -> ActionMessage {
    ActionMessage::InvokeBound {
        host: request.host,
        invocation: request.invocation,
        app_peer: request.peer_id.to_string(),
        app_id: request.app_id.clone(),
        record_id: request.record_id.clone(),
        resource: request.resource.clone(),
        action: request.action.clone(),
        argument: request
            .argument
            .as_ref()
            .map(|s| serde_json::from_str(s).unwrap()),
        confirmation: None,
        revision: request.revision.clone(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_peer_facade_real_relay_durable_broker_application_stream() {
    tokio::task::LocalSet::new().run_until(async {
        let root = tempfile::tempdir().unwrap();
        let relay_root = tempfile::tempdir().unwrap();
        let (mut fixture, local_caller, mut request) = durable_fixture(root.path(), None);
        package_primary(&mut fixture, &mut request);
        let room = Uuid::parse_str(&fixture.cluster_id).unwrap();
        let ws = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
        let authority = Arc::new(LiveAuthority { room, root: device(52, false).1.user_public_key, revocations: Mutex::new(RevocationList::new()) });
        assert!(fixture.state.certificate_authority.set(authority.clone()).is_ok());
        assert!(invoke(&request).validate(), "empty canonical package-primary record is valid");
        let (relay_key, relay_cert) = device(55, true);
        let relay = Relay::start(RelayConfig { state_root: relay_root.path().into(), workspaces: vec![WorkspaceBinding { id: ws, pin: true, replication: tilo_relay::ReplicationIntent::FullWorkspace, admitted_users: vec![], shell_authority: None, checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::default() }], ..Default::default() }, RelayIdentity { user_public_key: relay_cert.user_public_key, device_signing_key: relay_key, device_cert: relay_cert, revocations: RevocationList::new() }).await.unwrap();
        relay.set_workspace_membership(ws, true, vec![device(52, false).1.device_public_key, device(53, false).1.device_public_key, device(54, false).1.device_public_key], relay_root.path().into()).await.unwrap();
        let caller = client(&relay, 52, room).await;
        let other = client(&relay, 53, room).await;
        let host_client = client(&relay, 54, room).await;
        // Production transport stays unavailable before a healthy attachment.
        let denied = send(&caller, room, 52, invoke(&request));
        assert!(matches!(caller.recv_action().await, Some(EnvelopeV2::Unavailable { request_id, reason: tilo_delivery_protocol::peer_actions::UnavailableReason::AdmissionUnavailable, .. }) if request_id == denied));
        let session = Arc::new(RelayCoeditSession::join(host_client, ws, device(54, false).1.device_public_key).unwrap());
        assert!(matches!(session.recv().await, Some(tilo_relay::Inbound::Ready { .. })));
        let worker = fixture.state.start_peer_action_bridge(fixture.directories.clone()).unwrap();
        let (queue, host) = fixture.broker.peer_action_attachment().unwrap();
        let server_session = session.clone();
        let server = tokio::spawn(async move { queue.serve(server_session, room, host).await });
        let catalogue = tilo_test_env::within("attached broker must answer catalogue", async { loop {
            let id = send(
                &caller,
                room,
                52,
                ActionMessage::CatalogueRefresh { scope: None },
            );
            match tokio::time::timeout(Duration::from_millis(250), reply(&caller, id)).await {
                Ok(ActionMessage::BrokerCatalogue { host, catalogue }) => { assert_eq!(host, request.host); break catalogue; }
                _ => tokio::task::yield_now().await,
            }
        }}).await;
        assert!(catalogue["surfaces"].as_array().unwrap().iter().any(|s| s["app_id"] == request.app_id));
        // Enrol a further certified caller after startup, outside the fixture's
        // former fixed device list; current root authority accepts it dynamically.
        relay.set_workspace_membership(ws, true, [52,53,54,56].into_iter().map(|seed| device(seed, false).1.device_public_key).collect(), relay_root.path().into()).await.unwrap();
        let dynamic = client(&relay, 56, room).await;
        let dynamic_catalogue = send(
            &dynamic,
            room,
            56,
            ActionMessage::CatalogueRefresh { scope: None },
        );
        assert!(matches!(reply(&dynamic, dynamic_catalogue).await, ActionMessage::BrokerCatalogue { .. }));
        let mut revocation = tilo_user_identity::RevocationEntry { user_public_key: authority.root, revoked_device_public_key: device(56, false).1.device_public_key, revoked_at_ns: 1, reason: tilo_user_identity::RevocationReason::Lost, signature: [0;64] };
        tilo_user_identity::sign_revocation_entry(&mut revocation, &SigningKey::from_bytes(&[51;32])).unwrap();
        authority.revocations.lock().unwrap().insert(revocation).unwrap();
        let revoked = send(
            &dynamic,
            room,
            56,
            ActionMessage::CatalogueRefresh { scope: None },
        );
        assert!(matches!(reply(&dynamic, revoked).await, ActionMessage::Rejected { .. }));
        dynamic.close().await;
        // Fresh route, directory revision and exact confirmation are all
        // revalidated after catalogue discovery, before any application effect.
        let mut stale = request.clone(); stale.record_id = "another-record".into();
        let rejected = send(&caller, room, 52, invoke(&stale));
        assert!(matches!(reply(&caller, rejected).await, ActionMessage::Rejected { .. }));
        stale = request.clone(); stale.revision = Some("stale-directory".into());
        fixture.directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::Stale;
        let rejected = send(&caller, room, 52, invoke(&stale));
        assert!(matches!(reply(&caller, rejected).await, ActionMessage::Rejected { .. }));
        fixture.directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::Found;
        fixture.state.surfaces.lock().unwrap().get_mut(&71).unwrap().actions[0].contract.confirmation = tilo_actions::ConfirmationPolicy::Phrase { phrase: "DO IT".into() };
        fixture.state.surfaces.lock().unwrap().get_mut(&71).unwrap().actions[0].contract.execution = tilo_actions::ExecutionMode::Streaming { cancellable: true, item_schema: serde_json::json!({"type":"string"}) };
        let rejected = send(&caller, room, 52, invoke(&request));
        assert!(matches!(reply(&caller, rejected).await, ActionMessage::Rejected { .. }));
        let mut confirmed = invoke(&request);
        if let ActionMessage::InvokeBound { confirmation, .. } = &mut confirmed { *confirmation = Some(tilo_delivery_protocol::peer_actions::Confirmation::Phrase("wrong".into())); }
        let rejected = send(&caller, room, 52, confirmed.clone());
        assert!(matches!(reply(&caller, rejected).await, ActionMessage::Rejected { .. }));
        assert!(fixture.invocation_rx.try_recv().is_err());
        if let ActionMessage::InvokeBound { confirmation, .. } = &mut confirmed { *confirmation = Some(tilo_delivery_protocol::peer_actions::Confirmation::Phrase("DO IT".into())); }
        let first = send(&caller, room, 52, confirmed);
        let duplicate = send(&caller, room, 52, invoke(&request));
        assert!(matches!(reply(&caller, first).await, ActionMessage::Observation { update: Some(tilo_actions::InvocationUpdate::Accepted { invocation }), .. } if invocation == request.invocation));
        assert!(matches!(reply(&caller, duplicate).await, ActionMessage::Observation { update: Some(tilo_actions::InvocationUpdate::Accepted { .. }), .. }));
        let (_, empty_rx) = mpsc::unbounded_channel();
        let rx = Rc::new(AsyncMutex::new(std::mem::replace(&mut fixture.invocation_rx, empty_rx)));
        let stream: invocation_stream::Client = capnp_rpc::new_client(InvocationStreamServer { message_source: None, message_publisher: None, host_guard: None, rx: rx.clone(), delivered: None });
        let delivery = stream.read_request().send().promise.await.unwrap();
        let wire_id = delivery.get().unwrap().get_id();
        assert_eq!(delivery.get().unwrap().get_invocation_uuid().unwrap(), request.invocation.0.as_bytes());
        effect(root.path());
        // The application's resulting document mutation follows the ordinary
        // signed broker publication path and the same relay state session.
        use automerge::transaction::Transactable;
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "effect_count", 1_i64).unwrap();
        let artifact = document_artifact_id(&fixture.route.app_id, &fixture.route.cluster_id, &fixture.route.record_id);
        let mut mutation = SignedOp::new_unsigned(DocumentMutation::Snapshot(document.save()).encode().unwrap(), artifact, collab_types::Ed25519PublicKey::from_bytes(device(52, false).1.device_public_key), document_attestation_id(&fixture.route.app_id, &fixture.route.cluster_id, &fixture.route.record_id), collab_types::OpClass::Splice, std::time::SystemTime::now(), 1);
        collab_types::sign_op(&mut mutation, &device(52, false).0).unwrap();
        fixture.state.ensure_channel(&fixture.route.channel_key());
        let mut signed = fixture.state.subscribe_ops(&fixture.route.channel_key(), 72);
        handle_signed_op_send(&fixture.state, 71, &fixture.route, &local_caller.admitted, mutation.clone());
        let signed_delivery = signed.try_recv().expect("real application signed mutation admitted");
        assert_eq!(postcard::from_bytes::<SignedOp>(&signed_delivery.op_bytes).unwrap(), mutation);
        session.publish_retained_delta([7; 32], signed_delivery.op_bytes).unwrap();
        let Envelope::Delta { delta: payload, .. } = tilo_test_env::within("`caller.recv()`", caller.recv()).await.unwrap() else { panic!("signed resulting state missing") };
        let Some(tilo_relay::CoeditPayload::RetainedDelta { body, .. }) = tilo_relay::CoeditPayload::decode(&payload) else { panic!("retained signed state missing") };
        assert_eq!(postcard::from_bytes::<SignedOp>(&body).unwrap(), mutation);

        assert!(rx.lock().await.try_recv().is_err());
        let collision = send(&other, room, 53, invoke(&request));
        assert!(matches!(reply(&other, collision).await, ActionMessage::Rejected { .. }));
        let private = send(&other, room, 53, ActionMessage::Status { invocation: request.invocation });
        assert!(matches!(reply(&other, private).await, ActionMessage::Observation { update: None, .. }));
        let mut changed = request.clone(); changed.argument = Some("\"private changed intent\"".into());
        let changed_id = send(&caller, room, 52, invoke(&changed));
        assert!(matches!(reply(&caller, changed_id).await, ActionMessage::Rejected { .. }));
        let channel = fixture.invocation_channel();
        let mut deliberately_lagging = fixture.state.peer_action_queue.transitions_for_test();
        update_app(&channel, wire_id, &tilo_actions::InvocationUpdate::Progress { invocation: request.invocation, sequence: 1, phase: "working".into(), completed: Some(1), total: Some(2), message: Some("private progress".into()) }).await.unwrap();
        // No status query is needed to receive the durable owner-directed update.
        assert!(matches!(reply(&caller, duplicate).await, ActionMessage::Observation { update: Some(tilo_actions::InvocationUpdate::Progress { sequence: 1, .. }), .. }));
        for sequence in 2..=34 {
            update_app(&channel, wire_id, &tilo_actions::InvocationUpdate::Item { invocation: request.invocation, sequence, value: serde_json::json!(format!("private-item-{sequence}")) }).await.unwrap();
            assert!(matches!(reply(&caller, duplicate).await, ActionMessage::Observation { update: Some(tilo_actions::InvocationUpdate::Item { sequence: delivered, .. }), .. } if delivered == sequence));
        }
        assert!(matches!(deliberately_lagging.recv().await, Err(tokio::sync::broadcast::error::RecvError::Lagged(_))), "bounded transition loss is explicit, never silent item coalescing");
        assert!(tokio::time::timeout(Duration::from_millis(100), other.recv_action()).await.is_err(), "other certified owner never receives private notifications");
        let status = send(&caller, room, 52, ActionMessage::Status { invocation: request.invocation });
        assert!(matches!(reply(&caller, status).await, ActionMessage::Observation { update: Some(tilo_actions::InvocationUpdate::Item { sequence: 34, .. }), .. }));
        assert!(update_app(&channel, wire_id, &tilo_actions::InvocationUpdate::Progress { invocation: request.invocation, sequence: 1, phase: "invalid replay".into(), completed: None, total: None, message: None }).await.is_err());
        assert!(update_app(&channel, wire_id, &tilo_actions::InvocationUpdate::Succeeded { invocation: request.invocation, output: tilo_actions::ActionOutput { value: Some(serde_json::json!("undeclared private value")), resources: vec![] } }).await.is_err());
        let cancel = send(&caller, room, 52, ActionMessage::Cancel { invocation: request.invocation });
        assert!(matches!(reply(&caller, cancel).await, ActionMessage::CancellationRequested { .. }));
        let running = send(&caller, room, 52, ActionMessage::Status { invocation: request.invocation });
        assert!(matches!(reply(&caller, running).await, ActionMessage::Observation { update: Some(tilo_actions::InvocationUpdate::Item { sequence: 34, .. }), cancellation_requested: true, .. }));
        let terminal = tilo_actions::InvocationUpdate::Succeeded { invocation: request.invocation, output: tilo_actions::ActionOutput::default() };
        update_app(&channel, wire_id, &terminal).await.unwrap();
        let done = send(&caller, room, 52, invoke(&request));
        assert!(matches!(reply(&caller, done).await, ActionMessage::Observation { update: Some(update), .. } if update == terminal));
        assert_eq!(std::fs::read_to_string(root.path().join("effect-counter")).unwrap(), "1");
        let receipts = serde_json::to_string(&fixture.membership.as_ref().unwrap().read().workspaces[0].effect_receipts).unwrap();
        assert!(!receipts.contains("private"));
        assert!(receipts.contains("succeeded"));
        // Losing a private response and reconnecting queries the same key.
        let _lost_reply = send(&caller, room, 52, invoke(&request));
        caller.close().await;
        let caller = client(&relay, 52, room).await;
        let recovered = send(&caller, room, 52, ActionMessage::Status { invocation: request.invocation });
        assert!(matches!(reply(&caller, recovered).await, ActionMessage::Observation { update: Some(update), .. } if update == terminal));
        assert!(rx.lock().await.try_recv().is_err());
        // Current room authority gates catalogue, durable disclosure and invoke.
        let membership = fixture.membership.as_ref().unwrap().clone();
        membership.write().workspaces[0].set_owner("replacement-owner");
        for message in [
            ActionMessage::CatalogueRefresh { scope: None },
            invoke(&request),
            ActionMessage::Status {
                invocation: request.invocation,
            },
        ] {
            let revoked = send(&caller, room, 52, message);
            assert!(matches!(reply(&caller, revoked).await, ActionMessage::Rejected { .. }));
        }
        membership.write().workspaces[0].set_owner(&local_caller.admitted.principal_id);
        // Silence is retained as uncertain and cannot re-admit an effect.
        fixture.state.surfaces.lock().unwrap().get_mut(&71).unwrap().actions[0].contract.confirmation = tilo_actions::ConfirmationPolicy::None;
        let mut silent = request.clone(); silent.invocation = tilo_actions::InvocationId::new();
        let started = send(&caller, room, 52, invoke(&silent));
        assert!(matches!(reply(&caller, started).await, ActionMessage::Observation { update: Some(tilo_actions::InvocationUpdate::Accepted { .. }), .. }));
        let silent_delivery = stream.read_request().send().promise.await.unwrap();
        let silent_wire = silent_delivery.get().unwrap().get_id();
        fixture.state.pending_invocations.lock().unwrap().get_mut(&silent_wire).unwrap().last_activity = std::time::Instant::now().checked_sub(Duration::from_secs(2)).unwrap();
        fixture.state.expire_stale_invocations_at(std::time::Instant::now(), Duration::from_secs(1), Duration::from_secs(1));
        let unknown = send(&caller, room, 52, invoke(&silent));
        assert!(matches!(reply(&caller, unknown).await, ActionMessage::Observation { uncertain: true, .. }));
        assert!(rx.lock().await.try_recv().is_err());
        // Restart the real broker on its original private journal and original
        // room tree. Neither retained terminal nor uncertain identity dispatches.
        server.abort(); worker.abort(); let _ = server.await; let _ = worker.await;
        session.close().await; drop(session); drop(channel); drop(fixture); tokio::task::yield_now().await;
        let (mut reopened, _, mut reopened_request) = durable_fixture(root.path(), Some(membership));
        package_primary(&mut reopened, &mut reopened_request);
        assert!(reopened.state.certificate_authority.set(authority.clone()).is_ok());
        assert_eq!(reopened.state.invocation_host().unwrap(), request.host);
        let host_client = client(&relay, 54, room).await;
        let session = Arc::new(RelayCoeditSession::join(host_client, ws, device(54, false).1.device_public_key).unwrap());
        while !matches!(session.recv().await, Some(tilo_relay::Inbound::Ready { .. })) {}
        let worker = reopened.state.start_peer_action_bridge(reopened.directories.clone()).unwrap();
        let (queue, host) = reopened.broker.peer_action_attachment().unwrap();
        let server_session = session.clone();
        let server = tokio::spawn(async move { queue.serve(server_session, room, host).await });
        tilo_test_env::within("the recovered broker catalogue", async { loop {
            let id = send(
                &caller,
                room,
                52,
                ActionMessage::CatalogueRefresh { scope: None },
            );
            if matches!(tokio::time::timeout(Duration::from_millis(250), reply(&caller, id)).await, Ok(ActionMessage::BrokerCatalogue { .. })) { break; }
        }}).await;
        let recovered = send(&caller, room, 52, invoke(&request));
        assert!(matches!(reply(&caller, recovered).await, ActionMessage::Observation { update: Some(update), .. } if update == terminal));
        let unknown = send(&caller, room, 52, invoke(&silent));
        assert!(matches!(reply(&caller, unknown).await, ActionMessage::Observation { uncertain: true, .. }));
        assert!(reopened.invocation_rx.try_recv().is_err());
        assert_eq!(std::fs::read_to_string(root.path().join("effect-counter")).unwrap(), "1");
        // A failed durable barrier withdraws the live broker lane. It cannot
        // claim Accepted or cause another application stream delivery.
        reopened.state.journal.lock().unwrap().as_mut().unwrap().as_mut().unwrap().fail_at = Some(BarrierFailure::File);
        let mut blocked = request.clone(); blocked.invocation = tilo_actions::InvocationId::new();
        let refused = send(&caller, room, 52, invoke(&blocked));
        assert!(tilo_test_env::within("`server`", server).await.unwrap().is_err());
        let unavailable = send(&caller, room, 52, ActionMessage::Status { invocation: blocked.invocation });
        tilo_test_env::within("the refused request to become unavailable", async { loop {
            match caller.recv_action().await {
                Some(EnvelopeV2::Delivered { action, .. }) if action.request_id == refused => assert!(matches!(action.message, ActionMessage::Rejected { .. })),
                Some(EnvelopeV2::Unavailable { request_id, .. }) if request_id == unavailable => break,
                None => break,
                _ => {},
            }
        }}).await;
        assert!(reopened.invocation_rx.try_recv().is_err());
        assert_eq!(std::fs::read_to_string(root.path().join("effect-counter")).unwrap(), "1");
        worker.abort(); let _ = worker.await;
        session.close().await; caller.close().await; other.close().await; relay.shutdown().await;
    }).await;
}

#[test]
fn native_peer_facade_recording_only_startup_cannot_advertise_durable_attachment() {
    let root = tempfile::tempdir().unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .expect("secure broker fixture root");
    }
    let mut tree = WorkspaceTree::default();
    let mut room = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId(71),
        "Explicit native peer",
    );
    room.set_owner(principal_id_for_root(device(52, false).1.user_public_key));
    let room_id = Uuid::parse_str(&room.room.to_string()).unwrap();
    tree.workspaces.push(room);
    tree.persist_path = Some(root.path().join("must-not-be-used/state.json"));
    let journal_root = root.path().join("explicit-private-journal");
    let config = crate::peer_actions::NativePeerBrokerConfig {
        journal_root: journal_root.clone(),
        certificate_authority: Arc::new(LiveAuthority {
            room: room_id,
            root: device(52, false).1.user_public_key,
            revocations: Mutex::new(RevocationList::new()),
        }),
    };
    let sink = Arc::new(RecordingSink::default());
    let broker = crate::collab_broker::spawn_native_peer(
        root.path().join("app.sock"),
        tree.shared().authority(),
        PeerPublisher::new(sink),
        config,
    )
    .unwrap();
    assert!(
        broker
            .app_action_broker()
            .unwrap()
            .peer_action_attachment()
            .is_err(),
        "a void recording sink is not the serialized session owner barrier"
    );
    assert!(journal_root.join("records.json").is_file());
    assert!(!root.path().join("must-not-be-used").exists());
    drop(broker);
}

/// A real local capability whose pending resolve futures are released by the
/// test. This exercises draining/processing rather than a full unread channel.
struct BlockedBridgeDirectory {
    entered: Rc<std::cell::Cell<usize>>,
    releases: Rc<RefCell<std::collections::VecDeque<tokio::sync::oneshot::Sender<()>>>>,
}
#[allow(refining_impl_trait, reason = "generated Capnp endpoint Promise shape")]
impl action_directory::Server for BlockedBridgeDirectory {
    fn resolve(
        self: Rc<Self>,
        _params: action_directory::ResolveParams,
        mut results: action_directory::ResolveResults,
    ) -> Promise<(), capnp::Error> {
        self.entered.set(self.entered.get() + 1);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.releases.borrow_mut().push_back(tx);
        Promise::from_future(async move {
            rx.await
                .map_err(|_| capnp::Error::failed("test directory cancelled".into()))?;
            results.get().init_resolution().set_stale_revision(());
            Ok(())
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_peer_facade_draining_bridge_retains_inflight_permits() {
    let local = tokio::task::LocalSet::new();
    let (queue, caller, recipient, relay, _root, _relay_root, _releases) = local
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let relay_root = tempfile::tempdir().unwrap();
            let (fixture, _, mut request) = durable_fixture(root.path(), None);
            request.revision = Some("directory-r1".into());
            let room = Uuid::parse_str(&fixture.cluster_id).unwrap();
            let ws = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
            let (relay_key, relay_cert) = device(55, true);
            let relay = Relay::start(
                RelayConfig {
                    state_root: relay_root.path().into(),
                    workspaces: vec![WorkspaceBinding {
                        id: ws,
                        pin: true,
                        replication: tilo_relay::ReplicationIntent::FullWorkspace,
                        admitted_users: vec![],
                        shell_authority: None,
                        checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::default(),
                    }],
                    ..Default::default()
                },
                RelayIdentity {
                    user_public_key: relay_cert.user_public_key,
                    device_signing_key: relay_key,
                    device_cert: relay_cert,
                    revocations: RevocationList::new(),
                },
            )
            .await
            .unwrap();
            relay
                .set_workspace_membership(
                    ws,
                    true,
                    [52, 54]
                        .into_iter()
                        .map(|seed| device(seed, false).1.device_public_key)
                        .collect(),
                    relay_root.path().into(),
                )
                .await
                .unwrap();
            let caller = client(&relay, 52, room).await;
            let recipient = client(&relay, 54, room).await;
            recipient
                .send_action(EnvelopeV2::RegisterBroker {
                    room,
                    host: request.host,
                })
                .unwrap();
            assert!(matches!(
                recipient.recv_action().await,
                Some(EnvelopeV2::ActionsReady { .. })
            ));
            send(&caller, room, 52, invoke(&request));
            let tilo_relay::client::CertifiedActionEvent::Delivered(evidence) =
                recipient.recv_certified_action().await.unwrap()
            else {
                panic!("real certified request required")
            };

            let entered = Rc::new(std::cell::Cell::new(0));
            let releases = Rc::new(RefCell::new(std::collections::VecDeque::new()));
            fixture
                .directories
                .directories
                .borrow_mut()
                .get_mut(&71)
                .unwrap()
                .directory = capnp_rpc::new_client(BlockedBridgeDirectory {
                entered: entered.clone(),
                releases: releases.clone(),
            });
            fixture
                .state
                .start_peer_action_bridge(fixture.directories.clone())
                .unwrap();
            let queue = fixture.state.peer_action_queue();
            let completed = Rc::new(std::cell::Cell::new(0));
            for _ in 0..32 {
                let (queue, evidence, completed) =
                    (queue.clone(), evidence.clone(), completed.clone());
                tokio::task::spawn_local(async move {
                    assert!(matches!(
                        queue.request_for_test(Some(evidence)).await,
                        Ok(ActionMessage::Rejected { .. })
                    ));
                    completed.set(completed.get() + 1);
                });
            }
            tilo_test_env::within(
                "all32 requests must drain into actual blocked directory futures",
                async {
                    while entered.get() != 32 {
                        tokio::task::yield_now().await;
                    }
                },
            )
            .await;
            assert!(
                queue.request_for_test(None).await.is_err(),
                "33rd request must be refused while32 drained requests remain inflight"
            );
            assert_eq!(queue.available_permits_for_test(), 0);
            assert_eq!(completed.get(), 0);

            releases.borrow_mut().pop_front().unwrap().send(()).unwrap();
            tilo_test_env::within("the first request to complete", async {
                while completed.get() != 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            assert_eq!(
                queue.available_permits_for_test(),
                1,
                "one completed broker task restores one permit"
            );
            assert!(
                queue.request_for_test(None).await.is_ok(),
                "restored capacity accepts a health request"
            );
            assert_eq!(queue.available_permits_for_test(), 1);

            let (more, evidence) = (queue.clone(), evidence.clone());
            let waiting_caller =
                tokio::task::spawn_local(
                    async move { more.request_for_test(Some(evidence)).await },
                );
            tilo_test_env::within("the 33rd request to enter", async {
                while entered.get() != 33 {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            assert_eq!(queue.available_permits_for_test(), 0);
            waiting_caller.abort();
            assert!(waiting_caller.await.unwrap_err().is_cancelled());
            assert_eq!(
                queue.available_permits_for_test(),
                0,
                "caller cancellation must not release still-running broker work"
            );
            assert!(
                fixture.invocation_rx.is_empty(),
                "blocked or stale directory never dispatches effects"
            );
            (queue, caller, recipient, relay, root, relay_root, releases)
        })
        .await;
    drop(local); // Cancels the actual32 spawned broker tasks, dropping their guards.
    assert_eq!(
        queue.available_permits_for_test(),
        32,
        "worker cancellation restores all held permits"
    );
    assert!(
        queue.request_for_test(None).await.is_err(),
        "cancelled receiver cannot accept work"
    );
    caller.close().await;
    recipient.close().await;
    relay.shutdown().await;
}

/// A device session spoken frame by frame, so a test sends exactly what an
/// older build or a misbehaving device would: an unsigned action on
/// `tilo-relay/3`, or a chosen signed frame, stale or repeated, on `tilo-relay/4`.
struct RawSession {
    _endpoint: iroh::Endpoint,
    _connection: iroh::endpoint::Connection,
    send: iroh::endpoint::SendStream,
    reader: tilo_delivery_protocol::transport_v2::SessionReader<iroh::endpoint::RecvStream>,
}
impl RawSession {
    async fn open(relay: &Relay, seed: u8, room: Uuid, alpn: &[u8]) -> Self {
        let (key, cert) = device(seed, false);
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .bind()
            .await
            .unwrap();
        let connection = endpoint.connect(relay.node_addr(), alpn).await.unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let hello = tilo_relay::auth::Hello {
            cert,
            client_nonce: [seed; 32],
        };
        tilo_relay::frame::write_frame(&mut send, &postcard::to_allocvec(&hello).unwrap())
            .await
            .unwrap();
        let ack: tilo_relay::auth::HelloAck = postcard::from_bytes(
            &tilo_relay::frame::read_frame(&mut recv, tilo_relay::transport::MAX_FRAME_BYTES)
                .await
                .unwrap(),
        )
        .unwrap();
        let ack2 = tilo_relay::auth::build_hello_ack2(&key, &ack.server_nonce);
        tilo_relay::frame::write_frame(&mut send, &postcard::to_allocvec(&ack2).unwrap())
            .await
            .unwrap();
        let mut session = Self {
            _endpoint: endpoint,
            _connection: connection,
            send,
            reader: tilo_delivery_protocol::transport_v2::SessionReader::new(recv, true),
        };
        let workspace_id = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
        session
            .write(EnvelopeV2::State(Envelope::Subscribe { workspace_id }))
            .await;
        session
            .until("the raw session to be ready", |frame| {
                matches!(frame, EnvelopeV2::State(Envelope::Ready { .. })).then_some(())
            })
            .await;
        session.write(EnvelopeV2::EnableActions { room }).await;
        session
            .until("the raw private lane", |frame| {
                matches!(frame, EnvelopeV2::ActionsReady { .. }).then_some(())
            })
            .await;
        session.write(EnvelopeV2::EnableBrokerClient { room }).await;
        session
    }
    async fn write(&mut self, frame: EnvelopeV2) {
        self.send
            .write_all(&tilo_delivery_protocol::transport_v2::encode(&frame).unwrap())
            .await
            .unwrap();
    }
    async fn until<T>(&mut self, what: &str, mut pick: impl FnMut(EnvelopeV2) -> Option<T>) -> T {
        tilo_test_env::within(what, async {
            loop {
                if let Some(found) = pick(self.reader.read_next().await.unwrap().unwrap()) {
                    return found;
                }
            }
        })
        .await
    }
    /// The broker's answer to `id`, or the host's refusal to route it.
    async fn answer(&mut self, id: Uuid) -> Answer {
        self.until("the answer to this request", |frame| match frame {
            EnvelopeV2::Delivered { action, .. } if action.request_id == id => {
                Some(Ok(action.message))
            }
            EnvelopeV2::Unavailable {
                request_id, reason, ..
            } if request_id == id => Some(Err(reason)),
            _ => None,
        })
        .await
    }
}

type Answer = Result<ActionMessage, tilo_delivery_protocol::peer_actions::UnavailableReason>;

fn directed(room: Uuid, seed: u8, message: ActionMessage) -> DirectedAction {
    DirectedAction {
        room,
        request_id: Uuid::new_v4(),
        source: device(seed, false).1.device_public_key,
        target: device(54, false).1.device_public_key,
        message,
    }
}

fn signed_by(
    action: &DirectedAction,
    seed: u8,
    issued_at_ns: u64,
) -> tilo_delivery_protocol::peer_actions::SignedDirectedAction {
    tilo_delivery_protocol::peer_actions::SignedDirectedAction::sign(
        action,
        issued_at_ns,
        &device(seed, false).0,
    )
    .unwrap()
}

fn rejected(code: &str) -> ActionMessage {
    ActionMessage::Rejected { code: code.into() }
}

#[tokio::test(flavor = "current_thread")]
async fn native_peer_facade_admits_effects_only_on_their_source_device_signature() {
    use tilo_clock::unix_now_ns as now_ns;
    use tilo_delivery_protocol::transport_v2::{ALPN_AGENT_READS_V1, ALPN_SIGNED_ACTIONS_V1};
    tokio::task::LocalSet::new().run_until(async {
        let root = tempfile::tempdir().unwrap();
        let relay_root = tempfile::tempdir().unwrap();
        let (mut fixture, _, mut request) = durable_fixture(root.path(), None);
        package_primary(&mut fixture, &mut request);
        let room = Uuid::parse_str(&fixture.cluster_id).unwrap();
        let ws = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
        let authority = Arc::new(LiveAuthority { room, root: device(52, false).1.user_public_key, revocations: Mutex::new(RevocationList::new()) });
        assert!(fixture.state.certificate_authority.set(authority).is_ok());
        let (relay_key, relay_cert) = device(55, true);
        let relay = Relay::start(RelayConfig { state_root: relay_root.path().into(), workspaces: vec![WorkspaceBinding { id: ws, pin: true, replication: tilo_relay::ReplicationIntent::FullWorkspace, admitted_users: vec![], shell_authority: None, checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::default() }], ..Default::default() }, RelayIdentity { user_public_key: relay_cert.user_public_key, device_signing_key: relay_key, device_cert: relay_cert, revocations: RevocationList::new() }).await.unwrap();
        relay.set_workspace_membership(ws, true, [52, 53, 54].into_iter().map(|seed| device(seed, false).1.device_public_key).collect(), relay_root.path().into()).await.unwrap();
        let host_client = client(&relay, 54, room).await;
        let session = Arc::new(RelayCoeditSession::join(host_client, ws, device(54, false).1.device_public_key).unwrap());
        assert!(matches!(session.recv().await, Some(tilo_relay::Inbound::Ready { .. })));
        let worker = fixture.state.start_peer_action_bridge(fixture.directories.clone()).unwrap();
        let (queue, host) = fixture.broker.peer_action_attachment().unwrap();
        let server_session = session.clone();
        let server = tokio::spawn(async move { queue.serve(server_session, room, host).await });
        let mut current = RawSession::open(&relay, 52, room, ALPN_SIGNED_ACTIONS_V1).await;
        let mut older = RawSession::open(&relay, 53, room, ALPN_AGENT_READS_V1).await;
        tilo_test_env::within("the attached broker to serve reads", async { loop {
            let read = directed(room, 52, ActionMessage::CatalogueRefresh { scope: None });
            current.write(EnvelopeV2::SignedDirected(signed_by(&read, 52, now_ns()))).await;
            if matches!(tokio::time::timeout(Duration::from_millis(250), current.answer(read.request_id)).await, Ok(Ok(ActionMessage::BrokerCatalogue { .. }))) { break; }
        }}).await;

        // An older device, or a host vouching for a device it never heard
        // from: the broker sees an unsigned delivery under a valid member
        // certificate. Reads are still served; effects are refused before any
        // admission, with a code that says why.
        let read = directed(room, 53, ActionMessage::CatalogueRefresh { scope: None });
        older.write(EnvelopeV2::Directed(read.clone())).await;
        assert!(matches!(older.answer(read.request_id).await, Ok(ActionMessage::BrokerCatalogue { .. })));
        let mut unsigned = request.clone();
        unsigned.invocation = tilo_actions::InvocationId::new();
        for message in [invoke(&unsigned), ActionMessage::Cancel { invocation: unsigned.invocation }] {
            let effect = directed(room, 53, message);
            older.write(EnvelopeV2::Directed(effect.clone())).await;
            assert_eq!(older.answer(effect.request_id).await, Ok(rejected("action_signature_required")));
        }
        assert!(fixture.invocation_rx.try_recv().is_err(), "an unsigned effect never dispatches");

        // A signature by another device never leaves the host: device 53
        // signed it, and device 52's session claims it as its own.
        let by_other = signed_by(&directed(room, 53, invoke(&unsigned)), 53, now_ns());
        let mut wire = postcard::to_allocvec(&EnvelopeV2::SignedDirected(by_other)).unwrap();
        let at = wire.windows(32).position(|w| w == device(53, false).1.device_public_key).unwrap();
        wire.get_mut(at..at.saturating_add(32)).unwrap().copy_from_slice(&device(52, false).1.device_public_key);
        let EnvelopeV2::SignedDirected(claimed) = tilo_delivery_protocol::transport_v2::decode(&wire).unwrap() else { panic!("signed frame expected") };
        let claimed_id = claimed.action().request_id;
        current.write(EnvelopeV2::SignedDirected(claimed)).await;
        assert_eq!(current.answer(claimed_id).await, Err(tilo_delivery_protocol::peer_actions::UnavailableReason::NotAuthorised));

        // The device's own signature admits the effect, as that person.
        let effect = directed(room, 52, invoke(&request));
        let frame = signed_by(&effect, 52, now_ns());
        current.write(EnvelopeV2::SignedDirected(frame.clone())).await;
        assert!(matches!(current.answer(effect.request_id).await, Ok(ActionMessage::Observation { update: Some(tilo_actions::InvocationUpdate::Accepted { invocation }), .. }) if invocation == request.invocation));
        assert!(fixture.invocation_rx.try_recv().is_ok(), "the signed effect dispatched");
        // The same signed frame delivered again is refused.
        current.write(EnvelopeV2::SignedDirected(frame)).await;
        assert_eq!(current.answer(effect.request_id).await, Ok(rejected("action_replayed")));
        // So is a frame signed outside the freshness window, either way.
        let window = i64::try_from(tilo_delivery_protocol::peer_actions::ACTION_SIGNATURE_TOLERANCE_NS).unwrap();
        for skew in [window.saturating_mul(-2), window.saturating_mul(2)] {
            let mut late = request.clone();
            late.invocation = tilo_actions::InvocationId::new();
            let stale = directed(room, 52, invoke(&late));
            current.write(EnvelopeV2::SignedDirected(signed_by(&stale, 52, now_ns().saturating_add_signed(skew)))).await;
            assert_eq!(current.answer(stale.request_id).await, Ok(rejected("action_stale")));
        }
        assert!(fixture.invocation_rx.try_recv().is_err(), "refused frames never dispatch");

        server.abort(); worker.abort(); let _ = server.await; let _ = worker.await;
        session.close().await; relay.shutdown().await;
    }).await;
}
