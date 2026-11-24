//! A workspace whose file tree a mount of an earlier build wrote, naming none of its records,
//! reads in Files once the server governing it opens again.

use super::super::fixtures::{governor, test_host};
use super::*;
use automerge::transaction::Transactable as _;
use automerge::{ObjType, ROOT};
use tilo_server_protocol::WorkspaceId;

/// The tree's own record and the chunks of it a mounted folder holding `notes.txt` wrote before
/// a mount named its records, each by its route: the tree's own record pointing at its root
/// folder and listing chunk `1`, and chunk `1` holding the folder and the file. Neither has the
/// meta map a reader checks.
fn tree_as_an_earlier_mount_wrote_it() -> Vec<(String, Vec<u8>)> {
    let tree = collab_types::person_document_id(collab_types::FILES_TREE_KIND);
    let mut own = AutoCommit::new();
    own.put(ROOT, "root_document_id", "root-folder").unwrap();
    let chunks = own.put_object(ROOT, "chunks", ObjType::Map).unwrap();
    own.put(&chunks, "1", true).unwrap();
    let mut chunk = AutoCommit::new();
    let nodes = chunk.put_object(ROOT, "nodes", ObjType::Map).unwrap();
    for (node, name) in [("laptop/0", ""), ("laptop/1", "notes.txt")] {
        let entry = chunk.put_object(&nodes, node, ObjType::Map).unwrap();
        chunk.put(&entry, "name", name).unwrap();
        chunk.put(&entry, "author", "laptop").unwrap();
    }
    vec![
        (tree.clone(), own.save()),
        (format!("derived:chunk.1@{tree}"), chunk.save()),
    ]
}

/// Files' own record of the workspace as its window of the same build wrote it: a package's
/// primary record names no resource, and is read as it is.
fn files_primary() -> Vec<u8> {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "view", "list").unwrap();
    doc.save()
}

/// Keep `records` as `workspace`'s records of Files, as the governor holding `key` took them in.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the workspace stays held through its settling"
)]
async fn keep(
    workspaces: &Workspaces,
    workspace: &str,
    key: &ed25519_dalek::SigningKey,
    records: Vec<(String, Vec<u8>)>,
) {
    let id = WorkspaceId::parse(workspace).unwrap();
    let state = workspaces.held(id).await.unwrap();
    let mut governed = state.lock().await;
    let at = tilo_clock::wall();
    let mut candidate = governed.peer().clone();
    for (record, bytes) in records {
        candidate
            .publication_mut()
            .publish_document(FILES_APP, &record, bytes, key, at)
            .unwrap();
    }
    let collaboration = governed.collaboration().clone();
    governed
        .keep(candidate, key, at, |_, _| Ok(collaboration))
        .unwrap();
    workspaces.settle(&mut governed).await.unwrap();
}

/// Why a reader refuses `workspace`'s Files record `record`, as every reader decides it before
/// it opens one, or `None` when it opens it.
async fn refusal(workspaces: &Workspaces, workspace: &str, record: &str) -> Option<String> {
    let bytes = workspaces
        .resource_document(workspace, FILES_APP, record)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("the workspace keeps {record}"));
    let doc = AutoCommit::load(&bytes).unwrap();
    let checkout = ui::resource::DocumentCheckout::new(
        ui::resource::DocumentId::parse(record).unwrap(),
        ui::resource::ResourceType::new(collab_types::FILES_TREE_KIND),
    );
    ui::resource::validate_identity(&doc, &checkout, workspace)
        .err()
        .map(|error| error.to_string())
}

/// Every record of a tree an earlier mount wrote is one a reader opens once the workspace's
/// server opens again, holding everything the mount wrote; Files' own record reads as before,
/// and opening again names nothing more.
#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the server stays live until its consuming shutdown"
)]
async fn a_tree_an_earlier_mount_wrote_reads_once_its_server_opens_again() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspace = server.workspaces().create("Demo").await.unwrap().workspace;
    let tree = tree_as_an_earlier_mount_wrote_it();
    let mut records = tree.clone();
    let written_primary = files_primary();
    records.push((String::new(), written_primary.clone()));
    keep(server.workspaces(), &workspace, &key, records).await;
    for (record, _) in &tree {
        assert_eq!(
            refusal(server.workspaces(), &workspace, record)
                .await
                .as_deref(),
            Some("record identity does not match its binding"),
            "before the server opens again, a reader refuses {record}"
        );
    }
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    for (record, written) in &tree {
        assert_eq!(
            refusal(workspaces, &workspace, record).await,
            None,
            "a reader opens {record}"
        );
        let bytes = workspaces
            .resource_document(&workspace, FILES_APP, record)
            .await
            .unwrap()
            .unwrap();
        let mut held = AutoCommit::load(&bytes).unwrap();
        let mut written = AutoCommit::load(written).unwrap();
        for hash in written.get_heads() {
            assert!(
                held.get_change_by_hash(&hash).is_some(),
                "{record} keeps everything the mount wrote"
            );
        }
        // The name is the founding a mount of this build makes, so a mount naming the record
        // at the same time writes the same change.
        let mut founding = identity::founding(
            &DocumentId::parse(record).unwrap(),
            &ResourceType::new(collab_types::FILES_TREE_KIND),
            &workspace,
            &workspace,
        )
        .unwrap();
        for hash in founding.get_heads() {
            assert!(
                held.get_change_by_hash(&hash).is_some(),
                "{record} is named by the founding every holder derives"
            );
        }
    }
    let primary = workspaces
        .resource_document(&workspace, FILES_APP, "")
        .await
        .unwrap()
        .expect("the workspace keeps Files' own record");
    assert_eq!(
        AutoCommit::load(&primary).unwrap().get_heads(),
        AutoCommit::load(&written_primary).unwrap().get_heads(),
        "Files' own record is read as it was written, and nothing is written into it"
    );
    assert_eq!(workspaces.name_kept_tree(&workspace).await, Ok(0));
    server.shutdown().await;
}
