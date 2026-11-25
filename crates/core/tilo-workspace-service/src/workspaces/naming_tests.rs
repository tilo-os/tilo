//! The workspace's one name: what its governance records is what its host's
//! lists and listings name, whoever's permitted device wrote it.

use ed25519_dalek::SigningKey;
use std::time::SystemTime;

use super::fixtures::{hosting_device, test_host as test_holder};
use super::tests::admitted_member;

/// A member device other than the host renames the workspace in its own
/// replica and publishes the change: the host takes it in, and every list it
/// answers names the workspace what that device wrote, as every member reads
/// it.
#[tokio::test]
async fn a_rename_another_device_writes_is_what_the_hosts_lists_name() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_holder(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    let (writer, _salt) = admitted_member(&workspaces, &workspace, &cert, 3).await;
    let writer_key = SigningKey::from_bytes(&[3; 32]);
    let workspace_id = workspaces
        .workspace(&workspace)
        .await
        .unwrap()
        .lock()
        .await
        .workspace_id();
    let body = {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let hosted = state.lock().await;
        let mut written = hosted.peer().clone();
        let heads = written.workspace_heads().unwrap();
        written
            .governance_mut()
            .author_workspace_name("Tilo", &writer_key, SystemTime::now())
            .unwrap();
        let changes = written.workspace_changes_since(&heads);
        drop(hosted);
        postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
            changes,
            workspace: workspace.clone(),
        })
        .unwrap()
    };
    workspaces
        .absorb(&workspace, writer.device_public_key, &body)
        .await
        .unwrap();

    assert_eq!(workspaces.summary(&workspace).await.unwrap().name, "Tilo");
    assert_eq!(
        workspaces.listing_of(&workspace).await.unwrap().name,
        "Tilo"
    );
    let listed = workspaces
        .list()
        .await
        .into_iter()
        .find(|candidate| candidate.workspace_id == workspace_id)
        .expect("the workspace is listed");
    assert_eq!(listed.name, "Tilo", "{listed:?}");
    drop(workspaces);
    server.shutdown().await;
}
