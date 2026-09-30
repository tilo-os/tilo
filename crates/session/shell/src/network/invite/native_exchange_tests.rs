#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
use super::*;
use crate::network::actor::HostIdentity;
use crate::shell::native_workspace_owner::NativeWorkspaceOwner;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tilo_delivery_protocol::workspace_join::{WORKSPACE_JOIN_ALPN, WorkspaceJoinService};
use tilo_joined_room::enroll::prepare_and_complete;
use tilo_joined_room::testing::governor::TestGovernor;
use tilo_joined_room::testing::ns as now_ns;
use tilo_peer_state::NeighbourMsg;
use tilo_peer_state::join::{self, JoinTransportRequest};
use tilo_peer_state::join::{JoinTransportResponse, MemberRole};
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::workspace_join::WorkspaceJoinProtocol;
use tilo_user_identity::RevocationList;

#[path = "native_exchange_tests/refresh.rs"]
mod refresh;
type ActualAuthority = Arc<Mutex<Option<TestGovernor>>>;

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
        let mut state = self.authority.lock().unwrap();
        let Some(governor) = state.as_mut() else {
            return JoinTransportResponse::Error {
                code: 503,
                message: "fixture join service retired".into(),
            };
        };
        match request {
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
                governor.handle(remote, JoinTransportRequest::Complete(request))
            }
            request => governor.handle(remote, request),
        }
    }
}

#[tokio::test]
async fn native_enrollment_pinned_exchange_persists_complete_before_send() {
    Box::pin(actual_exchange(false)).await;
}

#[tokio::test]
async fn native_enrollment_restart_automatically_refreshes_and_resends_retained_history() {
    Box::pin(actual_exchange(true)).await;
}

#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end enrollment exchange over a real relay; the steps share live \
              endpoints and custody state that splitting would have to thread through helpers"
)]
async fn actual_exchange(restart: bool) {
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let mut remote_owner = TestGovernor::new();
    let governor = remote_owner.governor().clone();
    let governor_key = remote_owner.governor_key().clone();
    let host = remote_owner.host().clone();
    let host_key = remote_owner.host_key().clone();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let room = RoomId::parse(remote_owner.room()).unwrap();
    let topic = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    let hint = remote_owner.hint(server.addr());
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
    let now = now_ns(SystemTime::now()).unwrap();
    let signed_invitation = remote_owner.issue(
        vec![hint],
        MemberRole::Editor,
        Some(device.user_public_key),
        Some("Native receiver".into()),
        now.checked_add(60_000_000_000).unwrap(),
    );
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
    let local = tree.desktop.active;
    tree.persist_path = Some(directory.path().join("state.json"));
    let receiving_owner = crate::shell::native_workspace_owner::open(tree.shared()).unwrap();
    let saw = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let actual = ActualService {
        authority: Arc::new(Mutex::new(Some(remote_owner))),
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
                    tilo_peer_state::retained_proof::RetentionAuthority::new(
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
        workspace_device_certs: std::collections::HashMap::default(),
        workspace_revocations: std::collections::HashMap::default(),
        identity_paths: None,
    };
    let result = prepare_and_complete(
        WorkspaceJoinClient::new(endpoint.clone()),
        identity.clone(),
        topic,
        Some(local),
        join::encode_invitation_text(&signed_invitation),
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
            state.governed_access(SystemTime::now()).unwrap(),
            state
                .outgoing()
                .map(|(bytes, kind)| (bytes.to_vec(), kind))
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
    tilo_test_env::within("the publisher's Ready", async {
        loop {
            if matches!(
                publisher.recv().await,
                Some(tilo_relay::Inbound::Ready { .. })
            ) {
                break;
            }
        }
    })
    .await;
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
    tilo_test_env::within("the host to report ready", async {
        loop {
            if matches!(
                received.recv().await,
                Some(crate::network::actor::NetEvent::HostReady { .. })
            ) {
                break;
            }
        }
    })
    .await;
    for (bytes, kind) in &publications {
        let message: NeighbourMsg = postcard::from_bytes(bytes).unwrap();
        let (channel, _) = tilo_peer_state::transport::retention(
            &message,
            *kind == tilo_peer_state::transport::Retention::Checkpoint,
        )
        .unwrap();
        match kind {
            tilo_peer_state::transport::Retention::Checkpoint => {
                publisher.publish_checkpoint(channel, bytes.clone())
            }
            tilo_peer_state::transport::Retention::Delta => {
                publisher.publish_retained_delta(channel, bytes.clone())
            }
        }
        .unwrap();
    }
    let mut last = None;
    tilo_test_env::within("every native input publication", async {
        let mut count = 0;
        while count < publications.len() {
            if let Some(crate::network::actor::NetEvent::NativeInput(input)) = received.recv().await
            {
                assert_eq!(input.author(), governor.device_public_key);
                input
                    .admit(&identity)
                    .expect("genuine native relay input must cross actual owner barrier");
                last = Some(input);
                count = count.saturating_add(1);
            }
        }
    })
    .await;
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
    tilo_test_env::within("the removed device to unsubscribe", async {
        loop {
            if !relay.is_subscribed(topic, &device.device_public_key).await {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
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
            refresh::LiveExchange {
                directory: &directory,
                identity: &identity,
                endpoint: &endpoint,
                relay: &relay,
                publisher: &publisher,
            },
            receiving_owner,
            room,
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
