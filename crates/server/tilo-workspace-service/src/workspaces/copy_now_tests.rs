//! What this service holds reads without waiting on any workspace's lock: a caller on the
//! runtime that serves the workspaces (a search asking the action catalogue which workspaces a
//! device holds) gets its answer while a task of that runtime holds a workspace's lock, rather
//! than waiting on a lock that only that runtime can let go.

use super::fixtures::{governor, test_host as test_holder};

#[tokio::test]
async fn a_held_workspace_reads_while_its_lock_and_the_registry_are_held() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_holder(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Company").await.unwrap().workspace;
    let state = workspaces.workspace(&workspace).await.unwrap();
    let id = state.lock().await.workspace_id();

    // Held as a long change holds them: the workspace's own lock and the service's list.
    let governed = state.lock().await;
    let listed = workspaces.inner.workspaces.lock().await;
    let copy = workspaces
        .copy_now(id)
        .expect("the workspace reads while its lock is held");
    assert_eq!(copy.name, "Company");
    assert!(!copy.member, "governed here, not a member copy");
    assert_eq!(copy.peer.governance().workspace_name().as_deref(), Some("Company"));
    let all: Vec<_> = workspaces.copies_now().into_iter().map(|(held, _)| held).collect();
    assert_eq!(all, vec![id]);
    drop((listed, governed));

    // A workspace this service does not hold reads as none.
    assert!(
        workspaces
            .copy_now(tilo_server_protocol::WorkspaceId::from_u128(7))
            .is_none()
    );
}
