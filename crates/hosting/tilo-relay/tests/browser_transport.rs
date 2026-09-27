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
use iroh_base::{EndpointAddr, RelayUrl};
use iroh_relay::server::{RelayConfig as PacketConfig, Server, ServerConfig};
use std::net::Ipv4Addr;
use tilo_relay::{
    Envelope, Relay, RelayClient, RelayConfig, WorkspaceBinding, daemon::RelayIdentity,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn certified_clients_use_only_the_injected_packet_relay() {
    let mut packet_config = ServerConfig::default();
    packet_config.relay = Some(PacketConfig::new((Ipv4Addr::LOCALHOST, 0)));
    let packet = Server::spawn(packet_config).await.unwrap();
    let url: RelayUrl = format!("http://{}", packet.http_addr().unwrap())
        .parse()
        .unwrap();
    let mode = RelayMode::Custom(RelayMap::from(url.clone()));
    let user = TestUser::fresh("browser transport test");
    let host = user.enrol("host", DeviceCapabilities::host());
    let a = user.enrol("browser-a", DeviceCapabilities::member());
    let b = user.enrol("browser-b", DeviceCapabilities::member());
    let tmp = tempfile::tempdir().unwrap();
    let workspace_id =
        tilo_relay::room_workspace_id("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let config = RelayConfig {
        state_root: tmp.path().to_owned(),
        identity_root: tmp.path().join("identity"),
        full_replica_status: tmp.path().join("full-replica/status.json"),
        home_peer_bundle: tmp.path().join("home-peer.toml"),
        trust_policy: tilo_relay::TrustPolicy::AllowList {
            users: vec![hex::encode(user.user_pk)],
        },
        iroh_listen: (Ipv4Addr::LOCALHOST, 0).into(),
        workspaces: vec![WorkspaceBinding {
            id: workspace_id,
            pin: true,
            replication: tilo_relay::ReplicationIntent::FullWorkspace,
            admitted_users: vec![],
            shell_authority: None,
            checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
        }],
        ..Default::default()
    };
    let identity = RelayIdentity {
        user_public_key: user.user_pk,
        device_signing_key: host.sk.clone(),
        device_cert: host.cert.clone(),
        revocations: RevocationList::new(),
    };
    let relay = Relay::start_with_relay_mode(config, identity, mode.clone())
        .await
        .unwrap();
    tilo_test_env::within(
        "custom packet relay must become online",
        relay.endpoint.online(),
    )
    .await;
    assert_eq!(
        relay.node_addr().relay_urls().collect::<Vec<_>>(),
        vec![&url]
    );
    relay
        .set_workspace_membership(
            workspace_id,
            true,
            vec![a.device_pubkey(), b.device_pubkey()],
            tmp.path().to_owned(),
        )
        .await
        .unwrap();
    // No IP addresses are passed to either client, as in a real browser.
    let relay_addr = EndpointAddr::new(relay.node_addr().id).with_relay_url(url);
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
    let first = RelayClient::connect(
        endpoint_a,
        relay_addr.clone(),
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let second = RelayClient::connect(
        endpoint_b,
        relay_addr,
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    for client in [&first, &second] {
        client.send(Envelope::Subscribe { workspace_id }).unwrap();
        assert!(matches!(
            tilo_test_env::within("a message on client", client.recv()).await,
            Some(Envelope::Ready { .. })
        ));
    }
    let mut directory =
        tilo_user_identity::WorkspaceDirectory::new(user.user_pk, a.device_pubkey());
    directory
        .add_workspace(workspace_id, "Shared browser workspace")
        .unwrap();
    directory
        .set_member_devices(workspace_id, &[a.device_pubkey(), b.device_pubkey()])
        .unwrap();
    let checkpoint = directory.save();
    let body = tilo_relay::CoeditPayload::Checkpoint {
        channel: [9; 32],
        body: checkpoint.clone(),
    }
    .encode()
    .unwrap();
    first
        .send(Envelope::Delta {
            workspace_id,
            author_device_public_key: a.device_pubkey(),
            authored_at_ns: 1,
            delta: body,
            signature: [0; 64],
        })
        .unwrap();
    let received = tilo_test_env::within("a message on second", second.recv())
        .await
        .unwrap();
    let Envelope::Delta {
        author_device_public_key,
        delta,
        ..
    } = received
    else {
        panic!("expected authenticated retained checkpoint");
    };
    assert_eq!(author_device_public_key, a.device_pubkey());
    let Some(tilo_relay::CoeditPayload::Checkpoint { body, .. }) =
        tilo_relay::CoeditPayload::decode(&delta)
    else {
        panic!("expected checkpoint carrier");
    };
    let replica =
        tilo_user_identity::WorkspaceDirectory::load(user.user_pk, b.device_pubkey(), &body)
            .unwrap();
    assert_eq!(replica.list_workspaces(), directory.list_workspaces());
    // A valid same-root certificate without workspace membership is still
    // rejected by the configured membership policy.
    let outsider = user.enrol("unlisted device", DeviceCapabilities::member());
    let outsider_endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(mode)
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let outsider_client = RelayClient::connect_to_enrolled_home_peer(
        outsider_endpoint,
        EndpointAddr::new(relay.node_addr().id)
            .with_relay_url(relay.node_addr().relay_urls().next().unwrap().clone()),
        user.user_pk,
        host.cert.device_public_key,
        outsider.cert,
        outsider.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    outsider_client
        .send(Envelope::Subscribe { workspace_id })
        .unwrap();
    assert!(matches!(
        tilo_test_env::within("a message on outsider_client", outsider_client.recv()).await,
        Some(Envelope::NotAuthorised { .. })
    ));
    outsider_client.close().await;
    first.close().await;
    second.close().await;
    relay.shutdown().await;
    packet.shutdown().await.unwrap();
}
