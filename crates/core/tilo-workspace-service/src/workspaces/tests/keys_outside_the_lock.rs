//! A write waits for the group keys it needs with its workspace's lock let go: the key service
//! can take seconds to open a group, and every other user of the workspace (a read, a hand-over
//! catching the workspace up) would wait as long under the lock, and give up.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use collab_types::store::Resource;
use collab_types::store::wire::{Command, Reply};

use crate::workspaces::fixtures::{TestGroupKeys, declare_app, hosting_device, test_host_holding};

const APP: &str = "test.keyed";
const NOTE: &str = "tl.test.keyed.Note";

/// The app's surface: one kind of note, written by its one verb.
fn surface() -> Vec<u8> {
    collab_types::surface::canonical(&serde_json::json!({
        "resources": [{
            "kind": NOTE,
            "label": "Note",
            "version": 1,
            "fields": [{"path": "title", "type": "text"}],
            "verbs": [{"id": "write", "kind": "undoable"}],
        }],
    }))
    .into_bytes()
}

/// How long a test waits for what happens at once before it fails: far less than the key
/// service's wait for a group, so a lock kept across that wait fails it.
const AT_ONCE: Duration = Duration::from_secs(5);

/// A note written while this device holds none of the workspace's keys waits for the group to
/// let it in. While it waits, the workspace is free to whoever else takes it; once the keys are
/// here, the note is written as any.
#[tokio::test]
async fn a_write_waiting_for_its_keys_leaves_the_workspace_to_others() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let keys = Arc::new(TestGroupKeys::default());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
    let workspaces = server.workspaces().clone();
    workspaces.set_member_device(Arc::new(crate::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    declare_app(&server, APP, &surface());
    let workspace = workspaces.create("Notes").await.unwrap().workspace;

    keys.hold_openings();
    let writing = tokio::spawn({
        let (workspaces, workspace) = (workspaces.clone(), workspace.clone());
        async move {
            let put = Command::Put {
                kind: NOTE.to_owned(),
                id: "kept".to_owned(),
                resource: Resource {
                    readers: collab_types::EVERY_MEMBER.to_owned(),
                    parent: None,
                    order: None,
                    fields: BTreeMap::from([(
                        "title".to_owned(),
                        collab_types::store::Cell::One("Kept".into()),
                    )]),
                },
                stamp: tilo_resource_store::Stamp {
                    verb: "write".to_owned(),
                    ..tilo_resource_store::Stamp::default()
                },
            };
            workspaces
                .own_resource_command(&workspace, APP, put, Vec::new())
                .await
        }
    });
    let mut writing = writing;
    tokio::select! {
        () = keys.opening_waits() => {}
        written = &mut writing => panic!("the write waited for no keys: {written:?}"),
        () = tokio::time::sleep(AT_ONCE) => panic!("the write never asked for the group's keys"),
    }

    let locked = tokio::time::timeout(AT_ONCE, workspaces.hold_workspace(&workspace))
        .await
        .expect("the workspace is free while the write waits for its keys")
        .unwrap();
    drop(locked);
    assert!(!writing.is_finished(), "the write still waits for its keys");

    keys.let_openings_go();
    let written = tokio::time::timeout(AT_ONCE, writing)
        .await
        .expect("the write goes on once its keys are here")
        .unwrap();
    assert!(
        matches!(written, Ok(Reply::Written)),
        "the note is written: {written:?}"
    );
    let read = workspaces
        .own_resource_command(
            &workspace,
            APP,
            Command::Get {
                kind: NOTE.to_owned(),
                id: "kept".to_owned(),
            },
            Vec::new(),
        )
        .await
        .unwrap();
    assert!(
        matches!(read, Reply::Resource(Some(_))),
        "the note reads back: {read:?}"
    );
    server.shutdown().await;
}

/// A device's own write of an app's document, sealed where its workspace's documents are, waits
/// for the workspace's group as a resource write does: with the workspace left to others, and
/// written once the keys are here.
#[tokio::test]
async fn a_documents_write_waiting_for_its_keys_leaves_the_workspace_to_others() {
    use automerge::transaction::Transactable as _;
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let keys = Arc::new(TestGroupKeys::default());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
    let workspaces = server.workspaces().clone();
    workspaces.set_member_device(Arc::new(crate::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Layout").await.unwrap().workspace;

    keys.hold_openings();
    let mut document = automerge::AutoCommit::new();
    document.put(automerge::ROOT, "front", "Main").unwrap();
    let mut writing = tokio::spawn({
        let (workspaces, workspace) = (workspaces.clone(), workspace.clone());
        async move {
            workspaces
                .own_write(
                    &workspace,
                    (DESKTOP_APP, DESKTOP_DOCUMENT),
                    None,
                    &document.save(),
                    true,
                    true,
                    None,
                )
                .await
                .map(drop)
        }
    });
    tokio::select! {
        () = keys.opening_waits() => {}
        written = &mut writing => panic!("the write waited for no keys: {written:?}"),
        () = tokio::time::sleep(AT_ONCE) => panic!("the write never asked for the group's keys"),
    }

    let locked = tokio::time::timeout(AT_ONCE, workspaces.hold_workspace(&workspace))
        .await
        .expect("the workspace is free while the write waits for its keys")
        .unwrap();
    drop(locked);
    assert!(!writing.is_finished(), "the write still waits for its keys");

    keys.let_openings_go();
    let written = tokio::time::timeout(AT_ONCE, writing)
        .await
        .expect("the write goes on once its keys are here")
        .unwrap();
    assert!(written.is_ok(), "the document is written: {written:?}");
    server.shutdown().await;
}
