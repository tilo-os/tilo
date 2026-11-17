//! A same-owner home host enrols its owner's devices into workspaces on demand,
//! and nobody else's — not even a member its handshake trusts because
//! another workspace's governance admits their root.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::path::Path;

use tilo_peer_state::GovernedAccess;
use tilo_server::{Envelope, Server, ServerClient, ServerConfig, TrustPolicy};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, client_endpoint};
use tilo_server::WorkspaceId;

/// The member's own workspace, governed natively on this host.
const GOVERNED: WorkspaceId = WorkspaceId::from_u128(801);
/// The owner's workspace, bound on demand by the owner's first subscribe.
const OWNERS: WorkspaceId = WorkspaceId::from_u128(802);
/// A workspace nobody bound yet.
const UNBOUND: WorkspaceId = WorkspaceId::from_u128(803);

struct HomeHost {
    server: Server,
    state: tempfile::TempDir,
    owner: TestUser,
    bob: TestUser,
}

/// A certified home host with the operator policy `AllowMatchingUser`
/// (autobind on), one governed workspace admitting Bob, and the owner's workspace
/// autobound by the owner's first device.
async fn home_host() -> HomeHost {
    let owner = TestUser::fresh("Owner");
    let bob = TestUser::fresh("Bob");
    let host = owner.enrol("home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = crate::common::start_server(
        ServerConfig {
            state_root: state.path().to_path_buf(),
            trust_policy: TrustPolicy::AllowMatchingUser,
            ..ServerConfig::default()
        },
        tilo_server::daemon::ServerIdentity {
            user_public_key: owner.user_pk,
            device_signing_key: host.sk,
            device_cert: host.cert,
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
    };
    let laptop = host.owner.enrol("laptop", DeviceCapabilities::primary());
    let laptop_key = laptop.device_pubkey();
    let laptop = connect(&host.server, laptop).await;
    assert!(
        matches!(
            subscribe(&laptop, OWNERS).await,
            Some(Envelope::Ready { .. })
        ),
        "the owner's first device binds and enters the owner's workspace"
    );
    assert!(members(host.state.path(), OWNERS).contains(&laptop_key));
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
    tilo_test_env::within("server answered subscription", client.recv()).await
}

/// The devices the workspace's access record admits, as its host keeps them.
fn members(state: &Path, workspace_id: WorkspaceId) -> Vec<[u8; 32]> {
    let directory = state.join(format!("workspaces/{workspace_id}"));
    if !directory.exists() {
        return Vec::new();
    }
    tilo_server::store::WorkspaceStore::open(state, workspace_id, true)
        .unwrap()
        .load_membership()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_user_device_is_not_auto_enrolled_into_same_owner_workspace() {
    let host = home_host().await;
    let bob_laptop = host.bob.enrol("bob-laptop", DeviceCapabilities::primary());
    let bob_key = bob_laptop.device_pubkey();
    host.server
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

    // The governed workspace's root lets Bob's laptop through the handshake, and
    // into the workspace that admits it.
    let bob = connect(&host.server, bob_laptop).await;
    assert!(matches!(
        subscribe(&bob, GOVERNED).await,
        Some(Envelope::Ready { .. })
    ));

    // The owner's workspace does not enrol him, and does not remember him.
    assert!(matches!(
        subscribe(&bob, OWNERS).await,
        Some(Envelope::NotAuthorised {
            workspace_id: OWNERS,
            ..
        })
    ));
    assert!(
        !members(host.state.path(), OWNERS).contains(&bob_key),
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
async fn owner_device_still_auto_enrols_on_home_host() {
    let host = home_host().await;
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
    // The workspace keeps no list of who came: the owner's root admits the owner's
    // devices, so another one is admitted on its first arrival as this one was.
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
    let kept = members(host.state.path(), OWNERS);
    assert!(
        kept.contains(&phone_key) && kept.contains(&tablet_key),
        "a home host keeps its own devices in the workspace's one record"
    );
    Server::shutdown(host.server).await;
}
