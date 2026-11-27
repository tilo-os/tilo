//! What the workspace service answered is on disk before it answered: a part
//! that ends as a killed process ends loses nothing it said was done.

use automerge::ReadDoc as _;

use super::fixtures::{hosting_device, test_host, test_host_keyed, write_record};

/// The record `(app, record)` of `workspace` as this service holds it now, opened.
async fn read_record(
    workspaces: &super::Workspaces,
    workspace: &str,
    (app, record): (&str, &str),
) -> automerge::AutoCommit {
    let bytes = workspaces
        .resource_document(workspace, app, record)
        .await
        .expect("the record reads")
        .expect("the workspace holds the record");
    automerge::AutoCommit::load(&bytes).expect("the record opens")
}

#[tokio::test]
async fn a_created_workspace_is_held_after_a_part_ends_that_saved_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let made = server
        .workspaces()
        .create("Studio")
        .await
        .expect("the workspace is made");
    // The part ends as the process being killed does: no stop, and nothing saved on the way out.
    let workspaces = server.workspaces().clone();
    drop(server);

    // The next part opens the same store: the workspace was held before the create answered.
    let reopened = test_host(dir.path(), &key, &cert).await;
    let held = reopened
        .workspaces()
        .workspace(&made.workspace)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "the workspace {} the create answered is held after a part that saved nothing: \
                 {error}; the files on disk: {:?}",
                made.workspace,
                std::fs::read_dir(dir.path().join("workspaces"))
                    .map(|entries| entries
                        .flatten()
                        .map(|entry| entry.file_name())
                        .collect::<Vec<_>>())
                    .unwrap_or_default()
            )
        });
    assert_eq!(
        held.lock().await.summary().name,
        "Studio",
        "the workspace the create answered is the one held"
    );
    drop(workspaces);
    reopened.shutdown().await;
}

#[tokio::test]
async fn the_persons_workspace_is_held_after_a_part_ends_that_saved_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let own = super::person_workspace::workspace_id(server.workspaces());
    super::person_workspace::found(server.workspaces())
        .await
        .expect("the person's workspace is founded here");
    // The part ends as the process being killed does: no stop, and nothing saved on the way out.
    let workspaces = server.workspaces().clone();
    drop(server);

    let reopened = test_host(dir.path(), &key, &cert).await;
    reopened
        .workspaces()
        .workspace(&own)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "the person's own workspace {own} the founding answered is held after a part \
                 that saved nothing: {error}; the files on disk: {:?}",
                std::fs::read_dir(dir.path().join("workspaces"))
                    .map(|entries| entries
                        .flatten()
                        .map(|entry| entry.file_name())
                        .collect::<Vec<_>>())
                    .unwrap_or_default()
            )
        });
    assert!(
        super::person_workspace::holds_person_workspace(reopened.workspaces()).await,
        "the reopened service names this device as one holding the person's workspace"
    );
    drop(workspaces);
    reopened.shutdown().await;
}

#[cfg(feature = "device-keys")]
#[tokio::test]
async fn a_record_a_part_wrote_is_there_after_it_ends_without_a_save() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    // A device whose channel service keeps its keys, as a device's does: they open what it sealed
    // when it starts again.
    let (server, keys) = test_host_keyed(dir.path(), &key, &cert).await;
    let workspace = server
        .workspaces()
        .create("Studio")
        .await
        .expect("the workspace is made")
        .workspace;
    write_record(
        server.workspaces(),
        &workspace,
        ("so.tilo.notes", "note"),
        |doc| {
            use automerge::transaction::Transactable as _;
            doc.put(automerge::ROOT, "title", "Groceries")
                .map_err(|error| error.to_string())
        },
    )
    .await
    .expect("the record is written");
    // The part ends as the process being killed does: no stop, and nothing saved on the way out.
    let workspaces = server.workspaces().clone();
    drop(server);
    drop(keys);

    let (reopened, keys) = test_host_keyed(dir.path(), &key, &cert).await;
    // The app that keeps the record declares its kind where it runs again, as every embedder
    // does: what names the record is the store's, not a surface's.
    super::fixtures::declare_record_kind(reopened.workspaces(), "so.tilo.notes");
    let record = read_record(reopened.workspaces(), &workspace, ("so.tilo.notes", "note")).await;
    assert_eq!(
        record
            .get(automerge::ROOT, "title")
            .expect("the title reads")
            .map(|(value, _)| value.to_string()),
        Some("\"Groceries\"".to_owned()),
        "the record a part wrote is there after it ends without a save"
    );
    drop(workspaces);
    drop(keys);
    reopened.shutdown().await;
}
