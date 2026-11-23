//! Same authenticated relay/1 client over a loopback packet relay, with no N0
//! lookup or direct socket-address fallback (the browser's transport shape).
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
use crate::common::TestUser;
use iroh::{Endpoint, RelayMap, RelayMode, endpoint::presets};
use iroh_base::EndpointAddr;
use std::net::Ipv4Addr;
use tilo_server::{Envelope, Server, ServerClient, ServerConfig, daemon::ServerIdentity};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn certified_clients_use_only_the_injected_packet_relay() {
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
    let mode = RelayMode::Custom(RelayMap::from(url.clone()));
    tilo_test_env::within(
        "custom packet relay must become online",
        server.endpoint.online(),
    )
    .await;
    assert_eq!(
        server.node_addr().relay_urls().collect::<Vec<_>>(),
        vec![&url]
    );
    crate::common::hold_workspace(&server, workspace_id, &[&a.cert, &b.cert]).await;
    // No IP addresses are passed to either client, as in a real browser.
    let server_addr = EndpointAddr::new(server.node_addr().id).with_relay_url(url);
    let endpoint_a = Endpoint::builder(presets::Minimal)
        .relay_mode(mode.clone())
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let endpoint_b = Endpoint::builder(presets::Minimal)
        .relay_mode(mode.clone())
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let page_a = endpoint_a.clone();
    let page_b = endpoint_b.clone();
    let first = ServerClient::connect(
        endpoint_a,
        server_addr.clone(),
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let second = ServerClient::connect(
        endpoint_b,
        server_addr,
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    // A page checks its server connection after a network change (an `online`
    // event, or a wall-clock jump such as a throttled tab): the connection it
    // was admitted over must stay usable, while asking for admission too.
    page_a.network_change().await;
    for (client, page) in [(&first, &page_a), (&second, &page_b)] {
        client.send(Envelope::Subscribe { workspace_id }).unwrap();
        page.network_change().await;
        // A held workspace sends its own state before it is ready.
        crate::common::next(client, |env| matches!(env, Envelope::Ready { .. })).await;
    }
    let mut directory =
        tilo_user_identity::WorkspaceDirectory::new(user.user_pk, a.device_pubkey());
    directory
        .add_workspace(workspace_id, "Shared browser workspace")
        .unwrap();
    let checkpoint = directory.save();
    let body = tilo_server::CoeditPayload::Checkpoint {
        channel: [9; 32],
        body: checkpoint.clone(),
    }
    .encode()
    .unwrap();
    page_b.network_change().await;
    first
        .send(Envelope::Delta {
            workspace_id,
            author_device_public_key: a.device_pubkey(),
            authored_at_ns: 1,
            delta: body,
            signature: [0; 64],
        })
        .unwrap();
    // The person's workspace list comes whenever their workspaces change; the change is what
    // the second page is waiting for.
    let received = crate::common::next(&second, |env| matches!(env, Envelope::Delta { .. })).await;
    let Envelope::Delta {
        author_device_public_key,
        delta,
        ..
    } = received
    else {
        panic!("expected authenticated retained checkpoint, got {received:?}");
    };
    assert_eq!(author_device_public_key, a.device_pubkey());
    let Some(tilo_server::CoeditPayload::Checkpoint { body, .. }) =
        tilo_server::CoeditPayload::decode(&delta)
    else {
        panic!("expected checkpoint carrier");
    };
    let replica =
        tilo_user_identity::WorkspaceDirectory::load(user.user_pk, b.device_pubkey(), &body)
            .unwrap();
    assert_eq!(replica.list_workspaces(), directory.list_workspaces());
    // A valid certificate of a person the operator does not list is still
    // rejected: membership follows the person, never a bare certificate.
    let outsider =
        TestUser::fresh("outsider").enrol("unlisted device", DeviceCapabilities::member());
    let outsider_endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(mode)
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let outsider_client = ServerClient::connect_to_pinned_host(
        outsider_endpoint,
        EndpointAddr::new(server.node_addr().id)
            .with_relay_url(server.node_addr().relay_urls().next().unwrap().clone()),
        outsider.cert,
        outsider.sk,
        RevocationList::new(),
    )
    .await;
    assert!(
        outsider_client.is_err(),
        "a person the operator does not list is refused at the handshake"
    );
    first.close().await;
    second.close().await;
    server.shutdown().await;
}
