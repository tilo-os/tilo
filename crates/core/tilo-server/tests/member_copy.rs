//! A device that holds a workspace as a member copy of a server reaches that server: the
//! server's workspace link asks the device to prove itself first, and the copy does before its
//! dial names the workspace.

use tilo_server::WorkspaceId;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{TestUser, own_service, spawn_server};

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(0x6d65_6d62_6572_2d63_6f70_79);

/// A laptop that holds the workspace as a member copy of its server reaches that server, proving
/// its device there first as the server's workspace link asks, and reads and writes the
/// workspace's tree through it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_device_opens_the_workspaces_tree_from_its_server() {
    let ada = TestUser::fresh("Ada");
    let server_device = ada.enrol("home-server", DeviceCapabilities::member());
    let laptop = ada.enrol("laptop", DeviceCapabilities::primary());
    let server_key = server_device.device_pubkey();
    let server = spawn_server(&ada, server_device, &[WORKSPACE], &[&laptop]).await;
    let hosted = server
        .server
        .workspaces()
        .expect("the server holds workspaces");
    let hint = hosted
        .hosts(&WORKSPACE.to_string())
        .await
        .expect("the server signs where it is reached")
        .into_iter()
        .find(|hint| hint.host.device_public_key == server_key)
        .expect("the server names its own address");
    let state = tempfile::tempdir().unwrap();
    let (service, _keys) = own_service(state.path(), &laptop).await;
    let copies = service.workspaces().clone();
    tilo_test_env::within(
        "the laptop holds the workspace as a member copy",
        copies.hold_as_member(WORKSPACE, &hint),
    )
    .await
    .expect("the server admits the laptop's membership");
    let tree = tilo_workspace_service::files::start(
        &copies,
        WORKSPACE,
        &state.path().join("tree"),
        (laptop.device_pubkey(), laptop.cert.user_public_key),
        None,
        None,
    )
    .await
    .expect("the laptop's tree opens");
    tree.change(None, |namespace| {
        namespace
            .create_dir(tilo_fs::namespace::ROOT_INO, "notes", 1)
            .map(|_| ())
            .ok_or_else(|| "the folder could not be made".to_owned())
    })
    .await
    .expect("the laptop reads the workspace's tree from its server and writes to it");
}
