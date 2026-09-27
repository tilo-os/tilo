//! Native governance can add and remove independent people while the relay is live.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]

use std::time::Duration;

use tilo_peer_state::GovernedAccess;
use tilo_relay::{Envelope, Relay, RelayClient, RelayConfig, WorkspaceBinding};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint};

fn binding(id: u64) -> WorkspaceBinding {
    WorkspaceBinding {
        id,
        pin: true,
        replication: tilo_relay::ReplicationIntent::FullWorkspace,
        admitted_users: Vec::new(),
        shell_authority: None,
        checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
    }
}

async fn connect(
    relay: &Relay,
    device: crate::common::TestDevice,
) -> Result<RelayClient, tilo_relay::ClientError> {
    RelayClient::connect_to_pinned_host(
        client_endpoint().await,
        relay.node_addr(),
        device.cert,
        device.sk,
        RevocationList::new(),
    )
    .await
}

async fn subscribe(client: &RelayClient, workspace_id: u64) -> Option<Envelope> {
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("relay answered subscription", client.recv()).await
}

fn retained_delta(body: &[u8]) -> Vec<u8> {
    tilo_relay::coedit::CoeditPayload::RetainedDelta {
        channel: [9; 32],
        body: body.to_vec(),
    }
    .encode()
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn governed_access_removes_a_live_person_without_widening_other_rooms() {
    let provider = TestUser::fresh("Provider");
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let charlie = TestUser::fresh("Charlie");
    let diana = TestUser::fresh("Diana");
    let host = provider.enrol("host", DeviceCapabilities::host());
    let alice_a = alice.enrol("alice-a", DeviceCapabilities::primary());
    let alice_b = alice.enrol("alice-b", DeviceCapabilities::primary());
    let bob_device = bob.enrol("bob", DeviceCapabilities::primary());
    let charlie_device = charlie.enrol("charlie-host", DeviceCapabilities::host());
    let diana_device = diana.enrol("diana", DeviceCapabilities::primary());
    let alice_a_key = alice_a.device_pubkey();
    let alice_reconnect_cert = alice_a.cert.clone();
    let alice_reconnect_key = alice_a.sk.clone();
    let bob_key = bob_device.device_pubkey();
    let charlie_key = charlie_device.device_pubkey();
    let bob_restart_cert = bob_device.cert.clone();
    let bob_restart_key = bob_device.sk.clone();
    let state = tempfile::tempdir().unwrap();
    let host_cert = host.cert.clone();
    let host_key = host.sk.clone();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            workspaces: vec![binding(701), binding(702)],
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: provider.user_pk,
            device_signing_key: host_key.clone(),
            device_cert: host_cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();

    relay
        .replace_governed_workspace_access(
            701,
            true,
            GovernedAccess {
                member_devices: vec![alice_a_key, bob_key, charlie_key],
                member_user_roots: vec![alice.user_pk, bob.user_pk, charlie.user_pk],
            },
            state.path().to_path_buf(),
        )
        .await
        .unwrap();
    relay
        .replace_governed_workspace_access(
            702,
            true,
            GovernedAccess {
                member_devices: vec![alice_a_key],
                member_user_roots: vec![alice.user_pk],
            },
            state.path().to_path_buf(),
        )
        .await
        .unwrap();

    let alice_client = connect(&relay, alice_a).await.unwrap();
    let bob_client = connect(&relay, bob_device).await.unwrap();
    assert!(matches!(
        subscribe(&alice_client, 701).await,
        Some(Envelope::Ready { .. })
    ));
    assert!(matches!(
        subscribe(&bob_client, 701).await,
        Some(Envelope::Ready { .. })
    ));
    let charlie_client = connect(&relay, charlie_device).await.unwrap();
    assert!(matches!(
        subscribe(&charlie_client, 701).await,
        Some(Envelope::NotAuthorised { .. })
    ));
    assert!(connect(&relay, diana_device).await.is_err());

    // A valid certificate under an admitted root is still not an exact member device.
    let alice_unlisted = connect(&relay, alice_b).await.unwrap();
    assert!(matches!(
        subscribe(&alice_unlisted, 701).await,
        Some(Envelope::NotAuthorised { .. })
    ));
    drop(alice_unlisted);

    relay
        .replace_governed_workspace_access(
            701,
            true,
            GovernedAccess {
                member_devices: vec![bob_key],
                member_user_roots: vec![bob.user_pk],
            },
            state.path().to_path_buf(),
        )
        .await
        .unwrap();

    // The already-connected writer gets an explicit authenticated refusal;
    // closure or lack of fanout alone is not sufficient revocation evidence.
    alice_client
        .send(Envelope::Delta {
            workspace_id: 701,
            author_device_public_key: alice_a_key,
            authored_at_ns: 2,
            delta: retained_delta(b"removed-live-writer"),
            signature: [0; 64],
        })
        .unwrap();
    assert!(matches!(
    tilo_test_env::within("a message on alice_client", alice_client.recv()).await,
    Some(Envelope::NotAuthorised { workspace_id: 701, reason })
            if reason == "workspace membership changed"
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(400), bob_client.recv())
            .await
            .is_err()
    );
    assert!(
        tilo_relay::store::WorkspaceStore::open(state.path(), 701, true)
            .unwrap()
            .load_retained()
            .unwrap()
            .is_empty(),
        "explicitly refused retained delta must not reach durable state"
    );

    // Alice stays globally handshake-admitted through room 702, but the
    // periodic audit then closes her room-701 stream and that global admission
    // cannot reopen room 701.
    assert!(
        tilo_test_env::within("alice_client to close", alice_client.recv())
            .await
            .is_none()
    );
    assert!(!relay.is_subscribed(701, &alice_a_key).await);
    assert!(relay.is_subscribed(701, &bob_key).await);

    let alice_reconnect = RelayClient::connect_to_pinned_host(
        client_endpoint().await,
        relay.node_addr(),
        alice_reconnect_cert,
        alice_reconnect_key,
        RevocationList::new(),
    )
    .await
    .unwrap();
    assert!(matches!(
        subscribe(&alice_reconnect, 701).await,
        Some(Envelope::NotAuthorised { .. })
    ));
    assert!(matches!(
        subscribe(&alice_reconnect, 702).await,
        Some(Envelope::Ready { .. })
    ));
    alice_reconnect
        .send(Envelope::Delta {
            workspace_id: 701,
            author_device_public_key: alice_a_key,
            authored_at_ns: 2,
            delta: b"removed-writer".to_vec(),
            signature: [0; 64],
        })
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(400), bob_client.recv())
            .await
            .is_err()
    );
    assert!(
        tilo_test_env::within("alice_reconnect to close", alice_reconnect.recv())
            .await
            .is_none()
    );

    let alice_room_b = connect(
        &relay,
        alice.enrol("alice-room-b", DeviceCapabilities::primary()),
    )
    .await
    .unwrap();
    assert!(matches!(
        subscribe(&alice_room_b, 702).await,
        Some(Envelope::NotAuthorised { .. })
    ));

    bob_client
        .send(Envelope::Delta {
            workspace_id: 701,
            author_device_public_key: bob_key,
            authored_at_ns: 1,
            delta: b"after-removal".to_vec(),
            signature: [0; 64],
        })
        .unwrap();
    assert!(alice_reconnect.recv().await.is_none());

    drop(alice_client);
    drop(alice_room_b);
    drop(alice_reconnect);
    drop(bob_client);
    relay.shutdown().await;

    let restarted = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            workspaces: vec![binding(701), binding(702)],
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: provider.user_pk,
            device_signing_key: host_key,
            device_cert: host_cert,
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let bob_after_restart = RelayClient::connect_to_pinned_host(
        client_endpoint().await,
        restarted.node_addr(),
        bob_restart_cert,
        bob_restart_key,
        RevocationList::new(),
    )
    .await
    .unwrap();
    assert!(matches!(
        subscribe(&bob_after_restart, 701).await,
        Some(Envelope::Ready { .. })
    ));
    let alice_after_restart = connect(
        &restarted,
        alice.enrol("alice-after-restart", DeviceCapabilities::primary()),
    )
    .await
    .expect("room 702 keeps Alice's root in the durable handshake union");
    assert!(matches!(
        subscribe(&alice_after_restart, 701).await,
        Some(Envelope::NotAuthorised { .. })
    ));
}
