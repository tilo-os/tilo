//! A device's own workspace service holds a workspace a server hosts as a member copy, the way
//! it holds one another device serves: it proves its membership at the address the server signs
//! for the workspace, and the copy syncs with the server from then on, across a restart of the
//! device's service too: what either seals, the other opens, with the keys the device's key
//! service takes part in the workspace's group for over the member link.

use std::sync::Arc;
use std::time::Duration;

use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
use tilo_peer_state::workspace_documents::{DeviceDocuments as _, DeviceWrites as _, DocumentRead};
use tilo_server::WorkspaceId;
use tilo_server::workspaces::Workspaces;
use tilo_user_identity::DeviceCapabilities;
use tilo_workspace_service::apps::own_device;
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};

use crate::common::{TestDevice, TestUser, device_endpoint, spawn_server};

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(0x6d65_6d62_6572);

/// The service `laptop` keeps under `state`, opened as a device opens its own: joining only,
/// with the device's key service beside it, which takes part in the groups of the workspaces the
/// service holds.
async fn own_service(
    state: &std::path::Path,
    laptop: &TestDevice,
) -> (
    tilo_server::service::WorkspaceService,
    own_device::KeyService,
) {
    let signer: Arc<dyn collab_types::signer::DeviceSigner> = Arc::new(laptop.sk.clone());
    let keys = own_device::KeyService::open(
        "laptop-groups",
        &state.join("groups"),
        key_group::VaultKey::new([3; 32]),
        Arc::clone(&signer),
        Arc::new(tilo_workspace_keys::NoSealChecks),
    )
    .expect("the laptop's key service opens");
    let service = own_device::open(
        state.join("service"),
        own_device::Device {
            cert: &laptop.cert,
            signer: &signer,
        },
        device_endpoint(laptop).await,
        keys.groups(),
        false,
        |config| config,
    )
    .await
    .expect("the laptop's own service opens");
    keys.attach(service.workspaces());
    (service, keys)
}

/// The workspace's layout, a record every member reads and its group seals.
fn layout_route() -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: DESKTOP_APP.into(),
        cluster_id: WORKSPACE.to_string(),
        document: DESKTOP_DOCUMENT.into(),
    }
}

/// The layout as `workspaces` reads it now, when it reads.
fn layout(workspaces: &Workspaces) -> Option<automerge::AutoCommit> {
    let read = ServiceDocuments::of(workspaces.clone())
        .read(&WORKSPACE.to_string(), DESKTOP_APP, DESKTOP_DOCUMENT, None)
        .ok()?;
    let DocumentRead::Whole { bytes, .. } = read else {
        return None;
    };
    automerge::AutoCommit::load(&bytes).ok()
}

/// The layout's front, as `layout` holds it.
fn front(layout: &automerge::AutoCommit) -> Option<String> {
    use automerge::ReadDoc as _;
    match layout.get(automerge::ROOT, "front").ok()? {
        Some((automerge::Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
        _ => None,
    }
}

/// Wait until `workspaces` reads `want` as the layout's front.
async fn reads_front(workspaces: &Workspaces, what: &str, want: &str) {
    tilo_test_env::within(what, async {
        loop {
            let read = workspaces.clone();
            let now = tokio::task::spawn_blocking(move || layout(&read).as_ref().and_then(front))
                .await
                .unwrap();
            if now.as_deref() == Some(want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
}

/// Write `front` as the layout's front on `workspaces`, sealed with the keys its device's key
/// service holds for the layout's group: the layout's founding write where none is held yet.
async fn write_front(workspaces: &Workspaces, what: &str, want: &str) {
    let sealed = tilo_test_env::within(what, async {
        loop {
            if let Some(Ok(keys)) = workspaces.own_route_keys(&layout_route(), true).await {
                return keys;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let read = workspaces.clone();
    let held = tokio::task::spawn_blocking(move || layout(&read))
        .await
        .unwrap();
    let founding = held.is_none();
    let mut document = held.unwrap_or_default();
    let heads = document.get_heads();
    automerge::transaction::Transactable::put(&mut document, automerge::ROOT, "front", want)
        .unwrap();
    let changes = if founding {
        document.save()
    } else {
        document.save_after(&heads)
    };
    ServiceWrites::of(workspaces.clone())
        .write(
            &WORKSPACE.to_string(),
            DESKTOP_APP,
            DESKTOP_DOCUMENT,
            None,
            &changes,
            founding,
            founding,
            Some(&sealed),
        )
        .await
        .unwrap_or_else(|refusal| panic!("{what}: {refusal:?}"));
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
#[expect(
    clippy::significant_drop_tightening,
    reason = "the test's server and the state root it keeps stay alive until the test ends"
)]
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
    let (service, keys) = own_service(state.path(), &laptop).await;
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

    // A sealed record both ways: the laptop's key service takes part in the workspace's group over
    // its member link, so what it seals the server opens, and what the server seals it opens.
    write_front(&copies, "the laptop seals the layout", "Main").await;
    reads_front(hosted, "the server opens the laptop's layout", "Main").await;
    write_front(hosted, "the server seals a change of the layout", "Moved").await;
    reads_front(&copies, "the laptop opens the server's change", "Moved").await;

    // The laptop's service opens again on the copy it kept, and syncs with the server from it.
    drop(copies);
    service.shutdown().await;
    drop(keys);
    let (service, keys) = own_service(state.path(), &laptop).await;
    let copies = service.workspaces().clone();
    assert!(
        copies.is_member(&workspace).await.unwrap(),
        "the copy is held as a member copy after a restart"
    );
    named(&copies, "Studio").await;
    reads_front(&copies, "the laptop opens the layout it kept", "Moved").await;
    write_front(
        &copies,
        "the laptop seals a change after its restart",
        "Again",
    )
    .await;
    reads_front(hosted, "the server opens the change", "Again").await;
    hosted
        .rename(&workspace, "Workshop")
        .await
        .expect("the server renames the workspace again");
    named(&copies, "Workshop").await;
    lease(&copies, "laptop-again").await;
    leased(hosted, "laptop-again", laptop.device_pubkey()).await;
    drop(copies);
    service.shutdown().await;
    drop(keys);
}
