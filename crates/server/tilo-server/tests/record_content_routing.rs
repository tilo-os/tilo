//! Record-content envelopes: the server hands a piece to the one addressed
//! device, stamps the authenticated sender, and drops what it cannot route.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
use std::time::Duration;

use tilo_server::transport::MAX_DOCUMENT_CONTENT_FRAME_BYTES;
use tilo_server::{Envelope, ServerClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, client_endpoint, spawn_server};
use tilo_server::WorkspaceId;

const WS: WorkspaceId = WorkspaceId::from_u128(12);

async fn subscribed(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    device: &TestDevice,
) -> ServerClient {
    let client = ServerClient::connect(
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
    // A held workspace sends its own state before it is ready.
    crate::common::next(&client, |env| matches!(env, Envelope::Ready { .. })).await;
    client
}

fn piece(to: [u8; 32], frame: Vec<u8>) -> Envelope {
    Envelope::DocumentContent {
        workspace_id: WS,
        to_device_public_key: to,
        // A sender cannot choose who it claims to be.
        from_device_public_key: [0xEE; 32],
        frame,
    }
}

async fn quiet(client: &ServerClient) -> bool {
    crate::common::quiet(client, Duration::from_millis(300)).await
}

#[tokio::test]
async fn a_piece_reaches_only_the_device_it_is_addressed_to() {
    let user = TestUser::fresh("Alice");
    let host = user.enrol("home", DeviceCapabilities::member());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("desktop", DeviceCapabilities::member());
    let c = user.enrol("phone", DeviceCapabilities::member());
    let members = [&a.cert, &b.cert, &c.cert];
    let server = Box::pin(spawn_server(&user, host, &[WS], &members)).await;
    let addr = server.server.node_addr();
    let a_client = subscribed(addr.clone(), &user, &a).await;
    let b_client = subscribed(addr.clone(), &user, &b).await;
    let c_client = subscribed(addr, &user, &c).await;

    let bytes = vec![7_u8; 192 * 1024];
    a_client
        .send(piece(b.device_pubkey(), bytes.clone()))
        .unwrap();
    // B's session also carries its person's workspace list.
    match tokio::time::timeout(
        tilo_test_env::hang_bound(),
        crate::common::next(&b_client, |env| {
            !matches!(env, Envelope::PersonalList { .. })
        }),
    )
    .await
    .expect("B timed out")
    {
        Envelope::DocumentContent {
            to_device_public_key,
            from_device_public_key,
            frame,
            ..
        } => {
            assert_eq!(to_device_public_key, b.device_pubkey());
            assert_eq!(from_device_public_key, a.device_pubkey());
            assert_eq!(frame, bytes, "the server forwards the piece untouched");
        }
        other => panic!("B expected a piece, got {other:?}"),
    }
    assert!(quiet(&c_client).await, "a piece reaches no other device");

    a_client
        .send(piece(
            b.device_pubkey(),
            vec![0; MAX_DOCUMENT_CONTENT_FRAME_BYTES + 1],
        ))
        .unwrap();
    a_client.send(piece([0x55; 32], vec![1])).unwrap();
    assert!(quiet(&b_client).await, "an oversized piece is dropped");
    assert!(
        quiet(&a_client).await,
        "nothing is answered for a piece the server could not route"
    );
}

/// One device's long burst does not crowd another device's piece out of the
/// recipient's connection: the relay carries each sender's pieces in turn,
/// and nothing is refused for being too much.
#[tokio::test]
async fn a_burst_from_one_device_leaves_anothers_piece_a_way() {
    let user = TestUser::fresh("Alice");
    let host = user.enrol("home", DeviceCapabilities::member());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("desktop", DeviceCapabilities::member());
    let c = user.enrol("phone", DeviceCapabilities::member());
    let members = [&a.cert, &b.cert, &c.cert];
    let server = Box::pin(spawn_server(&user, host, &[WS], &members)).await;
    let addr = server.server.node_addr();
    let a_client = subscribed(addr.clone(), &user, &a).await;
    let b_client = subscribed(addr.clone(), &user, &b).await;
    let c_client = subscribed(addr, &user, &c).await;

    // One device answers a transfer with a burst far past the recipient's
    // outbound queue; another sends one piece behind it.
    let burst = 256;
    for number in 0..burst {
        let mut frame = vec![0_u8; 1024];
        frame[0..2].copy_from_slice(&u16::try_from(number).unwrap().to_be_bytes());
        a_client.send(piece(c.device_pubkey(), frame)).unwrap();
    }
    b_client
        .send(piece(c.device_pubkey(), vec![0xBB; 1024]))
        .unwrap();

    let mut from_a = 0_usize;
    let mut b_at = None;
    while b_at.is_none() && from_a < 64 {
        let envelope = tokio::time::timeout(tilo_test_env::hang_bound(), c_client.recv())
            .await
            .expect("C timed out")
            .expect("C connection closed");
        if let Envelope::DocumentContent {
            from_device_public_key,
            ..
        } = envelope
        {
            if from_device_public_key == b.device_pubkey() {
                b_at = Some(from_a);
            } else {
                from_a = from_a.saturating_add(1);
            }
        }
    }
    let b_at = b_at.expect("B's piece reached the recipient among A's burst");
    // What the connection's queue already held when B's piece came, and one
    // turn of the queue: not the burst behind it.
    assert!(
        b_at <= 40,
        "B's piece came after {b_at} of A's pieces, behind the whole burst"
    );
    assert!(
        quiet(&a_client).await,
        "nothing is refused for being too much content"
    );
}
