//! A person's key writes through the API on a server whose operator is
//! someone else, as tilo.sh runs it, and every write lands in what the
//! workspace keeps, whichever document it falls in: a note moved in a group
//! of moves lands in the part of the notebook that holds the note, and a
//! folder larger than one chunk of the tree lands in every chunk it fills.
//! The server carries the key's calls out and signs what they change, and the
//! workspace decides each change as the key's, for its person, never as the
//! server's own.

use super::api::ISSUER;
use super::api_on_server::{OnServer, api_on_server, json_of};
use super::branch_workspace::{Key, person};
use super::put_files::tree_kept;
use super::*;
use serde_json::{Value, json};
use std::sync::Arc;

/// How many notes the key makes and then moves in one group, as an SDK's bulk
/// call sends them.
const NOTES: usize = 99;

/// More files than one chunk of the tree holds.
const FILES: usize = fs_protocol::tree::CHUNK_NODES + 200;

/// Each note the notebook of `workspace` keeps on the server, by id: the note
/// it sits under.
async fn parents_kept(on_server: &OnServer, workspace: &str) -> BTreeMap<String, Option<String>> {
    let workspaces = on_server.hosted.server.workspaces().unwrap();
    let mut kept = BTreeMap::new();
    for index in 0.. {
        let part_id = notes::note_list::chain().part(index);
        let Some(doc) = workspaces
            .resource_document(workspace, "so.tilo.notes", part_id.as_str())
            .await
            .ok()
            .flatten()
            .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
        else {
            break;
        };
        let Ok(part) = <notes::NoteList as ui::tilo_resource::Resource>::read(&doc) else {
            break;
        };
        for (id, note) in part.pages.iter() {
            kept.insert(
                id.to_string(),
                note.parent.as_ref().map(ToString::to_string),
            );
        }
    }
    kept
}

/// The id of the note a `new-note` answered.
fn page_of(made: &Value) -> String {
    made["result"]["page"]
        .as_str()
        .unwrap_or_else(|| panic!("a new note names its page: {made}"))
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keys_writes_land_in_every_part_and_chunk_the_workspace_keeps() {
    Box::pin(a_keys_writes_land()).await;
}

async fn a_keys_writes_land() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (root, token) = person(&on_server, "Ada").await;
    assert_ne!(
        root.verifying_key().to_bytes(),
        on_server.operator.verifying_key().to_bytes(),
        "the server is someone else's"
    );
    let key = Key { api: &api, token };
    let (status, made) = key
        .post("/v1/workspaces", &json!({"name": "Company"}), &[])
        .await;
    assert_eq!(status, 201, "{made}");
    let workspace = made["id"].as_str().unwrap().to_owned();
    let base = format!("/v1/workspaces/{workspace}");

    // Notes, then one group moving every one of them under a folder note.
    key.did(&format!("{base}/resources/notes/actions/open"), &json!({}))
        .await;
    let notebook = format!("{base}/resources/notes/Notes/actions");
    let folder = page_of(
        &key.did(
            &format!("{notebook}/new-note"),
            &json!({"title": "Imported"}),
        )
        .await,
    );
    let mut notes = Vec::with_capacity(NOTES);
    for index in 0..NOTES {
        notes.push(page_of(
            &key.did(
                &format!("{notebook}/new-note"),
                &json!({"title": format!("note {index}")}),
            )
            .await,
        ));
    }
    let moves: Vec<Value> = notes
        .iter()
        .map(|note| json!({"action": "move-note", "args": {"page": note, "parent": folder}}))
        .collect();
    let moved = key
        .did(&format!("{notebook}/group"), &json!({"actions": moves}))
        .await;
    assert_eq!(moved["status"], "ok", "{moved}");

    // A folder that fills more than one chunk of the tree.
    let mut entries = Vec::with_capacity(FILES);
    for index in 0..FILES {
        let (status, put) = json_of(
            key.send(
                hyper::Method::POST,
                &format!("{base}/uploads"),
                format!("file {index}\n").as_bytes(),
                &[("content-type", "application/octet-stream")],
            )
            .await,
        )
        .await;
        assert_eq!(status, 201, "{put}");
        entries.push(
            json!({"content": put["content"], "name": format!("f{index}.txt"), "dir": "repo"}),
        );
    }
    let opened = key
        .did(
            &format!("{base}/resources/files/actions/open"),
            &json!({"files": entries}),
        )
        .await;
    assert_eq!(opened["status"], "ok", "{opened}");

    // What the workspace keeps holds every one of them.
    let workspaces = on_server.hosted.server.workspaces().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let kept = parents_kept(&on_server, &workspace).await;
        let unmoved: Vec<&String> = notes
            .iter()
            .filter(|note| kept.get(*note).cloned().flatten().as_deref() != Some(&folder))
            .collect();
        let tree = tree_kept(workspaces, &workspace).await;
        let missing = (0..FILES)
            .filter(|index| !holds(&tree, "repo", &format!("f{index}.txt")))
            .count();
        if unmoved.is_empty() && missing == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace never kept every write of the key: {} notes not moved, {missing} files \
             not in its tree",
            unmoved.len()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Whether `tree` holds the file `dir/name`.
fn holds(tree: &fs_protocol::tree::Tree, dir: &str, name: &str) -> bool {
    use fs_protocol::files::FileNodeKind;
    let Some((folder, _)) =
        tree.children(fs_protocol::tree::ROOT_ID)
            .into_iter()
            .find(|(_, node)| {
                node.kind == FileNodeKind::Directory && node.name.as_bytes() == dir.as_bytes()
            })
    else {
        return false;
    };
    tree.children(folder)
        .into_iter()
        .any(|(_, node)| node.kind == FileNodeKind::File && node.name.as_bytes() == name.as_bytes())
}

/// What `server`'s tool `name` answers with `arguments`, which must succeed.
async fn did(
    server: &tilo_mcp::McpServer<tilo_session_client::remote::SessionActions>,
    name: &str,
    arguments: Value,
) -> String {
    let answer = server
        .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": name, "arguments": arguments}}))
        .await
        .expect("a call is answered");
    let text = answer["result"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        answer["result"]["isError"] != true && answer.get("error").is_none(),
        "{name} {arguments}: {answer}"
    );
    text
}

/// The note id an answer names after `"page":`.
fn page_in(text: &str) -> String {
    let at = text
        .find("\"page\":\"")
        .unwrap_or_else(|| panic!("the answer names its note: {text}"))
        .saturating_add("\"page\":\"".len());
    text.get(at..)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_default()
        .to_owned()
}

/// A person's logged-in computer, whose held session acts as its agent
/// (`tilo login`, then `tilo migrate`, `tilo do`), moves their things into a
/// workspace on a server someone else keeps: notes, a group moving them, and
/// issues made in one call. The server carries every call out and signs what
/// it changes, and all of it lands in what the workspace keeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computers_agent_moves_things_in_through_a_server() {
    Box::pin(a_computers_agent_moves_things_in()).await;
}

async fn a_computers_agent_moves_things_in() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (root, token) = person(&on_server, "Ada").await;
    let key = Key { api: &api, token };
    let (status, made) = key
        .post("/v1/workspaces", &json!({"name": "Company"}), &[])
        .await;
    assert_eq!(status, 201, "{made}");
    let workspace = made["id"].as_str().unwrap().to_owned();

    // The computer, logged in: a device of Ada's whose session is her agent.
    let computer = enrol(&root, "Ada's laptop", DeviceCapabilities::member());
    let agent = super::resource_support::open_admitted_peer(
        &on_server.hosted,
        &computer,
        &on_server.host,
        &workspace,
    )
    .await;
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Your agent").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = tilo_mcp::McpServer::new(
        tilo_session_client::remote::SessionActions::new(
            &agent.peer,
            agent.workspace,
            on_server.host.cert.device_public_key,
        ),
        workspace.clone(),
        person,
    )
    .with_agent_session("migrate", "Your agent");

    did(&server, "tilo_do", json!({"path": "notes#open"})).await;
    let folder = page_in(
        &did(
            &server,
            "tilo_do",
            json!({"path": "notes#new-note", "args": {"title": "Imported"}}),
        )
        .await,
    );
    let mut notes = Vec::with_capacity(NOTES);
    for index in 0..NOTES {
        notes.push(page_in(
            &did(
                &server,
                "tilo_do",
                json!({"path": "notes#new-note", "args": {"title": format!("note {index}")}}),
            )
            .await,
        ));
    }
    let moves: Vec<Value> = notes
        .iter()
        .map(|note| json!({"action": "move-note", "args": {"page": note, "parent": folder}}))
        .collect();
    did(
        &server,
        "tilo_do",
        json!({"path": "notes#group", "args": {"actions": moves}}),
    )
    .await;
    let issues: Vec<Value> = (0..NOTES)
        .map(|index| json!({"title": format!("Issue {index}")}))
        .collect();
    did(
        &server,
        "tilo_do",
        json!({"path": "tasks#create-many", "args": {"issues": issues}}),
    )
    .await;

    // Comments on the issues, in one group, as a tracker's history moves in.
    let issues = issues_kept(&on_server, &workspace).await;
    assert_eq!(issues.len(), NOTES, "every issue the agent made is kept");
    let comments: Vec<Value> = issues
        .iter()
        .take(COMMENTS)
        .map(|issue| json!({"action": "comment", "args": {"issue": issue, "text": "Moved in."}}))
        .collect();
    did(
        &server,
        "tilo_do",
        json!({"path": "tasks#group", "args": {"actions": comments}}),
    )
    .await;

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let kept = parents_kept(&on_server, &workspace).await;
        let unmoved = notes
            .iter()
            .filter(|note| kept.get(*note).cloned().flatten().as_deref() != Some(&folder))
            .count();
        let commented = comments_kept(&on_server, &workspace).await;
        if unmoved == 0 && commented == COMMENTS {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace never kept everything the agent moved in: {unmoved} notes not moved, \
             {commented} of {COMMENTS} comments"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// How many comments the agent moves in, in one group.
const COMMENTS: usize = 24;

/// Each part of the issues of `workspace` its server keeps, read.
async fn issue_parts(on_server: &OnServer, workspace: &str) -> Vec<tasks::issue_list::IssueList> {
    let workspaces = on_server.hosted.server.workspaces().unwrap();
    let mut parts = Vec::new();
    for index in 0.. {
        let part_id = tasks::issue_list::chain().part(index);
        let Some(list) = workspaces
            .resource_document(workspace, "so.tilo.tasks", part_id.as_str())
            .await
            .ok()
            .flatten()
            .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
            .and_then(|doc| {
                <tasks::issue_list::IssueList as ui::tilo_resource::Resource>::read(&doc).ok()
            })
        else {
            break;
        };
        parts.push(list);
    }
    parts
}

/// The ids of the issues of `workspace` its server keeps.
async fn issues_kept(on_server: &OnServer, workspace: &str) -> Vec<String> {
    issue_parts(on_server, workspace)
        .await
        .iter()
        .flat_map(|part| {
            part.issues
                .iter()
                .map(|(id, _)| id.to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// How many comments the issues of `workspace` its server keeps hold.
async fn comments_kept(on_server: &OnServer, workspace: &str) -> usize {
    issue_parts(on_server, workspace)
        .await
        .iter()
        .flat_map(|part| {
            part.issues
                .iter()
                .map(|(_, issue)| issue.comments.len())
                .collect::<Vec<_>>()
        })
        .sum()
}
