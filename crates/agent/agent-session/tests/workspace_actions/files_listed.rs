//! An agent that lists what a workspace holds sees its files: the workspace's own listing
//! offers its folder, and beneath it each file by its path, with Files' `read-text` on it. A
//! text file is kept one way, as its content in the tree like any file, so the row's read and a
//! write by its path both reach that one place.

use super::by_path::{asked, asked_of, uploaded};
use super::*;
use fs_protocol::tree::{FILE_RESOURCE_PREFIX, FILES_APP, LIST, READ_TEXT};
use tilo_actions::remote::{
    ActionProvider as _, Caller, CataloguePage, CatalogueQuery, CatalogueRow, FOUND_FILE,
};
use tilo_server::workspace_agents::WorkspaceActionProvider;

/// The folder the plan's notes are kept in.
const FOLDER: &str = "Plans/Launch";

/// What the notes say first.
const NOTES: &str = "# Launch\n\nOn Friday, in the hall.\n";

/// The page of `workspace`'s own listing `parent` names (the top level for none), as `caller`
/// is shown it.
async fn listed(
    provider: &WorkspaceActionProvider,
    caller: &Caller,
    workspace: &str,
    parent: Option<&str>,
) -> CataloguePage {
    let query = CatalogueQuery {
        provider: Some(tilo_actions::remote::workspace_provider(workspace)),
        parent_resource: parent.map(str::to_owned),
        max_results: 200,
        ..CatalogueQuery::default()
    };
    let (provider, caller) = (provider.clone(), caller.clone());
    tokio::task::spawn_blocking(move || {
        provider
            .query(&caller, query)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("the listing answers")
            .expect("the listing is offered")
    })
    .await
    .unwrap()
}

/// The row of `page` for the resource labelled `label`.
fn row_of<'a>(page: &'a CataloguePage, label: &str) -> Option<&'a CatalogueRow> {
    page.rows.iter().find(|row| row.resource.label == label)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_lists_the_workspace_and_reads_a_document_file_by_the_row_it_was_shown() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (server, actions) = tilo_server::agents::start_serving_documents(
        crate::harness::hosted::on_this_machine(state.path()),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: device.key.clone(),
            device_cert: device.cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
        },
        Some(std::sync::Arc::new(crate::common::catalogue)),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let id = tilo_server_protocol::WorkspaceId::parse(&workspace).unwrap();
    let provider = WorkspaceActionProvider::new(actions.clone(), id);
    let person = Caller::authenticated(
        root.verifying_key().to_bytes(),
        device.cert.device_public_key,
    )
    .in_workspace(&workspace);

    // A Markdown document goes into the workspace's folder.
    let content = uploaded(&actions, id, &person, NOTES.as_bytes()).await;
    asked(
        &provider,
        &person,
        &workspace,
        "open",
        json!({"content": content, "name": "notes.md", "dir": FOLDER}),
    )
    .await
    .unwrap();
    let path = format!("{FOLDER}/notes.md");

    // The top level of the listing offers the folder, with what it holds beneath it.
    let top = listed(&provider, &person, &workspace, None).await;
    let folder = top
        .rows
        .iter()
        .find(|row| {
            row.action
                .as_ref()
                .is_some_and(|action| action.address.action == LIST)
        })
        .expect("the listing offers the workspace's folder");
    assert_eq!(folder.resource.id, format!("app.{FILES_APP}"));
    assert!(folder.has_children, "the folder holds the files");
    assert!(
        !top.rows.iter().any(|row| row.resource.label == path),
        "the files are beneath the folder, not beside it"
    );

    // Beneath it, the document is listed by its path as a file, with its read on it.
    let files = listed(&provider, &person, &workspace, Some(&folder.resource.id)).await;
    let row = row_of(&files, &path).unwrap_or_else(|| panic!("{path} is listed: {files:?}"));
    assert_eq!(row.resource.kind, FOUND_FILE);
    assert_eq!(row.resource.id, format!("{FILE_RESOURCE_PREFIX}{path}"));
    assert_eq!(row.parent_resource, folder.resource.id);
    let target = row.target().expect("the row offers its read");
    assert_eq!(target.address.action, READ_TEXT);

    // The row's own action reads it, with no path besides.
    let read = asked_of(&provider, &person, target.clone(), json!({}))
        .await
        .unwrap();
    assert_eq!(read["text"], NOTES, "{read}");
    assert_eq!(read["file"], path, "{read}");

    // An edit by its path is a new content of the one file: the row reads it, and the tree's
    // node names that content, as it names every file's.
    let edited = "# Launch\n\nMoved to Monday.\n";
    asked(
        &provider,
        &person,
        &workspace,
        "write-file",
        json!({"folder": FOLDER, "name": "notes.md", "text": edited, "replace": true}),
    )
    .await
    .unwrap();
    let read = asked_of(&provider, &person, target, json!({}))
        .await
        .unwrap();
    assert_eq!(read["text"], edited, "{read}");
    let tree = super::tree_kept(&workspaces, &workspace).await;
    let (_, node) = tree
        .nodes
        .iter()
        .find(|(_, node)| node.name.as_bytes() == b"notes.md")
        .expect("the tree holds the file");
    assert_eq!(node.size, edited.len() as u64);
    assert!(node.hash.is_some(), "a document file is its content");
    let files = listed(&provider, &person, &workspace, Some(&folder.resource.id)).await;
    assert_eq!(
        files
            .rows
            .iter()
            .filter(|row| row.resource.label == path)
            .count(),
        1,
        "one file, one row: {files:?}"
    );

    // Only a file is a file's resource: a read named on one the workspace has none of is
    // refused, and nothing is made.
    let missing = asked_of(
        &provider,
        &person,
        tilo_actions::remote::RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(&workspace),
            address: tilo_actions::api::ActionAddress {
                resource: format!("{FILE_RESOURCE_PREFIX}{FOLDER}/gone.md"),
                action: READ_TEXT.to_owned(),
            },
        },
        json!({}),
    )
    .await
    .unwrap_err();
    assert!(missing.contains("gone.md"), "{missing}");
}
