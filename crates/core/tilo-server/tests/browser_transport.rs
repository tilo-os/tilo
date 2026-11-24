//! The same certified device over a loopback packet relay, with no N0 lookup or
//! direct socket-address fallback (the browser's transport shape).
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
use crate::common::{Linked, TestUser, hold_workspace};
use automerge::{ROOT, transaction::Transactable as _};
use std::net::Ipv4Addr;
use tilo_server::{Server, ServerConfig, daemon::ServerIdentity};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

/// The app and record a member writes through its link in these tests.
const APP: &str = "test.app";
const RECORD: &str = "note:1";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn certified_devices_use_only_the_injected_packet_relay() {
    let user = TestUser::fresh("browser transport test");
    let host = user.enrol("host", DeviceCapabilities::member());
    let a = user.enrol("browser-a", DeviceCapabilities::member());
    let b = user.enrol("browser-b", DeviceCapabilities::member());
    let tmp = tempfile::tempdir().unwrap();
    let workspace_id =
        tilo_server::WorkspaceId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let config = ServerConfig {
        state_root: tmp.path().to_owned(),
        identity_root: tmp.path().join("identity"),
        trust_policy: tilo_server::TrustPolicy::AllowList {
            users: vec![hex::encode(user.user_pk)],
        },
        iroh_listen: (Ipv4Addr::LOCALHOST, 0).into(),
        // The host reaches its browsers through the packet relay it runs
        // itself: they hold no UDP socket of their own.
        packet_relay: Some((Ipv4Addr::LOCALHOST, 0).into()),
        ..Default::default()
    };
    let identity = ServerIdentity {
        user_public_key: user.user_pk,
        device_signing_key: host.sk.clone(),
        device_cert: host.cert.clone(),
        revocations: RevocationList::new(),
    };
    let server = Server::start(config, identity).await.unwrap();
    // The host answers through its own packet relay from the start: what
    // `Relay::start` waits for before it serves anything.
    let url = server
        .node_addr()
        .relay_urls()
        .next()
        .expect("the host advertises its own packet relay")
        .clone();
    assert!(url.as_str().starts_with("http://127.0.0.1:"), "{url}");
    tilo_test_env::within(
        "custom packet relay must become online",
        server.endpoint.online(),
    )
    .await;
    assert_eq!(
        server.node_addr().relay_urls().collect::<Vec<_>>(),
        vec![&url]
    );
    hold_workspace(&server, workspace_id, &[&a, &b]).await;

    // No IP addresses are passed to either device, as in a real browser: each
    // dials the relay and nothing else.
    let first = Linked::open_relayed(&server, &a, workspace_id)
        .await
        .expect("the relay admits the first device");
    let second = Linked::open_relayed(&server, &b, workspace_id)
        .await
        .expect("the relay admits the second device");
    // A page checks its server connection after a network change (an `online`
    // event, or a wall-clock jump such as a throttled tab): asking for
    // admission again over the relay it already reached must still answer,
    // and the connection it was admitted over stays usable.
    let relinked = Linked::open_relayed(&server, &a, workspace_id)
        .await
        .expect("the device asks for a link again after a network change");
    drop(relinked);

    // A change the first device publishes reaches the second over the relay.
    first
        .write(&a.sk, (APP, RECORD), |doc| {
            doc.put(ROOT, "workspace", "Shared browser workspace")
                .unwrap();
        })
        .unwrap();
    crate::common::receives(&second, |peer| {
        crate::common::opened(peer, &crate::common::KEYS, (APP, RECORD)).is_some()
    })
    .await;

    // A valid certificate of a person the operator does not list is still
    // rejected: membership follows the person, never a bare certificate.
    let outsider =
        TestUser::fresh("outsider").enrol("unlisted device", DeviceCapabilities::member());
    let outsider_link = Linked::open_relayed(&server, &outsider, workspace_id).await;
    assert!(
        outsider_link.is_err(),
        "a person the operator does not list is refused at the handshake"
    );
    drop(first);
    drop(second);
    server.shutdown().await;
}
