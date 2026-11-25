//! Files a `files#open` names, put into the tree the service holds: the person's own open, one
//! file and a folder of them, the list given as content of its own, and what the tree refuses.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture failing is its test failing"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{self, FILES_APP, ROOT_ID, Tree, TreeChunks};
use serde_json::json;

use crate::workspaces::fixtures::{governor, test_host};

use super::*;

/// A content store's shelf in memory, as a device's own is: content kept under the scope a
/// record's content lives under, addressed by its hash.
#[derive(Default)]
struct Shelf(Mutex<HashMap<ContentScope, HashMap<String, Vec<u8>>>>);

impl ContentShelf for Shelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        self.0.lock().unwrap().get(scope)?.get(hash).cloned()
    }

    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
        let address = hex::encode(blake3::hash(bytes).as_bytes());
        if address != hash {
            return Err(format!("these bytes are {address}, not {hash}"));
        }
        self.0
            .lock()
            .unwrap()
            .entry(scope.clone())
            .or_default()
            .insert(hash.to_owned(), bytes.to_vec());
        Ok(())
    }

    fn size(&self, scope: &ContentScope, hash: &str) -> Option<u64> {
        self.read(scope, hash)
            .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
    }
}

/// Put `bytes` into `workspace`'s content, as an upload does, and answer their reference.
fn put_content(shelf: &Shelf, workspace: WorkspaceId, bytes: &[u8]) -> String {
    let hash = hex::encode(blake3::hash(bytes).as_bytes());
    shelf
        .keep(
            &ContentScope {
                workspace: workspace.to_string(),
                app: CONTENT_PUT_SCOPE.to_owned(),
            },
            &hash,
            bytes,
        )
        .expect("the shelf keeps the content");
    format!("tilo.content:{hash}")
}

/// The workspace's tree as its records read, `None` before anything was put into it.
async fn tree_if_kept(workspaces: &Workspaces, workspace: &str) -> Option<Tree> {
    let index = workspaces
        .resource_document(workspace, FILES_APP, &tree::document_id())
        .await
        .unwrap()?;
    let index = automerge::AutoCommit::load(&index).expect("the tree's own record reads");
    let mut kept = TreeChunks::default();
    kept.read_index(&index);
    for chunk in tree::listed_chunks(&index) {
        let record = workspaces
            .resource_document(workspace, FILES_APP, &tree::chunk_document_id(chunk))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("the workspace keeps chunk {chunk} of the tree"));
        kept.read_chunk(chunk, &automerge::AutoCommit::load(&record).expect("a chunk reads"));
    }
    Some(kept.tree())
}

/// The workspace's tree as its records read.
async fn tree_of(workspaces: &Workspaces, workspace: &str) -> Tree {
    tree_if_kept(workspaces, workspace)
        .await
        .expect("the workspace keeps the tree's own record")
}

/// Whether nothing is in the workspace's tree yet.
async fn tree_is_empty(workspaces: &Workspaces, workspace: &str) -> bool {
    tree_if_kept(workspaces, workspace)
        .await
        .is_none_or(|tree| tree.children(ROOT_ID).is_empty())
}

/// The file at `dir/name` in `tree`, as its node.
fn file_at<'a>(tree: &'a Tree, dir: &str, name: &str) -> Option<&'a fs_protocol::tree::TreeEntry> {
    let mut parent = ROOT_ID;
    for folder in dir.split('/').filter(|folder| !folder.is_empty()) {
        let (id, _) = tree.children(parent).into_iter().find(|(_, node)| {
            node.kind == FileNodeKind::Directory && node.name.as_bytes() == folder.as_bytes()
        })?;
        parent = id;
    }
    tree.children(parent).into_iter().find(|(_, node)| {
        node.kind == FileNodeKind::File && node.name.as_bytes() == name.as_bytes()
    })
    .map(|(_, node)| node)
}

/// The bytes the tree's file named `hash` opens as: its manifest's chunks, opened through the
/// workspace's Files keys where it names sealed ones.
fn opened(
    shelf: &Shelf,
    workspace: WorkspaceId,
    keys: Option<&key_group::ContentKeys>,
    hash: &str,
) -> Vec<u8> {
    let files = ContentScope {
        workspace: workspace.to_string(),
        app: FILES_APP.to_owned(),
    };
    let encoded = shelf.read(&files, hash).expect("the manifest is kept");
    let manifest = Manifest::decode(&encoded).expect("the manifest decodes");
    let mut file = Vec::new();
    for part in manifest.chunks() {
        let stored = shelf
            .read(&files, &hex::encode(part.hash))
            .expect("a chunk is kept");
        let clear = match (manifest.sealed(), keys) {
            (Some(sealed), Some(keys)) => keys
                .open_content(
                    collab_types::file_content_artifact(workspace),
                    &collab_types::EncryptionMetadata::content_group(
                        workspace,
                        WorkspaceId::from_u128(u128::from_be_bytes(sealed.group)),
                        sealed.epoch,
                        collab_types::ContentRetention::Delta,
                    ),
                    &stored,
                )
                .expect("the chunk opens under the Files keys")
                .to_vec(),
            _ => stored,
        };
        assert_eq!(
            clear.len(),
            usize::try_from(part.len).unwrap_or(usize::MAX),
            "a chunk is its clear length"
        );
        file.extend_from_slice(&clear);
    }
    file
}

/// A host keeping a workspace, the shelf its content is kept on, and the puts a call makes into
/// its tree.
struct Put {
    workspaces: Workspaces,
    shelf: Arc<Shelf>,
    puts: OpenedFiles,
    workspace: WorkspaceId,
    name: String,
    // Last, so the host stops before the state it keeps on disk is deleted.
    _host: crate::workspaces::fixtures::TestHost,
    _state: tempfile::TempDir,
}

async fn fixture() -> Put {
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(state.path(), &key, &cert).await;
    let workspaces = host.workspaces().clone();
    let name = workspaces.create("Files put").await.unwrap().workspace;
    let workspace = WorkspaceId::parse(&name).unwrap();
    let shelf = Arc::new(Shelf::default());
    let puts = OpenedFiles::new(workspaces.clone(), Arc::clone(&shelf) as Arc<dyn ContentShelf>);
    Put {
        workspaces,
        shelf,
        puts,
        workspace,
        name,
        _host: host,
        _state: state,
    }
}

impl Put {
    /// Put `arguments` as the dispatch does, on a thread of its own: the tree's sync then has
    /// this device's runtime to itself, as it does when a daemon serves a call.
    fn put(&self, arguments: Value) -> Result<Value, String> {
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    self.puts.put(
                        self.workspace,
                        arguments.as_object().unwrap(),
                        None,
                        &|| true,
                    )
                })
                .join()
                .expect("the put's thread ends")
        })
    }

    /// What the workspace's Files keys are, as this device keeps them.
    async fn keys(&self) -> Option<key_group::ContentKeys> {
        Some(
            self.workspaces
                .own_route_keys(&tree_route(self.workspace), false)
                .await
                .expect("this device takes part in the workspace's key group")
                .expect("the workspace's Files keys open here"),
        )
    }
}

/// A person puts one file into the workspace's folder: a node of its tree naming content the
/// workspace keeps, the folder `dir` made on the way, and the same name put again taking the
/// new content rather than a second node.
#[tokio::test]
async fn a_file_put_is_a_node_naming_content_the_workspace_keeps() {
    let put = fixture().await;
    let visa = b"%PDF-1.7\nApplication for Schengen Visa\n";
    let content = put_content(&put.shelf, put.workspace, visa);
    let answer = put.put(json!({"content": content, "name": "visa.pdf"})).unwrap();
    assert_eq!(answer["path"], "visa.pdf", "{answer}");
    assert_eq!(answer["content"], content.as_str(), "{answer}");
    assert_eq!(answer["size"], visa.len() as u64, "{answer}");
    assert_eq!(answer["replaced"], false, "{answer}");
    let tree = tree_of(&put.workspaces, &put.name).await;
    let node = file_at(&tree, "", "visa.pdf").expect("the file is in the workspace's tree");
    let hash = node.hash.clone().expect("the file names its content");
    assert_eq!(node.size, visa.len() as u64);
    let keys = put.keys().await;
    assert_eq!(
        opened(&put.shelf, put.workspace, keys.as_ref(), &hash),
        visa.as_slice()
    );

    // A folder path keeps the folder, and the file is in it, not at the root.
    let permit = b"%PDF-1.7\nApplication for Residence Permit\n";
    let permit_content = put_content(&put.shelf, put.workspace, permit);
    let answer = put
        .put(json!({"content": permit_content, "name": "permit.pdf", "dir": "forms/2026"}))
        .unwrap();
    assert_eq!(answer["path"], "forms/2026/permit.pdf", "{answer}");
    let tree = tree_of(&put.workspaces, &put.name).await;
    let node = file_at(&tree, "forms/2026", "permit.pdf").expect("the file is in its folder");
    assert_eq!(
        opened(&put.shelf, put.workspace, keys.as_ref(), node.hash.as_deref().unwrap()),
        permit.as_slice()
    );
    assert!(file_at(&tree, "", "permit.pdf").is_none(), "it is not at the root");

    // The same name put again takes the new content rather than making a second node.
    let cover = b"%PDF-1.7\nCover letter\n";
    let cover_content = put_content(&put.shelf, put.workspace, cover);
    let answer = put.put(json!({"content": cover_content, "name": "visa.pdf"})).unwrap();
    assert_eq!(answer["replaced"], true, "{answer}");
    assert_ne!(answer["content"], content.as_str(), "{answer}");
    let tree = tree_of(&put.workspaces, &put.name).await;
    let node = file_at(&tree, "", "visa.pdf").expect("still one file of that name");
    assert_eq!(
        opened(&put.shelf, put.workspace, keys.as_ref(), node.hash.as_deref().unwrap()),
        cover.as_slice()
    );
    let roots = tree
        .children(ROOT_ID)
        .into_iter()
        .filter(|(_, node)| node.name.as_bytes() == b"visa.pdf")
        .count();
    assert_eq!(roots, 1, "the file is not kept beside itself");
}

/// A folder of files goes in as one change, whether the list is given inline or as content of
/// its own: one answer with the count, and every file lands in its folder.
#[tokio::test]
async fn a_folder_of_files_goes_in_inline_or_by_its_content_reference() {
    let put = fixture().await;
    let mut listed = Vec::new();
    let mut names = Vec::new();
    for index in 0..3 {
        let bytes = format!("file {index}\n").into_bytes();
        let content = put_content(&put.shelf, put.workspace, &bytes);
        let name = format!("f{index}.txt");
        names.push(name.clone());
        listed.push(json!({"content": content, "name": name, "dir": "repo/src"}));
    }
    let answer = put.put(json!({"files": listed})).unwrap();
    assert_eq!(answer["count"], 3, "{answer}");
    let keys = put.keys().await;
    let tree = tree_of(&put.workspaces, &put.name).await;
    for (index, name) in names.iter().enumerate() {
        let node =
            file_at(&tree, "repo/src", name).unwrap_or_else(|| panic!("{name} is in the folder"));
        assert_eq!(
            opened(&put.shelf, put.workspace, keys.as_ref(), node.hash.as_deref().unwrap()),
            format!("file {index}\n").into_bytes(),
            "{name} opens as what was put"
        );
    }

    // The same kind of list, given as content the workspace keeps: one reference, however many
    // files it names.
    let mut second = Vec::new();
    for index in 3..5 {
        let bytes = format!("file {index}\n").into_bytes();
        let content = put_content(&put.shelf, put.workspace, &bytes);
        second.push(json!({
            "content": content,
            "name": format!("f{index}.txt"),
            "dir": "repo/src",
        }));
    }
    let list = put_content(&put.shelf, put.workspace, &serde_json::to_vec(&second).unwrap());
    let answer = put.put(json!({"files": list})).unwrap();
    assert_eq!(answer["count"], 2, "{answer}");
    let tree = tree_of(&put.workspaces, &put.name).await;
    for name in ["f3.txt", "f4.txt"] {
        assert!(
            file_at(&tree, "repo/src", name).is_some(),
            "{name} is in the folder the list named"
        );
    }
}

/// Content the workspace does not hold refuses the whole open, nothing is put, and what the tree
/// and a file's name may not be refuses it too.
#[tokio::test]
async fn content_the_workspace_lacks_and_names_the_tree_refuses_put_nothing() {
    let put = fixture().await;
    let missing = format!("tilo.content:{}", "0".repeat(64));
    let refused = |answer: Result<Value, String>| answer.expect_err("the open is refused");
    let said = refused(put.put(json!({"content": missing, "name": "gone.bin"})));
    assert!(said.contains("holds no"), "{said}");
    assert!(
        tree_is_empty(&put.workspaces, &put.name).await,
        "nothing was put"
    );

    let content = put_content(&put.shelf, put.workspace, b"words\n");
    for (arguments, why) in [
        (json!({"content": content, "name": "a/b.txt"}), "no file's name"),
        (json!({"content": content, "name": ".."}), "no file's name"),
        (json!({"content": content, "name": "a.txt", "dir": "../out"}), "no folder's name"),
        (json!({"content": "not-content", "name": "a.txt"}), "names no content"),
        (json!({"files": []}), "1 to"),
    ] {
        let said = refused(put.put(arguments.clone()));
        assert!(said.contains(why), "{arguments}: {said}");
    }
    assert!(
        tree_is_empty(&put.workspaces, &put.name).await,
        "a refused open put nothing"
    );
}

/// What a put names of its files: a content reference and a name a file may have, a folder path
/// kept as the tree keeps one, and the shapes the tree and the workspace refuse.
#[test]
fn a_put_names_its_files_and_refuses_what_the_tree_may_not_hold() {
    let content = format!("tilo.content:{}", "ab".repeat(32));
    let named = |arguments: Value| Putting::named(arguments.as_object().unwrap());
    let arguments = json!({"content": content, "name": "plan.md", "dir": " / Tasks // 2026 / "});
    let putting = named(arguments).expect("a file named by its content");
    assert_eq!(putting.dir, "Tasks/2026", "the folder path as the tree keeps one");
    assert_eq!(putting.name, "plan.md");
    assert_eq!(putting.hash, "ab".repeat(32));

    for (arguments, why) in [
        (json!({"content": "notes.md", "name": "plan.md"}), "names no content"),
        (json!({"content": content, "name": ""}), "no file's name"),
        (json!({"content": content, "name": "a/b.md"}), "no file's name"),
        (json!({"content": content, "name": "a.md", "dir": "../out"}), "no folder's name"),
        (json!({"content": content, "name": "a.md", "dir": "a/./b"}), "no folder's name"),
    ] {
        let said = named(arguments.clone())
            .err()
            .unwrap_or_else(|| panic!("{arguments} is refused"));
        assert!(said.contains(why), "{arguments}: {said}");
    }

    // A list is each file it names, given inline or as a content reference to one, and a list
    // of none or too many is refused as a whole.
    let listed = json!([
        {"content": content, "name": "a.md"},
        {"content": content, "name": "b.md", "dir": "Tasks"},
    ]);
    let files =
        Putting::all(json!({"files": listed}).as_object().unwrap(), &|_| None)
            .expect("a list is read");
    assert_eq!(files.len(), 2);
    assert_eq!(files[1].dir, "Tasks");
    let bytes = serde_json::to_vec(&listed).unwrap();
    let files = Putting::all(
        json!({"files": format!("tilo.content:{}", "cd".repeat(32))})
            .as_object()
            .unwrap(),
        &|_| Some(bytes.clone()),
    )
    .expect("a list given as content is read");
    assert_eq!(files.len(), 2);
    assert!(Putting::all(json!({"files": []}).as_object().unwrap(), &|_| None).is_err());
    let too_many: Vec<Value> = (0..=MAX_FILES)
        .map(|_| json!({"content": content, "name": "a.md"}))
        .collect();
    let said = Putting::all(json!({"files": too_many}).as_object().unwrap(), &|_| None)
        .err()
        .expect("more files than one open puts");
    assert!(said.contains("1 to"), "{said}");
}
