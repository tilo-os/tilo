//! An agent reads the person's Files window: the folders the workspace's tree holds, and an
//! empty workspace as the empty folder the person sees — never a skeleton of a tree on its
//! way. The window's own document says which folder it shows; the tree is a document of its
//! own, which the window opens beside it.
use super::agent_mcp::tool;
use super::*;
use serde_json::json;
use tilo_agent_session::ModelChoice;
use tilo_mcp::McpServer;
use tilo_session_actions::SessionActions;

/// A folder the person keeps in the workspace.
const FOLDER: &str = "Launch plan";

/// The app whose window an agent reads.
const FILES: &str = fs_protocol::tree::FILES_APP;

/// The lines the Files window of `server`'s workspace shows an agent, asked as the person's
/// own agent asks it.
async fn view(server: &McpServer<SessionActions>) -> String {
    let (lines, failed) = tool(server, "tilo_view", json!({"path": "file-explorer"})).await;
    assert!(!failed, "{lines}");
    lines
}

/// The person's Files founds the workspace's tree with the folder `name` in it, and publishes
/// it as their page does: the tree's own document and the chunk the folder went into.
fn person_keeps_a_folder(page: &Browser, name: &str, author: &str) {
    let owners = ui::resource::ResourceOwners::portable(
        ui::portable_storage::PortableStorage::new("files-window-view", "page-device")
            .expect("a page keeps its documents"),
    );
    let resources = ui::resource::workspace::WorkspaceResources::in_workspace(
        owners,
        WORKSPACE.into(),
        MemberId::person("ada"),
    );
    let open = |document: &str| {
        ui::framework::WorkspaceResource::<fs_protocol::tree::FilesTree>::open_at(
            &resources,
            FILES,
            document,
            (FILES, "Files"),
            None,
            None,
        )
        .expect("the window opens the tree's resources")
    };
    let chunk = open(&fs_protocol::tree::chunk_document_id(1));
    let index = open(&fs_protocol::tree::document_id());
    let mut tree = fs_protocol::tree::Tree::default();
    let mut chunk_doc = chunk.read(Clone::clone).expect("the chunk reads");
    fs_protocol::tree::place(
        (&mut chunk_doc, 1),
        &mut tree,
        fs_protocol::tree::ROOT_ID,
        name,
        fs_protocol::tree::Placing::Folder,
        (1_000, author),
    )
    .expect("the folder is placed");
    chunk
        .record()
        .merge_peer_document(chunk_doc.save())
        .expect("the chunk takes the folder");
    let mut index_doc = index
        .read(Clone::clone)
        .expect("the tree's own document reads");
    fs_protocol::tree::list_chunk(&mut index_doc, 1).expect("the chunk is listed");
    index
        .record()
        .merge_peer_document(index_doc.save())
        .expect("the tree takes the chunk");
    for (document, bytes) in [
        (
            fs_protocol::tree::document_id(),
            index
                .read(Clone::clone)
                .expect("the tree's own document reads")
                .save(),
        ),
        (
            fs_protocol::tree::chunk_document_id(1),
            chunk.read(Clone::clone).expect("the chunk reads").save(),
        ),
    ] {
        page.peer
            .write(
                DocumentRoute {
                    app_id: FILES.into(),
                    cluster_id: WORKSPACE.into(),
                    document,
                },
                &collab_types::DocumentMutation::Snapshot(bytes),
            )
            .expect("the person's page publishes it");
    }
}

/// A workspace whose person's page holds Files, and the agent of theirs on another computer,
/// served over that computer's session.
async fn agent_viewing_files(
    state: &std::path::Path,
) -> (
    crate::harness::hosted::Hosted,
    String,
    McpServer<SessionActions>,
    Browser,
) {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let home = enrol(&root, "tilo.sh", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let hosted = crate::harness::hosted::start(
        &root,
        &home,
        state,
        Options::offering(crate::common::catalogue, ModelChoice::Scripted),
    )
    .await;
    let mut page = Browser::open(&hosted, &browser_device, &home).await;
    let _files = page.create(FILES);
    page.put_layout();
    page.peer.barrier().unwrap().recv().unwrap().unwrap();
    let agent = Browser::open(&hosted, &computer, &home).await;
    // Its session is the agent's, as `tilo login` makes it say.
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, home.cert.device_public_key),
        WORKSPACE.into(),
        person.clone(),
    );
    (hosted, person, server, page)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_view_of_files_is_the_empty_folder_a_person_with_no_files_sees() {
    let state = crate::support::scratch_dir();
    let (_hosted, _person, server, _page) = agent_viewing_files(state.path()).await;
    let empty = view(&server).await;
    assert!(!empty.contains("Loading"), "{empty}");
    assert!(
        empty.contains("This folder is empty"),
        "an empty workspace is an empty folder, not a tree on its way: {empty}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_view_of_files_lists_the_folder_the_persons_files_hold() {
    let state = crate::support::scratch_dir();
    let (_hosted, person, server, page) = agent_viewing_files(state.path()).await;
    person_keeps_a_folder(&page, FOLDER, &person);
    page.peer.barrier().unwrap().recv().unwrap().unwrap();
    // The workspace holds the folder: what the window shows is the tree, not a skeleton.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let lines = view(&server).await;
        assert!(
            !lines.contains("Loading"),
            "the window shows the tree, never that it is on its way: {lines}"
        );
        if lines.contains(FOLDER) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{lines}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
