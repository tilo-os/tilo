//! A page's Files founds the workspace's tree document before the workspace sends it, and the
//! workspace founds the same document when an agent puts the first file in it: on any new
//! workspace, both foundings are one document's identity, so the page takes the workspace's
//! document in and lists the file.
//!
//! A workspace that sends a device a document only once it opens it answers a page's open of the
//! tree with "it holds none": that is the page's Files window on a fresh workspace, and it
//! starts the document. Later the agent's device, holding none of the tree either, starts it
//! again with the file in it. Each of them makes the same genesis — the identity every founder
//! derives — and their content follows; a founder that made a genesis of its own left a document
//! of conflicting metadata, which reads as no identity at all and lists nothing.

use super::hosted_documents::{content_in, put, tool};
use super::*;
use fs_protocol::tree::document_id as tree_document;
use fs_protocol::tree::{FILES_APP, TREE_KIND, Tree, TreeChunks, chunk_document_id};
use serde_json::json;
use tilo_mcp::McpServer;
use tilo_session_actions::SessionActions;
use ui::permissions::MemberId;
use ui::resource::ResourceOwners;
use ui::resource::workspace::WorkspaceResources;

/// The file the agent puts into the workspace's folder.
const FILE: &str = "agent-notes.md";

/// The file at `dir/name` in `tree`, as its node; `None` when it is not there.
fn file_at<'a>(tree: &'a Tree, dir: &str, name: &str) -> Option<&'a fs_protocol::tree::TreeEntry> {
    let mut parent = fs_protocol::tree::ROOT_ID;
    for folder in dir.split('/').filter(|folder| !folder.is_empty()) {
        let (id, node) = tree.children(parent).into_iter().find(|(_, node)| {
            node.kind == fs_protocol::files::FileNodeKind::Directory && node.name == folder
        })?;
        assert_eq!(
            node.kind,
            fs_protocol::files::FileNodeKind::Directory,
            "{folder} is a folder"
        );
        parent = id;
    }
    tree.children(parent)
        .into_iter()
        .find(|(_, node)| node.name == name)
        .map(|(_, node)| node)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_that_founded_files_lists_a_file_the_workspace_founded_after_it() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let serving = enrol(&root, "tilo.sh", DeviceCapabilities::member());
    let computer = enrol(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &root,
        &serving,
        state.path(),
        Options::offering(crate::common::catalogue),
    )
    .await;
    let agent = Browser::open(&hosted, &computer, &serving).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::for_person(
        SessionActions::new(&agent.peer, agent.workspace, serving.cert.device_public_key),
        WORKSPACE.into(),
        &person,
    );

    // Ada's page opens Files before the workspace holds a tree: the page's Files opens the
    // document, finds none, and founds it with the identity every founder derives.
    let owners = ResourceOwners::portable(
        ui::portable_storage::PortableStorage::new("files-founded-page", "page-device")
            .expect("a page keeps its documents"),
    );
    let resources =
        WorkspaceResources::in_workspace(owners, WORKSPACE.into(), MemberId::person("ada"));
    let open = |document: &str| {
        ui::framework::WorkspaceResource::<fs_protocol::tree::FilesTree>::open_at(
            &resources,
            FILES_APP,
            document,
            (FILES_APP, "Files"),
            None,
            None,
        )
        .unwrap_or_else(|| panic!("the page's Files opens {document}"))
    };
    // What a page's replication names when the workspace's document reaches it.
    let checkout = |document: &str| {
        ui::resource::DocumentCheckout::new(
            ui::resource::DocumentId::parse(document).expect("a document id"),
            ui::resource::ResourceType::new(TREE_KIND),
        )
    };
    let store = resources
        .store_of(FILES_APP)
        .expect("the page keeps its store");
    let index = open(&tree_document());

    // The agent puts a file in the workspace's folder over MCP. The workspace held no tree, so
    // its serving device founds the document too, with the file in it.
    let content =
        content_in(&put(&server, b"# Notes from your agent\n\nThe spring budget.\n").await);
    tool(
        &server,
        "tilo_do",
        json!({"path": "files#open", "args": {"content": content, "name": FILE}}),
    )
    .await;
    let workspaces = hosted.server.workspaces().unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let kept = loop {
        if let Some(kept) = workspaces
            .resource_document(WORKSPACE, FILES_APP, &tree_document())
            .await
            .unwrap()
        {
            break kept;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace never kept the tree the agent wrote"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // The workspace's document reaches the page as any change of it does: the page takes it in
    // beside what it founded.
    store
        .merge_peer_document(&checkout(&tree_document()), kept.clone())
        .expect("the page takes the workspace's tree in");
    let listed = automerge::AutoCommit::load(&kept).expect("the tree's own document reads");
    for chunk in fs_protocol::tree::listed_chunks(&listed) {
        // The page opens the chunk the tree lists, and takes the workspace's in.
        let _ = open(&chunk_document_id(chunk));
        let kept = workspaces
            .resource_document(WORKSPACE, FILES_APP, &chunk_document_id(chunk))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("the workspace keeps chunk {chunk}"));
        store
            .merge_peer_document(&checkout(&chunk_document_id(chunk)), kept)
            .expect("the page takes the workspace's chunk in");
    }

    // Files lists what the documents hold: the agent's file, from the workspace's document.
    let mut chunks = TreeChunks::default();
    index
        .read(|doc| chunks.read_index(doc))
        .expect("the tree's own document reads on the page");
    for chunk in chunks.listed() {
        let chunk_document = open(&chunk_document_id(chunk));
        chunk_document
            .read(|doc| chunks.read_chunk(chunk, doc))
            .expect("the page holds the chunk");
    }
    let tree = chunks.tree();
    assert!(
        file_at(&tree, "", FILE).is_some(),
        "the page's Files lists the agent's file: {tree:?}"
    );
    // The page's document names the tree it is, as the workspace's does: one, not two.
    assert!(
        index
            .read(collab_types::document::identity::identity)
            .is_some(),
        "the page's tree still names its identity ({TREE_KIND})"
    );
}
