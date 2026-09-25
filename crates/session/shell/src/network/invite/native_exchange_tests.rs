use super::*;
use qos_peer_state::NeighbourMsg;
use qos_peer_state::join::{JoinTransportResponse, MemberRole};
use qos_session_peer::{
    SessionOwner,
    collaboration::{CollaborationService, IssueInvitationRequest},
};
use std::sync::{Arc, Mutex};
use tilo_relay::workspace_join::{
    WORKSPACE_JOIN_ALPN, WorkspaceJoinProtocol, WorkspaceJoinService,
};

#[path = "native_exchange_tests/refresh.rs"]
mod refresh;
type ActualAuthority = Arc<Mutex<Option<(SessionOwner, CollaborationService)>>>;

#[derive(Clone)]
struct ActualService {
    authority: ActualAuthority,
    receiving_owner: Arc<Mutex<Option<NativeWorkspaceOwner>>>,
    room: RoomId,
    saw_persisted_complete: Arc<std::sync::atomic::AtomicBool>,
}
impl std::fmt::Debug for ActualService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActualService")
    }
}
impl WorkspaceJoinService for ActualService {
    async fn handle(
        &self,
        remote: [u8; 32],
        request: JoinTransportRequest,
    ) -> JoinTransportResponse {
        let now = now_ns(SystemTime::now()).unwrap();
        let mut state = self.authority.lock().unwrap();
        let Some((owner, service)) = state.as_mut() else {
            return JoinTransportResponse::Error {
                code: 503,
                message: "fixture join service retired".into(),
            };
        };
        let result = match request {
            JoinTransportRequest::Inspect { signed_invitation } => {
                service.inspect(owner, &signed_invitation, now).map(|join| {
                    JoinTransportResponse::Inspection(join::JoinInspection {
                        invite_id: join.invite_id(),
                        grant: join.grant().clone(),
                    })
                })
            }
            JoinTransportRequest::Challenge(request) => service
                .challenge(owner, remote, request, now)
                .map(JoinTransportResponse::Challenge),
            JoinTransportRequest::Complete(request) => {
                // An actual native store must already contain these exact signed bytes.
                let pending = self
                    .receiving_owner
                    .lock()
                    .unwrap()
                    .as_ref()
                    .expect("active native enrollment custody")
                    .pending_enrollment(self.room)
                    .unwrap()
                    .expect("Complete crossed network before native persistence");
                let intent =
                    VerifiedNativeEnrollmentIntent::restore(pending.intent_bytes()).unwrap();
                assert_eq!(
                    intent.complete_bytes(),
                    postcard::to_stdvec(&JoinTransportRequest::Complete(request.clone())).unwrap()
                );
                self.saw_persisted_complete
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                service
                    .complete(owner, remote, request, now)
                    .map(|completion| JoinTransportResponse::Complete(completion.bootstrap))
            }
            _ => {
                return JoinTransportResponse::Error {
                    code: 400,
                    message: "unexpected membership operation".into(),
                };
            }
        };
        result.unwrap_or_else(|error| JoinTransportResponse::Error {
            code: 403,
            message: error.to_string(),
        })
    }
}

#[tokio::test]
async fn native_enrollment_pinned_exchange_persists_complete_before_send() {
    actual_exchange(false).await;
}

#[tokio::test]
async fn native_enrollment_restart_automatically_refreshes_and_resends_retained_history() {
    Box::pin(actual_exchange(true)).await;
}

async fn actual_exchange(restart: bool) {
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{
        DeviceCapabilities,
        persist::{IdentityPaths, read_device_key},
        sign_device_cert,
    };
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let mut remote_owner =
        SessionOwner::initialize(&directory.path().join("actual-authority")).unwrap();
    let governor = remote_owner.peer().authority().clone();
    let governor_key =
        read_device_key(&IdentityPaths::under(remote_owner.root().join("identity")).device_privkey)
            .unwrap();
    let host = remote_owner.host_certificate().clone();
    let host_key = read_device_key(
        &IdentityPaths::under(remote_owner.root().join("host-identity")).device_privkey,
    )
    .unwrap();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let room = RoomId::parse(remote_owner.peer().room()).unwrap();
    let topic = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    let hint = IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::CertifiedHost,
        room: room.to_string(),
        workspace_id: topic,
        governing_authority: governor.clone(),
        endpoint_addr: server.addr(),
        host_user_public_key: host.user_public_key,
        host_device_certificate: host.clone(),
        packet_relay_url: "https://relay.invalid".into(),
    }
    .transport_hint(&host_key)
    .unwrap();
    let device_key = SigningKey::from_bytes(&[81; 32]);
    let root_key = SigningKey::from_bytes(&[82; 32]);
    let mut device = DeviceCert {
        user_public_key: root_key.verifying_key().to_bytes(),
        device_public_key: device_key.verifying_key().to_bytes(),
        device_label: "Native receiver".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut device, &root_key).unwrap();
    let service = CollaborationService::new(vec![hint.clone()]);
    let now = now_ns(SystemTime::now()).unwrap();
    let issued = service
        .issue(
            &mut remote_owner,
            governor.device_public_key,
            IssueInvitationRequest {
                request_id: [83; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(device.user_public_key),
                invitee_name: Some("Native receiver".into()),
                expires_at_ns: now + 60_000_000_000,
            },
            now,
        )
        .unwrap();
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
    let local = tree.active;
    tree.persist_path = Some(directory.path().join("state.json"));
    let receiving_owner = NativeWorkspaceOwner::open(tree.shared()).unwrap();
    let saw = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let actual = ActualService {
        authority: Arc::new(Mutex::new(Some((remote_owner, service)))),
        receiving_owner: Arc::new(Mutex::new(Some(receiving_owner.clone()))),
        room,
        saw_persisted_complete: saw.clone(),
    };
    let actual_authority = actual.authority.clone();
    let receiving_custody = actual.receiving_owner.clone();
    let relay_root = directory.path().join("relay");
    // Native hosting requires a real durable baseline. Reuse the exact
    // already-committed governor room graph, never an empty synthetic snapshot.
    {
        let authority = actual_authority.lock().unwrap();
        let authority = authority.as_ref().unwrap();
        let mut room_document = authority
            .0
            .peer()
            .admitted_room()
            .expect("actual admitted room")
            .clone();
        let store = tilo_relay::store::WorkspaceStore::open(&relay_root, topic, true).unwrap();
        store.save_snapshot(&room_document.save()).unwrap();
    }
    let relay = tilo_relay::Relay::start_on_endpoint_with_protocols(
        tilo_relay::RelayConfig {
            state_root: relay_root.clone(),
            identity_root: directory.path().join("relay-identity"),
            full_replica_status: directory.path().join("replica/status.json"),
            home_peer_bundle: directory.path().join("home-peer.toml"),
            trust_policy: tilo_relay::TrustPolicy::AllowList {
                users: vec![hex::encode(governor.user_public_key)],
            },
            workspaces: vec![tilo_relay::WorkspaceBinding {
                id: topic,
                pin: true,
                replication: tilo_relay::ReplicationIntent::FullWorkspace,
                admitted_users: vec![],
                shell_authority: Some(
                    qos_peer_state::retained_proof::RetentionAuthority::new(
                        &room.to_string(),
                        governor.clone(),
                    )
                    .unwrap(),
                ),
                checkpoint_prefix_recovery:
                    tilo_relay::CheckpointPrefixRecovery::NativeOwnerDurableBaseline,
            }],
            ..Default::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: governor.user_public_key,
            device_signing_key: host_key,
            device_cert: host.clone(),
            revocations: RevocationList::new(),
        },
        server.clone(),
        vec![(
            WORKSPACE_JOIN_ALPN.to_vec(),
            Box::new(WorkspaceJoinProtocol::new(actual)),
        )],
    )
    .await
    .unwrap();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device_key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let identity = HostIdentity {
        device_cert: device.clone(),
        device_signing_key: device_key,
        workspace_device_certs: Default::default(),
        workspace_revocations: Default::default(),
        identity_paths: None,
    };
    let result = prepare_and_complete(
        WorkspaceJoinClient::new(endpoint.clone()),
        identity.clone(),
        topic,
        local,
        join::encode_invitation_text(&issued.signed_invitation),
        receiving_owner.clone(),
    )
    .await;
    let enrollment = result.expect("actual pinned canonical exchange must complete");
    assert!(saw.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(enrollment.local_device(), &device);
    receiving_owner
        .install(&enrollment, SystemTime::now())
        .unwrap();
    receiving_owner
        .with_current(room, |view| {
            assert_eq!(view.device(), &device);
            assert!(view.peer().has_governance());
            Ok(())
        })
        .unwrap();
    // Reconcile relay access only from the actual issuing owner's committed peer.
    let (access, publications) = {
        let state = actual_authority.lock().unwrap();
        let state = state.as_ref().unwrap();
        (
            state.0.peer().governed_access(SystemTime::now()).unwrap(),
            state
                .0
                .outgoing()
                .iter()
                .map(|entry| (entry.bytes.to_vec(), entry.kind))
                .collect::<Vec<_>>(),
        )
    };
    relay
        .replace_governed_workspace_access(topic, true, access, relay_root)
        .await
        .unwrap();
    let publisher_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&governor_key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let client = tilo_relay::RelayClient::connect_v2_to_enrolled_home_peer(
        publisher_endpoint.clone(),
        server.addr(),
        host.user_public_key,
        host.device_public_key,
        governor.clone(),
        governor_key,
        RevocationList::new(),
    )
    .await
    .unwrap();
    let publisher =
        tilo_relay::RelayCoeditSession::join(client, topic, governor.device_public_key).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if matches!(
                publisher.recv().await,
                Some(tilo_relay::Inbound::Ready { .. })
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let (commands, actor) = crate::network::actor::Actor::spawn_reporting_addrs(
        endpoint.clone(),
        iroh::address_lookup::memory::MemoryLookup::new(),
        Some(identity.clone()),
        Arc::new(Mutex::new(Vec::new())),
    );
    let (events, mut received) = tokio::sync::mpsc::channel(64);
    commands
        .send(crate::network::actor::NetCmd::JoinWorkspace {
            ws: topic,
            on_event: events,
        })
        .await
        .unwrap();
    commands
        .send(crate::network::actor::NetCmd::NativeConnect(
            crate::network::actor::native_ingress::Connect::new(receiving_owner.clone(), room)
                .unwrap(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if matches!(
                received.recv().await,
                Some(crate::network::actor::NetEvent::HostReady { .. })
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    for (bytes, kind) in &publications {
        let message: NeighbourMsg = postcard::from_bytes(bytes).unwrap();
        let (channel, _) = qos_peer_state::transport::retention(
            &message,
            *kind == qos_peer_state::transport::Retention::Checkpoint,
        )
        .unwrap();
        match kind {
            qos_peer_state::transport::Retention::Checkpoint => {
                publisher.publish_checkpoint(channel, bytes.clone())
            }
            qos_peer_state::transport::Retention::Delta => {
                publisher.publish_retained_delta(channel, bytes.clone())
            }
        }
        .unwrap();
    }
    let mut last = None;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut count = 0;
        while count < publications.len() {
            if let Some(crate::network::actor::NetEvent::NativeInput(input)) = received.recv().await
            {
                assert_eq!(input.author(), governor.device_public_key);
                input
                    .admit(&identity)
                    .expect("genuine native relay input must cross actual owner barrier");
                last = Some(input);
                count += 1;
            }
        }
    })
    .await
    .unwrap();
    receiving_owner
        .with_current(room, |view| {
            assert!(
                view.peer().document("so.tilo.chat", "").is_some(),
                "actual retained Chat admitted before fanout"
            );
            Ok(())
        })
        .unwrap();
    commands
        .send(crate::network::actor::NetCmd::LeaveWorkspace { ws: topic })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if !relay.is_subscribed(topic, &device.device_public_key).await {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        last.unwrap().admit(&identity).is_err(),
        "closed session input cannot cross the owner again"
    );
    drop(commands);
    actor.abort();
    let _ = actor.await;
    drop(received);
    if restart {
        Box::pin(refresh::restart_after_offline_snapshot(
            &directory,
            receiving_owner,
            room,
            &identity,
            &endpoint,
            &relay,
            &publisher,
            actual_authority,
            receiving_custody,
        ))
        .await;
    }
    publisher.close().await;
    publisher_endpoint.close().await;
    endpoint.close().await;
    relay.shutdown().await;
}
