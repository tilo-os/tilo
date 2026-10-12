//! A same-owner home host enrols its owner's devices into rooms on demand,
//! and nobody else's — not even a collaborator its handshake trusts because
//! another room's governance admits their root.

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

use std::path::Path;

use tilo_peer_state::GovernedAccess;
use tilo_relay::{Envelope, Relay, RelayClient, RelayConfig, TrustPolicy};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, client_endpoint};

/// The collaborator's own room, governed natively on this host.
const GOVERNED: u64 = 801;
/// The owner's room, bound on demand by the owner's first subscribe.
const OWNERS: u64 = 802;
/// A room nobody bound yet.
const UNBOUND: u64 = 803;

struct HomeHost {
    relay: Relay,
    state: tempfile::TempDir,
    owner: TestUser,
    bob: TestUser,
}

/// A certified home host with the operator policy `AllowMatchingUser`
/// (autobind on), one governed room admitting Bob, and the owner's room
/// autobound by the owner's first device.
async fn home_host() -> HomeHost {
    let owner = TestUser::fresh("Owner");
    let bob = TestUser::fresh("Bob");
    let host = owner.enrol("home", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            trust_policy: TrustPolicy::AllowMatchingUser,
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: owner.user_pk,
            device_signing_key: host.sk,
            device_cert: host.cert,
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let host = HomeHost {
        relay,
        state,
        owner,
        bob,
    };
    let laptop = host.owner.enrol("laptop", DeviceCapabilities::primary());
    let laptop_key = laptop.device_pubkey();
    let laptop = connect(&host.relay, laptop).await;
    assert!(
        matches!(
            subscribe(&laptop, OWNERS).await,
            Some(Envelope::Ready { .. })
        ),
        "the owner's first device binds and enters the owner's room"
    );
    assert!(members(host.state.path(), OWNERS).contains(&hex::encode(laptop_key)));
    host
}

async fn connect(relay: &Relay, device: TestDevice) -> RelayClient {
    RelayClient::connect_to_pinned_host(
        client_endpoint().await,
        relay.node_addr(),
        device.cert,
        device.sk,
        RevocationList::new(),
    )
    .await
    .unwrap()
}

async fn subscribe(client: &RelayClient, workspace_id: u64) -> Option<Envelope> {
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("relay answered subscription", client.recv()).await
}

fn members(state: &Path, workspace_id: u64) -> Vec<String> {
    let path = state.join(format!("workspaces/{workspace_id}/membership.json"));
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("membership unreadable: {error}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_user_device_is_not_auto_enrolled_into_same_owner_room() {
    let host = home_host().await;
    let bob_laptop = host.bob.enrol("bob-laptop", DeviceCapabilities::primary());
    let bob_key = bob_laptop.device_pubkey();
    host.relay
        .replace_governed_workspace_access(
            GOVERNED,
            true,
            GovernedAccess {
                member_devices: vec![bob_key],
                member_user_roots: vec![host.bob.user_pk],
            },
            host.state.path().to_path_buf(),
        )
        .await
        .unwrap();

    // The governed room's root lets Bob's laptop through the handshake, and
    // into the room that admits it.
    let bob = connect(&host.relay, bob_laptop).await;
    assert!(matches!(
        subscribe(&bob, GOVERNED).await,
        Some(Envelope::Ready { .. })
    ));

    // The owner's room does not enrol him, and does not remember him.
    assert!(matches!(
        subscribe(&bob, OWNERS).await,
        Some(Envelope::NotAuthorised {
            workspace_id: OWNERS,
            ..
        })
    ));
    assert!(
        !members(host.state.path(), OWNERS).contains(&hex::encode(bob_key)),
        "a cross-user device was persisted into the owner's room"
    );

    // Nor does he get a room bound on this host for him.
    assert!(matches!(
        subscribe(&bob, UNBOUND).await,
        Some(Envelope::NotAuthorised {
            workspace_id: UNBOUND,
            ..
        })
    ));
    assert!(
        !host
            .state
            .path()
            .join(format!("workspaces/{UNBOUND}"))
            .exists(),
        "a cross-user device bound a room on the owner's host"
    );
    Relay::shutdown(host.relay).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owner_device_still_auto_enrols_on_home_host() {
    let host = home_host().await;
    let phone = host.owner.enrol("phone", DeviceCapabilities::primary());
    let phone_key = phone.device_pubkey();
    let phone = connect(&host.relay, phone).await;
    assert!(matches!(
        subscribe(&phone, OWNERS).await,
        Some(Envelope::Ready {
            workspace_id: OWNERS,
            ..
        })
    ));
    assert!(members(host.state.path(), OWNERS).contains(&hex::encode(phone_key)));
    Relay::shutdown(host.relay).await;
}
