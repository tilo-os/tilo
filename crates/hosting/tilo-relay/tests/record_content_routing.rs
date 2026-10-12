//! Record-content envelopes: the relay hands a piece to the one addressed
//! device, stamps the authenticated sender, and drops what it cannot route.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
use std::time::Duration;

use tilo_relay::transport::MAX_RECORD_CONTENT_FRAME_BYTES;
use tilo_relay::{Envelope, RelayClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, client_endpoint, spawn_relay};

const WS: u64 = 12;

async fn subscribed(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    device: &TestDevice,
) -> RelayClient {
    let client = RelayClient::connect(
        client_endpoint().await,
        addr,
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("connect");
    client
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();
    let ready = tilo_test_env::within("ready timeout", client.recv()).await;
    assert!(
        matches!(ready, Some(Envelope::Ready { .. })),
        "got {ready:?}"
    );
    client
}

fn piece(to: [u8; 32], frame: Vec<u8>) -> Envelope {
    Envelope::RecordContent {
        workspace_id: WS,
        to_device_public_key: to,
        // A sender cannot choose who it claims to be.
        from_device_public_key: [0xEE; 32],
        frame,
    }
}

async fn quiet(client: &RelayClient) -> bool {
    tokio::time::timeout(Duration::from_millis(300), client.recv())
        .await
        .is_err()
}

#[tokio::test]
async fn a_piece_reaches_only_the_device_it_is_addressed_to() {
    let user = TestUser::fresh("Alice");
    let host = user.enrol("home", DeviceCapabilities::host());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("desktop", DeviceCapabilities::member());
    let c = user.enrol("phone", DeviceCapabilities::member());
    let members = [a.device_pubkey(), b.device_pubkey(), c.device_pubkey()];
    let relay = Box::pin(spawn_relay(&user, host, &[WS], &members)).await;
    let addr = relay.relay.node_addr();
    let a_client = subscribed(addr.clone(), &user, &a).await;
    let b_client = subscribed(addr.clone(), &user, &b).await;
    let c_client = subscribed(addr, &user, &c).await;

    let bytes = vec![7_u8; 192 * 1024];
    a_client
        .send(piece(b.device_pubkey(), bytes.clone()))
        .unwrap();
    match tokio::time::timeout(tilo_test_env::hang_bound(), b_client.recv())
        .await
        .expect("B timed out")
        .expect("B connection closed")
    {
        Envelope::RecordContent {
            to_device_public_key,
            from_device_public_key,
            frame,
            ..
        } => {
            assert_eq!(to_device_public_key, b.device_pubkey());
            assert_eq!(from_device_public_key, a.device_pubkey());
            assert_eq!(frame, bytes, "the relay forwards the piece untouched");
        }
        other => panic!("B expected a piece, got {other:?}"),
    }
    assert!(quiet(&c_client).await, "a piece reaches no other device");

    a_client
        .send(piece(
            b.device_pubkey(),
            vec![0; MAX_RECORD_CONTENT_FRAME_BYTES + 1],
        ))
        .unwrap();
    a_client.send(piece([0x55; 32], vec![1])).unwrap();
    assert!(quiet(&b_client).await, "an oversized piece is dropped");
    assert!(
        quiet(&a_client).await,
        "nothing is answered for a piece the relay could not route"
    );
}
