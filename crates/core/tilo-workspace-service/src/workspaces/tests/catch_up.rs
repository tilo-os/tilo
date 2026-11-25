//! A catch-up a device asks of a host that keeps the workspace: answered with
//! where the host's store stands, whether or not its link is up yet.

use tilo_server_protocol::WorkspaceId;

use crate::workspaces::tests::{governor, test_host};

#[tokio::test]
async fn a_host_that_serves_no_link_answers_a_catch_up_where_its_store_stands() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let created = workspaces.create("Notes").await.unwrap();
    let workspace_id = WorkspaceId::parse(&created.workspace).unwrap();
    let standing = workspaces
        .direct()
        .link(workspace_id)
        .expect("a host serves the workspace it holds")
        .standing()
        .expect("its store stands");
    // The workspace stays kept here, its changes in this host's store, and no
    // link serves it: a catch-up is answered with where that store stands,
    // which is what the link read before it went, rather than refused.
    workspaces.direct().release(workspace_id);
    let answered = workspaces
        .catch_up_from(&cert, &created.workspace)
        .await
        .expect("the host answers where its store stands");
    assert_eq!(answered, standing, "the answer is where its store stands");
    host.shutdown().await;
}
