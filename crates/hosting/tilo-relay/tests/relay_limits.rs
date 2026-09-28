//! Admission limits and retained delivery for a room with many members,
//! over the real relay protocol: the per-workspace subscriber cap, the
//! relay-wide connection cap, and a retained message reaching a member that
//! was offline when it was posted without any per-member pending copy.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::time::Duration;

use tilo_relay::{CoeditPayload, Envelope, RelayClient, RelayLimits};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, client_endpoint, spawn_relay, spawn_relay_with_limits};

const WS: u64 = 71;

async fn connect(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    device: &TestDevice,
) -> RelayClient {
    RelayClient::connect(
        client_endpoint().await,
        addr,
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("relay connection")
}

/// Subscribe and return the relay's answer, skipping any replayed deltas.
async fn subscribe(client: &RelayClient) -> Envelope {
    client
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();
    loop {
        let envelope = tilo_test_env::within("the subscribe answer", client.recv())
            .await
            .expect("connection open");
        if matches!(
            envelope,
            Envelope::Ready { .. } | Envelope::NotAuthorised { .. }
        ) {
            return envelope;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_workspace_refuses_a_new_subscriber_but_not_a_returning_one() {
    let user = TestUser::fresh("community-operator");
    let relay_dev = user.enrol("host", DeviceCapabilities::host());
    let devices: Vec<TestDevice> = (0..3)
        .map(|n| user.enrol(&format!("member-{n}"), DeviceCapabilities::member()))
        .collect();
    let relay = spawn_relay_with_limits(
        &user,
        relay_dev,
        &[WS],
        &[],
        RelayLimits {
            max_subscribers_per_workspace: 2,
            ..RelayLimits::default()
        },
    )
    .await;
    let addr = relay.relay.node_addr();

    let first = connect(addr.clone(), &user, &devices[0]).await;
    assert!(matches!(subscribe(&first).await, Envelope::Ready { .. }));
    let second = connect(addr.clone(), &user, &devices[1]).await;
    assert!(matches!(subscribe(&second).await, Envelope::Ready { .. }));

    let third = connect(addr.clone(), &user, &devices[2]).await;
    match subscribe(&third).await {
        Envelope::NotAuthorised {
            workspace_id,
            reason,
        } => {
            assert_eq!(workspace_id, WS);
            assert_eq!(reason, "workspace subscriber limit reached");
        }
        other => panic!("a third subscriber must be refused, got {other:?}"),
    }
    // A device already counted may resubscribe at the limit.
    assert!(matches!(subscribe(&second).await, Envelope::Ready { .. }));

    // A departure frees the slot.
    first.close().await;
    tilo_test_env::within("the closed subscriber is unregistered", async {
        while relay
            .relay
            .is_subscribed(WS, &devices[0].device_pubkey())
            .await
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(matches!(subscribe(&third).await, Envelope::Ready { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connections_beyond_the_relay_limit_are_refused_until_one_closes() {
    let user = TestUser::fresh("community-operator");
    let relay_dev = user.enrol("host", DeviceCapabilities::host());
    let a = user.enrol("member-a", DeviceCapabilities::member());
    let b = user.enrol("member-b", DeviceCapabilities::member());
    let relay = spawn_relay_with_limits(
        &user,
        relay_dev,
        &[WS],
        &[],
        RelayLimits {
            max_connections: 1,
            ..RelayLimits::default()
        },
    )
    .await;
    let addr = relay.relay.node_addr();

    let held = connect(addr.clone(), &user, &a).await;
    assert!(matches!(subscribe(&held).await, Envelope::Ready { .. }));
    let refused = tilo_test_env::within(
        "a refused connection fails promptly",
        RelayClient::connect(
            client_endpoint().await,
            addr.clone(),
            user.user_pk,
            b.cert.clone(),
            b.sk.clone(),
            RevocationList::new(),
        ),
    )
    .await;
    assert!(
        refused.is_err(),
        "a second connection is closed at the limit"
    );

    held.close().await;
    let admitted = tilo_test_env::within("the freed slot admits the next connection", async {
        loop {
            if let Ok(client) = RelayClient::connect(
                client_endpoint().await,
                addr.clone(),
                user.user_pk,
                b.cert.clone(),
                b.sk.clone(),
                RevocationList::new(),
            )
            .await
            {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(matches!(subscribe(&admitted).await, Envelope::Ready { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_offline_member_receives_a_retained_message_without_a_pending_copy() {
    let user = TestUser::fresh("community-operator");
    let relay_dev = user.enrol("host", DeviceCapabilities::host());
    let author = user.enrol("author", DeviceCapabilities::member());
    let offline: Vec<TestDevice> = (0..50)
        .map(|n| user.enrol(&format!("offline-{n}"), DeviceCapabilities::member()))
        .collect();
    let mut members: Vec<[u8; 32]> = offline.iter().map(TestDevice::device_pubkey).collect();
    members.push(author.device_pubkey());
    let relay = spawn_relay(&user, relay_dev, &[WS], &members).await;
    let addr = relay.relay.node_addr();

    let channel = [0x42; 32];
    let body = b"a remark posted to #feedback".to_vec();
    let payload = CoeditPayload::RetainedDelta {
        channel,
        entry: *blake3::hash(&body).as_bytes(),
        body: body.clone(),
    }
    .encode()
    .unwrap();
    let writer = connect(addr.clone(), &user, &author).await;
    assert!(matches!(subscribe(&writer).await, Envelope::Ready { .. }));
    writer
        .send(Envelope::Delta {
            workspace_id: WS,
            author_device_public_key: author.device_pubkey(),
            authored_at_ns: 1,
            delta: payload.clone(),
            signature: [0u8; 64],
        })
        .unwrap();

    tilo_test_env::within("the message retained once for the room", async {
        loop {
            let store =
                tilo_relay::store::WorkspaceStore::open(&relay.state_root, WS, false).unwrap();
            if store.load_retained_channel(&channel).unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let pending = relay
        .state_root
        .join("workspaces")
        .join(WS.to_string())
        .join("pending");
    assert_eq!(
        std::fs::read_dir(&pending).unwrap().count(),
        0,
        "no offline member got a per-device copy"
    );

    let reader = connect(addr, &user, &offline[17]).await;
    reader
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();
    let replayed = tilo_test_env::within("the replayed message", reader.recv())
        .await
        .expect("connection open");
    match replayed {
        Envelope::Delta {
            delta,
            author_device_public_key,
            ..
        } => {
            assert_eq!(delta, payload);
            assert_eq!(author_device_public_key, author.device_pubkey());
        }
        other => panic!("expected the retained message first, got {other:?}"),
    }
}
