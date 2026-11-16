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

/// What a branch of `source` answers, asked by `key` under `name`.
async fn branch_of(key: &Key<'_>, source: &str, name: &str) -> (u16, serde_json::Value) {
    key.post(
        &format!("/v1/workspaces/{source}/branches"),
        &json!({"name": name}),
        &[],
    )
    .await
}

/// What a branch of `source` answers, asked by `key` under `name` with the
/// Idempotency-Key `retry`: the status, whether it is an answer replayed, and
/// its body.
async fn branch_retried(
    key: &Key<'_>,
    source: &str,
    name: &str,
    retry: &str,
) -> (u16, bool, serde_json::Value) {
    let reply = key
        .send(
            hyper::Method::POST,
            &format!("/v1/workspaces/{source}/branches"),
            json!({"name": name}).to_string().as_bytes(),
            &[("idempotency-key", retry)],
        )
        .await;
    let replayed = reply.headers().contains_key("idempotent-replayed");
    let (status, body) = json_of(reply).await;
    (status, replayed, body)
}

/// The ids of the workspaces named `name` `key` lists.
async fn listed_as(key: &Key<'_>, name: &str) -> Vec<String> {
    let (status, listed) = key.get("/v1/workspaces").await;
    assert_eq!(status, 200, "{listed}");
    listed["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["name"] == name)
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_its_host_is_still_gathering_makes_no_branch_until_it_is_here() {
    Box::pin(a_file_still_gathered_makes_no_branch_until_it_is_here()).await;
}

async fn a_file_still_gathered_makes_no_branch_until_it_is_here() {
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let workspaces = on_relay.hosted.relay.workspaces().unwrap().clone();
    let (_, token) = person(&on_relay, "Ada").await;
    let backend = Key { api: &api, token };
    let (status, made) = backend
        .post("/v1/workspaces", &json!({"name": "Scans"}), &[])
        .await;
    assert_eq!(status, 201, "{made}");
    let source = made["id"].as_str().unwrap().to_owned();
    let scan = varied(700_000, 4);
    put_file(&backend, &source, "scans", "passport.pdf", &scan).await;
    keeps(&on_relay, &source, ("scans", "passport.pdf"), &scan).await;

    // The tree names the file, but one of its chunks has not reached the host
    // yet, as when a device that wrote it is still sending it.
    let store = on_relay.hosted.relay.content().get(&source).unwrap();
    let (manifest, _) = fs_content::build(&scan, None);
    let chunk = hex::encode(manifest.chunks()[1].hash);
    let held = store.read(&chunk).expect("the host keeps the chunk");
    store.release(&chunk).unwrap();
    assert!(!store.has(&chunk));

    // Asked under a key the caller keeps for the customer, as the SDKs suggest.
    let retry = "branch-scans-acme";
    let (status, _, refused) = branch_retried(&backend, &source, "Scans: Acme", retry).await;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (422, &json!("branch_incomplete")),
        "{refused}"
    );
    let said = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(said.contains("the files its records name"), "{refused}");
    assert!(listed_as(&backend, "Scans: Acme").await.is_empty());
    assert!(
        !workspaces
            .list()
            .await
            .iter()
            .any(|summary| summary.name == "Scans: Acme"),
        "the relay governs nothing of it"
    );

    // The chunk arrives: the same call, under the same key, makes the branch,
    // which holds the file whole.
    store.put(&held).unwrap();
    let (status, replayed, branched) =
        branch_retried(&backend, &source, "Scans: Acme", retry).await;
    assert_eq!(status, 201, "{branched}");
    assert!(!replayed, "the refusal was not kept: the call ran again");
    let branch = branched["id"].as_str().unwrap().to_owned();
    assert_eq!(
        kept_bytes(&on_relay, &branch, &manifest_of(&scan)).as_deref(),
        Some(scan.as_slice())
    );
    // Asked once more under that key, it answers the same branch and makes
    // no second one.
    let (status, replayed, again) = branch_retried(&backend, &source, "Scans: Acme", retry).await;
    assert_eq!((status, replayed), (201, true), "{again}");
    assert_eq!(again["id"], branched["id"]);
    assert_eq!(listed_as(&backend, "Scans: Acme").await, [branch]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn content_only_an_earlier_version_named_does_not_stop_a_branch() {
    Box::pin(content_only_earlier_named_does_not_stop_a_branch()).await;
}

async fn content_only_earlier_named_does_not_stop_a_branch() {
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let (_, token) = person(&on_relay, "Ada").await;
    let backend = Key { api: &api, token };
    let (status, made) = backend
        .post("/v1/workspaces", &json!({"name": "Drafts"}), &[])
        .await;
    assert_eq!(status, 201, "{made}");
    let source = made["id"].as_str().unwrap().to_owned();

    // A file written, then written over: the tree's history names the first
    // version, its current state the second. The host let the first go.
    let first = varied(300_000, 5);
    put_file(&backend, &source, "", "draft.txt", &first).await;
    keeps(&on_relay, &source, ("", "draft.txt"), &first).await;
    let second = varied(300_000, 6);
    put_file(&backend, &source, "", "draft.txt", &second).await;
    keeps(&on_relay, &source, ("", "draft.txt"), &second).await;
    let store = on_relay.hosted.relay.content().get(&source).unwrap();
    let (old, _) = fs_content::build(&first, None);
    let (now, _) = fs_content::build(&second, None);
    let kept_now: Vec<String> = now
        .chunks()
        .iter()
        .map(|chunk| hex::encode(chunk.hash))
        .collect();
    store.release(&manifest_of(&first)).unwrap();
    for chunk in old.chunks() {
        let chunk = hex::encode(chunk.hash);
        if !kept_now.contains(&chunk) {
            store.release(&chunk).unwrap();
        }
    }
    assert!(!store.has(&manifest_of(&first)));

    let (status, branched) = branch_of(&backend, &source, "Drafts: Acme").await;
    assert_eq!(status, 201, "{branched}");
    let branch = branched["id"].as_str().unwrap().to_owned();
    let tree = tree_of(&on_relay, &branch).await;
    assert_eq!(
        file_at(&tree, "", "draft.txt").and_then(|node| node.hash.clone()),
        Some(manifest_of(&second))
    );
    assert_eq!(
        kept_bytes(&on_relay, &branch, &manifest_of(&second)).as_deref(),
        Some(second.as_slice())
    );
}
