//! A workspace's notes are `.md` files in its folder, kept in step by its host: a note an
//! agent makes through the API becomes a file, and a note written afterwards rewrites its
//! own file and no other.

use super::api::ISSUER;
use super::api_on_relay::{api_on_relay, approved_key, headers, json_of};
use super::*;
use fs_protocol::tree::FILES_APP;
use serde_json::{Value, json};
use tilo_fs::tree_record::record as tree_record;

/// `POST path` with `body` under the key `token`, answered `200`.
async fn post(api: &Arc<tilo_api::Server>, token: &str, path: &str, body: &Value) -> Value {
    let bearer = format!("Bearer {token}");
    let pairs = [
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
    ];
    let (status, answer) = json_of(
        api.dispatch(
            &hyper::Method::POST,
            path,
            "",
            &headers(&pairs),
            body.to_string().as_bytes(),
        )
        .await,
    )
    .await;
    assert!((200..300).contains(&status), "{path} {body}: {answer}");
    answer
}

/// The workspace's folder as its host keeps it.
async fn folder(
    on_relay: &super::api_on_relay::OnRelay,
    workspace: &str,
) -> Option<tilo_fs::Namespace> {
    let record = on_relay
        .hosted
        .relay
        .workspaces()
        .unwrap()
        .record_document(workspace, FILES_APP, &tree_record())
        .await
        .unwrap()?;
    tilo_fs::Namespace::from_bytes("reader", &record).ok()
}

/// What the file at `path` of `tree` reads, when it is there.
fn file(tree: &mut tilo_fs::Namespace, path: &str) -> Option<String> {
    let ino = tree.resolve_path_ino(path)?;
    let bytes = tree.export_document(ino, &tilo_content::NoContent)?;
    Some(String::from_utf8_lossy(&bytes).trim().to_owned())
}

/// The folder once `ready` holds of it, waited for up to a minute.
async fn folder_when(
    on_relay: &super::api_on_relay::OnRelay,
    workspace: &str,
    what: &str,
    ready: impl Fn(&mut tilo_fs::Namespace) -> bool,
) -> tilo_fs::Namespace {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(mut tree) = folder(on_relay, workspace).await
            && ready(&mut tree)
        {
            return tree;
        }
        assert!(Instant::now() < deadline, "the folder never showed {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_written_rewrites_its_own_file_and_no_other() {
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_relay, &ada, &page, "Writer").await;
    let made = post(&api, &token, "/v1/workspaces", &json!({"name": "Company"})).await;
    let workspace = made["id"].as_str().unwrap().to_owned();
    let base = format!("/v1/workspaces/{workspace}");
    post(
        &api,
        &token,
        &format!("{base}/resources/notes/actions/open"),
        &json!({}),
    )
    .await;
    let notebook = format!("{base}/resources/notes/Notebook/actions");
    let mut pages = Vec::new();
    for title in ["Terms", "Plan", "Minutes"] {
        let made = post(
            &api,
            &token,
            &format!("{notebook}/create-page"),
            &json!({"title": title, "markdown": format!("What {title} says.")}),
        )
        .await;
        pages.push(made["result"]["page"].as_str().unwrap().to_owned());
    }
    let mut before = folder_when(&on_relay, &workspace, "every note's file", |tree| {
        ["Terms", "Plan", "Minutes"].iter().all(|title| {
            file(tree, &format!("Notes/{title}.md")).as_deref()
                == Some(&format!("What {title} says."))
        })
    })
    .await;

    post(
        &api,
        &token,
        &format!("{notebook}/write-note"),
        &json!({"page": pages[1], "markdown": "The plan changed."}),
    )
    .await;
    let mut after = folder_when(&on_relay, &workspace, "the written note", |tree| {
        file(tree, "Notes/Plan.md").as_deref() == Some("The plan changed.")
    })
    .await;
    let (from, to) = (before.heads(), after.heads());
    for title in ["Terms", "Minutes"] {
        let ino = after
            .resolve_path_ino(&format!("Notes/{title}.md"))
            .unwrap();
        assert!(
            !after.document_changed_between(ino, &from, &to),
            "{title}'s file was written again for another note's change"
        );
    }
    assert_eq!(
        file(&mut before, "Notes/Terms.md"),
        file(&mut after, "Notes/Terms.md")
    );
}
