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
use super::hosted_documents::{content_in, put};
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

/// The titles of the notes the notebook of `workspace` keeps on the server.
async fn titles_kept(on_server: &OnServer, workspace: &str) -> std::collections::BTreeSet<String> {
    let workspaces = on_server.hosted.server.workspaces().unwrap();
    let mut titles = std::collections::BTreeSet::new();
    for index in 0.. {
        let part_id = notes::note_list::chain().part(index);
        let Some(part) = workspaces
            .resource_document(workspace, "so.tilo.notes", part_id.as_str())
            .await
            .ok()
            .flatten()
            .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
            .and_then(|doc| <notes::NoteList as ui::tilo_resource::Resource>::read(&doc).ok())
        else {
            break;
        };
        titles.extend(part.pages.iter().map(|(_, note)| note.title.clone()));
    }
    titles
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

    // Its person revokes the key's grant in the workspace: a write to the
    // tree's own document and one into a part of the notebook are refused
    // alike, and neither answers that it went in.
    let person = root.verifying_key().to_bytes();
    let grants = workspaces.grants_of(&workspace, &person).await.unwrap();
    assert_eq!(grants.len(), 1, "the workspace lists the key: {grants:?}");
    assert!(
        workspaces
            .revoke_grant(&workspace, &grants[0].id, tilo_clock::unix_now_ms())
            .await
            .unwrap()
    );
    let (status, put) = json_of(
        key.send(
            hyper::Method::POST,
            &format!("{base}/uploads"),
            b"after the revocation\n",
            &[("content-type", "application/octet-stream")],
        )
        .await,
    )
    .await;
    let tree_write = if status == 201 {
        key.post(
            &format!("{base}/resources/files/actions/open"),
            &json!({"content": put["content"], "name": "late.txt", "dir": ""}),
            &[],
        )
        .await
    } else {
        (status, put)
    };
    let part_write = key
        .post(
            &format!("{notebook}/new-note"),
            &json!({"title": "after the revocation"}),
            &[],
        )
        .await;
    for (what, (status, answer)) in [("the tree", tree_write), ("a part", part_write)] {
        assert!(
            !(200..300).contains(&status) || answer["status"] != "ok",
            "a revoked key's write to {what} answers an error: {status} {answer}"
        );
        assert!(
            answer.to_string().contains("tilo keys create"),
            "the refusal says how to approve a key again: {answer}"
        );
    }

    // Doing what it says gives a key that writes again.
    let page = enrol(&root, "Ada's browser again", DeviceCapabilities::member());
    let token = super::api_on_server::approved_key(&on_server, &root, &page, "Ada's backend").await;
    let again = Key { api: &api, token };
    again
        .did(
            &format!("{notebook}/new-note"),
            &json!({"title": "with the new key"}),
        )
        .await;
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

    // A folder that fills many chunks of the tree, as `tilo files put` of
    // a repository writes it: every file names one content, and the list
    // goes in as content of its own.
    let content = content_in(&put(&server, b"a line of a file\n").await);
    let entries: Vec<Value> = (0..FOLDER)
        .map(|index| json!({"content": content, "name": format!("f{index}.txt"), "dir": "repo"}))
        .collect();
    // In opens of a few thousand, each after the one before, as the
    // command sends a large folder: the chunks a later open fills are made
    // after the first open was decided.
    for part in entries.chunks(FOLDER_OPEN) {
        let list = content_in(&put(&server, &serde_json::to_vec(part).unwrap()).await);
        did(
            &server,
            "tilo_do",
            json!({"path": "files#open", "args": {"files": list}}),
        )
        .await;
    }

    // Pages moved in from another tool, one call each after the one before.
    for index in 0..PAGES {
        did(
            &server,
            "tilo_do",
            json!({"path": "notes#import-markdown", "args": {
                "title": format!("page {index}"),
                "markdown": format!("# Page {index}\n\n{}\n", "A paragraph moved in. ".repeat(40)),
                "images": [],
            }}),
        )
        .await;
    }

    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let tree = tree_kept(on_server.hosted.server.workspaces().unwrap(), &workspace).await;
        let missing = (0..FOLDER)
            .filter(|index| !holds(&tree, "repo", &format!("f{index}.txt")))
            .count();
        if missing == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace never kept every file the agent put: {missing} of {FOLDER} missing"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let kept = parents_kept(&on_server, &workspace).await;
        let unmoved = notes
            .iter()
            .filter(|note| kept.get(*note).cloned().flatten().as_deref() != Some(&folder))
            .count();
        let commented = comments_kept(&on_server, &workspace).await;
        let titles = titles_kept(&on_server, &workspace).await;
        let unimported = (0..PAGES)
            .filter(|index| !titles.contains(&format!("page {index}")))
            .count();
        if unmoved == 0 && commented == COMMENTS && unimported == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace never kept everything the agent moved in: {unmoved} notes not moved, \
             {commented} of {COMMENTS} comments, {unimported} of {PAGES} pages missing"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// More files than 22 chunks of the tree hold.
const FOLDER: usize = 22 * fs_protocol::tree::CHUNK_NODES + 300;

/// How many files one open puts.
const FOLDER_OPEN: usize = 8000;

/// How many pages the agent imports, one call each.
const PAGES: usize = 40;

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

/// A migration's tree, as an importer sends it: one group of `import-markdown`
/// makes a collection page and the documents under it, and a second group of
/// `move-note` nests them. The workspace must keep the tree, not a flat list:
/// a group that answers ok and changes nothing loses a customer's data.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_imported_tree_sent_as_import_then_move_groups_is_kept_nested() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (_root, token) = person(&on_server, "Ada").await;
    let key = Key { api: &api, token };
    let (status, made) = key
        .post("/v1/workspaces", &json!({"name": "Company"}), &[])
        .await;
    assert_eq!(status, 201, "{made}");
    let workspace = made["id"].as_str().unwrap().to_owned();
    let base = format!("/v1/workspaces/{workspace}");
    key.did(&format!("{base}/resources/notes/actions/open"), &json!({}))
        .await;
    let notebook = format!("{base}/resources/notes/Notes/actions");

    // As a migration sends it: a collection page, over a hundred documents
    // with pages of text, two deep, all imported in one group.
    let id = || uuid::Uuid::new_v4().to_string();
    let collection = id();
    let docs: Vec<String> = (0..103).map(|_| id()).collect();
    let body = |index: usize| {
        format!(
            "# Document {index}\n\n{}",
            format!("Paragraph of document {index}, written to be moved. ").repeat(80)
        )
    };
    let import = |note: &str, title: &str, markdown: &str| {
        json!({"action": "import-markdown", "args": {
            "title": title, "markdown": markdown, "images": [], "note": note, "origin": null,
        }})
    };
    let mut imports = vec![import(&collection, "Collection", "")];
    for (index, note) in docs.iter().enumerate() {
        imports.push(import(note, &format!("Doc {index}"), &body(index)));
    }
    let answered = key
        .did(&format!("{notebook}/group"), &json!({"actions": imports}))
        .await;
    assert_eq!(answered["status"], "ok", "{answered}");

    let move_note = |page: &str, parent: &str| json!({"action": "move-note", "args": {"page": page, "parent": parent, "before": null}});
    let mut moves = vec![move_note(&docs[0], &collection)];
    for (index, page) in docs.iter().enumerate().skip(1) {
        let parent = if index % 2 == 0 {
            &collection
        } else if index % 3 == 0 {
            &docs[0]
        } else {
            &docs[index - 1]
        };
        moves.push(move_note(page, parent));
    }
    let answered = key
        .did(&format!("{notebook}/group"), &json!({"actions": moves}))
        .await;
    assert_eq!(answered["status"], "ok", "{answered}");

    // What each document sits under: the collection page has none, the first
    // under the collection, and every other under the one before or the
    // collection, as the moves named.
    let mut expected = vec![(collection.clone(), None)];
    for (index, page) in docs.iter().enumerate() {
        let parent = if index == 0 {
            Some(collection.clone())
        } else if index % 2 == 0 {
            Some(collection.clone())
        } else if index % 3 == 0 {
            Some(docs[0].clone())
        } else {
            Some(docs[index - 1].clone())
        };
        expected.push((page.clone(), parent));
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let kept = parents_kept(&on_server, &workspace).await;
        if expected.iter().all(|(note, _)| kept.contains_key(note)) {
            for (note, parent) in &expected {
                assert_eq!(
                    kept[note].as_deref(),
                    parent.as_deref(),
                    "note {note} sits under {parent:?} as imported"
                );
            }
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace never kept the imported notes: {kept:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The agent's server over a computer of the person's, as that other test
/// starts it: a workspace made for it and the projection of that workspace
/// the agent calls.
async fn agent_on_a_server() -> (
    OnServer,
    tilo_mcp::McpServer<tilo_session_client::remote::SessionActions>,
) {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (root, token) = person(&on_server, "Ada").await;
    let key = Key { api: &api, token };
    let (status, made) = key
        .post("/v1/workspaces", &json!({"name": "Company"}), &[])
        .await;
    assert_eq!(status, 201, "{made}");
    let workspace = made["id"].as_str().unwrap().to_owned();
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
        workspace,
        person,
    )
    .with_agent_session("migrate", "Your agent");
    (on_server, server)
}

/// A notebook imported as a tree lists as one: the collection lists its root
/// pages only, a page lists its child pages beside its body, and two pages
/// with the same title are told apart by a readable name the path takes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_imported_notebook_lists_as_the_tree_it_is() {
    let (_on_server, server) = agent_on_a_server().await;
    did(&server, "tilo_do", json!({"path": "notes#open"})).await;
    let make = |title: &str| json!({"path": "notes#new-note", "args": {"title": title}});
    let tilo = page_in(&did(&server, "tilo_do", make("Tilo")).await);
    let other = page_in(&did(&server, "tilo_do", make("Other")).await);
    // Three pages titled alike, each with words of its own: two under one
    // parent, one under another.
    let todos: Vec<String> = (0..3).map(|_| uuid::Uuid::new_v4().to_string()).collect();
    let imports = json!({"actions": todos.iter().enumerate().map(|(at, note)| {
        json!({"action": "import-markdown", "args": {
            "title": "TODO", "markdown": format!("the {at} todo"), "images": [],
            "note": note, "origin": null,
        }})
    }).collect::<Vec<_>>()});
    did(
        &server,
        "tilo_do",
        json!({"path": "notes#group", "args": imports}),
    )
    .await;
    let moves = json!({"actions": [
        {"action": "move-note", "args": {"page": todos[0], "parent": tilo}},
        {"action": "move-note", "args": {"page": todos[1], "parent": other}},
        {"action": "move-note", "args": {"page": todos[2], "parent": tilo}},
    ]});
    did(
        &server,
        "tilo_do",
        json!({"path": "notes#group", "args": moves}),
    )
    .await;

    // The collection lists the pages at its top, and nothing under them.
    let roots = did(&server, "tilo_ls", json!({"path": "notes/Notes"})).await;
    assert!(roots.lines().any(|line| line.contains("Tilo")), "{roots}");
    assert!(roots.lines().any(|line| line.contains("Other")), "{roots}");
    assert!(
        !roots.contains("TODO"),
        "a child page is not at the collection's top: {roots}"
    );
    // A page lists its own children beside its body.
    let under_tilo = did(&server, "tilo_ls", json!({"path": "notes/Notes/Tilo"})).await;
    assert!(
        under_tilo.lines().any(|line| line.contains("TODO")),
        "{under_tilo}"
    );
    assert!(
        under_tilo.lines().any(|line| line.contains("Body of Tilo")),
        "{under_tilo}"
    );
    // Two pages titled alike under one parent are told apart by their place
    // among them, and that name is a path.
    assert!(
        under_tilo.lines().any(|line| line.contains("TODO 2")),
        "the second TODO wears its place: {under_tilo}"
    );
    assert!(
        !under_tilo.contains(&todos[0]) && !under_tilo.contains(&todos[2]),
        "no page is named by its id: {under_tilo}"
    );
    let second_todo = did(
        &server,
        "tilo_read",
        json!({"path": "notes/Notes/Tilo/TODO 2"}),
    )
    .await;
    assert!(second_todo.contains("the 2 todo"), "{second_todo}");
    // Another parent's page keeps the title to itself.
    let under_other = did(&server, "tilo_ls", json!({"path": "notes/Notes/Other"})).await;
    assert!(
        under_other.lines().any(|line| line.contains("TODO")),
        "{under_other}"
    );
    assert!(!under_other.contains("TODO 2"), "{under_other}");
}
