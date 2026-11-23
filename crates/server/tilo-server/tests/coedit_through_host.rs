//! Liveness of a co-edit session through the workspace host.
//!
//! A session measures how long the host has been silent, and a ping is
//! answered: what a device's watchdog uses to tell a host that stopped
//! serving its session from one that is merely idle.

use std::time::Duration;

use tilo_server::{Inbound, ServerClient, ServerCoeditSession};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_server_on};
use tilo_server::WorkspaceId;

/// A session measures how long the host has been silent, and a ping is
/// answered: what a device's watchdog uses to tell a host that stopped
/// serving its session from one that is merely idle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pinged_host_answers_and_resets_the_silence() {
    const WS: WorkspaceId = WorkspaceId::from_u128(23);

    let user = TestUser::fresh("Alice");
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let host_dev = user.enrol("home-server", DeviceCapabilities::member());
    let host = spawn_server_on(state.path(), &user, host_dev, &[WS], &[&a.cert]).await;
    let client = ServerClient::connect(
        client_endpoint().await,
        host.node_addr(),
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("connect");
    let session = ServerCoeditSession::join(client, WS, a.device_pubkey()).unwrap();
    // A held workspace sends its own state before it is ready.
    tilo_test_env::within("Ready timeout", async {
        loop {
            if let Inbound::Ready { .. } = session.recv().await.expect("the session stays open") {
                break;
            }
        }
    })
    .await;

    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(session.silent_for() >= Duration::from_secs(1));
    session.ping().unwrap();
    // A Pong carries no co-edit event, so `recv` keeps waiting after it.
    let _ = tokio::time::timeout(Duration::from_millis(200), session.recv()).await;
    assert!(
        session.silent_for() < Duration::from_millis(500),
        "the Pong counts as hearing from the host"
    );
    // The host serves until here.
    drop(host);
}
