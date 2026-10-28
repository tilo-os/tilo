//! What a room's file says it published when the process is killed: only
//! changes the relay has made durable, so the next start publishes the rest
//! again instead of losing them.
use super::tests::{governor, view_read_relay};
use super::*;

/// `published` as the room's file on disk holds it now: what a start after
/// a hard stop would read.
fn published_on_disk(path: &Path) -> usize {
    let record: RoomRecord = postcard::from_bytes(&std::fs::read(path).unwrap()).unwrap();
    record.published
}

/// Decisions whose changes are still waiting in the workspace owner's queue
/// are written to the room file (each decision persists the room) before
/// those changes are durable. The file must not count them as published:
/// a room killed then reopens publishing them again; one that counted them
/// would never retain them, and pages would never read what they decided.
#[tokio::test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Relay::shutdown consumes the relay after the assertions"
)]
async fn a_room_file_counts_only_durable_changes_as_published() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = view_read_relay(dir.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let created = rooms.create("Public").await.unwrap();
    let records = rooms.inner.host.records.clone();
    // The creation's own changes are durable before the owner stalls.
    records.retained(created.workspace_id).await.unwrap();
    let state = rooms.room(&created.room).await.unwrap();
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
    // Each rename is a decision: it persists the room, then hands its change
    // over. The second and third persist after earlier changes were handed
    // over, none of them durable while the owner is stalled.
    for index in 0..3 {
        tilo_test_env::within(
            format!("rename {index} while the owner is stalled"),
            rooms.rename(&created.room, &format!("Public {index}")),
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
    // The next decision persists the room again, counting what is durable now.
    rooms.rename(&created.room, "Public").await.unwrap();
    assert!(
        published_on_disk(&path) >= durable + 3,
        "durable changes are published on disk"
    );
    drop(state);
    drop(rooms);
    relay.shutdown().await;
}
