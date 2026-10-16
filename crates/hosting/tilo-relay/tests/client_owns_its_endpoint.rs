//! A dialled session outlives the caller's endpoint handle.
//!
//! `RelayClient::connect` takes the iroh `Endpoint` **by value**. A
//! `Connection` does not keep its endpoint open, so if the client did
//! not hold on to it, handing over the only handle would close the
//! socket the moment `connect` returned — and the failure is silent
//! rather than loud: the handshake bytes are already in flight, so
//! `connect` still succeeds, and only the *next* exchange goes missing.
//! Every caller in this suite and in the toolkit's relay bridge dials
//! that way, so the whole tier goes quiet on a single dropped handle.
//!
//! This test dials with an endpoint that has no other owner and then
//! requires two post-handshake exchanges — the `Ready` that answers a
//! `Subscribe`, and a `Ping`/`Pong` round trip after it.

#![allow(
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_relay::{Envelope, RelayClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay};

/// Take the next envelope or fail loudly. A dropped endpoint shows up
/// here as a timeout, so the message has to name the suspect.
async fn next_envelope(client: &RelayClient, what: &str) -> Envelope {
    tokio::time::timeout(tilo_test_env::hang_bound(), client.recv())
        .await
        .unwrap_or_else(|_| {
            panic!("timed out waiting for {what} — the session's endpoint went away")
        })
        .unwrap_or_else(|| panic!("session closed before {what}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_survives_the_caller_handing_over_its_only_endpoint() {
    let user = TestUser::fresh("Alice");
    let relay_dev = user.enrol("home-server", DeviceCapabilities::host());
    let device = user.enrol("laptop", DeviceCapabilities::primary());

    let relay = Box::pin(spawn_relay(
        &user,
        relay_dev,
        &[7],
        &[device.device_pubkey()],
    ))
    .await;

    // The endpoint is constructed inside the call: the test keeps no
    // handle of its own, so only the returned client can hold it open.
    let client = RelayClient::connect(
        client_endpoint().await,
        relay.relay.node_addr(),
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("connect");

    client
        .send(Envelope::Subscribe { workspace_id: 7 })
        .expect("the session must still accept a subscribe");
    match next_envelope(&client, "the Ready that answers Subscribe").await {
        Envelope::Ready {
            workspace_id: 7,
            pending_deltas: 0,
        } => {}
        other => panic!("expected Ready(7, 0), got {other:?}"),
    }

    // A second exchange, after the subscribe: proves the wire is still
    // live rather than having delivered one queued frame on its way out.
    client
        .send(Envelope::Ping)
        .expect("the session must still accept a ping");
    match next_envelope(&client, "the Pong that answers Ping").await {
        Envelope::Pong => {}
        other => panic!("expected Pong, got {other:?}"),
    }
}
