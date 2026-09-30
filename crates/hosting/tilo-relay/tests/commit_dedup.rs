//! Idempotent commits: a device that resends a commit the host already
//! admitted (after a reconnect, or because the answer was lost) gets the
//! original host order back, and nobody receives the change twice — also
//! after the host restarts.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::time::Duration;

use tilo_relay::{Envelope, RelayClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, client_endpoint, spawn_relay_on};

const WS: u64 = 43;

async fn connect(
    user: &TestUser,
    device: &TestDevice,
    addr: iroh_base::EndpointAddr,
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
    .unwrap();
    client
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();
    next(&client, |env| matches!(env, Envelope::Ready { .. })).await;
    client
}

async fn next(client: &RelayClient, mut want: impl FnMut(&Envelope) -> bool) -> Envelope {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let env = client.recv().await.expect("session open");
            if want(&env) {
                return env;
            }
        }
    })
    .await
    .expect("expected envelope")
}

async fn no_delta(client: &RelayClient) {
    let deadline = tokio::time::Instant::now()
        .checked_add(Duration::from_millis(300))
        .unwrap();
    while let Ok(Some(env)) = tokio::time::timeout_at(deadline, client.recv()).await {
        assert!(
            !matches!(env, Envelope::Delta { .. }),
            "a resent commit was fanned out again: {env:?}"
        );
    }
}

fn commit(author: &TestDevice, op: [u8; 16], delta: Vec<u8>) -> Envelope {
    Envelope::Commit {
        workspace_id: WS,
        op,
        author_device_public_key: author.device_pubkey(),
        authored_at_ns: 0,
        delta,
    }
}

async fn committed(client: &RelayClient, op: [u8; 16]) -> u64 {
    match next(client, |env| {
        matches!(env, Envelope::Committed { op: acked, .. } | Envelope::CommitRefused { op: acked, .. } if *acked == op)
    })
    .await
    {
        Envelope::Committed { seq, .. } => seq,
        other => panic!("commit refused: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resent_commits_keep_their_host_order_and_are_not_fanned_out_again() {
    let user = TestUser::fresh("Ada");
    let a = user.enrol("browser", DeviceCapabilities::member());
    let b = user.enrol("laptop", DeviceCapabilities::primary());
    let members = [a.device_pubkey(), b.device_pubkey()];
    let state = tempfile::tempdir().unwrap();
    let host = spawn_relay_on(
        state.path(),
        &user,
        user.enrol("home-server", DeviceCapabilities::host()),
        &[WS],
        &members,
    )
    .await;
    let a_client = connect(&user, &a, host.node_addr()).await;
    let b_client = connect(&user, &b, host.node_addr()).await;

    a_client.send(commit(&a, [1; 16], vec![1, 2, 3])).unwrap();
    let seq = committed(&a_client, [1; 16]).await;
    next(&b_client, |env| matches!(env, Envelope::Delta { .. })).await;

    a_client.send(commit(&a, [1; 16], vec![1, 2, 3])).unwrap();
    assert_eq!(committed(&a_client, [1; 16]).await, seq, "same host order");
    no_delta(&b_client).await;

    a_client.send(commit(&a, [1; 16], vec![9])).unwrap();
    assert!(matches!(
        next(&a_client, |env| matches!(env, Envelope::CommitRefused { .. })).await,
        Envelope::CommitRefused { op, .. } if op == [1; 16]
    ));
    no_delta(&b_client).await;

    // The ledger is durable: after a restart the resend is still recognised.
    a_client.close().await;
    b_client.close().await;
    host.endpoint.close().await;
    drop(host);
    let host = spawn_relay_on(
        state.path(),
        &user,
        user.enrol("home-server-2", DeviceCapabilities::host()),
        &[WS],
        &members,
    )
    .await;
    let a_client = connect(&user, &a, host.node_addr()).await;
    let b_client = connect(&user, &b, host.node_addr()).await;
    a_client.send(commit(&a, [1; 16], vec![1, 2, 3])).unwrap();
    assert_eq!(committed(&a_client, [1; 16]).await, seq);
    no_delta(&b_client).await;

    a_client.send(commit(&a, [2; 16], vec![4])).unwrap();
    assert!(committed(&a_client, [2; 16]).await > seq);
    next(&b_client, |env| matches!(env, Envelope::Delta { .. })).await;
}
