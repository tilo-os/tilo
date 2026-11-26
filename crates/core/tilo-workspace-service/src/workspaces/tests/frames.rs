//! A write is kept before a frame of it is carried: one the workspace refuses when it keeps it
//! puts nothing on the wire, under the numbers its store's next draft takes again.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

use collab_types::store::Resource;
use collab_types::store::wire::Command;
use collab_types::{CommitAuthor, CommitPart, WorkspaceCommit};

/// The app the tests write: the note one test keeps, and the page one has refused.
const APP: &str = "test.frames";
const NOTE: &str = "tl.test.frames.Note";
const PAGE: &str = "tl.test.frames.Page";

/// The app's surface: kinds its one verb writes. The page's kind is its own, so a write of it
/// founds a document of its own.
fn surface() -> Vec<u8> {
    let kind = |kind: &str, label: &str| {
        serde_json::json!({
            "kind": kind,
            "label": label,
            "version": 1,
            "fields": [{"path": "title", "type": "text"}],
            "verbs": [{"id": "write", "kind": "undoable"}],
        })
    };
    collab_types::surface::canonical(&serde_json::json!({
        "resources": [kind(NOTE, "Note"), kind(PAGE, "Page")],
    }))
    .into_bytes()
}

/// The record `id` of `kind`, written by the app's verb, in the document every member reads.
fn write_record(kind: &str, id: &str) -> Command {
    Command::Put {
        kind: kind.to_owned(),
        id: id.to_owned(),
        resource: Resource {
            readers: collab_types::EVERY_MEMBER.to_owned(),
            parent: None,
            order: None,
            fields: BTreeMap::from([(
                "title".to_owned(),
                collab_types::store::Cell::One(collab_types::store::Value::from(id)),
            )]),
        },
        stamp: tilo_resource_store::Stamp {
            verb: "write".to_owned(),
            ..tilo_resource_store::Stamp::default()
        },
    }
}

/// How many of `bodies`, the frames a workspace serves, are writes of `app`.
fn served_app_documents(bodies: &[crate::workspace_documents::RetainedBody], app: &str) -> usize {
    bodies
        .iter()
        .filter(|body| {
            postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body.body).is_ok_and(|message| {
                matches!(
                    message,
                    neighbour_wire::NeighbourMsg::AppDocument { app_id, .. } if app_id == app
                )
            })
        })
        .count()
}

/// How many of the frames `workspace_id` serves are writes of `app`.
async fn served(
    server: &crate::workspaces::fixtures::TestHost,
    workspace_id: tilo_server_protocol::WorkspaceId,
    app: &str,
) -> usize {
    let bodies = server
        .workspace_documents()
        .retained(workspace_id)
        .await
        .unwrap();
    served_app_documents(&bodies, app)
}

/// A note the workspace keeps goes out; one it refuses when it keeps it does not. Its commit
/// names another device than the one that signs it, so its keep stops after its frames were
/// made: nothing of it is carried, and no number it was drafted under is on the wire for the
/// next draft to take again with other bytes.
#[tokio::test]
async fn a_write_refused_at_its_keep_puts_no_frame_out() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    crate::workspaces::fixtures::declare_app(&server, APP, &surface());
    let created = workspaces.create("Notes").await.unwrap();
    let workspace = created.workspace.clone();
    let device = workspaces.signing_device();

    // A note the workspace keeps is served.
    {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let mut hosted = state.lock().await;
        let writes = {
            let mut store = workspaces.store_of(&mut hosted);
            tilo_resource_store::wire::draft(&mut store, write_record(NOTE, "kept"))
                .map_err(|refused| refused.error.to_string())
                .unwrap()
        };
        drop(hosted);
        let hosted = std::sync::Arc::clone(&state).lock_owned().await;
        workspaces
            .keep_writes(hosted, std::sync::Arc::clone(&device), APP, writes, None)
            .await
            .map(|_| ())
            .unwrap();
    }
    let kept = served(&server, created.workspace_id, APP).await;
    assert!(kept > 0, "a write the workspace keeps is served");
    // A note whose commit names another device: its keep refuses it after its frames were made.
    {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let mut hosted = state.lock().await;
        let writes = {
            let mut store = workspaces.store_of(&mut hosted);
            tilo_resource_store::wire::draft(&mut store, write_record(PAGE, "refused"))
                .map_err(|refused| refused.error.to_string())
                .unwrap()
        };
        drop(hosted);
        let commit = WorkspaceCommit::new(
            created.workspace_id.as_uuid(),
            CommitAuthor {
                actor: None,
                device: [0x5c; 32],
            },
            1,
            None,
            None,
            writes
                .iter()
                .map(|write| CommitPart {
                    app: APP.to_owned(),
                    document: write.doc.clone(),
                    heads: vec![write.change.hash().0],
                })
                .collect(),
        );
        let hosted = std::sync::Arc::clone(&state).lock_owned().await;
        let refused = workspaces
            .keep_writes(hosted, device, APP, writes, Some(commit))
            .await
            .map(|_| ());
        let refused = refused.expect_err("the workspace refuses the write at its keep");
        assert!(
            refused.contains("another device") || refused.contains("signed by a device"),
            "the store refuses it as its commit names another device: {refused}"
        );
    }
    let after = served(&server, created.workspace_id, APP).await;
    assert_eq!(after, kept, "a write refused at its keep puts no frame out");
}

/// One action's write of two documents raises a notice for each, after both are kept, and each
/// names the other: whoever reads one document of the action reads what it wrote beside it, so a
/// message's files are read with the message.
#[tokio::test]
async fn a_write_of_two_documents_notices_them_together() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    crate::workspaces::fixtures::declare_app(&server, APP, &surface());
    let created = workspaces.create("Notes").await.unwrap();
    let workspace = created.workspace.clone();
    let device = workspaces.signing_device();
    let mut notices = workspaces.document_writes();

    let documents = {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let mut hosted = state.lock().await;
        let writes = {
            let mut store = workspaces.store_of(&mut hosted);
            let command =
                Command::Batch(vec![write_record(NOTE, "one"), write_record(PAGE, "two")]);
            tilo_resource_store::wire::draft(&mut store, command)
                .map_err(|refused| refused.error.to_string())
                .unwrap()
        };
        drop(hosted);
        let documents: BTreeSet<String> = writes.iter().map(|write| write.doc.clone()).collect();
        let hosted = std::sync::Arc::clone(&state).lock_owned().await;
        workspaces
            .keep_writes(hosted, device, APP, writes, None)
            .await
            .map(|_| ())
            .unwrap();
        documents
    };
    let mut told = Vec::new();
    while let Ok(notice) = notices.try_recv() {
        told.push(notice);
    }
    assert_eq!(
        told.len(),
        documents.len(),
        "one notice for each document the action wrote: {told:?}"
    );
    for notice in &told {
        assert!(
            documents.contains(&notice.record),
            "a document it wrote: {notice:?}"
        );
        let beside: BTreeSet<&String> = notice.together.iter().collect();
        assert_eq!(
            beside,
            documents.iter().collect::<BTreeSet<_>>(),
            "the notice names every document the action wrote: {notice:?}"
        );
    }
}
