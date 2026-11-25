//! What a watch of a workspace reads from the service's own copy of its records.

use std::collections::BTreeMap;
use std::sync::Arc;

use tilo_actions::ActionContext;
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_resource_store::history::authors::{HistoryAuthor, ResolveAuthor};
use tilo_resource_store::history::words::HistoryWords;
use tilo_resource_store::wire::{Command, Reply};
use tilo_resource_store::{Resource, Stamp};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use super::DocumentsFeed;
use crate::apps::services::ResourceRequestContext;
use crate::device_documents::ServiceDocuments;
use crate::service::local_caller;
use crate::workspaces::MemberDevice;
use crate::workspaces::fixtures::{TestGroupKeys, declare_app, governor_with, test_host_holding};

/// The words of a window reading the history: each step as the verb it ran.
struct Words;

impl ResolveAuthor for Words {
    fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
        HistoryAuthor::new(actor_hex.to_owned(), 0.0, actor_hex)
    }
}

impl HistoryWords for Words {
    fn sentence(&self, entry: &ActionContext) -> String {
        format!("{:?}", entry.what)
    }
}

const APP: &str = "so.tilo.tasks";
const LISTS: &str = "tl.so.tilo.tasks.Tasks";
const TASKS: &str = "tl.so.tilo.tasks.Task";

/// The app's types and verbs, as a device that runs it declares them.
const SURFACE: &str = r#"format = 2

[[resources]]
kind = "tl.so.tilo.tasks.Tasks"
label = "Tasks"
version = 1
fields = [
  { path = "title", type = "text" },
]
verbs = [
  { id = "new_task", label = "New task", kind = "undoable" },
]

[[resources]]
kind = "tl.so.tilo.tasks.Task"
label = "Task"
version = 1
fields = [
  { path = "title", type = "text" },
]
verbs = [
  { id = "new_task", label = "New task", kind = "undoable" },
]
"#;

fn written(kind: &str, id: &str, title: &str) -> Command {
    Command::Put {
        kind: kind.to_owned(),
        id: id.to_owned(),
        resource: Resource {
            readers: "*".to_owned(),
            parent: None,
            order: None,
            fields: BTreeMap::from([(
                "title".to_owned(),
                tilo_resource_store::Cell::One(tilo_resource_store::Value::Text(title.to_owned())),
            )]),
        },
        stamp: Stamp {
            verb: "new_task".to_owned(),
            ..Stamp::default()
        },
    }
}

fn context(workspace: &str, cert: &DeviceCert) -> ResourceRequestContext {
    ResourceRequestContext::authenticated(
        APP,
        workspace,
        local_caller(cert).in_workspace(workspace),
    )
}

/// The document the store placed `id` of `kind` in.
async fn held_document(
    workspaces: &crate::workspaces::Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    kind: &str,
    id: &str,
) -> String {
    let reply = workspaces
        .resource_command(
            &context(workspace, cert),
            Command::Get {
                kind: kind.to_owned(),
                id: id.to_owned(),
            },
        )
        .await
        .unwrap();
    match reply {
        Reply::Resource(Some(held)) => held.doc,
        other => panic!("{kind}/{id} is in the store: {other:?}"),
    }
}

/// An action writing two documents leaves both of them reading whole on the service's copy,
/// and a watch reads each change it made.
#[tokio::test]
async fn an_actions_resource_documents_read_with_their_changes() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (key, cert) = governor_with(DeviceCapabilities::member());
    let server = test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = server.workspaces().clone();
    declare_app(&server, APP, SURFACE.as_bytes());
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key),
    }));
    let created = workspaces.create("Tasks").await.unwrap();
    let workspace = created.workspace;
    let feed = DocumentsFeed::new(Arc::new(Words));
    let reads = ServiceDocuments::of(workspaces.clone());
    let caller = local_caller(&cert).in_workspace(workspace.clone());

    // Two kinds, so the two resources are placed in two documents.
    let make = || {
        Command::Batch(vec![
            written(LISTS, "list-1", "first"),
            written(TASKS, "task-1", "first"),
        ])
    };
    workspaces
        .resource_command(&context(&workspace, &cert), make())
        .await
        .unwrap();

    let (list, task) = (
        held_document(&workspaces, &workspace, &cert, LISTS, "list-1").await,
        held_document(&workspaces, &workspace, &cert, TASKS, "task-1").await,
    );
    assert_ne!(list, task, "the action writes two documents");
    for document in [&list, &task] {
        let read = reads
            .read_awaited(&workspace, (APP, document), &[])
            .await
            .unwrap();
        match read {
            DocumentRead::Whole { heads, .. } => {
                assert!(!heads.is_empty(), "{document} reads with its change");
            }
            other => panic!("{document} reads whole: {other:?}"),
        }
    }

    // The watch reads from now: what was written before it is no news.
    let primed = feed.page_of_service(&reads, &caller, None).await.unwrap();

    // One action changing both documents: each write is a part of one commit.
    workspaces
        .resource_command(
            &context(&workspace, &cert),
            Command::Batch(vec![
                written(LISTS, "list-1", "second"),
                written(TASKS, "task-1", "second"),
            ]),
        )
        .await
        .unwrap();

    for document in [&list, &task] {
        let read = reads
            .read_awaited(&workspace, (APP, document), &[])
            .await
            .unwrap();
        match read {
            DocumentRead::Whole { heads, .. } => {
                assert!(
                    !heads.is_empty(),
                    "{document} reads with the action's change"
                );
            }
            other => panic!("{document} reads whole: {other:?}"),
        }
    }
    let page = feed
        .page_of_service(&reads, &caller, Some(&primed.cursor))
        .await
        .unwrap();
    // The watch reads what the action did, on the documents it wrote: the entry names the
    // document it is of, and the verb and `happened` the store read from the change.
    let told: Vec<_> = page
        .changes
        .iter()
        .filter(|change| change.record == list || change.record == task)
        .collect();
    assert!(
        !told.is_empty(),
        "the watch reads what the action changed: {:#?}",
        page.changes
    );
    assert!(
        told.iter().any(|change| change.entry["happened"] == "edited"
            && change.entry["verb"] == "new_task"),
        "the watch reads what the action did there: {told:#?}"
    );
}
