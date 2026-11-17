//! Files put into a hosted workspace land in the workspace's own file tree:
//! `files#open content=… name=… dir=…`, the one way the API, `tilo files put`,
//! an agent and a person's page put a file there.
//!
//! The workspace has one folder — its Files app record, the tree its members
//! mount — and a put writes a node into it: a folder `dir` that is not there
//! yet is made on the way, a file of the same name takes the new content, and
//! no app's record is made of the file. What opens it afterwards is the
//! person's choice, from the file.

#![allow(
    clippy::string_slice,
    reason = "an answer's ASCII prefix; a slip is the failure it reports"
)]

use super::hosted_documents::{content_in, form_pdf, put, tool};
use super::*;
use base64::Engine as _;
use collab_types::DocumentMutation;
use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{FILES_APP, ROOT_ID, Tree};
use serde_json::json;
use tilo_fs::tree_document::record as tree_document;
use tilo_mcp::{McpServer, Surface};
use tilo_session_client::remote::SessionActions;

/// The tree the workspace's host keeps, as the records its members read: the
/// tree's own record and each chunk it lists.
async fn tree_of(hosted: &crate::harness::hosted::Hosted, workspace: &str) -> Tree {
    let workspaces = hosted.relay.workspaces().unwrap();
    let index = workspaces
        .resource_document(workspace, FILES_APP, &tree_document())
        .await
        .unwrap()
        .expect("the host keeps the workspace's tree");
    let index = automerge::AutoCommit::load(&index).expect("the tree's own record reads");
    let mut tree = fs_protocol::TreeChunks::default();
    tree.read_index(&index);
    for chunk in fs_protocol::tree::listed_chunks(&index) {
        let record = workspaces
            .resource_document(
                workspace,
                FILES_APP,
                &fs_protocol::tree::chunk_document_id(chunk),
            )
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("the host keeps chunk {chunk} of the tree"));
        let doc = automerge::AutoCommit::load(&record).expect("a chunk of the tree reads");
        tree.read_chunk(chunk, &doc);
    }
    tree.tree()
}

/// Write every record of `folder` to the workspace as a peer writes a tree: a
/// record nobody holds yet is founded with the empty genesis and then carries
/// its changes, each to its own record. The chunks go first, because the
/// tree's own record names them.
async fn publish(laptop: &Browser, folder: &mut tilo_fs::Namespace) {
    let mut writes = folder.document_writes(&BTreeMap::new());
    writes.sort_by_key(|write| std::cmp::Reverse(write.chunk));
    for write in writes {
        let route = DocumentRoute {
            app_id: FILES_APP.to_owned(),
            cluster_id: laptop.cluster_id.clone(),
            record: fs_protocol::tree::chunk_document_id(write.chunk),
        };
        laptop
            .peer
            .write(
                route.clone(),
                &DocumentMutation::Snapshot(automerge::AutoCommit::new().save()),
            )
            .unwrap();
        laptop
            .peer
            .write(route, &DocumentMutation::Incremental(write.changes))
            .unwrap();
    }
    laptop.durable().await;
}

/// The host's tree once it reads as `ready` says, or as it stands after ten seconds. An
/// action answers once the host holds its writes, or after a grace with the writes still on
/// their way, so a read just after it may come early.
async fn tree_when(
    hosted: &crate::harness::hosted::Hosted,
    workspace: &str,
    ready: impl Fn(&Tree) -> bool,
) -> Tree {
    let started = std::time::Instant::now();
    loop {
        let tree = tree_of(hosted, workspace).await;
        if ready(&tree) || started.elapsed() > std::time::Duration::from_secs(10) {
            return tree;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// What a tree names a file of `bytes` by: the BLAKE3 address of its content
/// manifest, whose chunks the workspace keeps beside it.
fn manifest_of(bytes: &[u8]) -> String {
    let (manifest, _) = fs_content::build(bytes, None);
    blake3::hash(&manifest.encode()).to_hex().to_string()
}

/// The file at `dir/name` in `tree`, as its node; `None` when it is not there.
fn file_at<'a>(tree: &'a Tree, dir: &str, name: &str) -> Option<&'a fs_protocol::tree::TreeEntry> {
    let mut parent = ROOT_ID;
    for folder in dir.split('/').filter(|folder| !folder.is_empty()) {
        let (id, node) = tree.children(parent).into_iter().find(|(_, node)| {
            node.kind == FileNodeKind::Directory && node.name.as_bytes() == folder.as_bytes()
        })?;
        assert_eq!(node.kind, FileNodeKind::Directory, "{folder} is a folder");
        parent = id;
    }
    tree.children(parent)
        .into_iter()
        .find(|(_, node)| node.name.as_bytes() == name.as_bytes())
        .map(|(_, node)| node)
}

/// The bytes the workspace holds under `hash`, read back through the session.
async fn content_bytes(server: &McpServer<SessionActions>, hash: &str) -> Vec<u8> {
    use base64::Engine as _;
    let answer = tool(
        server,
        "tilo_get",
        json!({"content": format!("tilo.content:{hash}"), "offset": 0}),
    )
    .await;
    // `tilo_get` answers a piece as text, its bytes base64 beside the rest.
    let start = answer
        .find("\"bytes\":\"")
        .unwrap_or_else(|| panic!("no bytes in: {answer}"))
        + "\"bytes\":\"".len();
    let encoded = &answer[start..];
    let encoded = &encoded[..encoded
        .find('"')
        .unwrap_or_else(|| panic!("no end: {answer}"))];
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("base64")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_put_file_is_a_node_of_the_workspace_tree_and_no_app_record() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo.sh", DeviceCapabilities::member());
    let computer = enrol(&root, "uploading agent", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        Options::offering(crate::common::catalogue, ModelChoice::Scripted),
    )
    .await;
    let agent = Browser::open(&hosted, &computer, &host).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    )
    .with_surface(Surface::Agent);

    // A PDF at the workspace's root: the node names its content, and the
    // workspace holds the bytes the node names.
    let form = form_pdf("Application for Schengen Visa");
    let content = content_in(&put(&server, &form).await);
    let hash = content.trim_start_matches("tilo.content:").to_owned();
    tool(
        &server,
        "tilo_do",
        json!({"path": "files#open", "args": {"content": content, "name": "visa.pdf"}}),
    )
    .await;
    let tree = tree_when(&hosted, WORKSPACE, |tree| file_at(tree, "", "visa.pdf").is_some()).await;
    let node = file_at(&tree, "", "visa.pdf").expect("the file is in the workspace's tree");
    assert_eq!(node.kind, FileNodeKind::File);
    assert_eq!(
        node.hash.as_deref(),
        Some(manifest_of(&form).as_str()),
        "the node names its content, as a mounted computer names a file"
    );
    assert_eq!(node.size, form.len() as u64);
    assert_eq!(
        content_bytes(&server, &hash).await,
        form,
        "the member reads the file back"
    );

    // A folder path keeps the folder: a file put at `forms/2026` is what a
    // mounted copy shows as `forms/2026/permit.pdf`.
    let permit = form_pdf("Application for Residence Permit");
    let permit_content = content_in(&put(&server, &permit).await);
    tool(
        &server,
        "tilo_do",
        json!({"path": "files#open",
            "args": {"content": permit_content, "name": "permit.pdf", "dir": "forms/2026"}}),
    )
    .await;
    let tree = tree_when(&hosted, WORKSPACE, |tree| {
        file_at(tree, "forms/2026", "permit.pdf").is_some()
    })
    .await;
    let node = file_at(&tree, "forms/2026", "permit.pdf").expect("the file is in its folder");
    assert_eq!(node.hash.as_deref(), Some(manifest_of(&permit).as_str()));
    assert!(
        file_at(&tree, "", "permit.pdf").is_none(),
        "it is not at the root"
    );

    // The same name put again takes the new content, as a copy over a file
    // does, rather than making a second node beside it.
    let cover = form_pdf("Cover letter");
    let cover_content = content_in(&put(&server, &cover).await);
    tool(
        &server,
        "tilo_do",
        json!({"path": "files#open",
            "args": {"content": cover_content, "name": "visa.pdf"}}),
    )
    .await;
    let tree = tree_when(&hosted, WORKSPACE, |tree| {
        file_at(tree, "", "visa.pdf").and_then(|node| node.hash.as_deref())
            == Some(manifest_of(&cover).as_str())
    })
    .await;
    let node = file_at(&tree, "", "visa.pdf").expect("still one file of that name");
    assert_eq!(
        node.hash.as_deref(),
        Some(manifest_of(&cover).as_str()),
        "the content is the new one"
    );
    let roots = tree
        .children(ROOT_ID)
        .into_iter()
        .filter(|(_, node)| node.name.as_bytes() == b"visa.pdf")
        .count();
    assert_eq!(roots, 1, "the file is not kept beside itself");

    // No app's record was made of any of them: the workspace holds the tree
    // and whatever its members opened, not a record per file.
    let workspaces = hosted.relay.workspaces().unwrap();
    let records = workspaces.records(WORKSPACE).await.unwrap_or_default();
    let made: Vec<&(String, String)> = records
        .iter()
        .filter(|(app, _)| app.as_str() != FILES_APP)
        .collect();
    assert!(
        made.iter().all(|(app, _)| !app.contains("pdf-viewer")),
        "no PDF Viewer record holds a put file: {made:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resumed_upload_of_a_large_file_lands_once_and_whole() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo.sh", DeviceCapabilities::member());
    let computer = enrol(&root, "uploading agent", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        Options::offering(crate::common::catalogue, ModelChoice::Scripted),
    )
    .await;
    let agent = Browser::open(&hosted, &computer, &host).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    )
    .with_surface(Surface::Agent);

    // A file of many pieces, its upload interrupted after the first ones and
    // picked up where the host says it stands.
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend((0..300_000_u32).map(|n| u8::try_from(n % 251).unwrap_or_default()));
    let begun = tool(&server, "tilo_put_begin", json!({"size": bytes.len()})).await;
    let begun: serde_json::Value = serde_json::from_str(&begun).expect("an upload");
    let upload = begun["upload"].as_u64().expect("an upload id");
    let half = bytes.len() / 2;
    let first = base64::engine::general_purpose::STANDARD.encode(&bytes[..half]);
    tool(
        &server,
        "tilo_put_append",
        json!({"upload": upload, "offset": 0, "chunk": first}),
    )
    .await;
    let rest = base64::engine::general_purpose::STANDARD.encode(&bytes[half..]);
    tool(
        &server,
        "tilo_put_append",
        json!({"upload": upload, "offset": half, "chunk": rest}),
    )
    .await;
    let content = content_in(&tool(&server, "tilo_put_finish", json!({"upload": upload})).await);
    let hash = content.trim_start_matches("tilo.content:").to_owned();
    tool(
        &server,
        "tilo_do",
        json!({"path": "files#open",
            "args": {"content": content, "name": "scan.pdf", "dir": "scans"}}),
    )
    .await;

    let tree = tree_when(&hosted, WORKSPACE, |tree| {
        file_at(tree, "scans", "scan.pdf").is_some()
    })
    .await;
    let node = file_at(&tree, "scans", "scan.pdf").expect("the large file is in the tree");
    assert_eq!(node.hash.as_deref(), Some(manifest_of(&bytes).as_str()));
    assert_eq!(node.size, bytes.len() as u64);
    assert_eq!(
        content_bytes(&server, &hash).await,
        bytes,
        "every piece arrived, in order"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_put_file_makes_no_app_record_where_opening_it_in_an_app_does() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo.sh", DeviceCapabilities::member());
    let computer = enrol(&root, "uploading agent", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        Options::offering(crate::common::catalogue, ModelChoice::Scripted),
    )
    .await;
    let agent = Browser::open(&hosted, &computer, &host).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    )
    .with_surface(Surface::Agent);

    // The same bytes put into the workspace make no app's record: the
    // workspace keeps the file, and the person decides what opens it.
    let content = content_in(&put(&server, &form_pdf("Filed form")).await);
    tool(
        &server,
        "tilo_do",
        json!({"path": "files#open", "args": {"content": content, "name": "filed.pdf"}}),
    )
    .await;
    let workspaces = hosted.relay.workspaces().unwrap();
    let records = workspaces.records(WORKSPACE).await.unwrap_or_default();
    assert!(
        records.iter().all(|(app, _)| app != "so.tilo.pdf-viewer"),
        "a put file makes no PDF Viewer record: {records:?}"
    );

    // Opened in the app itself, the same bytes do make its record: Files is
    // where a file goes, an app's record is what the person opens it in.
    let content = content_in(&put(&server, &form_pdf("Opened form")).await);
    let opened = tool(
        &server,
        "tilo_do",
        json!({"path": "pdf-viewer#open", "args": {"content": content, "name": "opened.pdf"}}),
    )
    .await;
    assert!(opened.contains("opened in pdf-viewer"), "{opened}");
}

/// A file put into a workspace whose tree a computer of the person's wrote
/// first (its folder, mounted as `tilo login` mounts it) goes into that tree:
/// the host waits for the tree to open with its keys rather than refusing
/// the put as a tree whose keys it does not hold yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_put_file_goes_into_the_tree_a_computer_wrote_first() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo.sh", DeviceCapabilities::member());
    let computer = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        Options::offering(crate::common::catalogue, ModelChoice::Scripted),
    )
    .await;
    let laptop = Browser::open(&hosted, &computer, &host).await;

    // The laptop's folder: a file copied in, the tree written as the peer
    // writes it — each record of it founded with the empty genesis and then
    // its changes, the chunk first because the tree's own record names it —
    // sealed as every write of the workspace is.
    let mut folder = tilo_fs::Namespace::new(hex::encode(computer.cert.device_public_key)).unwrap();
    folder
        .create_file(tilo_fs::ROOT_INO, "contract.txt", None, 0, 1)
        .unwrap();
    publish(&laptop, &mut folder).await;
    assert!(
        file_at(
            &tree_when(&hosted, WORKSPACE, |tree| file_at(tree, "", "contract.txt").is_some())
                .await,
            "",
            "contract.txt"
        )
        .is_some(),
        "the host keeps the laptop's tree"
    );

    // The folder is one chunk, named by the tree's own record, and the host
    // keeps that chunk beside it: a reader takes the folder record by record.
    let workspaces = hosted.relay.workspaces().unwrap();
    let index = workspaces
        .resource_document(WORKSPACE, FILES_APP, &tree_document())
        .await
        .unwrap()
        .expect("the host keeps the tree's own record");
    let index = automerge::AutoCommit::load(&index).expect("the tree's own record reads");
    assert_eq!(
        fs_protocol::tree::listed_chunks(&index),
        [1],
        "the file's chunk is named by the tree's own record"
    );
    assert!(
        workspaces
            .resource_document(WORKSPACE, FILES_APP, &fs_protocol::tree::chunk_document_id(1))
            .await
            .unwrap()
            .is_some(),
        "the host keeps the chunk the laptop's file is in"
    );

    // An agent puts a file: it lands beside the laptop's.
    let agent = Browser::open(
        &hosted,
        &enrol(&root, "agent", DeviceCapabilities::member()),
        &host,
    )
    .await;
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        hex::encode(root.verifying_key().to_bytes()),
    )
    .with_surface(Surface::Agent);
    let notice = form_pdf("Notice of termination");
    let content = content_in(&put(&server, &notice).await);
    let said = tool(
        &server,
        "tilo_do",
        json!({"path": "files#open", "args": {"content": content, "name": "notice.pdf"}}),
    )
    .await;
    let tree = tree_when(&hosted, WORKSPACE, |tree| file_at(tree, "", "notice.pdf").is_some()).await;
    assert!(file_at(&tree, "", "notice.pdf").is_some(), "{said}");
    assert!(
        file_at(&tree, "", "contract.txt").is_some(),
        "the laptop's file is still there"
    );
}
