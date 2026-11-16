//! A branch of a hosted workspace holds the workspace's Files: every file its
//! tree names, under the same path, with the bytes it is made of (its manifest
//! and every chunk the manifest lists), readable from the branch's own content
//! alone. From then on each tree goes its own way: a file changed or added in
//! one is not in the other.

use super::api::ISSUER;
use super::api_on_relay::{OnRelay, api_on_relay, json_of};
use super::branch_workspace::{Key, person};
use super::*;
use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{FILES_APP, ROOT_ID, Tree, TreeEntry};
use serde_json::json;
use std::sync::Arc;
use tilo_fs::tree_record::record as tree_record;

/// The tree of `workspace`'s Files as its host keeps it; empty while it keeps none.
async fn tree_of(on_relay: &OnRelay, workspace: &str) -> Tree {
    let workspaces = on_relay.hosted.relay.workspaces().unwrap();
    workspaces
        .record_document(workspace, FILES_APP, &tree_record())
        .await
        .ok()
        .flatten()
        .and_then(|record| Tree::of_record(&record).ok())
        .unwrap_or_default()
}

/// The file at `dir/name` in `tree`; `None` when it is not there.
fn file_at<'a>(tree: &'a Tree, dir: &str, name: &str) -> Option<&'a TreeEntry> {
    let mut parent = ROOT_ID;
    for folder in dir.split('/').filter(|folder| !folder.is_empty()) {
        let (id, _) = tree.children(parent).into_iter().find(|(_, node)| {
            node.kind == FileNodeKind::Directory && node.name.as_bytes() == folder.as_bytes()
        })?;
        parent = id;
    }
    tree.children(parent)
        .into_iter()
        .find(|(_, node)| {
            node.kind == FileNodeKind::File && node.name.as_bytes() == name.as_bytes()
        })
        .map(|(_, node)| node)
}

/// What a tree names a file of `bytes` by: the address of its content manifest.
fn manifest_of(bytes: &[u8]) -> String {
    let (manifest, _) = fs_content::build(bytes, None);
    blake3::hash(&manifest.encode()).to_hex().to_string()
}

/// How many chunks a file of `bytes` is kept as.
fn chunks_of(bytes: &[u8]) -> usize {
    fs_content::build(bytes, None).0.chunks().len()
}

/// The bytes of the file whose manifest is `manifest`, read from `workspace`'s
/// own content on its host alone: the manifest, then every chunk it lists.
fn kept_bytes(on_relay: &OnRelay, workspace: &str, manifest: &str) -> Option<Vec<u8>> {
    let store = on_relay.hosted.relay.content().get(workspace).ok()?;
    let listed = store.read(manifest)?;
    fs_content::whole(listed, u64::MAX, |chunk| store.read(&hex::encode(chunk)))
}

/// Put `bytes` into `workspace`'s Files at `dir/name`, as the API puts a file.
async fn put_file(key: &Key<'_>, workspace: &str, dir: &str, name: &str, bytes: &[u8]) {
    let base = format!("/v1/workspaces/{workspace}");
    let (status, put) = json_of(
        key.send(
            hyper::Method::POST,
            &format!("{base}/uploads"),
            bytes,
            &[("content-type", "application/octet-stream")],
        )
        .await,
    )
    .await;
    assert_eq!(status, 201, "{put}");
    key.did(
        &format!("{base}/resources/files/actions/open"),
        &json!({"content": put["content"], "name": name, "dir": dir}),
    )
    .await;
}

/// Wait until `workspace`'s host keeps the file `dir/name` as `bytes`: its tree
/// names it, and its content holds every byte of it.
async fn keeps(on_relay: &OnRelay, workspace: &str, (dir, name): (&str, &str), bytes: &[u8]) {
    let manifest = manifest_of(bytes);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let tree = tree_of(on_relay, workspace).await;
        let named = file_at(&tree, dir, name).and_then(|node| node.hash.clone());
        if named.as_deref() == Some(manifest.as_str())
            && kept_bytes(on_relay, workspace, &manifest).as_deref() == Some(bytes)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{workspace} never kept {dir}/{name} whole: its tree names {named:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// `len` bytes no two runs of which repeat, so a content-defined chunker cuts
/// them where it would cut a real file's.
fn varied(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state.to_be_bytes()[0]
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_branch_holds_the_workspaces_files_and_each_goes_its_own_way() {
    Box::pin(a_branch_holds_the_files_and_each_tree_goes_its_own_way()).await;
}

async fn a_branch_holds_the_files_and_each_tree_goes_its_own_way() {
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let (_, token) = person(&on_relay, "Ada").await;
    let backend = Key { api: &api, token };
    let (status, made) = backend
        .post("/v1/workspaces", &json!({"name": "Contracts"}), &[])
        .await;
    assert_eq!(status, 201, "{made}");
    let source = made["id"].as_str().unwrap().to_owned();

    // A file at the root, and a folder with two: one of one chunk, one larger
    // than a chunk, which the workspace keeps as many.
    let readme = b"Every contract of the customer, one folder a year.\n".repeat(200);
    let small = varied(20_000, 1);
    let large = varied(700_000, 2);
    assert_eq!(chunks_of(&small), 1);
    assert!(chunks_of(&large) > 1, "the large file is kept as chunks");
    let files: [(&str, &str, &[u8]); 3] = [
        ("", "README.txt", &readme),
        ("contracts", "terms.txt", &small),
        ("contracts", "signed.pdf", &large),
    ];
    for (dir, name, bytes) in files {
        put_file(&backend, &source, dir, name, bytes).await;
    }
    // The host keeps every one of them whole before the branch: what the
    // branch then lacks is the branch's loss.
    for (dir, name, bytes) in files {
        keeps(&on_relay, &source, (dir, name), bytes).await;
    }

    let (status, branched) = backend
        .post(
            &format!("/v1/workspaces/{source}/branches"),
            &json!({"name": "Contracts: Acme"}),
            &[],
        )
        .await;
    assert_eq!(status, 201, "{branched}");
    let branch = branched["id"].as_str().unwrap().to_owned();

    // Every file is in the branch's tree under the same path, and the
    // branch's own content holds every byte of it: its manifest and every
    // chunk the manifest lists.
    let tree = tree_of(&on_relay, &branch).await;
    for (dir, name, bytes) in files {
        let node = file_at(&tree, dir, name)
            .unwrap_or_else(|| panic!("the branch's tree lacks {dir}/{name}"));
        assert_eq!(node.hash.as_deref(), Some(manifest_of(bytes).as_str()));
        assert_eq!(node.size, bytes.len() as u64);
        assert_eq!(
            kept_bytes(&on_relay, &branch, &manifest_of(bytes)).as_deref(),
            Some(bytes),
            "the branch's content holds {dir}/{name} whole"
        );
    }

    // A file changed in the branch is changed there alone, and the reverse.
    let in_branch = b"Acme's own terms.\n".repeat(300);
    put_file(&backend, &branch, "contracts", "terms.txt", &in_branch).await;
    keeps(&on_relay, &branch, ("contracts", "terms.txt"), &in_branch).await;
    let in_source = b"The template's next terms.\n".repeat(300);
    put_file(&backend, &source, "", "README.txt", &in_source).await;
    keeps(&on_relay, &source, ("", "README.txt"), &in_source).await;
    // A file added to the source after the branch is the source's alone.
    let later = varied(9_000, 3);
    put_file(&backend, &source, "contracts", "later.txt", &later).await;
    keeps(&on_relay, &source, ("contracts", "later.txt"), &later).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let (source_tree, branch_tree) = (
        tree_of(&on_relay, &source).await,
        tree_of(&on_relay, &branch).await,
    );
    let named = |tree: &Tree, dir: &str, name: &str| {
        file_at(tree, dir, name).and_then(|node| node.hash.clone())
    };
    assert_eq!(
        named(&source_tree, "contracts", "terms.txt"),
        Some(manifest_of(&small)),
        "the branch's edit stays in the branch"
    );
    assert_eq!(
        named(&branch_tree, "", "README.txt"),
        Some(manifest_of(&readme)),
        "the source's edit stays in the source"
    );
    assert_eq!(
        named(&branch_tree, "contracts", "terms.txt"),
        Some(manifest_of(&in_branch))
    );
    assert!(
        file_at(&branch_tree, "contracts", "later.txt").is_none(),
        "a file added to the source after the branch is not in the branch"
    );
    assert!(
        file_at(&branch_tree, "contracts", "signed.pdf").is_some(),
        "the branch keeps what it took"
    );
}
