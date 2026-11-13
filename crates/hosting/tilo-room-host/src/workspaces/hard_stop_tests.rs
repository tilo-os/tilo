//! What a workspace's file says it published when the process is killed: only
//! changes the relay has made durable, so the next start publishes the rest
//! again instead of losing them.
#![allow(
    clippy::significant_drop_tightening,
    reason = "the relay and the owner's record handle are this test's subject until its last assertion"
)]
use super::tests::{governor, test_host};
use super::*;

/// `published` as the workspace's file on disk holds it now: what a start after
/// a hard stop would read.
fn published_on_disk(path: &Path) -> usize {
    let record: super::record_format::StoredRecord =
        postcard::from_bytes(&std::fs::read(path).unwrap()).unwrap();
    record.published
}

/// Decisions whose changes are still waiting in the workspace owner's queue
/// are written to the workspace file (each decision persists the workspace) before
/// those changes are durable. The file must not count them as published:
/// a workspace killed then reopens publishing them again; one that counted them
/// would never retain them, and pages would never read what they decided.
#[tokio::test]
async fn a_room_file_counts_only_durable_changes_as_published() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Public").await.unwrap();
    let records = workspaces.inner.host.records.clone();
    // The creation's own changes are durable before the owner stalls.
    records.retained(created.workspace_id).await.unwrap();
    let state = workspaces.workspace(&created.workspace).await.unwrap();
    let (path, durable) = {
        let governed = state.lock().await;
        let at = (governed.path.clone(), governed.record.published);
        drop(governed);
        at
    };

    let root = dir
        .path()
        .join("workspaces")
        .join(created.workspace_id.to_string());
    let (release, reached) = crate::store::test_barrier::install(&root, "state-persist");
    // Each rename is a decision: it persists the workspace, then hands its change
    // over. The second and third persist after earlier changes were handed
    // over, none of them durable while the owner is stalled.
    for index in 0..3 {
        tilo_test_env::within(
            format!("rename {index} while the owner is stalled"),
            workspaces.rename(&created.workspace, &format!("Public {index}")),
        )
        .await
        .unwrap();
    }
    tilo_test_env::within("the owner stalls on the first change", reached)
        .await
        .unwrap();
    assert_eq!(
        published_on_disk(&path),
        durable,
        "a change that is not durable yet is not published on disk"
    );

    drop(release);
    tilo_test_env::within(
        "every change retained once the owner moves",
        records.retained(created.workspace_id),
    )
    .await
    .unwrap();
    // The next decision persists the workspace again, counting what is durable now.
    workspaces
        .rename(&created.workspace, "Public")
        .await
        .unwrap();
    assert!(
        published_on_disk(&path) >= durable + 3,
        "durable changes are published on disk"
    );
    drop(state);
    drop(workspaces);
    relay.shutdown().await;
}
