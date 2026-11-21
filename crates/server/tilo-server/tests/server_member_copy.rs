//! A device's own workspace service holds a workspace a server hosts as a member copy, the way
//! it holds one another device serves: it proves its membership at the address the server signs
//! for the workspace, and the copy syncs with the server from then on, across a restart of the
//! device's service too.

use std::sync::Arc;
use std::time::Duration;

use tilo_server::WorkspaceId;
use tilo_server::workspaces::Workspaces;
use tilo_user_identity::DeviceCapabilities;
use tilo_workspace_service::apps::own_device;

use crate::common::{TestDevice, TestUser, device_endpoint, spawn_server};

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(0x6d65_6d62_6572);

/// The service `laptop` keeps under `state`, opened as a device opens its own: joining only.
async fn own_service(
    state: &std::path::Path,
    laptop: &TestDevice,
) -> tilo_server::service::WorkspaceService {
    let signer: Arc<dyn collab_types::signer::DeviceSigner> = Arc::new(laptop.sk.clone());
    own_device::open(
        state.to_path_buf(),
        own_device::Device {
            cert: &laptop.cert,
            signer: &signer,
        },
        device_endpoint(laptop).await,
        own_device::key_groups(tilo_workspace_keys::KeyProvider::default(), None, None),
        false,
        |config| config,
    )
    .await
    .expect("the laptop's own service opens")
}

/// Wait until `workspaces` reads `name` as the workspace's name.
async fn named(workspaces: &Workspaces, name: &str) {
    let workspace = WORKSPACE.to_string();
    tilo_test_env::within("the member copy reads the server's rename", async {
        loop {
            if workspaces
                .summary(&workspace)
                .await
                .is_ok_and(|summary| summary.name == name)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
}

/// Wait until the server reads the lease `id` held by `device`.
async fn leased(server: &Workspaces, id: &str, device: [u8; 32]) {
    let workspace = WORKSPACE.to_string();
    tilo_test_env::within("the server reads the member copy's write", async {
        loop {
            let holder = server
                .state(&workspace)
                .await
                .and_then(|state| state.workspace().lease(id, tilo_clock::wall()).holder);
            if holder.is_some_and(|claim| claim.device == device) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
}

/// Write the lease `id` on the member copy, as `copies`' device.
async fn lease(copies: &Workspaces, id: &str) {
    let workspace = WORKSPACE.to_string();
    let write = copies
        .member_membership_write(
            &workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Lease {
                id: id.into(),
                ttl_ms: 60_000,
                rank: tilo_peer_state::unranked(),
            },
        )
        .await
        .expect("the member copy writes the lease");
    copies
        .commit_member_document(&workspace, write)
        .await
        .expect("the member copy keeps the lease");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_devices_own_service_holds_a_workspace_a_server_hosts_as_a_member_copy() {
    let ada = TestUser::fresh("Ada");
    let server_device = ada.enrol("home-server", DeviceCapabilities::member());
    let laptop = ada.enrol("laptop", DeviceCapabilities::primary());
    let server_key = server_device.device_pubkey();
    let server = spawn_server(&ada, server_device, &[WORKSPACE], &[&laptop.cert]).await;
    let hosted = server
        .server
        .workspaces()
        .expect("the server holds workspaces");
    let workspace = WORKSPACE.to_string();
    let hint = hosted
        .hosts(&workspace)
        .await
        .expect("the server signs where it is reached")
        .into_iter()
        .find(|hint| hint.host.device_public_key == server_key)
        .expect("the server names its own address");

    let state = tempfile::tempdir().unwrap();
    let service = own_service(state.path(), &laptop).await;
    let copies = service.workspaces().clone();
    assert_eq!(
        tilo_test_env::within(
            "the laptop holds the workspace as a member copy",
            copies.hold_as_member(WORKSPACE, &hint),
        )
        .await
        .expect("the server admits the laptop's membership"),
        WORKSPACE
    );
    assert!(copies.is_member(&workspace).await.unwrap());

    // Both ways: the laptop's write reaches the server, and the server's reaches the laptop.
    lease(&copies, "laptop").await;
    leased(hosted, "laptop", laptop.device_pubkey()).await;
    hosted
        .rename(&workspace, "Studio")
        .await
        .expect("the server renames the workspace");
    named(&copies, "Studio").await;

    // The laptop's service opens again on the copy it kept, and syncs with the server from it.
    drop(copies);
    service.shutdown().await;
    let service = own_service(state.path(), &laptop).await;
    let copies = service.workspaces().clone();
    assert!(
        copies.is_member(&workspace).await.unwrap(),
        "the copy is held as a member copy after a restart"
    );
    named(&copies, "Studio").await;
    hosted
        .rename(&workspace, "Workshop")
        .await
        .expect("the server renames the workspace again");
    named(&copies, "Workshop").await;
    lease(&copies, "laptop-again").await;
    leased(hosted, "laptop-again", laptop.device_pubkey()).await;
    drop(copies);
    service.shutdown().await;
}
