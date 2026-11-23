//! A home server admits the devices its workspaces name, and nobody else —
//! not even a member its handshake trusts because another workspace's
//! governance admits their root. A workspace the server does not hold is not
//! bound on demand.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use tilo_peer_state::join::Role;
use tilo_server::{Envelope, Server, ServerClient, ServerConfig, TrustPolicy};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{
    TestDevice, TestUser, client_endpoint, hold_workspace, invitation, member_devices, redeem,
};
use tilo_server::WorkspaceId;

/// The member's own workspace, governed natively on this host.
const GOVERNED: WorkspaceId = WorkspaceId::from_u128(801);
/// The owner's workspace, made on their server.
const OWNERS: WorkspaceId = WorkspaceId::from_u128(802);
/// A workspace the server does not hold.
const UNBOUND: WorkspaceId = WorkspaceId::from_u128(803);

struct HomeHost {
    server: Server,
    state: tempfile::TempDir,
    owner: TestUser,
    bob: TestUser,
    /// The server's own device, which governors the workspaces it makes.
    server_dev: TestDevice,
}

/// A certified home server with the operator policy `AllowMatchingUser`, the
/// owner's workspace made on it and the owner's first device entered.
async fn home_host() -> HomeHost {
    let owner = TestUser::fresh("Owner");
    let bob = TestUser::fresh("Bob");
    let server_dev = owner.enrol("home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = crate::common::start_server(
        ServerConfig {
            state_root: state.path().to_path_buf(),
            trust_policy: TrustPolicy::AllowMatchingUser,
            ..ServerConfig::default()
        },
        tilo_server::daemon::ServerIdentity {
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
    hold_workspace(&host.server, OWNERS, &[&laptop.cert]).await;
    let laptop = connect(&host.server, laptop).await;
    assert!(
        matches!(
            subscribe(&laptop, OWNERS).await,
            Some(Envelope::Ready { .. })
        ),
        "the owner's first device enters the owner's workspace"
    );
    assert!(
        member_devices(&host.server, OWNERS)
            .await
            .contains(&laptop_key)
    );
    host
}

async fn connect(server: &Server, device: TestDevice) -> ServerClient {
    ServerClient::connect_to_pinned_host(
        client_endpoint().await,
        server.node_addr(),
        device.cert,
        device.sk,
        RevocationList::new(),
    )
    .await
    .unwrap()
}

async fn subscribe(client: &ServerClient, workspace_id: WorkspaceId) -> Option<Envelope> {
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("server answered subscription", async {
        loop {
            match client.recv().await {
                Some(answer @ (Envelope::Ready { .. } | Envelope::NotAuthorised { .. })) => {
                    break Some(answer);
                }
                Some(_) => {}
                None => break None,
            }
        }
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_user_device_is_not_admitted_into_a_workspace_it_does_not_hold() {
    let host = Box::pin(home_host()).await;
    let bob_laptop = host.bob.enrol("bob-laptop", DeviceCapabilities::primary());
    let bob_key = bob_laptop.device_pubkey();
    let workspaces = host.server.workspaces().unwrap();
    hold_workspace(&host.server, GOVERNED, &[]).await;
    let issued = invitation(
        workspaces,
        &host.server_dev,
        &GOVERNED.to_string(),
        Role::Editor,
        &bob_laptop,
    )
    .await;
    redeem(&bob_laptop, &issued).await.unwrap();

    // The governed workspace's root lets Bob's laptop through the handshake,
    // and into the workspace that admits it.
    let bob = connect(&host.server, bob_laptop).await;
    assert!(matches!(
        subscribe(&bob, GOVERNED).await,
        Some(Envelope::Ready { .. })
    ));

    // The owner's workspace does not admit him, and does not remember him.
    assert!(matches!(
        subscribe(&bob, OWNERS).await,
        Some(Envelope::NotAuthorised {
            workspace_id: OWNERS,
            ..
        })
    ));
    assert!(
        !member_devices(&host.server, OWNERS)
            .await
            .contains(&bob_key),
        "a cross-user device was persisted into the owner's workspace"
    );

    // Nor does he get a workspace bound on this host for him.
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
        "a cross-user device bound a workspace on the owner's host"
    );
    Server::shutdown(host.server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owner_device_still_enters_on_a_home_host() {
    let host = Box::pin(home_host()).await;
    let phone = host.owner.enrol("phone", DeviceCapabilities::primary());
    let phone_key = phone.device_pubkey();
    let phone = connect(&host.server, phone).await;
    assert!(matches!(
        subscribe(&phone, OWNERS).await,
        Some(Envelope::Ready {
            workspace_id: OWNERS,
            ..
        })
    ));
    // The workspace keeps no list of who came: the owner's root is a member,
    // so another device of theirs is admitted on its first arrival as this
    // one was.
    let tablet = host.owner.enrol("tablet", DeviceCapabilities::primary());
    let tablet_key = tablet.device_pubkey();
    let tablet = connect(&host.server, tablet).await;
    assert!(matches!(
        subscribe(&tablet, OWNERS).await,
        Some(Envelope::Ready {
            workspace_id: OWNERS,
            ..
        })
    ));
    let kept = member_devices(&host.server, OWNERS).await;
    assert!(
        kept.contains(&phone_key) && kept.contains(&tablet_key),
        "a home host keeps its own devices in the workspace's one record"
    );
    Server::shutdown(host.server).await;
}
