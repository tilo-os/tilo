//! A dialled session outlives the caller's endpoint handle.
//!
//! `ServerClient::connect` takes the iroh `Endpoint` **by value**. A
//! `Connection` does not keep its endpoint open, so if the client did
//! not hold on to it, handing over the only handle would close the
//! socket the moment `connect` returned — and the failure is silent
//! rather than loud: the handshake bytes are already in flight, so
//! `connect` still succeeds, and only the *next* exchange goes missing.
//! Every caller in this suite and in the toolkit's server bridge dials
//! that way, so the whole tier goes quiet on a single dropped handle.
//!
//! This test dials with an endpoint that has no other owner and then
//! requires two post-handshake exchanges — the `Ready` that answers a
//! `Subscribe`, and a `Ping`/`Pong` round trip after it.

#![allow(
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_server::{Envelope, ServerClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, next, spawn_server};
use tilo_server::WorkspaceId;

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(7);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_survives_the_caller_handing_over_its_only_endpoint() {
    let user = TestUser::fresh("Alice");
    let server_dev = user.enrol("home-server", DeviceCapabilities::member());
    let device = user.enrol("laptop", DeviceCapabilities::primary());

    let server = Box::pin(spawn_server(
        &user,
        server_dev,
        &[WORKSPACE],
        &[&device.cert],
    ))
    .await;

    // The endpoint is constructed inside the call: the test keeps no
    // handle of its own, so only the returned client can hold it open.
    let client = ServerClient::connect(
        client_endpoint().await,
        server.server.node_addr(),
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("connect");

    client
        .send(Envelope::Subscribe {
            workspace_id: WORKSPACE,
        })
        .expect("the session must still accept a subscribe");
    // A held workspace sends its own state before it is ready.
    match next(&client, |env| matches!(env, Envelope::Ready { .. })).await {
        Envelope::Ready {
            workspace_id: WORKSPACE,
            pending_deltas: 0,
            ..
        } => {}
        other => panic!("expected Ready(7, 0), got {other:?}"),
    }

    // A second exchange, after the subscribe: proves the wire is still
    // live rather than having delivered one queued frame on its way out.
    client
        .send(Envelope::Ping)
        .expect("the session must still accept a ping");
    match next(&client, |env| matches!(env, Envelope::Pong)).await {
        Envelope::Pong => {}
        other => panic!("expected Pong, got {other:?}"),
    }
}
