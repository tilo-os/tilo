//! Certified relay v2 transport tests. No action executor is installed here.
use crate::common::{TestUser, client_endpoint, spawn_relay};
use tilo_user_identity::DeviceCapabilities;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addressed_actions_alpn_is_served_by_the_existing_relay() {
    let user = TestUser::fresh("Alice");
    let relay = spawn_relay(
        &user,
        user.enrol("relay", DeviceCapabilities::host()),
        &[7],
        &[],
    )
    .await;
    let endpoint = client_endpoint().await;
    let connection = endpoint
        .connect(relay.relay.node_addr(), b"qos-relay/2")
        .await;
    assert!(
        connection.is_ok(),
        "same relay must serve the private v2 lane: {connection:?}"
    );
    endpoint.close().await;
}

use crate::common::{TestDevice, TestRelay};
use std::time::Duration;
use tilo_relay::peer_actions::{ActionMessage, DirectedAction, UnavailableReason};
use tilo_relay::transport_v2::{ActionCapability, EnvelopeV2};
use tilo_relay::{Envelope, RelayClient};
use tilo_user_identity::RevocationList;
use uuid::Uuid;

async fn next_state(client: &RelayClient) -> Envelope {
    tokio::time::timeout(Duration::from_secs(5), client.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn next_action(client: &RelayClient) -> EnvelopeV2 {
    tokio::time::timeout(Duration::from_secs(5), client.recv_action())
        .await
        .unwrap()
        .unwrap()
}
async fn join_v2(relay: &TestRelay, device: &TestDevice, room: Uuid) -> RelayClient {
    let client = RelayClient::connect_v2_to_enrolled_home_peer(
        client_endpoint().await,
        relay.relay.node_addr(),
        relay.relay.own_user_public_key(),
        relay.relay.handle().mesh_profile().device,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: tilo_relay::room_workspace_id(&room.to_string()).unwrap(),
        })
        .unwrap();
    assert!(matches!(next_state(&client).await, Envelope::Ready { .. }));
    client
        .send_action(EnvelopeV2::EnableActions { room })
        .unwrap();
    assert_eq!(
        next_action(&client).await,
        EnvelopeV2::ActionsReady {
            room,
            capability: ActionCapability::TransportOnly
        }
    );
    client
}
fn refresh(room: Uuid, sender: &TestDevice, target: &TestDevice) -> DirectedAction {
    DirectedAction {
        room,
        request_id: Uuid::from_u128(100),
        source: sender.device_pubkey(),
        target: target.device_pubkey(),
        message: ActionMessage::CatalogueRefresh { scope: None },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addressed_actions_are_private_live_and_demultiplexed_from_v1_state() {
    let user = TestUser::fresh("Alice");
    let a = user.enrol("a", DeviceCapabilities::primary());
    let b = user.enrol("b", DeviceCapabilities::primary());
    let c = user.enrol("v1", DeviceCapabilities::primary());
    let room = Uuid::from_u128(7);
    let workspace_id = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    let relay = spawn_relay(
        &user,
        user.enrol("relay", DeviceCapabilities::host()),
        &[workspace_id],
        &[a.device_pubkey(), b.device_pubkey(), c.device_pubkey()],
    )
    .await;
    let ca = join_v2(&relay, &a, room).await;
    let cb = join_v2(&relay, &b, room).await;
    let cc = RelayClient::connect(
        client_endpoint().await,
        relay.relay.node_addr(),
        user.user_pk,
        c.cert.clone(),
        c.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    cc.send(Envelope::Subscribe { workspace_id }).unwrap();
    assert!(matches!(next_state(&cc).await, Envelope::Ready { .. }));
    cb.send(Envelope::Subscribe { workspace_id }).unwrap();
    assert!(matches!(next_state(&cb).await, Envelope::Ready { .. }));
    let directed = refresh(room, &a, &b);
    ca.send_action(EnvelopeV2::Directed(directed.clone()))
        .unwrap();
    ca.send(Envelope::Presence {
        workspace_id,
        payload: b"shared".to_vec(),
    })
    .unwrap();
    assert_eq!(
        next_state(&cb).await,
        Envelope::Presence {
            workspace_id,
            payload: b"shared".to_vec()
        }
    );
    assert_eq!(
        next_state(&cc).await,
        Envelope::Presence {
            workspace_id,
            payload: b"shared".to_vec()
        }
    );
    assert_eq!(
        next_action(&cb).await,
        EnvelopeV2::Delivered {
            action: directed,
            source_user: user.user_pk
        }
    );
    // Reply carries the existing typed lifecycle, privately, with full UUID.
    let invocation = tilo_actions::InvocationId(Uuid::from_u128(0x123456789abcdef123456789abcdef));
    let update = DirectedAction {
        room,
        request_id: Uuid::from_u128(101),
        source: b.device_pubkey(),
        target: a.device_pubkey(),
        message: ActionMessage::Update {
            update: tilo_actions::InvocationUpdate::Progress {
                invocation,
                sequence: 1,
                phase: "private-phase".into(),
                completed: Some(1),
                total: Some(2),
                message: Some("private-result".into()),
            },
        },
    };
    cb.send_action(EnvelopeV2::Directed(update.clone()))
        .unwrap();
    assert_eq!(
        next_action(&ca).await,
        EnvelopeV2::Delivered {
            action: update,
            source_user: user.user_pk
        }
    );
    ca.send_action(EnvelopeV2::Directed(refresh(room, &a, &c)))
        .unwrap();
    assert!(
        matches!(next_action(&ca).await, EnvelopeV2::Unavailable {reason:UnavailableReason::UnsupportedPeer, request_id,..} if request_id==Uuid::from_u128(100))
    );
    assert!(
        cc.send_action(EnvelopeV2::Directed(refresh(room, &c, &a)))
            .is_err()
    );
    assert!(cc.recv_action().await.is_none());
    // A v1 peer never sees private variants; state keeps working.
    cc.send(Envelope::Ping).unwrap();
    assert_eq!(next_state(&cc).await, Envelope::Pong);
    cb.close().await;
    ca.send_action(EnvelopeV2::Directed(refresh(room, &a, &b)))
        .unwrap();
    assert!(matches!(
        next_action(&ca).await,
        EnvelopeV2::Unavailable {
            reason: UnavailableReason::Offline,
            ..
        }
    ));
    let cb = join_v2(&relay, &b, room).await;
    cb.send(Envelope::Ping).unwrap();
    assert_eq!(next_state(&cb).await, Envelope::Pong);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), cb.recv_action())
            .await
            .is_err()
    );
    // No action values reached a pending queue, retained tail or checkpoint.
    fn scan(path: &std::path::Path) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path)
            } else {
                let data = std::fs::read(path).unwrap();
                assert!(!data.windows(13).any(|v| v == b"private-phase"));
                assert!(!data.windows(14).any(|v| v == b"private-result"));
            }
        }
    }
    scan(&relay.state_root);
    let store =
        tilo_relay::store::WorkspaceStore::open(&relay.state_root, workspace_id, true).unwrap();
    for device in [&a, &b, &c] {
        assert_eq!(store.count_pending(&device.device_pubkey()).unwrap(), 0);
    }
    assert!(store.load_retained().unwrap().is_empty());
    assert!(store.load_snapshot().unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addressed_actions_replacement_survives_old_cleanup_and_rechecks_membership() {
    let user = TestUser::fresh("Alice");
    let a = user.enrol("a", DeviceCapabilities::primary());
    let b = user.enrol("b", DeviceCapabilities::primary());
    let room = Uuid::from_u128(8);
    let workspace_id = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    let relay = spawn_relay(
        &user,
        user.enrol("relay", DeviceCapabilities::host()),
        &[workspace_id],
        &[a.device_pubkey(), b.device_pubkey()],
    )
    .await;
    let ca = join_v2(&relay, &a, room).await;
    let old = join_v2(&relay, &b, room).await;
    let cb = join_v2(&relay, &b, room).await;
    old.close().await;
    ca.send_action(EnvelopeV2::Directed(refresh(room, &a, &b)))
        .unwrap();
    assert!(matches!(
        next_action(&cb).await,
        EnvelopeV2::Delivered { .. }
    ));
    relay
        .relay
        .set_workspace_membership(
            workspace_id,
            true,
            vec![a.device_pubkey()],
            relay.state_root.clone(),
        )
        .await
        .unwrap();
    ca.send_action(EnvelopeV2::Directed(refresh(room, &a, &b)))
        .unwrap();
    assert!(matches!(
        next_action(&ca).await,
        EnvelopeV2::Unavailable {
            reason: UnavailableReason::NotAuthorised,
            ..
        }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addressed_actions_reject_source_forgery_and_external_effect_admission() {
    let user = TestUser::fresh("Alice");
    let a = user.enrol("a", DeviceCapabilities::primary());
    let b = user.enrol("b", DeviceCapabilities::primary());
    let room = Uuid::from_u128(9);
    let workspace_id = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    let relay = spawn_relay(
        &user,
        user.enrol("relay", DeviceCapabilities::host()),
        &[workspace_id],
        &[a.device_pubkey(), b.device_pubkey()],
    )
    .await;
    let ca = join_v2(&relay, &a, room).await;
    let cb = join_v2(&relay, &b, room).await;
    let mut forged = refresh(room, &b, &a);
    forged.target = b.device_pubkey();
    ca.send_action(EnvelopeV2::Directed(forged)).unwrap();
    assert!(matches!(
        next_action(&ca).await,
        EnvelopeV2::Unavailable {
            reason: UnavailableReason::NotAuthorised,
            ..
        }
    ));
    let mut invoke = refresh(room, &a, &b);
    invoke.message = ActionMessage::Invoke {
        invocation: tilo_actions::InvocationId(Uuid::from_u128(55)),
        app_peer: "live-app".into(),
        resource: "doc".into(),
        action: "send".into(),
        argument: None,
        confirmation: None,
        revision: None,
    };
    ca.send_action(EnvelopeV2::Directed(invoke)).unwrap();
    assert!(
        matches!(next_action(&ca).await,EnvelopeV2::Unavailable {reason:UnavailableReason::AdmissionUnavailable,invocation:Some(tilo_actions::InvocationId(id)),..} if id==Uuid::from_u128(55))
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), cb.recv_action())
            .await
            .is_err()
    );
    let mut wrong_room = refresh(Uuid::from_u128(987), &a, &b);
    ca.send_action(EnvelopeV2::Directed(wrong_room.clone()))
        .unwrap();
    assert!(matches!(
        next_action(&ca).await,
        EnvelopeV2::Unavailable {
            reason: UnavailableReason::NotAuthorised,
            ..
        }
    ));
    wrong_room.room = room;
    wrong_room.message = ActionMessage::Update {
        update: tilo_actions::InvocationUpdate::Progress {
            invocation: tilo_actions::InvocationId(Uuid::from_u128(55)),
            sequence: 1,
            phase: "x".repeat(513),
            completed: None,
            total: None,
            message: None,
        },
    };
    ca.send_action(EnvelopeV2::Directed(wrong_room)).unwrap();
    assert!(matches!(
        next_action(&ca).await,
        EnvelopeV2::Unavailable {
            reason: UnavailableReason::InvalidRequest,
            ..
        }
    ));
    ca.send(Envelope::Unsubscribe { workspace_id }).unwrap();
    ca.send(Envelope::Ping).unwrap();
    assert_eq!(next_state(&ca).await, Envelope::Pong);
    ca.send_action(EnvelopeV2::Directed(refresh(room, &a, &b)))
        .unwrap();
    assert!(matches!(
        next_action(&ca).await,
        EnvelopeV2::Unavailable {
            reason: UnavailableReason::NotAuthorised,
            ..
        }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addressed_actions_v2_preserves_cert_and_enrolled_host_pinning() {
    let user = TestUser::fresh("Alice");
    let other = TestUser::fresh("Mallory");
    let relay = spawn_relay(
        &user,
        user.enrol("relay", DeviceCapabilities::host()),
        &[7],
        &[],
    )
    .await;
    let device = user.enrol("a", DeviceCapabilities::primary());
    let bad = other.enrol("intruder", DeviceCapabilities::primary());
    for (caller, server_root, server_device) in [
        (
            &bad,
            user.user_pk,
            relay.relay.handle().mesh_profile().device,
        ),
        (
            &device,
            other.user_pk,
            relay.relay.handle().mesh_profile().device,
        ),
        (&device, user.user_pk, [99; 32]),
    ] {
        let result = RelayClient::connect_v2_to_enrolled_home_peer(
            client_endpoint().await,
            relay.relay.node_addr(),
            server_root,
            server_device,
            caller.cert.clone(),
            caller.sk.clone(),
            RevocationList::new(),
        )
        .await;
        assert!(
            result.is_err(),
            "v2 must retain the existing mutual certificate and exact host checks"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addressed_actions_coedit_uses_the_same_demultiplexer() {
    use automerge::transaction::Transactable;
    use tilo_relay::{Inbound, RelayCoeditSession};
    let user = TestUser::fresh("Alice");
    let a = user.enrol("a", DeviceCapabilities::primary());
    let b = user.enrol("b", DeviceCapabilities::primary());
    let room = Uuid::from_u128(10);
    let workspace_id = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    let relay = spawn_relay(
        &user,
        user.enrol("relay", DeviceCapabilities::host()),
        &[workspace_id],
        &[a.device_pubkey(), b.device_pubkey()],
    )
    .await;
    let ca = join_v2(&relay, &a, room).await;
    let cb = join_v2(&relay, &b, room).await;
    let a_session = RelayCoeditSession::join(ca, workspace_id, a.device_pubkey()).unwrap();
    let b_session = RelayCoeditSession::join(cb, workspace_id, b.device_pubkey()).unwrap();
    for session in [&a_session, &b_session] {
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), session.recv())
                .await
                .unwrap(),
            Some(Inbound::Ready { .. })
        ));
        session.enable_actions(room).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), session.recv_action())
                .await
                .unwrap(),
            Some(EnvelopeV2::ActionsReady { .. })
        ));
    }
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "text", "shared edit").unwrap();
    a_session.send_action(refresh(room, &a, &b)).unwrap();
    a_session
        .publish_retained_delta([7; 32], doc.save_incremental())
        .unwrap();
    let received_delta = tokio::time::timeout(Duration::from_secs(5), b_session.recv()).await;
    if received_delta.is_err() {
        crate::common::print_phase_diagnostic();
    }
    let Some(Inbound::Delta { data, .. }) = received_delta.unwrap() else {
        panic!("state lane lost edit")
    };
    let mut received = automerge::AutoCommit::new();
    received.load_incremental(&data).unwrap();
    assert_eq!(received.get_heads(), doc.get_heads());
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(5),b_session.recv_action()).await.unwrap(),Some(EnvelopeV2::Delivered{source_user,..}) if source_user==user.user_pk)
    );
    assert!(a_session.enable_actions(Uuid::from_u128(11)).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addressed_actions_sustained_state_flood_keeps_private_roundtrips_and_reader_live() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let user = TestUser::fresh("Alice");
    let a = user.enrol("a", DeviceCapabilities::primary());
    let b = user.enrol("b", DeviceCapabilities::primary());
    let room = Uuid::from_u128(15);
    let workspace_id = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
    let relay = spawn_relay(
        &user,
        user.enrol("relay", DeviceCapabilities::host()),
        &[workspace_id],
        &[a.device_pubkey(), b.device_pubkey()],
    )
    .await;
    let ca = join_v2(&relay, &a, room).await;
    let cb = join_v2(&relay, &b, room).await;
    // Bound offered network backlog independently of the local scheduling
    // queues: 1024 is four times the 256-entry state queue, allowing repeated
    // refills. An ordered stream cannot move a private frame ahead of hundreds
    // of thousands of previously written Pings; that is not scheduler fairness.
    let in_flight = tokio::sync::Semaphore::new(1024);
    let produced = AtomicUsize::new(0);
    let received = AtomicUsize::new(0);
    let (done, mut producer_done) = tokio::sync::watch::channel(false);
    let mut consumer_done = producer_done.clone();
    tokio::time::timeout(Duration::from_secs(15), async {
        let producer = async {
            loop {
                let permit = tokio::select! {
                    biased;
                    _=producer_done.changed()=>break,
                    permit=in_flight.acquire()=>permit.unwrap(),
                };
                tokio::select! {
                    biased;
                    _=producer_done.changed()=>break,
                    sent=ca.outbound.send(Envelope::Ping)=>{
                        sent.expect("state transport closed under mixed traffic");
                        permit.forget();
                        produced.fetch_add(1,Ordering::SeqCst);
                    }
                }
            }
        };
        let consumer = async {
            loop {
                tokio::select! {
                    biased;
                    _=consumer_done.changed()=>break,
                    state=ca.recv()=>{
                        assert_eq!(state,Some(Envelope::Pong));
                        received.fetch_add(1,Ordering::SeqCst);
                        in_flight.add_permits(1);
                    }
                }
            }
        };
        let private = async {
            // Begin private work only after the state queue has been filled
            // and refilled. The producer keeps running until every reply lands.
            while produced.load(Ordering::SeqCst) < 512 {
                tokio::task::yield_now().await;
            }
            for id in 1..=32 {
                let mut request = refresh(room, &a, &b);
                request.request_id = Uuid::from_u128(id);
                ca.send_action(EnvelopeV2::Directed(request.clone()))
                    .unwrap();
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(5), cb.recv_action())
                        .await
                        .unwrap_or_else(|_| panic!(
                            "target stalled at request {id}; state sent {} received {}",
                            produced.load(Ordering::SeqCst),
                            received.load(Ordering::SeqCst)
                        ))
                        .expect("target closed"),
                    EnvelopeV2::Delivered {
                        action: request.clone(),
                        source_user: user.user_pk
                    }
                );
                let reply = DirectedAction {
                    source: b.device_pubkey(),
                    target: a.device_pubkey(),
                    message: ActionMessage::Catalogue {
                        catalogue: serde_json::json!({"version":1,"surfaces":[]}),
                    },
                    ..request
                };
                cb.send_action(EnvelopeV2::Directed(reply.clone())).unwrap();
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(5), ca.recv_action())
                        .await
                        .unwrap_or_else(|_| panic!(
                            "reply stalled at request {id}; state sent {} received {}",
                            produced.load(Ordering::SeqCst),
                            received.load(Ordering::SeqCst)
                        ))
                        .expect("source closed"),
                    EnvelopeV2::Delivered {
                        action: reply,
                        source_user: user.user_pk
                    }
                );
            }
            done.send(true).unwrap();
        };
        tokio::join!(producer, consumer, private);
    })
    .await
    .expect("sustained state traffic starved a private lane or the sole reader");
    assert!(produced.load(Ordering::SeqCst) >= 512);
    assert!(received.load(Ordering::SeqCst) > 0);
    let store =
        tilo_relay::store::WorkspaceStore::open(&relay.state_root, workspace_id, true).unwrap();
    assert!(store.load_retained().unwrap().is_empty());
    assert_eq!(store.count_pending(&a.device_pubkey()).unwrap(), 0);
    assert_eq!(store.count_pending(&b.device_pubkey()).unwrap(), 0);
}
