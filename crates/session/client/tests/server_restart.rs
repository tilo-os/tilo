//! A server restarted under a computer's held session: the session hears the server go, dials
//! its address again through the packet relay that outlived it, and the computer's next action
//! is served within seconds, with nobody asking it to reconnect. And a session its device gave
//! up on while the server was away stops dialing as it is closed: a device enters anew under the
//! same key, and a server keeps one connection per key, so a session left dialing would cut the
//! new one off.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use tilo_session_client::{Outbox, TransportPhase};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{
    HostSettings, enrol, ready, restart_host_relayed, route, say, start_host_serving, start_peer,
};

/// How long a restart may stay visible to a held session once the server is back: its next dial
/// comes within a backoff of a few seconds.
const BACK_WITHIN: Duration = Duration::from_secs(20);

/// How long a closed session may take to end: no further dial, and its endpoint's packet relay
/// given the three seconds iroh grants it to hear the endpoint go. A session left dialing takes
/// up to three dials of ten seconds.
const CLOSED_WITHIN: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_session_is_served_again_within_seconds_of_its_server_restarting() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_host_serving(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings {
            relayed: true,
            ..HostSettings::default()
        },
    )
    .await;
    let session = start_peer(
        server.iroh(),
        laptop.cert.clone(),
        server_device.cert.clone(),
        laptop.key.clone(),
    );
    session
        .join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    ready(&[&session]).await;
    let mut note = automerge::AutoCommit::new();
    session.write(route(), &say(&mut note, "before")).unwrap();
    durable(&session, BACK_WITHIN).await.unwrap();

    drop(server.web);
    server.server.shutdown().await;
    let server = restart_host_relayed(&root, &server_device, state.path()).await;
    let back = Instant::now();

    // Nothing tells the session to reconnect: the computer's next action is simply served.
    session.write(route(), &say(&mut note, "after")).unwrap();
    let kept = durable(&session, BACK_WITHIN).await;
    assert!(
        kept.is_ok(),
        "the computer's next action after its server restarted was not served within {} s \
         (the session is {:?}: {:?})",
        BACK_WITHIN.as_secs(),
        session.status().phase,
        session.status().failures,
    );
    eprintln!(
        "served again {} ms after the restart",
        back.elapsed().as_millis()
    );
    session.close();
    server.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_closed_while_its_server_is_away_stops_dialing_at_once() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_host_serving(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings {
            relayed: true,
            ..HostSettings::default()
        },
    )
    .await;
    let address = server.iroh();
    drop(server.web);
    server.server.shutdown().await;

    // A device enters while its server is away, and gives up on the entry while it dials.
    let session = start_peer(
        address,
        laptop.cert.clone(),
        server_device.cert.clone(),
        laptop.key.clone(),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let closing = Instant::now();
    session.close();
    let closed = tokio::time::timeout(CLOSED_WITHIN, async {
        while session.status().phase != TransportPhase::Closed {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "a session closed mid-dial was still {:?} {} ms later",
        session.status().phase,
        closing.elapsed().as_millis()
    );
    eprintln!(
        "closed {} ms after it was closed",
        closing.elapsed().as_millis()
    );
}

/// Whether everything `session` wrote is durable at its server within `within`.
async fn durable(session: &tilo_session_client::Link, within: Duration) -> Result<(), ()> {
    let receipt = session.barrier().map_err(drop)?;
    tokio::time::timeout(within, async move {
        loop {
            if let Ok(answer) = receipt.try_recv() {
                return answer.map_err(drop);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(drop)?
}
