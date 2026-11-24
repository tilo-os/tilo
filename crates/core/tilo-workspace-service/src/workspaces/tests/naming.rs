//! A workspace is handed to a server only with its owner's change naming the
//! server among its holders: the person decides who holds their workspace, and
//! a server that finds one is given nothing.

use tilo_peer_state::{CopyRole, join::Role};
use tilo_user_identity::DeviceCapabilities;

use crate::workspaces::HostWorkspaceRequest;
use crate::workspaces::tests::{device_cert, governor, test_host};

#[tokio::test]
async fn a_hand_over_without_the_change_naming_the_server_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (owner_key, owner_cert) = governor();
    let owner = test_host(&dir.path().join("owner"), &owner_key, &owner_cert).await;
    let (server_key, server_cert) = device_cert(&owner_key, "server", DeviceCapabilities::member());
    let server = test_host(&dir.path().join("server"), &server_key, &server_cert).await;
    let workspace = owner.workspaces().create("Notes").await.unwrap().workspace;
    // What the owner's device would hand over before naming the server among
    // the workspace's holders: the workspace as a joining member gets it.
    let (workspace_id, holder_hint, bootstrap) = {
        let governed = owner.workspaces().workspace(&workspace).await.unwrap();
        let governed = governed.lock().await;
        let mut bootstrap = governed
            .governed(&owner_cert)
            .bootstrap([0; 32], server_cert.clone(), Role::Editor)
            .expect("the workspace as a joining member gets it");
        bootstrap.records.clear();
        let hint = owner
            .workspaces()
            .transport_hint(&governed)
            .expect("the owner's signed address");
        (governed.workspace_id(), hint, bootstrap)
    };
    let request = HostWorkspaceRequest {
        workspace: workspace.clone(),
        workspace_id,
        holder_hint,
        bootstrap,
        role: CopyRole::KeyHolder,
    };
    let refused = server
        .workspaces()
        .hold(&owner_cert, &request)
        .await
        .expect_err("a hand-over that names no server is refused");
    assert!(
        refused.contains("naming the server among its holders"),
        "{refused}"
    );
    assert!(
        server.workspaces().state(&workspace).await.is_none(),
        "nothing of a refused hand-over is kept"
    );
    owner.shutdown().await;
    server.shutdown().await;
}
