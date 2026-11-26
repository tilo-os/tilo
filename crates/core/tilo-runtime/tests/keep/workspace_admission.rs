//! A home server admits the devices its workspaces name, and nobody else —
//! not even a member its handshake trusts because another workspace's
//! governance admits their root. A workspace the server does not hold is not
//! bound on demand.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use tilo_peer_state::join::Role;
use tilo_runtime::device::keep::server::Server;
use tilo_server_protocol::config::ServerConfig;
use tilo_server_protocol::policy::TrustPolicy;
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::keep::common::{
    Linked, TestDevice, TestUser, hold_workspace, invitation, member_devices, redeem,
};
use tilo_server_protocol::WorkspaceId;

/// The member's own workspace, made by its owner on this host.
const LISTED: WorkspaceId = WorkspaceId::from_u128(801);
/// The owner's workspace, made on their server.
const OWNERS: WorkspaceId = WorkspaceId::from_u128(802);
/// A workspace the server does not hold.
const UNBOUND: WorkspaceId = WorkspaceId::from_u128(803);

struct HomeHost {
    server: Server,
    state: tempfile::TempDir,
    owner: TestUser,
    bob: TestUser,
    /// The server's own device, which hosts the workspaces it makes.
    server_dev: TestDevice,
}

/// A certified home server with the operator policy `AllowMatchingUser`, the
/// owner's workspace made on it and the owner's first device entered.
async fn home_host() -> HomeHost {
    let owner = TestUser::fresh("Owner");
    let bob = TestUser::fresh("Bob");
    let server_dev = owner.enrol("home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = crate::keep::common::start_server(
        ServerConfig {
            state_root: state.path().to_path_buf(),
            trust_policy: TrustPolicy::AllowMatchingUser,
            ..ServerConfig::default()
        },
        tilo_runtime::device::keep::server::ServerIdentity {
            user_public_key: owner.user_pk,
            device_signing_key: server_dev.sk.clone(),
            device_cert: server_dev.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let host = HomeHost {
        server,
        state,
        owner,
        bob,
        server_dev,
    };
    let laptop = host.owner.enrol("laptop", DeviceCapabilities::primary());
    let laptop_key = laptop.device_pubkey();
    hold_workspace(&host.server, OWNERS, &[&laptop]).await;
    let laptop = Linked::open(&host.server, &laptop, OWNERS)
        .await
        .expect("the owner's first device enters the owner's workspace");
    assert!(
        member_devices(&host.server, OWNERS)
            .await
            .contains(&laptop_key)
    );
    drop(laptop);
    host
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_user_device_is_not_admitted_into_a_workspace_it_does_not_hold() {
    let host = Box::pin(home_host()).await;
    let bob_laptop = host.bob.enrol("bob-laptop", DeviceCapabilities::primary());
    let bob_key = bob_laptop.device_pubkey();
    let workspaces = host.server.workspaces().unwrap();
    hold_workspace(&host.server, LISTED, &[]).await;
    let issued = invitation(
        workspaces,
        &host.server_dev,
        &LISTED.to_string(),
        Role::Editor,
        &bob_laptop,
    )
    .await;
    redeem(&bob_laptop, &issued).await.unwrap();

    // The workspace's root lets Bob's laptop through the handshake,
    // and into the workspace that admits it.
    let bob = Linked::open(&host.server, &bob_laptop, LISTED).await;
    assert!(
        bob.is_ok(),
        "the workspace that admits Bob's device opens its link"
    );
    drop(bob);

    // The owner's workspace does not admit him, and does not remember him.
    let owners = Linked::open(&host.server, &bob_laptop, OWNERS).await;
    assert!(
        owners.is_err(),
        "a cross-user device links the owner's workspace"
    );
    assert!(
        !member_devices(&host.server, OWNERS)
            .await
            .contains(&bob_key),
        "a cross-user device was persisted into the owner's workspace"
    );

    // Nor does he get a workspace bound on this host for him.
    let unbound = Linked::open(&host.server, &bob_laptop, UNBOUND).await;
    assert!(
        unbound.is_err(),
        "a cross-user device bound a workspace on the owner's host"
    );
    assert!(
        !host
            .state
            .path()
            .join(format!("workspaces/{UNBOUND}"))
            .exists(),
        "a cross-user device bound a workspace on the owner's host"
    );
    Server::shutdown(host.server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owner_device_still_enters_on_a_home_host() {
    let host = Box::pin(home_host()).await;
    let phone = host.owner.enrol("phone", DeviceCapabilities::primary());
    let phone_key = phone.device_pubkey();
    let phone = Linked::open(&host.server, &phone, OWNERS)
        .await
        .expect("an owner's device enters the owner's workspace");
    drop(phone);
    // The workspace keeps no list of who came: the owner's root is a member,
    // so another device of theirs is admitted on its first arrival as this
    // one was.
    let tablet = host.owner.enrol("tablet", DeviceCapabilities::primary());
    let tablet_key = tablet.device_pubkey();
    let tablet = Linked::open(&host.server, &tablet, OWNERS)
        .await
        .expect("another device of the owner's enters the same workspace");
    drop(tablet);
    let kept = member_devices(&host.server, OWNERS).await;
    assert!(
        kept.contains(&phone_key) && kept.contains(&tablet_key),
        "a home host keeps its own devices in the workspace's one record"
    );
    Server::shutdown(host.server).await;
}
