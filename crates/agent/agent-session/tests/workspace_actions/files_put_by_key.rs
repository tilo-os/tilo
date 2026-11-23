//! A key's folder of files, put through the API as `tilo files put` sends it,
//! lands in the workspace's folder where its listing and Files find it: the
//! workspace is the server's own, with no computer of the person's logged in
//! to hold the folder.

use super::api::ISSUER;
use super::api_on_server::{api_on_server, approved_key, json_of};
use super::key::Key;
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;

/// A folder's worth of files, as the command sends them: their names here
/// and the bytes each holds.
fn folder() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("Reports/report-001.txt", b"the first report\n".to_vec()),
        ("Reports/report-002.txt", b"the second report\n".to_vec()),
        ("Reports/report-003.txt", b"the third report\n".to_vec()),
        ("notes/plan.txt", b"a plan\n".to_vec()),
    ]
}

/// [`folder`]'s contents put into `workspace`, each at its own address as the
/// command puts a file, answering each one's `tilo.content:` reference.
async fn uploaded(key: &Key<'_>, workspace: &str, files: &[(&str, Vec<u8>)]) -> Vec<String> {
    let mut contents = Vec::with_capacity(files.len());
    for (_, bytes) in files {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let (status, put) = json_of(
            key.send(
                hyper::Method::PUT,
                &format!("/v1/workspaces/{workspace}/contents/{hash}"),
                bytes,
                &[("content-type", "application/octet-stream")],
            )
            .await,
        )
        .await;
        assert_eq!(status, 201, "{put}");
        contents.push(
            put["content"]
                .as_str()
                .expect("the put answers its content")
                .to_owned(),
        );
    }
    contents
}

/// The rows of the workspace's listing at `path`: the API's, page after page.
async fn listed_whole(key: &Key<'_>, base: &str, path: &str) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let at = match &cursor {
            Some(cursor) => format!("{base}/resources?path={path}&limit=100&cursor={cursor}"),
            None => format!("{base}/resources?path={path}&limit=100"),
        };
        let (status, listed) = key.get(&at).await;
        assert_eq!(status, 200, "listing {at}: {listed}");
        rows.extend(listed["rows"].as_array().cloned().unwrap_or_default());
        match listed["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => return rows,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keys_folder_put_lists_in_the_workspaces_files() {
    Box::pin(put_and_list()).await;
}

async fn put_and_list() {
    let on_server = Box::pin(api_on_server(ISSUER)).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let computer = enrol(&ada, "Ada's computer", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &computer, "Ada's backend").await;
    let key = Key { api: &api, token };
    let (status, made) = key
        .post(
            "/v1/resources/account/workspaces/actions/create",
            &json!({"name": "Company"}),
            &[],
        )
        .await;
    assert_eq!(status, 200, "{made}");
    let workspace = made["result"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let base = format!("/v1/workspaces/{workspace}");

    // The bytes go up, then the list of them into Files, as the command puts a
    // folder of files: one open carries the whole list as content of its own.
    let files = folder();
    let contents = uploaded(&key, &workspace, &files).await;
    let entries: Vec<Value> = files
        .iter()
        .zip(&contents)
        .map(|((name, _), content)| {
            let (dir, name) = name.rsplit_once('/').unwrap();
            json!({"content": content, "name": name, "dir": dir})
        })
        .collect();
    // The open takes the list inline: what the command does for a folder.
    let (status, answered) = key
        .post(
            &format!("{base}/resources/files/actions/open"),
            &json!({"files": entries}),
            &[],
        )
        .await;
    assert_eq!(status, 200, "the open: {answered}");
    assert_eq!(
        answered.pointer("/result/count"),
        Some(&json!(4)),
        "{answered}"
    );

    // The workspace's folder lists the folder put into it, and beneath that
    // folder its files, each read by the row it is shown as.
    let root = listed_whole(&key, &base, "files").await;
    let reports = root
        .iter()
        .find(|row| row["label"] == "Reports")
        .unwrap_or_else(|| panic!("Reports is not listed: {root:#?}"));
    assert_eq!(reports["kind"], "folder");
    assert_eq!(reports["has_children"], true);
    let inside = listed_whole(&key, &base, "files/Reports").await;
    for (path, _) in &files[..3] {
        let id = format!("file:{path}");
        assert!(
            inside.iter().any(|row| row["id"] == id),
            "{path} is not in the folder's listing: {inside:#?}"
        );
    }
    let (status, read) = key
        .get(&format!("{base}/resources/files/Reports/report-001.txt"))
        .await;
    assert_eq!(status, 200, "reading a file back: {read}");
    assert_eq!(read["fields"]["text"], "the first report\n", "{read}");

    // The tree is a document a page's Files window opens: it names itself as Files' shared
    // document of the workspace, as a window that founds one does.
    let kept = on_server
        .hosted
        .server
        .workspaces()
        .unwrap()
        .resource_document(
            &workspace,
            fs_protocol::tree::FILES_APP,
            &fs_protocol::tree::document_id(),
        )
        .await
        .unwrap()
        .expect("the workspace keeps the tree");
    let tree = automerge::AutoCommit::load(&kept).expect("the tree reads");
    let identity = ui::resource::identity(&tree).expect("the tree names itself");
    assert_eq!(identity.id.as_str(), fs_protocol::tree::document_id());
    assert_eq!(identity.kind.as_str(), fs_protocol::tree::TREE_KIND);
    assert_eq!(
        ui::resource::workspace_scope(&tree).as_deref(),
        Some(workspace.as_str())
    );
}

/// Put `files` into the workspace at `base` as `tilo files put` does: their bytes, then one open
/// listing them all.
async fn put_folder(key: &Key<'_>, workspace: &str, files: &[(&str, Vec<u8>)]) -> Value {
    let contents = uploaded(key, workspace, files).await;
    let entries: Vec<Value> = files
        .iter()
        .zip(&contents)
        .map(|((name, _), content)| {
            let (dir, name) = name.rsplit_once('/').unwrap();
            json!({"content": content, "name": name, "dir": dir})
        })
        .collect();
    let (status, answered) = key
        .post(
            &format!("/v1/workspaces/{workspace}/resources/files/actions/open"),
            &json!({"files": entries}),
            &[],
        )
        .await;
    assert_eq!(status, 200, "the open: {answered}");
    answered
}

/// A folder put after the server restarted lands in the folder the workspace already has: the
/// restarted server holds none of the workspace's tree until it reads it, and a put that took
/// the tree's parts for ones nobody wrote founded them again over the workspace's own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_put_after_the_server_restarts_lands_beside_what_is_there() {
    let on_server = Box::pin(api_on_server(ISSUER)).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let computer = enrol(&ada, "Ada's computer", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &computer, "Ada's backend").await;
    let workspace = {
        let key = Key {
            api: &on_server.api,
            token: token.clone(),
        };
        let (status, made) = key
            .post(
                "/v1/resources/account/workspaces/actions/create",
                &json!({"name": "Company"}),
                &[],
            )
            .await;
        assert_eq!(status, 200, "{made}");
        let workspace = made["result"]["workspace"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        put_folder(&key, &workspace, &folder()).await;
        workspace
    };

    let on_server = Box::pin(on_server.restart(false)).await;
    let key = Key {
        api: &on_server.api,
        token,
    };
    let more = vec![
        ("Quarterly/report-1.txt", b"the first quarter\n".to_vec()),
        ("Quarterly/report-2.txt", b"the second quarter\n".to_vec()),
        ("Reports/report-004.txt", b"the fourth report\n".to_vec()),
    ];
    let answered = put_folder(&key, &workspace, &more).await;
    assert_eq!(
        answered.pointer("/result/count"),
        Some(&json!(3)),
        "{answered}"
    );
    let base = format!("/v1/workspaces/{workspace}");
    let inside = listed_whole(&key, &base, "files/Reports").await;
    for path in ["Reports/report-001.txt", "Reports/report-004.txt"] {
        let id = format!("file:{path}");
        assert!(
            inside.iter().any(|row| row["id"] == id),
            "{path} is not in the folder's listing after the restart: {inside:#?}"
        );
    }
    let quarterly = listed_whole(&key, &base, "files/Quarterly").await;
    assert_eq!(quarterly.len(), 2, "{quarterly:#?}");
}
