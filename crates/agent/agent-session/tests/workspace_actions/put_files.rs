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

use super::hosted_documents::{content_in, form_pdf, json_in, put, tool};
use super::*;
use base64::Engine as _;
use collab_types::DocumentMutation;
use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::document_id as tree_document;
use fs_protocol::tree::{FILES_APP, ROOT_ID, Tree};
use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_session_actions::SessionActions;

/// The tree the workspace's host keeps, as the records its members read: the
/// tree's own record and each chunk it lists.
async fn tree_of(hosted: &crate::harness::hosted::Hosted, workspace: &str) -> Tree {
    tree_kept(hosted.server.workspaces().unwrap(), workspace).await
}

/// [`tree_of`], read from the workspaces a server keeps.
pub(super) async fn tree_kept(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
) -> Tree {
    tree_if_kept(workspaces, workspace)
        .await
        .expect("the workspace keeps a tree")
}

/// [`tree_kept`], `None` where the workspace holds no tree: none was written yet.
pub(super) async fn tree_if_kept(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
) -> Option<Tree> {
    let index = workspaces
        .resource_document(workspace, FILES_APP, &tree_document())
        .await
        .unwrap()?;
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
    Some(tree.tree())
}

/// Write every record of `folder` to the workspace as a peer writes a tree: a
/// record nobody holds yet is founded with the empty genesis and then carries
/// its changes, each to its own record. The chunks go first, because the
/// tree's own record names them.
async fn publish(laptop: &Browser, folder: &mut tilo_fs::Namespace) {
    write_tree(laptop, folder).unwrap();
    laptop.durable().await;
}

/// [`publish`] without waiting for the server to keep it: the first write the laptop's own
/// replica refuses, if one is.
pub(super) fn write_tree(laptop: &Browser, folder: &mut tilo_fs::Namespace) -> Result<(), String> {
    let mut writes = folder.document_writes(&BTreeMap::new());
    writes.sort_by_key(|write| std::cmp::Reverse(write.chunk));
    for write in writes {
        let route = DocumentRoute {
            app_id: FILES_APP.to_owned(),
            cluster_id: laptop.cluster_id.clone(),
            document: fs_protocol::tree::chunk_document_id(write.chunk),
        };
        laptop
            .peer
            .write(
                route.clone(),
                &DocumentMutation::Snapshot(automerge::AutoCommit::new().save()),
            )
            .map_err(|error| format!("{error:?}"))?;
        laptop
            .peer
            .write(route, &DocumentMutation::Incremental(write.changes))
            .map_err(|error| format!("{error:?}"))?;
    }
    Ok(())
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
    let workspaces = hosted.server.workspaces().unwrap();
    loop {
        if let Some(tree) = tree_if_kept(workspaces, workspace).await
            && ready(&tree)
        {
            return tree;
        }
        if started.elapsed() > std::time::Duration::from_secs(10) {
            return tree_of(hosted, workspace).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// What a tree names a file of `bytes` by: the BLAKE3 address of its content
/// manifest, whose chunks the workspace keeps beside it.
pub(super) async fn opened_file_bytes(
    server: &tilo_server::daemon::Server,
    workspace: &str,
    hash: &str,
) -> Option<Vec<u8>> {
    use collab_types::{ContentRetention, EncryptionMetadata, WorkspaceId, file_content_artifact};
    let store = server.content().get(workspace).ok()?;
    let bytes = store.read(hash)?;
    let manifest = fs_content::Manifest::decode(&bytes).ok()?;
    let chunk = |id: &[u8; 32]| store.read(&hex::encode(id));
    let Some(sealed) = manifest.sealed() else {
        return fs_content::whole(bytes, manifest.total_len(), chunk);
    };
    let route = tilo_session_client::DocumentRoute {
        cluster_id: workspace.to_owned(),
        app_id: FILES_APP.to_owned(),
        document: tree_document(),
    };
    let keys = server
        .workspaces()?
        .own_route_keys(&route, false)
        .await?
        .ok()?;
    let workspace = WorkspaceId::parse(workspace)?;
    let metadata = EncryptionMetadata::content_group(
        workspace,
        WorkspaceId::from_u128(u128::from_be_bytes(sealed.group)),
        sealed.epoch,
        ContentRetention::Delta,
    );
    fs_content::whole_with(bytes, manifest.total_len(), chunk, |_, cipher| {
        keys.open_content(file_content_artifact(workspace), &metadata, cipher)
            .ok()
            .map(|text| text.to_vec())
    })
}

async fn assert_sealed_file(
    server: &tilo_server::daemon::Server,
    workspace: &str,
    hash: &str,
    expected: &[u8],
) {
    let store = server.content().get(workspace).unwrap();
    let bytes = store
        .read(hash)
        .expect("the workspace keeps the file manifest");
    let manifest = fs_content::Manifest::decode(&bytes).expect("the file manifest decodes");
    assert!(
        manifest.sealed().is_some(),
        "the document names sealed chunks"
    );
    assert_eq!(manifest.total_len(), expected.len() as u64);
    assert!(
        fs_content::whole(bytes, expected.len() as u64, |id| store
            .read(&hex::encode(id)))
        .is_none()
    );
    assert_eq!(
        opened_file_bytes(server, workspace, hash).await.as_deref(),
        Some(expected),
        "the serving device opens exactly the uploaded bytes under the Files keys",
    );
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
        Options::offering(crate::common::catalogue),
    )
    .await;
    let agent = Browser::open(&hosted, &computer, &host).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::for_person(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        &person,
    );

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
    let tree = tree_when(&hosted, WORKSPACE, |tree| {
        file_at(tree, "", "visa.pdf").is_some()
    })
    .await;
    let node = file_at(&tree, "", "visa.pdf").expect("the file is in the workspace's tree");
    assert_eq!(node.kind, FileNodeKind::File);
    let first = node
        .hash
        .as_deref()
        .expect("the file names its content")
        .to_owned();
    assert_sealed_file(&hosted.server, WORKSPACE, &first, &form).await;
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
    assert_sealed_file(
        &hosted.server,
        WORKSPACE,
        node.hash.as_deref().unwrap(),
        &permit,
    )
    .await;
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
        file_at(tree, "", "visa.pdf")
            .and_then(|node| node.hash.as_deref())
            .is_some_and(|hash| hash != first)
    })
    .await;
    let node = file_at(&tree, "", "visa.pdf").expect("still one file of that name");
    assert_sealed_file(
        &hosted.server,
        WORKSPACE,
        node.hash.as_deref().unwrap(),
        &cover,
    )
    .await;
    let roots = tree
        .children(ROOT_ID)
        .into_iter()
        .filter(|(_, node)| node.name.as_bytes() == b"visa.pdf")
        .count();
    assert_eq!(roots, 1, "the file is not kept beside itself");

    // No app's record was made of any of them: the workspace holds the tree
    // and whatever its members opened, not a record per file.
    let workspaces = hosted.server.workspaces().unwrap();
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
async fn a_folder_put_is_one_run_and_one_write_per_chunk() {
    const FILES: usize = 1200;
    const INLINE: usize = 600;
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo.sh", DeviceCapabilities::member());
    let computer = enrol(&root, "uploading agent", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &root,
        &device,
        state.path(),
        Options::offering(crate::common::catalogue),
    )
    .await;
    let agent = Browser::open(&hosted, &computer, &device).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::for_person(
        SessionActions::new(&agent.peer, agent.workspace, device.cert.device_public_key),
        WORKSPACE.into(),
        &person,
    );

    // Many small files are kept whole in one call, each answered by the reference that opens
    // it (sealed, so its address is the ciphertext's), and the workspace says which of those
    // addresses it holds.
    let first = b"first small file".to_vec();
    let second = b"second small file".to_vec();
    let files: Vec<String> = [&first, &second]
        .iter()
        .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
        .collect();
    let said = tool(&server, "tilo_put_many", json!({"files": files})).await;
    let kept: serde_json::Value = serde_json::from_str(&said).expect("contents");
    let contents = kept["contents"].as_array().expect("contents");
    assert_eq!(contents.len(), 2, "one answer for each file: {said}");
    let mut addresses = Vec::with_capacity(contents.len());
    for (bytes, content) in [&first, &second].iter().zip(contents) {
        let reference = content
            .as_str()
            .and_then(|content| content.strip_prefix("tilo.content:"))
            .unwrap_or_else(|| panic!("no content reference for the file: {said}"));
        assert_eq!(
            content_bytes(&server, reference).await,
            **bytes,
            "the reference opens the file: {said}"
        );
        // A sealed reference is its ciphertext's address and the key that opens it.
        let address = reference.split('.').next().unwrap_or(reference);
        addresses.push(address.to_owned());
    }
    let known = addresses.first().cloned().unwrap_or_default();
    let unknown = "0".repeat(64);
    let said = tool(
        &server,
        "tilo_put_have",
        json!({"hashes": [known, unknown]}),
    )
    .await;
    let held: serde_json::Value = serde_json::from_str(&said).expect("held");
    assert_eq!(held["have"], json!([true, false]), "{said}");

    // A folder large enough to fill more than one chunk of the tree, put in
    // an open whose list a small part of it carries beside the file's own
    // path, and then the rest as content of its own: every file goes in before
    // anything is written.
    let mut entries = Vec::with_capacity(FILES);
    let mut names = Vec::with_capacity(FILES);
    for index in 0..FILES {
        let bytes = format!("file {index}\n").into_bytes();
        let content = content_in(&put(&server, &bytes).await);
        let name = format!("f{index}.txt");
        names.push(name.clone());
        entries.push(json!({"content": content, "name": name, "dir": "repo/src"}));
    }
    let inline = entries.split_off(INLINE);
    let said = tool(
        &server,
        "tilo_do",
        json!({"path": "files#open", "args": {"files": entries}}),
    )
    .await;
    let list = content_in(&put(&server, &serde_json::to_vec(&inline).unwrap()).await);
    tool(
        &server,
        "tilo_do",
        json!({"path": "files#open", "args": {"files": list}}),
    )
    .await;

    let tree = tree_when(&hosted, WORKSPACE, |tree| {
        file_at(tree, "repo/src", &format!("f{}.txt", FILES - 1)).is_some()
    })
    .await;
    for name in &names {
        assert!(
            file_at(&tree, "repo/src", name).is_some(),
            "{name} is in the folder: {said}"
        );
    }

    // The records the folder changed each carry one change, not one per file:
    // the tree's own record and its chunks.
    let workspaces = hosted.server.workspaces().unwrap();
    let index = workspaces
        .resource_document(WORKSPACE, FILES_APP, &tree_document())
        .await
        .unwrap()
        .expect("the workspace keeps the tree's own record");
    let mut index = automerge::AutoCommit::load(&index).expect("the tree's own record reads");
    let chunks = fs_protocol::tree::listed_chunks(&index);
    assert!(chunks.len() > 1, "the folder fills more than one chunk");
    let mut changes = index.get_changes(&[]).len();
    for chunk in &chunks {
        let record = workspaces
            .resource_document(
                WORKSPACE,
                FILES_APP,
                &fs_protocol::tree::chunk_document_id(*chunk),
            )
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("the workspace keeps chunk {chunk} of the tree"));
        changes += automerge::AutoCommit::load(&record)
            .expect("a chunk of the tree reads")
            .get_changes(&[])
            .len();
    }
    assert!(
        changes.saturating_mul(10) < FILES,
        "the folder is a change per record per open ({} changes over {} records), not one per file",
        changes,
        chunks.len() + 1
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
        Options::offering(crate::common::catalogue),
    )
    .await;
    let agent = Browser::open(&hosted, &computer, &host).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::for_person(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        &person,
    );

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
    assert_sealed_file(
        &hosted.server,
        WORKSPACE,
        node.hash.as_deref().unwrap(),
        &bytes,
    )
    .await;
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
        Options::offering(crate::common::catalogue),
    )
    .await;
    let agent = Browser::open(&hosted, &computer, &host).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::for_person(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        &person,
    );

    // The same bytes put into the workspace make no app's record: the
    // workspace keeps the file, and the person decides what opens it.
    let content = content_in(&put(&server, &form_pdf("Filed form")).await);
    tool(
        &server,
        "tilo_do",
        json!({"path": "files#open", "args": {"content": content, "name": "filed.pdf"}}),
    )
    .await;
    let workspaces = hosted.server.workspaces().unwrap();
    let records = workspaces.records(WORKSPACE).await.unwrap_or_default();
    assert!(
        records.iter().all(|(app, _)| app != "so.tilo.pdf-viewer"),
        "a put file makes no PDF Viewer record: {records:?}"
    );

    // Opened in the app itself, the same bytes do make its record: Files is
    // where a file goes, an app's record is what the person opens it in. One
    // record, however long the app takes to take the file in: a busy machine
    // makes the app's worker slow to answer, and an open that took the record
    // it made up again from the start made another each time.
    let content = content_in(&put(&server, &form_pdf("Opened form")).await);
    let slowed = tilo_agent_session::workspace_actions::slowed::Slowed::new(
        WORKSPACE,
        "so.tilo.pdf-viewer",
        Duration::from_millis(300),
    );
    let opened = tool(
        &server,
        "tilo_do",
        json!({"path": "pdf-viewer#open", "args": {"content": content, "name": "opened.pdf"}}),
    )
    .await;
    drop(slowed);
    assert!(opened.contains("opened in pdf-viewer"), "{opened}");
    let records = workspaces.records(WORKSPACE).await.unwrap_or_default();
    // The app's own resource in the workspace (named by no id) is no record of a file.
    let viewers = records
        .iter()
        .filter(|(app, record)| app == "so.tilo.pdf-viewer" && !record.is_empty())
        .count();
    assert_eq!(
        viewers, 1,
        "one open makes one PDF Viewer record: {records:?}"
    );
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
        Options::offering(crate::common::catalogue),
    )
    .await;
    let laptop = Browser::open(&hosted, &computer, &host).await;

    // The laptop's folder: a file copied in, the tree written as the peer
    // writes it — each record of it founded with the empty genesis and then
    // its changes, the chunk first because the tree's own record names it —
    // sealed as every write of the workspace is.
    let mut folder = super::resource_support::computer_folder(&computer.cert);
    folder
        .create_file(tilo_fs::ROOT_INO, "contract.txt", None, 0, 1)
        .unwrap();
    publish(&laptop, &mut folder).await;
    assert!(
        file_at(
            &tree_when(&hosted, WORKSPACE, |tree| file_at(tree, "", "contract.txt")
                .is_some())
            .await,
            "",
            "contract.txt"
        )
        .is_some(),
        "the host keeps the laptop's tree"
    );

    // The folder is one chunk, named by the tree's own record, and the host
    // keeps that chunk beside it: a reader takes the folder record by record.
    let workspaces = hosted.server.workspaces().unwrap();
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
            .resource_document(
                WORKSPACE,
                FILES_APP,
                &fs_protocol::tree::chunk_document_id(1)
            )
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
    let server = McpServer::for_person(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        &hex::encode(root.verifying_key().to_bytes()),
    );
    let notice = form_pdf("Notice of termination");
    let content = content_in(&put(&server, &notice).await);
    let said = tool(
        &server,
        "tilo_do",
        json!({"path": "files#open", "args": {"content": content, "name": "notice.pdf"}}),
    )
    .await;
    let tree = tree_when(&hosted, WORKSPACE, |tree| {
        file_at(tree, "", "notice.pdf").is_some()
    })
    .await;
    assert!(file_at(&tree, "", "notice.pdf").is_some(), "{said}");
    assert!(
        file_at(&tree, "", "contract.txt").is_some(),
        "the laptop's file is still there"
    );
}

/// A member who edits a public workspace founds its file tree from their computer's mounted
/// folder, as Files' own new-file and new-folder are theirs to take: the file they put there
/// lands. A viewer's computer founds nothing and writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_editor_founds_a_public_workspace_tree_and_a_viewer_does_not() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Owner", 1);
    let serving = enrol(&owner, "lounge-server", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &owner,
        &serving,
        state.path(),
        Options::offering(crate::common::catalogue),
    )
    .await;
    let workspaces = hosted.server.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &serving.cert,
            tilo_server_protocol::directory::Listing {
                description: "Everyone".into(),
                default_role: tilo_peer_state::join::Role::Viewer,
            },
        )
        .await
        .unwrap();
    let workspace = lounge.workspace.clone();
    let (editor, _) = tilo_user_identity::UserIdentity::generate("Editor", 1);
    let (viewer, _) = tilo_user_identity::UserIdentity::generate("Viewer", 1);
    let editing = enrol(&editor, "editor laptop", DeviceCapabilities::member());
    let viewing = enrol(&viewer, "viewer laptop", DeviceCapabilities::member());
    for (device, role) in [
        (&editing, tilo_peer_state::join::Role::Editor),
        (&viewing, tilo_peer_state::join::Role::Viewer),
    ] {
        super::resource_support::admit_person(&hosted, &serving, &workspace, device, role).await;
    }
    let kept = || async {
        workspaces
            .resource_document(&workspace, FILES_APP, &tree_document())
            .await
            .unwrap()
    };

    // The viewer's computer mounts the workspace first: what it founds stays its own.
    let viewer_laptop =
        super::resource_support::open_admitted_peer(&hosted, &viewing, &serving, &workspace).await;
    let mut theirs = tilo_fs::Namespace::new(hex::encode(viewing.cert.device_public_key)).unwrap();
    theirs
        .create_file(tilo_fs::ROOT_INO, "notes.txt", None, 0, 1)
        .unwrap();
    let refused = write_tree(&viewer_laptop, &mut theirs);
    let settled = Instant::now() + Duration::from_secs(3);
    while Instant::now() < settled {
        assert!(
            kept().await.is_none(),
            "a viewer founded the tree ({refused:?})"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The editor's computer founds it with a file in it, and the file lands.
    let editor_laptop =
        super::resource_support::open_admitted_peer(&hosted, &editing, &serving, &workspace).await;
    let mut folder = tilo_fs::Namespace::new(hex::encode(editing.cert.device_public_key)).unwrap();
    folder
        .create_file(tilo_fs::ROOT_INO, "contract.txt", None, 0, 1)
        .unwrap();
    publish(&editor_laptop, &mut folder).await;
    let deadline = tilo_test_env::hang_deadline();
    while kept().await.is_none() {
        assert!(Instant::now() < deadline, "the editor's tree never lands");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let tree = tree_when(&hosted, &workspace, |tree| {
        file_at(tree, "", "contract.txt").is_some()
    })
    .await;
    assert!(
        file_at(&tree, "", "contract.txt").is_some(),
        "the editor's file lands"
    );
    assert!(
        file_at(&tree, "", "notes.txt").is_none(),
        "the viewer's file stays out"
    );
}

/// In a public workspace, whose server sends a device a document only once it opens it, the tree a
/// computer's mounted folder founds after the server's action service looked for it, and found
/// none, is the tree the service then reads and writes: an agent lists the computer's file and
/// makes a folder beside it. What an agent on the computer then writes through the mounted
/// folder, Files' History names as that agent's for its person, and its undo takes it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tree_a_computer_founds_after_the_service_found_none_is_read_and_written() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Owner", 1);
    let serving = enrol(&owner, "lounge-server", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &owner,
        &serving,
        state.path(),
        Options::offering(crate::common::catalogue),
    )
    .await;
    let workspaces = hosted.server.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &serving.cert,
            tilo_server_protocol::directory::Listing {
                description: "Everyone".into(),
                default_role: tilo_peer_state::join::Role::Editor,
            },
        )
        .await
        .unwrap();
    let workspace = lounge.workspace.clone();
    let computer = enrol(
        &owner,
        "Claude Code on laptop",
        DeviceCapabilities::member(),
    );
    let laptop =
        super::resource_support::open_admitted_peer(&hosted, &computer, &serving, &workspace).await;
    let server = McpServer::for_person(
        SessionActions::new(
            &laptop.peer,
            laptop.workspace,
            serving.cert.device_public_key,
        ),
        workspace.clone(),
        &hex::encode(owner.verifying_key().to_bytes()),
    );

    // The service looks at the workspace's files before any folder wrote them: it finds none.
    let before = tool(&server, "tilo_ls", json!({"path": "files"})).await;
    assert!(!before.contains("contract.txt"), "{before}");

    // The laptop's mounted folder founds the tree with a file in it.
    let mut folder = super::resource_support::computer_folder(&computer.cert);
    folder
        .create_file(tilo_fs::ROOT_INO, "contract.txt", None, 0, 1)
        .unwrap();
    publish(&laptop, &mut folder).await;
    assert!(
        file_at(
            &tree_when(&hosted, &workspace, |tree| file_at(
                tree,
                "",
                "contract.txt"
            )
            .is_some())
            .await,
            "",
            "contract.txt"
        )
        .is_some(),
        "the server keeps the laptop's tree"
    );

    // The service reads the tree the laptop founded, and writes into it.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let listed = tool(&server, "tilo_ls", json!({"path": "files"})).await;
        if listed.contains("contract.txt") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the service never lists the laptop's file: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tool(
        &server,
        "tilo_do",
        json!({"path": "files#new-folder", "args": {"parent": "", "name": "Drafts"}}),
    )
    .await;
    let tree = tree_when(&hosted, &workspace, |tree| {
        file_at(tree, "", "Drafts").is_some()
    })
    .await;
    assert!(
        file_at(&tree, "", "Drafts").is_some(),
        "the agent's folder is in the tree"
    );
    assert!(
        file_at(&tree, "", "contract.txt").is_some(),
        "beside the laptop's file"
    );

    // A coding agent on the laptop makes a file through the mounted folder and then writes it,
    // for its person: two steps.
    let person = hex::encode(owner.verifying_key().to_bytes());
    let agent = tilo_actions::ActionInvoker::admitted(
        &tilo_actions::Acting::agent("claude")
            .handed()
            .in_session("0f3c"),
    )
    .named(Some("Claude Code".into()))
    .acting_for(person.as_str());
    let as_agent = || {
        tilo_fs::namespace::acting_as(Some(
            tilo_actions::ActionContext::default().with_invoker(agent.clone()),
        ))
    };
    let made = {
        let _acting = as_agent();
        folder
            .create_file(tilo_fs::ROOT_INO, "agent-notes.md", None, 0, 2)
            .unwrap()
    };
    publish(&laptop, &mut folder).await;
    {
        let _acting = as_agent();
        assert!(folder.set_content(made, None, 12, 3));
    }
    publish(&laptop, &mut folder).await;

    // Files' History names both steps by the file's path, the agent and its person.
    let deadline = tilo_test_env::hang_deadline();
    let steps = loop {
        let history = json_in(
            &tool(
                &server,
                "tilo_do",
                json!({"path": "files#history", "args": {"last": 20}}),
            )
            .await,
        );
        let steps: Vec<Value> = history["versions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|version| version["path"] == "agent-notes.md")
            .cloned()
            .collect();
        if steps.len() == 2 {
            break steps;
        }
        assert!(
            Instant::now() < deadline,
            "History never lists the agent's two steps: {history}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    for step in &steps {
        assert_eq!(step["actor"]["name"], "Claude Code", "{step}");
        assert!(
            step.to_string().contains(&person),
            "the change is the agent's for its person: {step}"
        );
    }
    let did = |step: &Value| {
        step["what"]["action"]["action"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let (making, writing) = match (did(&steps[0]).as_str(), did(&steps[1]).as_str()) {
        ("new-file", "write") => (&steps[0], &steps[1]),
        ("write", "new-file") => (&steps[1], &steps[0]),
        other => panic!("a making and a write: {other:?}"),
    };

    // Files' undo of the write and then of the making takes the file away whole, and leaves
    // the rest of the folder.
    for step in [writing, making] {
        let undone = json_in(
            &tool(
                &server,
                "tilo_do",
                json!({"path": "files#undo", "args": {"step": step["change"]}}),
            )
            .await,
        );
        assert_eq!(undone["taken"], 1, "{undone}");
    }
    let tree = tree_when(&hosted, &workspace, |tree| {
        file_at(tree, "", "agent-notes.md").is_none()
    })
    .await;
    assert!(
        file_at(&tree, "", "agent-notes.md").is_none(),
        "no empty file is left behind"
    );
    assert!(file_at(&tree, "", "contract.txt").is_some());
    assert!(file_at(&tree, "", "Drafts").is_some());
    // The laptop's mounted folder, taking what the workspace keeps, shows no such file either.
    folder
        .take_documents(kept_documents(&hosted, &workspace).await)
        .unwrap();
    assert!(folder.lookup(tilo_fs::ROOT_INO, "agent-notes.md").is_none());
    assert!(folder.lookup(tilo_fs::ROOT_INO, "contract.txt").is_some());
}

/// The tree's documents the workspace keeps, by chunk: what a mounted folder takes in.
async fn kept_documents(
    hosted: &crate::harness::hosted::Hosted,
    workspace: &str,
) -> Vec<(fs_protocol::TreeChunk, Vec<u8>)> {
    let workspaces = hosted.server.workspaces().unwrap();
    let index = workspaces
        .resource_document(workspace, FILES_APP, &tree_document())
        .await
        .unwrap()
        .expect("the workspace keeps its tree");
    let listed = fs_protocol::tree::listed_chunks(&automerge::AutoCommit::load(&index).unwrap());
    let mut documents = vec![(0, index)];
    for chunk in listed {
        if let Some(document) = workspaces
            .resource_document(
                workspace,
                FILES_APP,
                &fs_protocol::tree::chunk_document_id(chunk),
            )
            .await
            .unwrap()
        {
            documents.push((chunk, document));
        }
    }
    documents
}

/// Files' verbs by a path in the workspace, on the tree the workspace's server keeps.
#[path = "put_files_by_path.rs"]
mod by_path;

/// The workspace's files as its listing offers them to an agent.
#[path = "files_listed.rs"]
mod files_listed;

/// Why a page's store refuses `bytes` as `document` of `workspace`'s Files, or `None` when it
/// opens it: the identity every page checks before it shows a document, which a mount that writes
/// the tree must carry as a page's own window does.
fn page_refusal(bytes: &[u8], document: &str, workspace: &str) -> Option<String> {
    let doc = automerge::AutoCommit::load(bytes).expect("the document reads");
    let checkout = ui::resource::DocumentCheckout::new(
        ui::resource::DocumentId::parse(document).expect("a document id"),
        ui::resource::ResourceType::new(fs_protocol::tree::TREE_KIND),
    );
    ui::resource::validate_identity(&doc, &checkout, workspace)
        .err()
        .map(|refusal| refusal.to_string())
}

/// A computer's mounted folder founds the workspace's tree, as the folder `tilo login` mounts
/// does: the page of the person whose computer it is opens every document the mount wrote.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tree_a_computers_mount_founds_is_one_a_page_opens() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Owner", 1);
    let serving = enrol(&owner, "lounge-server", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &owner,
        &serving,
        state.path(),
        Options::offering(crate::common::catalogue),
    )
    .await;
    let workspaces = hosted.server.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &serving.cert,
            tilo_server_protocol::directory::Listing {
                description: "Everyone".into(),
                default_role: tilo_peer_state::join::Role::Editor,
            },
        )
        .await
        .unwrap();
    let workspace = lounge.workspace.clone();
    let computer = enrol(&owner, "Ada's laptop", DeviceCapabilities::member());
    let laptop =
        super::resource_support::open_admitted_peer(&hosted, &computer, &serving, &workspace).await;
    let mount = tilo_fs::namespace::MountAuthor::person(
        hex::encode(computer.cert.device_public_key),
        ui::permissions::MemberId::person(hex::encode(owner.verifying_key().to_bytes())),
    )
    .in_workspace(&workspace);
    let mut folder = tilo_fs::Namespace::new(mount).unwrap();
    folder
        .create_file(tilo_fs::ROOT_INO, "notes.txt", None, 0, 1)
        .unwrap();
    publish(&laptop, &mut folder).await;
    tree_when(&hosted, &workspace, |tree| {
        file_at(tree, "", "notes.txt").is_some()
    })
    .await;

    let index = workspaces
        .resource_document(&workspace, FILES_APP, &tree_document())
        .await
        .unwrap()
        .expect("the workspace keeps the tree's own document");
    let mut kept = vec![(tree_document(), index)];
    let listed = automerge::AutoCommit::load(&kept[0].1).expect("the tree's own document reads");
    for chunk in fs_protocol::tree::listed_chunks(&listed) {
        let document = fs_protocol::tree::chunk_document_id(chunk);
        let bytes = workspaces
            .resource_document(&workspace, FILES_APP, &document)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("the workspace keeps chunk {chunk}"));
        kept.push((document, bytes));
    }
    for (document, bytes) in kept {
        assert_eq!(
            page_refusal(&bytes, &document, &workspace),
            None,
            "{document} is one a page opens"
        );
    }
}
